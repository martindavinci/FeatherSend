//! Everything this device starts: sending files, text and clipboard, pairing
//! (initiator side), and the retry/resume loop of extension transfers.

use super::devices::Entry;
use super::files::{self, LocalFile};
use super::localsend;
use super::transfers::{ResumeJob, TransferState};
use super::{PairOutcome, SendProblem, Shared, UiEvent};
use crate::clip::{ClipContent, MAX_CLIPBOARD_BYTES, MIME_PNG, MIME_TEXT};
use crate::ext::ExtError;
use crate::ext::client::ExtClient;
use crate::ext::dto::*;
use crate::pairing::{commitment, format_sas, sas_code};
use crate::settings::extension_port;
use crate::trust::TrustedDevice;
use crate::util::{file_stamp, hex, random_32, rfc3339, unhex, unix_now};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

pub(crate) fn clipboard_temp_dir() -> PathBuf {
    std::env::temp_dir().join("FeatherSend")
}

/// Deletes clipboard images written for sending more than 7 days ago.
pub(crate) fn purge_clipboard_temp() {
    let Ok(entries) = std::fs::read_dir(clipboard_temp_dir()) else { return };
    let week = Duration::from_secs(crate::resume::MAX_AGE_SECS);
    for entry in entries.flatten() {
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > week);
        if old {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn lookup(shared: &Shared, fingerprint: &str) -> Option<Entry> {
    shared.devices.lock().unwrap().get(fingerprint).cloned()
}

fn offline(shared: &Shared, device: &str) {
    shared.emit(UiEvent::SendFailed {
        device: device.to_string(),
        problem: SendProblem::Offline,
        detail: String::new(),
    });
}

fn ext_client(
    shared: &Shared,
    fingerprint: &str,
    entry: &Entry,
    timeout: Option<Duration>,
) -> Result<ExtClient, ExtError> {
    ExtClient::new(&shared.identity, fingerprint, entry.ip, extension_port(entry.port), timeout)
        .map_err(ExtError::internal)
}

fn use_extension(shared: &Shared, fingerprint: &str, entry: &Entry) -> bool {
    entry.feathersend && shared.trust.lock().unwrap().is_trusted(fingerprint)
}

fn problem_of(e: &ExtError) -> SendProblem {
    match e.status {
        0 => SendProblem::Offline,
        403 | 409 => SendProblem::Declined,
        _ => SendProblem::Other,
    }
}

pub(crate) async fn send_files(
    shared: &Arc<Shared>,
    device: &str,
    paths: Vec<PathBuf>,
    clipboard: bool,
) {
    let Some(entry) = lookup(shared, device) else { return offline(shared, device) };
    let local = match tokio::task::spawn_blocking(move || files::collect(&paths)).await {
        Ok(Ok(local)) if !local.is_empty() => local,
        Ok(Ok(_)) => return,
        Ok(Err(e)) => {
            return shared.emit(UiEvent::SendFailed {
                device: entry.alias.clone(),
                problem: SendProblem::Other,
                detail: e.to_string(),
            });
        }
        Err(_) => return,
    };
    if use_extension(shared, device, &entry) {
        let purpose = if clipboard { Purpose::Clipboard } else { Purpose::Files };
        let total: u64 = local.iter().map(|f| f.size).sum();
        let item = files::label(local.iter().map(|f| f.rel.as_str()), local.len());
        let (transfer, _) = shared.transfers.create(shared, &entry.alias, &item, false, total);
        let job = ResumeJob { device: device.to_string(), files: local, purpose };
        run_extension(shared, transfer, job).await;
    } else {
        localsend::send_files(
            shared,
            device,
            &entry.alias,
            entry.ip,
            entry.port,
            entry.https,
            local,
        )
        .await;
    }
}

pub(crate) async fn send_text(shared: &Arc<Shared>, device: &str, text: String) {
    let Some(entry) = lookup(shared, device) else { return offline(shared, device) };
    localsend::send_text(shared, device, &entry.alias, entry.ip, entry.port, entry.https, text)
        .await;
}

pub(crate) async fn send_clipboard(shared: &Arc<Shared>, device: &str, content: ClipContent) {
    let Some(entry) = lookup(shared, device) else { return offline(shared, device) };
    let extension = use_extension(shared, device, &entry);
    match content {
        ClipContent::Text(text) if extension => {
            push_clipboard(shared, device, &entry, MIME_TEXT, text.into_bytes()).await
        }
        ClipContent::Text(text) => {
            localsend::send_text(
                shared,
                device,
                &entry.alias,
                entry.ip,
                entry.port,
                entry.https,
                text,
            )
            .await
        }
        ClipContent::Png(png) if extension && png.len() <= MAX_CLIPBOARD_BYTES => {
            push_clipboard(shared, device, &entry, MIME_PNG, png).await
        }
        ClipContent::Png(png) => {
            // Kept after sending: a paused transfer resumes from this file.
            // Old ones are purged at startup (see `purge_clipboard_temp`).
            let path =
                clipboard_temp_dir().join(format!("clipboard-{}.png", file_stamp(unix_now())));
            let written = async {
                tokio::fs::create_dir_all(clipboard_temp_dir()).await?;
                tokio::fs::write(&path, &png).await
            }
            .await;
            if written.is_ok() {
                send_files(shared, device, vec![path], true).await;
            }
        }
        ClipContent::Files(paths) => send_files(shared, device, paths, true).await,
    }
}

async fn push_clipboard(shared: &Shared, device: &str, entry: &Entry, mime: &str, bytes: Vec<u8>) {
    // The receiver may ask its user first: wait as long as it does.
    let timeout = Some(shared.decision_timeout + Duration::from_secs(10));
    let result = match ext_client(shared, device, entry, timeout) {
        Ok(client) => client.clipboard(mime, bytes).await,
        Err(e) => Err(e),
    };
    if let Err(e) = result {
        shared.emit(UiEvent::SendFailed {
            device: entry.alias.clone(),
            problem: problem_of(&e),
            detail: e.to_string(),
        });
    }
}

/// Auto-sync: text and images go to every online paired device with
/// auto-sync on; files are never auto-synced.
pub(crate) async fn sync_clipboard(shared: &Arc<Shared>, content: ClipContent) {
    let (mime, bytes) = match content {
        ClipContent::Text(text) => (MIME_TEXT, text.into_bytes()),
        ClipContent::Png(png) if png.len() <= MAX_CLIPBOARD_BYTES => (MIME_PNG, png),
        _ => return,
    };
    let targets = {
        let trust = shared.trust.lock().unwrap();
        shared.devices.lock().unwrap().auto_sync_targets(&trust)
    };
    for (fingerprint, entry) in targets {
        let shared = shared.clone();
        let bytes = bytes.clone();
        tokio::spawn(async move {
            if let Ok(client) =
                ext_client(&shared, &fingerprint, &entry, Some(Duration::from_secs(20)))
                && let Err(e) = client.clipboard(mime, bytes).await
            {
                tracing::info!("auto-sync to {} failed: {e}", entry.alias);
            }
        });
    }
}

pub(crate) async fn pair(shared: &Arc<Shared>, device: &str) {
    let Some(entry) = lookup(shared, device) else { return offline(shared, device) };
    let alias = entry.alias.clone();
    let outcome = match pair_inner(shared, device, &entry).await {
        Ok(outcome) => outcome,
        Err(e) => {
            tracing::warn!("pairing with {alias} failed: {e}");
            if e.status == 0 && !entry.feathersend {
                PairOutcome::NotFeatherSend
            } else {
                PairOutcome::Failed
            }
        }
    };
    shared.emit(UiEvent::PairResult { alias, outcome });
    shared.emit_devices();
}

async fn pair_inner(shared: &Shared, device: &str, entry: &Entry) -> Result<PairOutcome, ExtError> {
    // No overall timeout: confirm waits for the other user's decision.
    let client = ext_client(shared, device, entry, None)?;
    let info = client.info().await?;
    if !info.features.iter().any(|f| f == "pair") {
        return Ok(PairOutcome::NotFeatherSend);
    }
    let nonce_a = random_32();
    let commit = client
        .pair_commit(&PairCommitReq {
            alias: shared.alias(),
            commitment: hex(&commitment(&nonce_a)),
        })
        .await?;
    let nonce_b = unhex::<32>(&commit.nonce_b).ok_or_else(|| ExtError::bad_request("bad nonce"))?;
    client
        .pair_reveal(&PairRevealReq { pair_id: commit.pair_id.clone(), nonce_a: hex(&nonce_a) })
        .await?;
    let code = format_sas(sas_code(&shared.identity.fingerprint, device, &nonce_a, &nonce_b));
    let local =
        shared.ask(|reply| UiEvent::PairCode { alias: entry.alias.clone(), code, reply }).await;
    let remote =
        client.pair_confirm(&PairConfirmReq { pair_id: commit.pair_id, accepted: local }).await?;
    if !(local && remote.accepted) {
        return Ok(PairOutcome::Failed);
    }
    let mut trust = shared.trust.lock().unwrap();
    trust.upsert(TrustedDevice {
        fingerprint: device.to_string(),
        alias: entry.alias.clone(),
        paired_at: rfc3339(unix_now()),
        auto_accept_files: false,
        auto_sync_clipboard: false,
    });
    trust.save().map_err(ExtError::internal)?;
    Ok(PairOutcome::Paired)
}

pub(crate) async fn resume(shared: &Arc<Shared>, transfer: u64, job: ResumeJob) {
    shared.transfers.set_state(shared, transfer, TransferState::Running);
    run_extension(shared, transfer, job).await;
}

/// Runs an extension transfer with automatic retries; after the last one it
/// pauses and keeps the job for a manual resume.
async fn run_extension(shared: &Arc<Shared>, transfer: u64, job: ResumeJob) {
    let Some(cancel) = shared.transfers.token(transfer) else { return };
    let hashes = {
        let paths: Vec<PathBuf> = job.files.iter().map(|f| f.path.clone()).collect();
        match tokio::task::spawn_blocking(move || {
            paths.iter().map(|p| files::sha256_file(p)).collect::<std::io::Result<Vec<_>>>()
        })
        .await
        {
            Ok(Ok(hashes)) => hashes,
            Ok(Err(e)) => {
                return shared.transfers.set_state(
                    shared,
                    transfer,
                    TransferState::Failed(e.to_string()),
                );
            }
            Err(_) => return,
        }
    };
    let mut attempt = 0;
    let current = std::sync::Mutex::new(None::<String>);
    loop {
        let result = tokio::select! {
            r = attempt_extension(shared, transfer, &job, &hashes, &current) => r,
            _ = cancel.cancelled() => {
                // Tell the receiver, so it stops and forgets the transfer.
                let remote = current.lock().unwrap().take();
                if let (Some(id), Some(entry)) = (remote, lookup(shared, &job.device))
                    && let Ok(client) =
                        ext_client(shared, &job.device, &entry, Some(Duration::from_secs(5)))
                {
                    let _ = client.cancel(&id).await;
                }
                return;
            }
        };
        let alias = lookup(shared, &job.device).map(|e| e.alias).unwrap_or_default();
        match result {
            Ok(finish) if finish.failed.is_empty() => {
                return shared.transfers.set_state(shared, transfer, TransferState::Done);
            }
            Ok(finish) => {
                let detail = format!("{}/{}", finish.failed.len(), job.files.len());
                return shared.transfers.set_state(shared, transfer, TransferState::Failed(detail));
            }
            Err(e) if matches!(e.status, 403 | 409) => {
                localsend::finish_outgoing(
                    shared,
                    transfer,
                    &alias,
                    Err((problem_of(&e), e.to_string())),
                );
                return;
            }
            Err(e) => {
                tracing::info!("transfer {transfer} interrupted: {e}");
                let Some(delay) = shared.retry_delays.get(attempt) else {
                    shared.transfers.set_resume(transfer, job);
                    shared.transfers.set_state(shared, transfer, TransferState::Paused);
                    return;
                };
                attempt += 1;
                tokio::select! {
                    _ = tokio::time::sleep(*delay) => {}
                    _ = cancel.cancelled() => return,
                }
            }
        }
    }
}

async fn attempt_extension(
    shared: &Arc<Shared>,
    transfer: u64,
    job: &ResumeJob,
    hashes: &[String],
    current: &std::sync::Mutex<Option<String>>,
) -> Result<FinishResp, ExtError> {
    let entry = lookup(shared, &job.device).ok_or_else(|| ExtError::new(0, "device offline"))?;
    let client = ext_client(shared, &job.device, &entry, None)?;
    let request = PrepareReq {
        purpose: job.purpose,
        files: job
            .files
            .iter()
            .zip(hashes)
            .enumerate()
            .map(|(i, (f, hash)): (usize, (&LocalFile, &String))| ExtFile {
                id: i.to_string(),
                path: f.rel.clone(),
                size: f.size,
                sha256: Some(hash.clone()),
            })
            .collect(),
    };
    let prepared = client.prepare(&request).await?;
    *current.lock().unwrap() = Some(prepared.transfer_id.clone());
    let cancel = shared.transfers.token(transfer).unwrap_or_default();
    let mut done_before: u64 = job
        .files
        .iter()
        .enumerate()
        .filter(|(i, _)| !prepared.offsets.contains_key(&i.to_string()))
        .map(|(_, f)| f.size)
        .sum();
    for (i, f) in job.files.iter().enumerate() {
        let Some(&offset) = prepared.offsets.get(&i.to_string()) else { continue };
        let progress_shared = shared.clone();
        let base = done_before;
        client
            .upload(
                &prepared.transfer_id,
                &i.to_string(),
                offset,
                &f.path,
                Arc::new(move |n| {
                    progress_shared.transfers.progress(&progress_shared, transfer, base + n)
                }),
                cancel.clone(),
            )
            .await?;
        done_before += f.size;
    }
    client.finish(&prepared.transfer_id).await
}
