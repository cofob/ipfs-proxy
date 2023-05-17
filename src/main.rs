use anyhow::Result;
use axum::body::Body;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::Response;
use axum::response::IntoResponse;
use axum::Json;
use axum::{routing::get, Router};
use futures::Future;
use futures::Stream;
use reqwest::Error;
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::fs::File;
use tokio::fs::OpenOptions;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufWriter;
use tokio::sync::RwLock;
use tokio::task::JoinSet;
use tokio_util::io::ReaderStream;

struct CachingStream<S> {
    inner: S,
    writer: BufWriter<File>,
}

impl<S> CachingStream<S>
where
    S: Stream<Item = Result<Bytes, reqwest::Error>> + Unpin,
{
    fn new(inner: S, writer: BufWriter<File>) -> Self {
        Self { inner, writer }
    }
}

impl<S> Stream for CachingStream<S>
where
    S: Stream<Item = Result<Bytes, Error>> + Unpin,
{
    type Item = Result<Bytes, Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.as_mut().get_mut();
        match Pin::new(&mut this.inner).poll_next(cx) {
            Poll::Ready(Some(Ok(data))) => {
                let data_clone = data.clone();
                let fut = this.writer.write_all(&data);
                tokio::pin!(fut);
                match fut.poll(cx) {
                    Poll::Ready(Ok(_)) => {
                        let _ = this.writer.flush();
                        Poll::Ready(Some(Ok(data_clone)))
                    }
                    Poll::Ready(Err(_)) => Poll::Ready(None),
                    Poll::Pending => Poll::Pending,
                }
            }
            Poll::Ready(Some(Err(e))) => Poll::Ready(Some(Err(e))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

#[macro_use]
extern crate log;

#[derive(Deserialize)]
struct CidPage {
    cid: String,
}

struct SharedState {
    allowed_cids: RwLock<Vec<String>>,
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
    let url = format!("https://api.web3.storage/user/uploads?page={page}&size={fetch_page_size}");
    let res = client
        .get(&url)
        .header("Authorization", format!("Bearer {api_key}"))
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
) -> Result<impl IntoResponse, CustomError> {
    let cid = path.split('/').next().unwrap().to_string();

    let is_allowed = true;
    let cache_path = format!("./cache/{}", cid.replace("/", "_"));
    let cache_meta_path = format!("{}.meta", cache_path);

    // {
    //     let lock = state.allowed_cids.read().await;
    //     is_allowed = lock.contains(&cid);
    // }
    if is_allowed {
        debug!("CID: {}", cid);
        if PathBuf::from(&cache_path).exists() && PathBuf::from(&cache_meta_path).exists() {
            debug!("Serving from cache: {}", cache_path);

            // Load meta data
            let mut file = File::open(&cache_meta_path).await.unwrap();
            let mut meta = String::new();
            file.read_to_string(&mut meta).await.unwrap();
            let meta: Vec<&str> = meta.split(' ').collect();
            let content_type = meta[0].to_string();
            let content_length = meta[1].parse::<u64>().unwrap();

            // Load file
            let file = File::open(&cache_path).await.unwrap();
            let stream = ReaderStream::new(file);

            // Return response
            let body = Body::wrap_stream(stream);
            return Ok(Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", content_type)
                .header("Content-Length", content_length)
                .header("Content-Type", "text/plain")
                .body(body)
                .unwrap());
        } else {
            let mut tasks = JoinSet::new();
            for gateway in &state.ipfs_gateways {
                let url = format!("{}/{}", gateway, path);
                let state = state.clone();
                let cache_path = cache_path.clone();
                let cache_meta_path = cache_meta_path.clone();
                tasks.spawn(async move {
                    debug!("Requesting gateway: {}", url);
                    let res = state.client.get(&url).send().await;
                    if res.is_err() {
                        return None;
                    }
                    let res = res.expect("Failed to request gateway");
                    if res.status().is_success() {
                        debug!("Gateway responded: {}", url);

                        let content_type = res
                            .headers()
                            .get("Content-Type")
                            .unwrap()
                            .to_str()
                            .unwrap()
                            .to_string();
                        let content_length = match res.headers().get("Content-Length") {
                            Some(x) => x.to_str().unwrap().to_string(),
                            None => "0".to_string(),
                        };
                        let status = res.status();

                        // Save meta data
                        let mut file = OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(&cache_meta_path)
                            .await
                            .unwrap();
                        let meta = format!("{} {}", content_type, content_length);
                        file.write_all(meta.as_bytes()).await.unwrap();
                        file.flush().await.unwrap();
                        file.shutdown().await.unwrap();

                        let stream = res.bytes_stream();
                        let file = OpenOptions::new()
                            .write(true)
                            .create_new(true)
                            .open(&cache_path)
                            .await
                            .unwrap();
                        let writer = BufWriter::new(file);
                        let stream = CachingStream::new(stream, writer);
                        Some((status, content_type, content_length, stream))
                    } else {
                        None
                    }
                });
            }
            while let Some(Ok(Some((status, content_type, content_length, stream)))) =
                tasks.join_next().await
            {
                if !status.is_success() {
                    continue;
                }
                return Ok(Response::builder()
                    .status(status)
                    .header("Content-Type", content_type)
                    .header("Content-Length", content_length)
                    .body(Body::wrap_stream(stream))
                    .unwrap());
            }
        }
        Err(CustomError::InternalServerError)
    } else {
        debug!("CID not found: {}", cid);
        Err(CustomError::CidNotFound)
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
        if cids.is_empty() {
            break;
        };
        let mut lock = state.allowed_cids.write().await;
        for cid in &cids {
            if !lock.contains(cid) {
                lock.push(cid.to_string());
                updated += 1;
                changed = true;
            }
        }
        if !changed {
            break;
        }
        page += 1;
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
            "https://ipfs.io/ipfs,https://cloudflare-ipfs.com/ipfs,https://hardbin.com/ipfs,https://gateway.pinata.cloud/ipfs"
                .to_string()
        })
        .split(',')
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

    // create a cache folder
    let cache_folder = std::env::var("CACHE_FOLDER").unwrap_or_else(|_| "cache".to_string());
    std::fs::create_dir_all(&cache_folder).expect("Failed to create cache folder");

    // create a reqwest client
    let client = Client::new();

    let shared_state = Arc::new(SharedState {
        allowed_cids: RwLock::new(Vec::with_capacity(1000)),
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
