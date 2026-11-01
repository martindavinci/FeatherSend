//! The extension HTTPS server: mutual TLS, fixed routes, body limits. What
//! each request means is decided by an [`ExtHandler`] (the backend), which
//! receives the caller's certificate fingerprint with every request.

use super::dto::*;
use super::{API, ExtError, MAX_JSON_BYTES, tls};
use crate::clip::MAX_CLIPBOARD_BYTES;
use crate::identity::Identity;
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, oneshot};

pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = Result<T, ExtError>> + Send + 'a>>;
pub type BodyStream = Pin<Box<dyn Stream<Item = io::Result<Bytes>> + Send>>;

const MAX_CONNECTIONS: usize = 32;

/// Who is calling: the fingerprint comes from the TLS client certificate,
/// so it cannot be spoofed.
#[derive(Clone, Debug)]
pub struct Peer {
    pub fingerprint: String,
    pub addr: SocketAddr,
}

pub trait ExtHandler: Send + Sync + 'static {
    /// Paired devices may use the clipboard and transfer endpoints.
    fn is_trusted(&self, fingerprint: &str) -> bool;
    fn info(&self) -> InfoDto;
    fn pair_commit(&self, peer: Peer, req: PairCommitReq) -> BoxFut<'_, PairCommitResp>;
    fn pair_reveal(&self, peer: Peer, req: PairRevealReq) -> BoxFut<'_, ()>;
    fn pair_confirm(&self, peer: Peer, req: PairConfirmReq) -> BoxFut<'_, PairConfirmResp>;
    fn clipboard(&self, peer: Peer, mime: String, body: Bytes) -> BoxFut<'_, ()>;
    fn prepare(&self, peer: Peer, req: PrepareReq) -> BoxFut<'_, PrepareResp>;
    fn upload(
        &self,
        peer: Peer,
        transfer_id: String,
        file_id: String,
        offset: u64,
        body: BodyStream,
    ) -> BoxFut<'_, ()>;
    fn finish(&self, peer: Peer, transfer_id: String) -> BoxFut<'_, FinishResp>;
    fn cancel(&self, peer: Peer, transfer_id: String) -> BoxFut<'_, ()>;
}

pub struct ExtServerHandle {
    port: u16,
    stop: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
}

impl ExtServerHandle {
    pub fn port(&self) -> u16 {
        self.port
    }

    pub async fn stop(mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        let _ = (&mut self.task).await;
    }
}

pub struct ExtServer;

impl ExtServer {
    /// Binds `0.0.0.0:port` (0 picks a free port) and serves until stopped.
    pub async fn start(
        port: u16,
        id: &Identity,
        handler: Arc<dyn ExtHandler>,
    ) -> anyhow::Result<ExtServerHandle> {
        let acceptor = tokio_rustls::TlsAcceptor::from(tls::server_config(id)?);
        let listener = TcpListener::bind(("0.0.0.0", port)).await?;
        let port = listener.local_addr()?.port();
        let (stop_tx, mut stop_rx) = oneshot::channel::<()>();
        let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
        let task = tokio::spawn(async move {
            loop {
                let (tcp, addr) = tokio::select! {
                    _ = &mut stop_rx => break,
                    accepted = listener.accept() => match accepted {
                        Ok(pair) => pair,
                        Err(e) => {
                            tracing::warn!("extension accept failed: {e}");
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                            continue;
                        }
                    },
                };
                let Ok(permit) = slots.clone().try_acquire_owned() else {
                    continue; // too many connections: drop this one
                };
                let acceptor = acceptor.clone();
                let handler = handler.clone();
                tokio::spawn(async move {
                    let _permit = permit;
                    let tls = match tokio::time::timeout(
                        std::time::Duration::from_secs(15),
                        acceptor.accept(tcp),
                    )
                    .await
                    {
                        Ok(Ok(tls)) => tls,
                        _ => return,
                    };
                    let Some(fingerprint) = tls::peer_fingerprint(tls.get_ref().1) else { return };
                    let peer = Peer { fingerprint, addr };
                    let service = hyper::service::service_fn(move |req| {
                        let handler = handler.clone();
                        let peer = peer.clone();
                        async move { Ok::<_, hyper::Error>(route(handler, peer, req).await) }
                    });
                    let _ = hyper::server::conn::http1::Builder::new()
                        .timer(hyper_util::rt::TokioTimer::new())
                        .header_read_timeout(std::time::Duration::from_secs(30))
                        .serve_connection(TokioIo::new(tls), service)
                        .await;
                });
            }
        });
        Ok(ExtServerHandle { port, stop: Some(stop_tx), task })
    }
}

type Resp = Response<Full<Bytes>>;

fn status(code: u16, message: &str) -> Resp {
    let mut resp = Response::new(Full::new(Bytes::copy_from_slice(message.as_bytes())));
    *resp.status_mut() = StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    resp
}

fn json<T: serde::Serialize>(value: &T) -> Resp {
    match serde_json::to_vec(value) {
        Ok(body) => {
            let mut resp = Response::new(Full::new(Bytes::from(body)));
            resp.headers_mut()
                .insert(hyper::header::CONTENT_TYPE, "application/json".parse().unwrap());
            resp
        }
        Err(e) => status(500, &e.to_string()),
    }
}

fn reply<T: serde::Serialize>(result: Result<T, ExtError>) -> Resp {
    match result {
        Ok(value) => json(&value),
        Err(e) => status(e.status, &e.message),
    }
}

fn empty(result: Result<(), ExtError>) -> Resp {
    match result {
        Ok(()) => status(204, ""),
        Err(e) => status(e.status, &e.message),
    }
}

/// Whole bodies (JSON, clipboard) must arrive within this time.
const BODY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

async fn read_limited(body: Incoming, limit: usize) -> Result<Bytes, ExtError> {
    let collected = tokio::time::timeout(BODY_TIMEOUT, Limited::new(body, limit).collect())
        .await
        .map_err(|_| ExtError::new(408, "body timeout"))?;
    match collected {
        Ok(collected) => Ok(collected.to_bytes()),
        Err(e) if e.is::<http_body_util::LengthLimitError>() => {
            Err(ExtError::new(413, "body too large"))
        }
        Err(e) => Err(ExtError::bad_request(e.to_string())),
    }
}

async fn read_json<T: serde::de::DeserializeOwned>(body: Incoming) -> Result<T, ExtError> {
    let bytes = read_limited(body, MAX_JSON_BYTES).await?;
    serde_json::from_slice(&bytes).map_err(|e| ExtError::bad_request(e.to_string()))
}

fn query_u64(req: &Request<Incoming>, key: &str) -> Option<u64> {
    req.uri().query()?.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| v.parse().ok()).flatten()
    })
}

async fn route(handler: Arc<dyn ExtHandler>, peer: Peer, req: Request<Incoming>) -> Resp {
    let path = req.uri().path().to_string();
    let Some(rest) = path.strip_prefix(API) else { return status(404, "not found") };
    let segments: Vec<&str> = rest.trim_start_matches('/').split('/').collect();
    let method = req.method().clone();
    // Everything except info and pairing is for paired devices only: refuse
    // before reading a body, so unpaired peers cannot hold connections open.
    let protected = !matches!(segments.as_slice(), ["info"] | ["pair", ..]);
    if protected && !handler.is_trusted(&peer.fingerprint) {
        return status(403, "not paired");
    }
    match (method, segments.as_slice()) {
        (Method::GET, ["info"]) => json(&handler.info()),
        (Method::POST, ["pair", "commit"]) => match read_json(req.into_body()).await {
            Ok(body) => reply(handler.pair_commit(peer, body).await),
            Err(e) => status(e.status, &e.message),
        },
        (Method::POST, ["pair", "reveal"]) => match read_json(req.into_body()).await {
            Ok(body) => empty(handler.pair_reveal(peer, body).await),
            Err(e) => status(e.status, &e.message),
        },
        (Method::POST, ["pair", "confirm"]) => match read_json(req.into_body()).await {
            Ok(body) => reply(handler.pair_confirm(peer, body).await),
            Err(e) => status(e.status, &e.message),
        },
        (Method::POST, ["clipboard"]) => {
            let mime = req
                .headers()
                .get(hyper::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            match read_limited(req.into_body(), MAX_CLIPBOARD_BYTES).await {
                Ok(bytes) => empty(handler.clipboard(peer, mime, bytes).await),
                Err(e) => status(e.status, &e.message),
            }
        }
        (Method::POST, ["transfer", "prepare"]) => match read_json(req.into_body()).await {
            Ok(body) => reply(handler.prepare(peer, body).await),
            Err(e) => status(e.status, &e.message),
        },
        (Method::PUT, ["transfer", transfer_id, file_id]) => {
            let Some(offset) = query_u64(&req, "offset") else {
                return status(400, "missing offset");
            };
            let (transfer_id, file_id) = (transfer_id.to_string(), file_id.to_string());
            let stream =
                http_body_util::BodyStream::new(req.into_body()).filter_map(|frame| async {
                    match frame {
                        Ok(frame) => frame.into_data().ok().map(Ok),
                        Err(e) => Some(Err(io::Error::other(e))),
                    }
                });
            empty(handler.upload(peer, transfer_id, file_id, offset, Box::pin(stream)).await)
        }
        (Method::POST, ["transfer", transfer_id, "finish"]) => {
            reply(handler.finish(peer, transfer_id.to_string()).await)
        }
        (Method::POST, ["transfer", transfer_id, "cancel"]) => {
            empty(handler.cancel(peer, transfer_id.to_string()).await)
        }
        _ => status(404, "not found"),
    }
}
