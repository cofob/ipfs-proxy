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
use tokio::task::JoinSet;

#[macro_use]
extern crate log;

#[derive(Deserialize)]
struct CidPage {
    cid: String,
}

struct SharedState {
    allowed_cids: Mutex<Vec<String>>,
    client: Client,
    api_key: String,
    ipfs_gateways: Vec<String>,
}

async fn fetch_cid_page(
    client: &Client,
    api_key: &str,
    page: usize,
    fetch_page_size: usize,
) -> Result<Vec<String>> {
    let url = format!(
        "https://api.web3.storage/user/uploads?page={}&size={}",
        page, fetch_page_size
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
        debug!("CID: {}", cid);
        let mut tasks = JoinSet::new();
        for gateway in &state.ipfs_gateways {
            let url = format!("{}/{}", gateway, path);
            let state = state.clone();
            tasks.spawn(async move {
                debug!("Requesting gateway: {}", url);
                let res = state.client.get(&url).send().await;
                if res.is_err() {
                    return None;
                }
                let res = res.expect("Failed to request gateway");
                if res.status().is_success() {
                    debug!("Gateway responded: {}", url);
                    return Some(res);
                } else {
                    return None;
                }
            });
        }
        while let Some(res) = tasks.join_next().await {
            if res.is_ok() {
                let res = res.unwrap();
                if res.is_some() {
                    let res = res.unwrap();
                    return Ok(Response::builder()
                        .header(
                            "Content-Type",
                            res.headers().get("Content-Type").unwrap().to_str().unwrap(),
                        )
                        .body(res.bytes().await.unwrap().into())
                        .unwrap());
                }
            }
        }
        Err(CustomError::InternalServerError)
    } else {
        debug!("CID not found: {}", cid);
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

async fn cid_updater(state: Arc<SharedState>, fetch_page_size: usize) -> Result<()> {
    debug!("CID updater started");
    let time_start = std::time::Instant::now();
    let mut page = 1;
    let mut updated = 0;
    loop {
        let mut changed = false;
        let cids = fetch_cid_page(&state.client, &state.api_key, page, fetch_page_size).await?;
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
    let time_end = std::time::Instant::now();
    debug!(
        "CID updater finished in {}ms",
        time_end.duration_since(time_start).as_millis()
    );
    if updated > 0 {
        info!("Fetched {} new CID's", updated);
    }
    Ok(())
}

async fn cid_updater_scheduler(
    state: Arc<SharedState>,
    fetch_interval: usize,
    fetch_page_size: usize,
) {
    loop {
        let time_start = std::time::Instant::now();
        let current = tokio::spawn(cid_updater(state.clone(), fetch_page_size));
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            if current.is_finished() {
                break;
            }
        }
        let time_end = std::time::Instant::now();
        let time_elapsed = time_end.duration_since(time_start).as_secs();
        if time_elapsed < fetch_interval as u64 {
            tokio::time::sleep(std::time::Duration::from_secs(
                fetch_interval as u64 - time_elapsed,
            ))
            .await;
        } else {
            warn!("CID updater took longer than fetch interval");
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // set default log level to info
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "info");
    }
    pretty_env_logger::init();

    // get web3.stogade API key from env
    let api_key = std::env::var("STORAGE_API_KEY").expect("STORAGE_API_KEY must be set");

    // get host and port from env
    let host = std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0:3000".to_string());

    // get IPFS_GATEWAYS
    let ipfs_gateways = std::env::var("IPFS_GATEWAYS")
        .unwrap_or_else(|_| {
            "https://ipfs.io/ipfs,https://w3s.link/ipfs,https://cloudflare-ipfs.com/ipfs,https://hardbin.com/ipfs,https://gateway.pinata.cloud/ipfs"
                .to_string()
        })
        .split(",")
        .map(|x| x.to_string())
        .collect();

    // get FETCH_INTERVAL
    let fetch_interval = std::env::var("FETCH_INTERVAL")
        .unwrap_or_else(|_| "3".to_string())
        .parse::<usize>()
        .unwrap_or(2);

    // get FETCH_PAGE_SIZE
    let fetch_page_size = std::env::var("FETCH_PAGE_SIZE")
        .unwrap_or_else(|_| "500".to_string())
        .parse::<usize>()
        .unwrap_or(100);

    // create a reqwest client
    let client = Client::new();

    let shared_state = Arc::new(SharedState {
        allowed_cids: Mutex::new(Vec::with_capacity(1000)),
        client,
        api_key,
        ipfs_gateways,
    });

    tokio::spawn(cid_updater_scheduler(
        shared_state.clone(),
        fetch_interval,
        fetch_page_size,
    ));

    // build our application with a single route
    let app = Router::new()
        .route("/ipfs/*path", get(handler))
        .with_state(shared_state);

    // run it with hyper on localhost:3000
    info!("Listening on {}", host);
    axum::Server::bind(&host.parse().unwrap())
        .serve(app.into_make_service())
        .await
        .unwrap();

    Ok(())
}
