//! What this device does when another FeatherSend device calls its extension
//! server: pairing (responder side), clipboard and resumable file transfers.
//! The server already refuses unpaired callers on everything but info and
//! pairing; handlers still check trust where they need the device's flags.

use super::files;
use super::transfers::TransferState;
use super::{IncomingRequest, PairOutcome, Shared, UiEvent};
use crate::clip::{ClipContent, MIME_PNG, png_within_limits};
use crate::ext::ExtError;
use crate::ext::dto::*;
use crate::ext::server::{BodyStream, BoxFut, ExtHandler, Peer};
use crate::pairing::{MAX_PENDING, PAIR_TIMEOUT_SECS, PairError, PendingPairings, format_sas};
use crate::paths::{TransferFolders, long_path, part_path, sanitize_relative};
use crate::resume::{Sidecar, transfer_key};
use crate::trust::TrustedDevice;
use crate::update::Version;
use crate::util::{hex, random_32, rfc3339, unhex, unix_now};
use futures_util::StreamExt;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

/// An upload that sends nothing for this long is dropped (the data so far
/// stays for a resume).
const UPLOAD_IDLE: Duration = Duration::from_secs(60);

pub(crate) struct Handler(pub Arc<Shared>);

struct IncFile {
    key: String,
    path: String,
    size: u64,
    sha256: Option<String>,
    part: PathBuf,
    final_path: PathBuf,
    received: u64,
    done: bool,
    /// One PUT at a time per file.
    uploading: bool,
}

pub(crate) struct ExtIncoming {
    peer: String,
    alias: String,
    purpose: Purpose,
    transfer: u64,
    cancel: CancellationToken,
    files: HashMap<String, IncFile>,
    download_dir: PathBuf,
}

fn pair_error(e: PairError) -> ExtError {
    match e {
        PairError::TooMany => ExtError::new(429, "too many pairings in progress"),
        PairError::Unknown => ExtError::new(404, "unknown pairing"),
        PairError::Expired => ExtError::new(410, "pairing expired"),
        PairError::BadCommitment => ExtError::bad_request("commitment mismatch"),
        PairError::WrongPeer => ExtError::forbidden(),
    }
}

/// Withdraws the approval of a cancelled transfer: without partial data and
/// receipts, the same files need a new answer from the user. Files already at
/// their destination stay.
fn forget_files(shared: &Shared, t: &ExtIncoming) {
    for f in t.files.values() {
        shared.partial.remove(&f.key, !f.done);
        if !f.done {
            shared.names.release(&f.final_path);
        }
    }
}

/// The receiver's user cancelled transfer row `transfer`.
pub(crate) fn withdraw_cancelled(shared: &Shared, transfer: u64) {
    let removed = {
        let mut incoming = shared.ext_incoming.lock().unwrap();
        let id = incoming.iter().find(|(_, t)| t.transfer == transfer).map(|(id, _)| id.clone());
        id.and_then(|id| incoming.remove(&id))
    };
    if let Some(t) = removed {
        forget_files(shared, &t);
    }
}

/// Stops and forgets every incoming extension transfer from `fingerprint`,
/// with all its data (used on unpair).
pub(crate) fn drop_peer_transfers(shared: &Shared, fingerprint: &str) {
    let removed: Vec<ExtIncoming> = {
        let mut incoming = shared.ext_incoming.lock().unwrap();
        let ids: Vec<String> = incoming
            .iter()
            .filter(|(_, t)| t.peer.eq_ignore_ascii_case(fingerprint))
            .map(|(id, _)| id.clone())
            .collect();
        ids.iter().filter_map(|id| incoming.remove(id)).collect()
    };
    for t in removed {
        t.cancel.cancel();
        forget_files(shared, &t);
        shared.transfers.set_state(shared, t.transfer, TransferState::Cancelled);
    }
    shared.partial.remove_sender(fingerprint);
}

/// Clears the upload lease of a file when the PUT ends, however it ends.
struct Lease<'a> {
    shared: &'a Shared,
    transfer_id: &'a str,
    file_id: &'a str,
}

impl Drop for Lease<'_> {
    fn drop(&mut self) {
        if let Some(f) = self
            .shared
            .ext_incoming
            .lock()
            .unwrap()
            .get_mut(self.transfer_id)
            .and_then(|t| t.files.get_mut(self.file_id))
        {
            f.uploading = false;
        }
    }
}

impl Handler {
    fn trusted_alias(&self, peer: &Peer) -> Result<(String, bool, bool), ExtError> {
        let trust = self.0.trust.lock().unwrap();
        let device = trust.get(&peer.fingerprint).ok_or_else(ExtError::forbidden)?;
        Ok((device.alias.clone(), device.auto_accept_files, device.auto_sync_clipboard))
    }

    fn not_paused(&self) -> Result<(), ExtError> {
        if self.0.paused.load(Ordering::Relaxed) {
            return Err(ExtError::new(409, "receiving is paused"));
        }
        Ok(())
    }

    fn update_progress(&self, transfer_id: &str) {
        let (transfer, done) = {
            let incoming = self.0.ext_incoming.lock().unwrap();
            let Some(t) = incoming.get(transfer_id) else { return };
            (t.transfer, t.files.values().map(|f| f.received).sum::<u64>())
        };
        self.0.transfers.progress(&self.0, transfer, done);
    }

    async fn prepare_inner(&self, peer: Peer, req: PrepareReq) -> Result<PrepareResp, ExtError> {
        let shared = &self.0;
        let (alias, auto_accept, _) = self.trusted_alias(&peer)?;
        self.not_paused()?;
        if req.files.is_empty() {
            return Err(ExtError::bad_request("no files"));
        }
        let clipboard = req.purpose == Purpose::Clipboard;
        let keyed: Vec<(ExtFile, String)> = req
            .files
            .into_iter()
            .map(|f| {
                let key = transfer_key(&peer.fingerprint, &f.path, f.size);
                (f, key)
            })
            .collect();
        let keys: HashSet<&str> = keyed.iter().map(|(_, k)| k.as_str()).collect();
        if keys.len() != keyed.len() {
            return Err(ExtError::bad_request("duplicate file paths"));
        }
        // Partial or completed data from an earlier attempt counts only for
        // the same content (checksum) and the same purpose: that is what the
        // user accepted then. Cancelling removes it (see `forget_files`).
        let now = unix_now();
        let existing: Vec<Option<(Sidecar, u64)>> = keyed
            .iter()
            .map(|(f, key)| {
                shared.partial.find(key).filter(|(sidecar, _)| {
                    f.sha256.is_some()
                        && sidecar.sha256 == f.sha256
                        && sidecar.clipboard == clipboard
                        && sidecar.approves_retry(now)
                })
            })
            .collect();
        let resuming = existing.iter().all(Option::is_some);
        let total: u64 = keyed.iter().map(|(f, _)| f.size).sum();
        let mut names: Vec<String> = keyed.iter().map(|(f, _)| f.path.clone()).collect();
        names.sort_unstable();
        let item = files::label(names.iter().map(String::as_str), names.len());
        let accepted = auto_accept
            || resuming
            || shared
                .ask(|reply| UiEvent::IncomingTransfer {
                    request: IncomingRequest {
                        from: alias.clone(),
                        fingerprint: peer.fingerprint.clone(),
                        paired: true,
                        count: keyed.len(),
                        total_bytes: total,
                        names: names.iter().take(10).cloned().collect(),
                        clipboard,
                    },
                    reply,
                })
                .await;
        if !accepted {
            return Err(ExtError::new(403, "declined"));
        }

        // An earlier attempt of this transfer (same files from this peer) is
        // superseded: stop its writers and continue in its row. Independent
        // transfers from the same peer are left alone.
        let superseded: Option<ExtIncoming> = {
            let mut incoming = shared.ext_incoming.lock().unwrap();
            let id = incoming
                .iter()
                .find(|(_, t)| {
                    t.peer == peer.fingerprint
                        && t.files.values().any(|f| keys.contains(f.key.as_str()))
                })
                .map(|(id, _)| id.clone());
            id.and_then(|id| incoming.remove(&id))
        };
        let (transfer, cancel) = match superseded {
            Some(old) => {
                old.cancel.cancel();
                match shared.transfers.renew_token(old.transfer) {
                    Some(token) => (old.transfer, token),
                    None => shared.transfers.create(shared, &alias, &item, true, total),
                }
            }
            None => shared.transfers.create(shared, &alias, &item, true, total),
        };

        let download_dir = shared.download_dir();
        let mut folders = TransferFolders::default();
        // Files already placed by an earlier attempt keep their folder.
        for ((f, _), found) in keyed.iter().zip(&existing) {
            let (Some((sidecar, _)), Ok(components)) = (found, sanitize_relative(&f.path)) else {
                continue;
            };
            if components.len() > 1
                && let Some(actual) = sidecar
                    .final_path
                    .strip_prefix(&download_dir)
                    .ok()
                    .and_then(|rel| rel.components().next())
                    .and_then(|c| c.as_os_str().to_str())
            {
                folders.remember(&components[0], actual);
            }
        }
        let mut offsets = HashMap::new();
        let mut files = HashMap::new();
        for ((f, key), found) in keyed.into_iter().zip(existing) {
            let (part, final_path, received, done) = match found {
                Some((sidecar, _)) if sidecar.completed => {
                    (sidecar.part_path, sidecar.final_path, f.size, true)
                }
                Some((sidecar, len)) => (sidecar.part_path, sidecar.final_path, len, false),
                None => {
                    let Ok(components) = sanitize_relative(&f.path) else {
                        tracing::warn!("rejected received name {:?}", f.path);
                        continue;
                    };
                    let final_path = folders
                        .place(&shared.names, &download_dir, components)
                        .map_err(ExtError::internal)?;
                    let part = part_path(&final_path);
                    // create_new: never truncate a file that appeared meanwhile.
                    std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(long_path(&part))
                        .map_err(ExtError::internal)?;
                    shared
                        .partial
                        .create(
                            &key,
                            &Sidecar {
                                sender_fp: peer.fingerprint.clone(),
                                path: f.path.clone(),
                                size: f.size,
                                sha256: f.sha256.clone(),
                                part_path: part.clone(),
                                final_path: final_path.clone(),
                                updated_at: unix_now(),
                                clipboard,
                                completed: false,
                            },
                        )
                        .map_err(ExtError::internal)?;
                    (part, final_path, 0, false)
                }
            };
            if !done {
                offsets.insert(f.id.clone(), received);
            }
            files.insert(
                f.id,
                IncFile {
                    key,
                    path: f.path,
                    size: f.size,
                    sha256: f.sha256,
                    part,
                    final_path,
                    received,
                    done,
                    uploading: false,
                },
            );
        }
        let transfer_id = uuid::Uuid::new_v4().to_string();
        shared.ext_incoming.lock().unwrap().insert(
            transfer_id.clone(),
            ExtIncoming {
                peer: peer.fingerprint,
                alias,
                purpose: req.purpose,
                transfer,
                cancel,
                files,
                download_dir,
            },
        );
        self.update_progress(&transfer_id);
        Ok(PrepareResp { transfer_id, offsets })
    }

    async fn upload_inner(
        &self,
        peer: Peer,
        transfer_id: String,
        file_id: String,
        offset: u64,
        mut body: BodyStream,
    ) -> Result<(), ExtError> {
        let shared = &self.0;
        let (part, size, cancel) = {
            let mut incoming = shared.ext_incoming.lock().unwrap();
            let t = incoming
                .get_mut(&transfer_id)
                .ok_or_else(|| ExtError::new(404, "unknown transfer"))?;
            if t.peer != peer.fingerprint {
                return Err(ExtError::forbidden());
            }
            let cancel = t.cancel.clone();
            let f = t.files.get_mut(&file_id).ok_or_else(|| ExtError::new(404, "unknown file"))?;
            if f.done {
                return Err(ExtError::new(409, "already complete"));
            }
            if f.uploading {
                return Err(ExtError::new(409, "upload already in progress"));
            }
            f.uploading = true;
            (f.part.clone(), f.size, cancel)
        };
        let _lease = Lease { shared, transfer_id: &transfer_id, file_id: &file_id };
        let current = tokio::fs::metadata(long_path(&part)).await.map(|m| m.len()).unwrap_or(0);
        if current != offset {
            return Err(ExtError::new(409, format!("expected offset {current}")));
        }
        let mut out = tokio::fs::OpenOptions::new()
            .append(true)
            .open(long_path(&part))
            .await
            .map_err(ExtError::internal)?;
        let mut received = offset;
        let outcome: Result<(), ExtError> = loop {
            let chunk = tokio::select! {
                chunk = tokio::time::timeout(UPLOAD_IDLE, body.next()) => match chunk {
                    Ok(chunk) => chunk,
                    Err(_) => break Err(ExtError::new(408, "upload idle")),
                },
                _ = cancel.cancelled() => break Err(ExtError::new(409, "cancelled by receiver")),
            };
            match chunk {
                None => break Ok(()),
                Some(Err(e)) => break Err(ExtError::bad_request(e.to_string())),
                Some(Ok(bytes)) => {
                    received += bytes.len() as u64;
                    if received > size {
                        break Err(ExtError::bad_request("more data than announced"));
                    }
                    if let Err(e) = out.write_all(&bytes).await {
                        break Err(ExtError::internal(e));
                    }
                    if let Some(f) = shared
                        .ext_incoming
                        .lock()
                        .unwrap()
                        .get_mut(&transfer_id)
                        .and_then(|t| t.files.get_mut(&file_id))
                    {
                        f.received = received;
                    }
                    self.update_progress(&transfer_id);
                }
            }
        };
        let _ = out.flush().await;
        drop(out);
        if received > size {
            // Never keep data that cannot be right.
            let _ = tokio::fs::File::create(long_path(&part)).await;
        }
        let key = {
            let incoming = shared.ext_incoming.lock().unwrap();
            incoming.get(&transfer_id).and_then(|t| t.files.get(&file_id)).map(|f| f.key.clone())
        };
        if let Some(key) = &key {
            shared.partial.touch(key, unix_now());
        }
        outcome?;
        if received < size {
            return Err(ExtError::bad_request("incomplete body"));
        }
        self.complete_file(&transfer_id, &file_id).await
    }

    async fn complete_file(&self, transfer_id: &str, file_id: &str) -> Result<(), ExtError> {
        let shared = &self.0;
        let (key, part, final_path, sha256) = {
            let incoming = shared.ext_incoming.lock().unwrap();
            let f = incoming
                .get(transfer_id)
                .and_then(|t| t.files.get(file_id))
                .ok_or_else(|| ExtError::new(404, "unknown file"))?;
            (f.key.clone(), f.part.clone(), f.final_path.clone(), f.sha256.clone())
        };
        if let Some(expected) = sha256 {
            let path = part.clone();
            let actual = tokio::task::spawn_blocking(move || files::sha256_file(&path))
                .await
                .map_err(ExtError::internal)?
                .map_err(ExtError::internal)?;
            if !actual.eq_ignore_ascii_case(&expected) {
                shared.partial.remove(&key, true);
                return Err(ExtError::new(422, "checksum mismatch"));
            }
        }
        // The destination may have appeared since it was reserved.
        let destination = if long_path(&final_path).exists() {
            let parent = final_path.parent().map(Path::to_path_buf).unwrap_or_default();
            let name = final_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            shared.names.reserve_file(&parent, &[name]).map_err(ExtError::internal)?
        } else {
            final_path.clone()
        };
        tokio::fs::rename(long_path(&part), long_path(&destination))
            .await
            .map_err(ExtError::internal)?;
        shared.partial.complete(&key, &destination, unix_now());
        shared.names.release(&final_path);
        shared.names.release(&destination);
        if let Some(f) = shared
            .ext_incoming
            .lock()
            .unwrap()
            .get_mut(transfer_id)
            .and_then(|t| t.files.get_mut(file_id))
        {
            f.done = true;
            f.received = f.size;
            f.final_path = destination;
        }
        Ok(())
    }

    fn finish_inner(&self, peer: Peer, transfer_id: String) -> Result<FinishResp, ExtError> {
        let shared = &self.0;
        let t = {
            let mut incoming = shared.ext_incoming.lock().unwrap();
            match incoming.get(&transfer_id) {
                Some(t) if t.peer == peer.fingerprint => incoming.remove(&transfer_id).unwrap(),
                Some(_) => return Err(ExtError::forbidden()),
                None => return Err(ExtError::new(404, "unknown transfer")),
            }
        };
        let mut resp = FinishResp::default();
        let mut tops: Vec<PathBuf> = Vec::new();
        let mut first = String::new();
        for (id, f) in &t.files {
            if f.done {
                resp.completed.push(id.clone());
                if first.is_empty() || f.path < first {
                    first = f.path.clone();
                }
                let top = f
                    .final_path
                    .strip_prefix(&t.download_dir)
                    .ok()
                    .and_then(|rel| rel.components().next())
                    .map(|c| t.download_dir.join(c.as_os_str()))
                    .unwrap_or_else(|| f.final_path.clone());
                if !tops.contains(&top) {
                    tops.push(top);
                }
            } else {
                resp.failed.push(id.clone());
            }
        }
        if resp.failed.is_empty() {
            shared.transfers.set_state(shared, t.transfer, TransferState::Done);
            shared.emit(UiEvent::Received {
                from: t.alias.clone(),
                item: files::label([first.as_str()].into_iter(), t.files.len()),
            });
        } else {
            let detail = format!("{}/{}", resp.failed.len(), t.files.len());
            shared.transfers.set_state(shared, t.transfer, TransferState::Failed(detail));
        }
        if t.purpose == Purpose::Clipboard && !tops.is_empty() {
            tops.sort();
            shared.emit(UiEvent::ClipboardReceived {
                from: t.alias,
                content: ClipContent::Files(tops),
            });
        }
        Ok(resp)
    }
}

impl ExtHandler for Handler {
    fn is_trusted(&self, fingerprint: &str) -> bool {
        self.0.trust.lock().unwrap().is_trusted(fingerprint)
    }

    fn info(&self) -> InfoDto {
        InfoDto {
            alias: self.0.alias(),
            fingerprint: self.0.identity.fingerprint.clone(),
            version: Version::current().to_string(),
            features: vec!["pair".into(), "clipboard".into(), "transfer".into()],
        }
    }

    fn pair_commit(&self, peer: Peer, req: PairCommitReq) -> BoxFut<'_, PairCommitResp> {
        Box::pin(async move {
            let commitment = unhex::<32>(&req.commitment)
                .ok_or_else(|| ExtError::bad_request("bad commitment"))?;
            let nonce_b = random_32();
            let pair_id = self
                .0
                .pairing
                .lock()
                .unwrap()
                .begin(&peer.fingerprint, &req.alias, commitment, nonce_b, unix_now())
                .map_err(pair_error)?;
            Ok(PairCommitResp { pair_id, nonce_b: hex(&nonce_b) })
        })
    }

    fn pair_reveal(&self, peer: Peer, req: PairRevealReq) -> BoxFut<'_, ()> {
        Box::pin(async move {
            let nonce_a =
                unhex::<32>(&req.nonce_a).ok_or_else(|| ExtError::bad_request("bad nonce"))?;
            let now = unix_now();
            {
                // Waiting decisions expire with their pairing and are capped
                // like pending pairings, so prompts cannot pile up.
                let mut waits = self.0.pair_waits.lock().unwrap();
                waits.retain(|_, (created, _)| now.saturating_sub(*created) < PAIR_TIMEOUT_SECS);
                if waits.len() >= MAX_PENDING {
                    return Err(pair_error(PairError::TooMany));
                }
            }
            let pending = self
                .0
                .pairing
                .lock()
                .unwrap()
                .reveal(
                    &req.pair_id,
                    &peer.fingerprint,
                    &nonce_a,
                    &self.0.identity.fingerprint,
                    now,
                )
                .map_err(pair_error)?;
            let (tx, rx) = tokio::sync::oneshot::channel();
            self.0.pair_waits.lock().unwrap().insert(req.pair_id, (now, rx));
            self.0.emit(UiEvent::PairCode {
                alias: pending.alias,
                code: format_sas(pending.sas.unwrap_or_default()),
                reply: tx,
            });
            Ok(())
        })
    }

    fn pair_confirm(&self, peer: Peer, req: PairConfirmReq) -> BoxFut<'_, PairConfirmResp> {
        Box::pin(async move {
            let now = unix_now();
            let pending = self
                .0
                .pairing
                .lock()
                .unwrap()
                .take(&req.pair_id, &peer.fingerprint, now)
                .ok_or_else(|| ExtError::new(404, "unknown pairing"))?;
            let (_, wait) = self
                .0
                .pair_waits
                .lock()
                .unwrap()
                .remove(&req.pair_id)
                .ok_or_else(|| ExtError::new(404, "unknown pairing"))?;
            // The decision must come before the pairing expires.
            let left = Duration::from_secs(PendingPairings::remaining(&pending, now));
            let limit = self.0.decision_timeout.min(left);
            let local = matches!(tokio::time::timeout(limit, wait).await, Ok(Ok(true)));
            let outcome = if local && req.accepted {
                let mut trust = self.0.trust.lock().unwrap();
                trust.upsert(TrustedDevice {
                    fingerprint: peer.fingerprint.clone(),
                    alias: pending.alias.clone(),
                    paired_at: rfc3339(unix_now()),
                    auto_accept_files: false,
                    auto_sync_clipboard: false,
                });
                if let Err(e) = trust.save() {
                    tracing::warn!("cannot save trusted devices: {e}");
                }
                PairOutcome::Paired
            } else {
                PairOutcome::Failed
            };
            self.0.emit(UiEvent::PairResult { alias: pending.alias, outcome });
            self.0.emit_devices();
            Ok(PairConfirmResp { accepted: local })
        })
    }

    fn clipboard(&self, peer: Peer, mime: String, body: bytes::Bytes) -> BoxFut<'_, ()> {
        Box::pin(async move {
            let (alias, _, auto_sync) = self.trusted_alias(&peer)?;
            self.not_paused()?;
            let content = if mime.starts_with("text/plain") {
                ClipContent::Text(
                    String::from_utf8(body.to_vec())
                        .map_err(|_| ExtError::bad_request("not UTF-8"))?,
                )
            } else if mime == MIME_PNG {
                if !png_within_limits(&body) {
                    return Err(ExtError::new(413, "image too large or not a PNG"));
                }
                ClipContent::Png(body.to_vec())
            } else {
                return Err(ExtError::new(415, "unsupported clipboard type"));
            };
            let accepted = auto_sync
                || self
                    .0
                    .ask(|reply| UiEvent::IncomingClipboard {
                        from: alias.clone(),
                        content: content.clone(),
                        reply,
                    })
                    .await;
            if !accepted {
                return Err(ExtError::new(409, "rejected"));
            }
            self.0.emit(UiEvent::ClipboardReceived { from: alias, content });
            Ok(())
        })
    }

    fn prepare(&self, peer: Peer, req: PrepareReq) -> BoxFut<'_, PrepareResp> {
        Box::pin(self.prepare_inner(peer, req))
    }

    fn upload(
        &self,
        peer: Peer,
        transfer_id: String,
        file_id: String,
        offset: u64,
        body: BodyStream,
    ) -> BoxFut<'_, ()> {
        Box::pin(self.upload_inner(peer, transfer_id, file_id, offset, body))
    }

    fn finish(&self, peer: Peer, transfer_id: String) -> BoxFut<'_, FinishResp> {
        Box::pin(async move { self.finish_inner(peer, transfer_id) })
    }

    fn cancel(&self, peer: Peer, transfer_id: String) -> BoxFut<'_, ()> {
        Box::pin(async move {
            let t = {
                let mut incoming = self.0.ext_incoming.lock().unwrap();
                match incoming.get(&transfer_id) {
                    Some(t) if t.peer == peer.fingerprint => incoming.remove(&transfer_id),
                    Some(_) => return Err(ExtError::forbidden()),
                    None => None,
                }
            };
            if let Some(t) = t {
                // Stop running uploads before forgetting the transfer.
                t.cancel.cancel();
                forget_files(&self.0, &t);
                self.0.transfers.set_state(&self.0, t.transfer, TransferState::Cancelled);
            }
            Ok(())
        })
    }
}
