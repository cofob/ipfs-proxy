use anyhow::Result;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::Response;
use axum::response::IntoResponse;
use axum::Json;
use axum::{routing::get, Router};
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use std::sync::{Arc, Mutex};

const IPFS_GATEWAY: &str = "https://w3s.link/ipfs";

#[derive(Deserialize)]
struct CidPage {
    cid: String,
}

struct SharedState {
    allowed_cids: Mutex<Vec<String>>,
    client: Client,
    api_key: String,
}

async fn fetch_cid_page(client: &Client, api_key: &str, page: usize) -> Result<Vec<String>> {
    let url = format!(
        "https://api.web3.storage/user/uploads?page={}&size=500",
        page
    );
    let res = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", api_key))
        .send()
        .await?;
    if res.status() == 416 {
        return Ok(Vec::new());
    } else if !res.status().is_success() {
        return Err(anyhow::anyhow!(
            "Failed to fetch CID page: {}",
            res.status()
        ));
    }
    let body: Vec<CidPage> = res.json().await?;
    Ok(body.into_iter().map(|x| x.cid).collect())
}

async fn handler(
    State(state): State<Arc<SharedState>>,
    Path(path): Path<String>,
) -> Result<Response<Body>, CustomError> {
    let cid = path.split('/').next().unwrap().to_string();

    let is_allowed;

    {
        let mut attempts = 0;
        loop {
            if attempts > 10 {
                return Err(CustomError::InternalServerError);
            }
            if state.allowed_cids.try_lock().is_ok() {
                let lock = state.allowed_cids.lock().unwrap();
                is_allowed = lock.contains(&cid);
                break;
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            attempts += 1;
        }
    }
    if is_allowed {
        println!("CID: {}", cid);
        let url = format!("{}/{}", IPFS_GATEWAY, path);
        let res = state.client.get(&url).send().await;
        if res.is_err() {
            return Err(CustomError::InternalServerError);
        }
        let res = res.unwrap();
        if res.status().is_success() {
            return Ok(Response::builder()
                .status(res.status())
                .header(
                    "Content-Type",
                    res.headers().get("Content-Type").unwrap().to_str().unwrap(),
                )
                .body(res.bytes().await.unwrap().into())
                .unwrap());
        } else {
            return Err(CustomError::InternalServerError);
        }
    } else {
        println!("CID not found: {}", cid);
        return Err(CustomError::CidNotFound);
    }
}

pub enum CustomError {
    InternalServerError,
    CidNotFound,
}

impl IntoResponse for CustomError {
    fn into_response(self) -> axum::response::Response {
        let (status, error_message) = match self {
            Self::InternalServerError => {
                (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error")
            }
            Self::CidNotFound => (StatusCode::FORBIDDEN, "CID not allowed"),
        };
        (status, Json(serde_json::json!({ "error": error_message }))).into_response()
    }
}

async fn cid_updater(state: Arc<SharedState>) {
    let mut page = 1;
    let mut updated = 0;
    loop {
        let mut changed = false;
        let cids = fetch_cid_page(&state.client, &state.api_key, page)
            .await
            .unwrap();
        if cids.len() == 0 {
            break;
        };
        loop {
            if state.allowed_cids.try_lock().is_ok() {
                let mut lock = state.allowed_cids.lock().unwrap();
                for cid in cids {
                    if !lock.contains(&cid) {
                        lock.push(cid);
                        updated += 1;
                        changed = true;
                    }
                }
                if !changed {
                    break;
                }
                page += 1;
                break;
            } else {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
        if !changed {
            break;
        }
    }
    if updated > 0 {
        println!("Fetched {} new CID's", updated);
    }
}

async fn cid_updater_scheduler(state: Arc<SharedState>) {
    loop {
        cid_updater(state.clone()).await;
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // get web3.stogade API key from env
    let api_key = std::env::var("STORAGE_API_KEY").expect("STORAGE_API_KEY must be set");

    // get host and port from env
    let host = std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0:3000".to_string());

    // create a reqwest client
    let client = Client::new();

    // build a vector of allowed IPFS CID's
    println!("Fetching allowed CIDs, this may take a while...");
    let mut allowed_cids: Vec<String> = Vec::with_capacity(1000);
    {
        let mut page = 1;
        loop {
            println!("Fetching page {}...", page);
            let cids = fetch_cid_page(&client, &api_key, page).await?;
            if cids.len() == 0 {
                break;
            }
            page += 1;
            allowed_cids.extend(cids);
        }
    }
    println!("Found {} allowed CIDs", allowed_cids.len());

    let shared_state = Arc::new(SharedState {
        allowed_cids: Mutex::new(allowed_cids),
        client,
        api_key,
    });

    tokio::spawn(cid_updater_scheduler(shared_state.clone()));

    // build our application with a single route
    let app = Router::new()
        .route("/ipfs/*path", get(handler))
        .with_state(shared_state);

    // run it with hyper on localhost:3000
    println!("Listening on {}", host);
    axum::Server::bind(&host.parse().unwrap())
        .serve(app.into_make_service())
        .await
        .unwrap();

    Ok(())
}
