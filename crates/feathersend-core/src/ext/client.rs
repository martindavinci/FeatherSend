//! Client for another device's extension server, pinned to its fingerprint.

use super::dto::*;
use super::{API, ExtError, tls};
use crate::identity::Identity;
use crate::paths::long_path;
use futures_util::StreamExt;
use localsend::reqwest;
use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncSeekExt;
use tokio_util::sync::CancellationToken;

pub const UPLOAD_CHUNK: usize = 256 * 1024;

pub type Progress = Arc<dyn Fn(u64) + Send + Sync>;

#[derive(Clone)]
pub struct ExtClient {
    http: reqwest::Client,
    base: String,
}

fn transport(e: impl std::fmt::Display) -> ExtError {
    // 0: no HTTP status, the request did not get an answer.
    ExtError::new(0, e.to_string())
}

impl ExtClient {
    /// `timeout` bounds whole requests; uploads use no overall timeout.
    pub fn new(
        id: &Identity,
        peer_fp: &str,
        host: IpAddr,
        port: u16,
        timeout: Option<Duration>,
    ) -> anyhow::Result<ExtClient> {
        let mut builder = reqwest::Client::builder()
            .tls_backend_preconfigured(tls::client_config(id, peer_fp)?)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5));
        if let Some(timeout) = timeout {
            builder = builder.timeout(timeout);
        }
        let host = match host {
            IpAddr::V4(v4) => v4.to_string(),
            IpAddr::V6(v6) => format!("[{v6}]"),
        };
        Ok(ExtClient { http: builder.build()?, base: format!("https://{host}:{port}{API}") })
    }

    async fn check(resp: reqwest::Response) -> Result<reqwest::Response, ExtError> {
        let code = resp.status().as_u16();
        if resp.status().is_success() {
            return Ok(resp);
        }
        let message = resp.text().await.unwrap_or_default();
        Err(ExtError::new(code, message))
    }

    async fn post_json<B: serde::Serialize, T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, ExtError> {
        let resp = self
            .http
            .post(format!("{}{path}", self.base))
            .json(body)
            .send()
            .await
            .map_err(transport)?;
        Self::check(resp).await?.json().await.map_err(transport)
    }

    async fn post_empty<B: serde::Serialize>(&self, path: &str, body: &B) -> Result<(), ExtError> {
        let resp = self
            .http
            .post(format!("{}{path}", self.base))
            .json(body)
            .send()
            .await
            .map_err(transport)?;
        Self::check(resp).await.map(|_| ())
    }

    pub async fn info(&self) -> Result<InfoDto, ExtError> {
        let resp = self.http.get(format!("{}/info", self.base)).send().await.map_err(transport)?;
        Self::check(resp).await?.json().await.map_err(transport)
    }

    pub async fn pair_commit(&self, req: &PairCommitReq) -> Result<PairCommitResp, ExtError> {
        self.post_json("/pair/commit", req).await
    }

    pub async fn pair_reveal(&self, req: &PairRevealReq) -> Result<(), ExtError> {
        self.post_empty("/pair/reveal", req).await
    }

    pub async fn pair_confirm(&self, req: &PairConfirmReq) -> Result<PairConfirmResp, ExtError> {
        self.post_json("/pair/confirm", req).await
    }

    pub async fn clipboard(&self, mime: &str, bytes: Vec<u8>) -> Result<(), ExtError> {
        let resp = self
            .http
            .post(format!("{}/clipboard", self.base))
            .header("Content-Type", mime)
            .body(bytes)
            .send()
            .await
            .map_err(transport)?;
        Self::check(resp).await.map(|_| ())
    }

    pub async fn prepare(&self, req: &PrepareReq) -> Result<PrepareResp, ExtError> {
        self.post_json("/transfer/prepare", req).await
    }

    /// Streams `path` from byte `offset` to its end. `progress` receives the
    /// total bytes the receiver has (offset included).
    pub async fn upload(
        &self,
        transfer_id: &str,
        file_id: &str,
        offset: u64,
        path: &Path,
        progress: Progress,
        cancel: CancellationToken,
    ) -> Result<(), ExtError> {
        let mut file = tokio::fs::File::open(long_path(path)).await.map_err(transport)?;
        file.seek(std::io::SeekFrom::Start(offset)).await.map_err(transport)?;
        let mut sent = offset;
        let stop = cancel.clone();
        let stream =
            tokio_util::io::ReaderStream::with_capacity(file, UPLOAD_CHUNK).map(move |chunk| {
                // The connection task polls this and outlives the select below: without
                // this check the rest of the file still goes out after a cancel.
                if stop.is_cancelled() {
                    return Err(std::io::Error::other("cancelled"));
                }
                if let Ok(bytes) = &chunk {
                    sent += bytes.len() as u64;
                    progress(sent);
                }
                chunk
            });
        let request = self
            .http
            .put(format!("{}/transfer/{transfer_id}/{file_id}?offset={offset}", self.base))
            .body(reqwest::Body::wrap_stream(stream))
            .send();
        // Biased: a cancel also fails the body, and it must not read as a network error.
        let resp = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(ExtError::new(0, "cancelled")),
            resp = request => resp.map_err(transport)?,
        };
        Self::check(resp).await.map(|_| ())
    }

    pub async fn finish(&self, transfer_id: &str) -> Result<FinishResp, ExtError> {
        self.post_json(&format!("/transfer/{transfer_id}/finish"), &()).await
    }

    pub async fn cancel(&self, transfer_id: &str) -> Result<(), ExtError> {
        self.post_empty(&format!("/transfer/{transfer_id}/cancel"), &()).await
    }
}

#[cfg(test)]
mod tests {
    use super::super::server::{BodyStream, BoxFut, ExtHandler, ExtServer, Peer};
    use super::*;
    use crate::identity::tests::test_identity;
    use bytes::Bytes;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Mock {
        seen: Mutex<Vec<String>>,
        uploaded: Mutex<Vec<u8>>,
    }

    impl ExtHandler for Mock {
        fn is_trusted(&self, _: &str) -> bool {
            true
        }
        fn info(&self) -> InfoDto {
            InfoDto {
                alias: "mock".into(),
                fingerprint: test_identity(0).fingerprint,
                version: "0.1.0".into(),
                features: vec!["pair".into()],
            }
        }
        fn pair_commit(&self, peer: Peer, _: PairCommitReq) -> BoxFut<'_, PairCommitResp> {
            self.seen.lock().unwrap().push(peer.fingerprint);
            Box::pin(async { Ok(PairCommitResp { pair_id: "p".into(), nonce_b: "00".into() }) })
        }
        fn pair_reveal(&self, _: Peer, _: PairRevealReq) -> BoxFut<'_, ()> {
            Box::pin(async { Ok(()) })
        }
        fn pair_confirm(&self, _: Peer, _: PairConfirmReq) -> BoxFut<'_, PairConfirmResp> {
            Box::pin(async { Ok(PairConfirmResp { accepted: true }) })
        }
        fn clipboard(&self, _: Peer, _: String, _: Bytes) -> BoxFut<'_, ()> {
            Box::pin(async { Ok(()) })
        }
        fn prepare(&self, _: Peer, req: PrepareReq) -> BoxFut<'_, PrepareResp> {
            Box::pin(async move {
                Ok(PrepareResp {
                    transfer_id: "t".into(),
                    offsets: req.files.into_iter().map(|f| (f.id, 0)).collect(),
                })
            })
        }
        fn upload(
            &self,
            _: Peer,
            _: String,
            _: String,
            _: u64,
            mut body: BodyStream,
        ) -> BoxFut<'_, ()> {
            Box::pin(async move {
                while let Some(chunk) = body.next().await {
                    self.uploaded
                        .lock()
                        .unwrap()
                        .extend_from_slice(&chunk.map_err(ExtError::internal)?);
                }
                Ok(())
            })
        }
        fn finish(&self, _: Peer, _: String) -> BoxFut<'_, FinishResp> {
            Box::pin(async { Ok(FinishResp::default()) })
        }
        fn cancel(&self, _: Peer, _: String) -> BoxFut<'_, ()> {
            Box::pin(async { Ok(()) })
        }
    }

    async fn setup() -> (Arc<Mock>, super::super::server::ExtServerHandle, ExtClient) {
        let mock = Arc::new(Mock::default());
        let server = ExtServer::start(0, &test_identity(0), mock.clone()).await.unwrap();
        let client = ExtClient::new(
            &test_identity(1),
            &test_identity(0).fingerprint,
            "127.0.0.1".parse().unwrap(),
            server.port(),
            Some(Duration::from_secs(10)),
        )
        .unwrap();
        (mock, server, client)
    }

    #[tokio::test]
    async fn info_roundtrip() {
        let (_, server, client) = setup().await;
        assert_eq!(client.info().await.unwrap().alias, "mock");
        server.stop().await;
    }

    #[tokio::test]
    async fn handler_sees_client_fingerprint() {
        let (mock, server, client) = setup().await;
        client
            .pair_commit(&PairCommitReq { alias: "a".into(), commitment: "c".into() })
            .await
            .unwrap();
        assert_eq!(mock.seen.lock().unwrap().as_slice(), [test_identity(1).fingerprint]);
        server.stop().await;
    }

    #[tokio::test]
    async fn pinned_mismatch_fails_handshake() {
        let (mock, server, _) = setup().await;
        let wrong = ExtClient::new(
            &test_identity(1),
            &test_identity(2).fingerprint,
            "127.0.0.1".parse().unwrap(),
            server.port(),
            Some(Duration::from_secs(10)),
        )
        .unwrap();
        let err =
            wrong.pair_commit(&PairCommitReq { alias: "a".into(), commitment: "c".into() }).await;
        assert_eq!(err.unwrap_err().status, 0);
        assert!(mock.seen.lock().unwrap().is_empty(), "nothing reached the handler");
        server.stop().await;
    }

    #[tokio::test]
    async fn json_body_over_1mib_413() {
        let (_, server, client) = setup().await;
        let big = PairCommitReq { alias: "x".repeat(1024 * 1024 + 10), commitment: "c".into() };
        assert_eq!(client.pair_commit(&big).await.unwrap_err().status, 413);
        server.stop().await;
    }

    #[tokio::test]
    async fn clipboard_over_16mib_413() {
        let (_, server, client) = setup().await;
        let err = client.clipboard("image/png", vec![0u8; 16 * 1024 * 1024 + 1]).await;
        assert_eq!(err.unwrap_err().status, 413);
        server.stop().await;
    }

    #[tokio::test]
    async fn upload_stops_reading_the_file_once_cancelled() {
        let (_, server, client) = setup().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.bin");
        std::fs::write(&path, vec![7u8; 64 * UPLOAD_CHUNK]).unwrap();
        let cancel = CancellationToken::new();
        let (stop, seen) = (cancel.clone(), Arc::new(Mutex::new(0u64)));
        let s = seen.clone();
        let progress: Progress = Arc::new(move |n| {
            *s.lock().unwrap() = n;
            stop.cancel();
        });
        let err = client.upload("t", "f", 0, &path, progress, cancel).await.unwrap_err();
        assert_eq!((err.status, err.message.as_str()), (0, "cancelled"));
        // The connection task outlives the call: give it time to read on.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let sent = *seen.lock().unwrap();
        assert!(sent <= UPLOAD_CHUNK as u64, "kept reading after the cancel: {sent}");
        server.stop().await;
    }

    #[tokio::test]
    async fn upload_streams_from_offset() {
        let (mock, server, client) = setup().await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.bin");
        std::fs::write(&path, b"0123456789").unwrap();
        let seen = Arc::new(Mutex::new(0u64));
        let s = seen.clone();
        client
            .upload(
                "t",
                "f",
                4,
                &path,
                Arc::new(move |n| *s.lock().unwrap() = n),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(mock.uploaded.lock().unwrap().as_slice(), b"456789");
        assert_eq!(*seen.lock().unwrap(), 10);
        server.stop().await;
    }
}
