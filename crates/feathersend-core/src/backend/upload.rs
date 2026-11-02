//! Writes a file received from a LocalSend device. The LocalSend core can
//! write it too, but then a cancel on this side cannot stop it.

use crate::util::hex;
use bytes::Bytes;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::time::SystemTime;
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Enough to coalesce TLS records into larger writes, small for old PCs.
const WRITE_BUFFER: usize = 256 * 1024;

pub(super) struct Upload {
    pub path: PathBuf,
    pub size: u64,
    pub sha256: Option<String>,
    pub modified: Option<SystemTime>,
    pub accessed: Option<SystemTime>,
}

/// Saves the chunks to `upload.path`. Returning, on success or not, drops
/// `chunks`, which fails the sender's request. The caller removes the file
/// after an error.
pub(super) async fn save(
    upload: Upload,
    mut chunks: mpsc::Receiver<Bytes>,
    progress: mpsc::Sender<u64>,
    cancel: CancellationToken,
) -> Result<(), String> {
    let file = tokio::fs::File::create(&upload.path)
        .await
        .map_err(|e| format!("cannot create the file: {e}"))?;
    let mut out = tokio::io::BufWriter::with_capacity(WRITE_BUFFER, file);
    let mut hasher = upload.sha256.as_ref().map(|_| Sha256::new());
    let mut written: u64 = 0;
    loop {
        let chunk = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err("cancelled".into()),
            chunk = chunks.recv() => chunk,
        };
        let Some(chunk) = chunk else { break };
        written += chunk.len() as u64;
        if written > upload.size {
            return Err(format!("expected {} bytes, received {written}", upload.size));
        }
        if let Some(hasher) = &mut hasher {
            hasher.update(&chunk);
        }
        out.write_all(&chunk).await.map_err(|e| format!("cannot write the file: {e}"))?;
        // Best effort: dropped when the display lags.
        let _ = progress.try_send(written);
    }
    if written != upload.size {
        return Err(format!("expected {} bytes, received {written}", upload.size));
    }
    if let (Some(hasher), Some(expected)) = (hasher, &upload.sha256) {
        let actual = hex(&hasher.finalize());
        if !actual.eq_ignore_ascii_case(expected) {
            return Err("checksum mismatch".into());
        }
    }
    out.flush().await.map_err(|e| format!("cannot write the file: {e}"))?;
    let file = out.into_inner();
    let mut times = std::fs::FileTimes::new();
    if let Some(modified) = upload.modified {
        times = times.set_modified(modified);
    }
    if let Some(accessed) = upload.accessed {
        times = times.set_accessed(accessed);
    }
    let file = file.into_std().await;
    // Best effort: a file without its original date is still the file.
    if let Ok(Err(e)) = tokio::task::spawn_blocking(move || file.set_times(times)).await {
        tracing::warn!("cannot set the file's dates: {e}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::sha256;
    use std::time::{Duration, UNIX_EPOCH};

    struct Case {
        dir: tempfile::TempDir,
        chunks: mpsc::Sender<Bytes>,
        rx: mpsc::Receiver<Bytes>,
        cancel: CancellationToken,
    }

    fn case() -> Case {
        let (chunks, rx) = mpsc::channel(4);
        Case { dir: tempfile::tempdir().unwrap(), chunks, rx, cancel: CancellationToken::new() }
    }

    fn upload(c: &Case, size: u64, sha256: Option<String>) -> Upload {
        Upload { path: c.dir.path().join("f"), size, sha256, modified: None, accessed: None }
    }

    async fn run(c: Case, upload: Upload, data: &[u8]) -> (Result<(), String>, tempfile::TempDir) {
        let (progress, _progress_rx) = mpsc::channel(4);
        c.chunks.send(Bytes::copy_from_slice(data)).await.unwrap();
        drop(c.chunks);
        (save(upload, c.rx, progress, c.cancel).await, c.dir)
    }

    #[tokio::test]
    async fn saves_a_file_with_its_dates() {
        let c = case();
        let mut u = upload(&c, 5, Some(hex(&sha256(b"hello"))));
        let date = UNIX_EPOCH + Duration::from_secs(1_500_000_000);
        u.modified = Some(date);
        let (result, dir) = run(c, u, b"hello").await;
        result.unwrap();
        let path = dir.path().join("f");
        assert_eq!(std::fs::read(&path).unwrap(), b"hello");
        assert_eq!(std::fs::metadata(&path).unwrap().modified().unwrap(), date);
    }

    #[tokio::test]
    async fn refuses_wrong_sizes_and_checksums() {
        let c = case();
        let u = upload(&c, 6, None);
        assert!(run(c, u, b"hello").await.0.unwrap_err().contains("expected 6"));
        let c = case();
        let u = upload(&c, 4, None);
        assert!(run(c, u, b"hello").await.0.unwrap_err().contains("expected 4"));
        let c = case();
        let u = upload(&c, 5, Some(hex(&sha256(b"other"))));
        assert_eq!(run(c, u, b"hello").await.0.unwrap_err(), "checksum mismatch");
    }

    #[tokio::test]
    async fn cancel_stops_it_and_cuts_off_the_sender() {
        let c = case();
        let u = upload(&c, 100, None);
        let (progress, _progress_rx) = mpsc::channel(4);
        let (sender, cancel) = (c.chunks.clone(), c.cancel.clone());
        let task = tokio::spawn(save(u, c.rx, progress, c.cancel));
        sender.send(Bytes::from_static(b"hello")).await.unwrap();
        cancel.cancel();
        assert_eq!(task.await.unwrap().unwrap_err(), "cancelled");
        assert!(sender.send(Bytes::from_static(b"more")).await.is_err());
    }
}
