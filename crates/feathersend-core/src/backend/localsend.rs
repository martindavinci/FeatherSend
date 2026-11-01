//! Interop with LocalSend devices through LocalSend's own core: the v2
//! server (receive), discovery, and the v2 client (send files and text).

use super::files::{self, LocalFile};
use super::transfers::TransferState;
use super::{IncomingRequest, SendProblem, Shared, UiEvent};
use crate::ext::client::ExtClient;
use crate::identity::Identity;
use crate::paths::{TransferFolders, long_path, sanitize_relative};
use crate::settings::{Settings, extension_port};
use ::localsend::discovery::{
    DEFAULT_DISCOVERY_TIMEOUT, DeviceIdentity, DiscoveryConfig, DiscoveryEvent, DiscoveryHandle,
};
use ::localsend::http::client::ClientError;
use ::localsend::http::client::LsHttpClientV2;
use ::localsend::http::dto_v2::{PrepareUploadRequestDtoV2, RegisterDtoV2};
use ::localsend::http::server::common::save::FileUploadTarget;
use ::localsend::http::server::v2::{PrepareUploadDecisionV2, ServerEventV2, SessionEndReasonV2};
use ::localsend::http::server::web::WebConfig;
use ::localsend::http::server::{ServerConfigV2, ServerHandle, TlsConfig, start_with_port};
use ::localsend::http::state::ClientInfo;
use ::localsend::model::discovery::{DeviceType, PROTOCOL_VERSION_V2, ProtocolType};
use ::localsend::model::transfer::{FileContent, FileDto, FileMetadata};
use ::localsend::multicast::{DEFAULT_MULTICAST_GROUP, DEFAULT_PORT, MulticastDevice};
use ::localsend::reqwest;
use ::localsend::util::interface::{InterfaceFilter, local_interface_addresses};
use futures_util::StreamExt;
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

/// Above this, a text message travels as a `.txt` file (LocalSend's limit).
const MAX_PREVIEW_BYTES: usize = 64_000;

pub(crate) struct LsSession {
    transfer: u64,
    from: String,
    folders: TransferFolders,
    sizes: HashMap<String, u64>,
    progress: HashMap<String, u64>,
    failed: usize,
    first_name: String,
    count: usize,
}

pub(crate) async fn start_server(
    id: &Identity,
    settings: &Settings,
    port: u16,
    event_tx: mpsc::Sender<ServerEventV2>,
    stop_rx: oneshot::Receiver<()>,
) -> anyhow::Result<ServerHandle> {
    let info = ClientInfo {
        alias: settings.alias.clone(),
        version: PROTOCOL_VERSION_V2.into(),
        device_model: Some("Windows".into()),
        device_type: Some(DeviceType::Desktop),
        token: id.fingerprint.clone(),
    };
    start_with_port(
        port,
        Some(TlsConfig { cert: id.cert_pem.clone(), private_key: id.key_pem.clone() }),
        info,
        None,
        Some(ServerConfigV2 { pin: None, verify_checksums: true, event_tx }),
        WebConfig::default(),
        stop_rx,
    )
    .await
}

fn register_dto(shared: &Shared) -> RegisterDtoV2 {
    RegisterDtoV2 {
        alias: shared.alias(),
        version: PROTOCOL_VERSION_V2.into(),
        device_model: Some("Windows".into()),
        device_type: Some(DeviceType::Desktop),
        fingerprint: shared.identity.fingerprint.clone(),
        port: shared.ls_port,
        protocol: ProtocolType::Https,
        download: false,
    }
}

pub(crate) async fn start_discovery(
    shared: &Arc<Shared>,
    event_tx: mpsc::Sender<DiscoveryEvent>,
    stop_rx: oneshot::Receiver<()>,
) -> Arc<DiscoveryHandle> {
    let config = DiscoveryConfig {
        group: DEFAULT_MULTICAST_GROUP,
        group_v6: None,
        port: DEFAULT_PORT,
        interface_filter: InterfaceFilter::default(),
        device: MulticastDevice {
            alias: shared.alias(),
            version: PROTOCOL_VERSION_V2.into(),
            device_model: Some("Windows".into()),
            device_type: Some(DeviceType::Desktop),
            fingerprint: shared.identity.fingerprint.clone(),
            port: shared.ls_port,
            protocol: ProtocolType::Https,
            download: false,
        },
        identity: DeviceIdentity {
            cert_pem: shared.identity.cert_pem.clone(),
            private_key_pem: shared.identity.key_pem.clone(),
        },
        timeout: DEFAULT_DISCOVERY_TIMEOUT,
        event_tx: Some(event_tx),
    };
    let handle = Arc::new(::localsend::discovery::start(config, stop_rx).await);
    if let Some(e) = handle.multicast_error() {
        tracing::warn!("multicast unavailable, HTTP discovery only: {e:#}");
    }
    let _ = shared.discovery.set(handle.clone());
    let shared = shared.clone();
    tokio::spawn(async move { rescan(&shared).await });
    handle
}

pub(crate) async fn rescan(shared: &Shared) {
    let Some(discovery) = shared.discovery.get().cloned() else { return };
    let ips = tokio::task::spawn_blocking(|| {
        local_interface_addresses(&InterfaceFilter::default()).unwrap_or_default()
    })
    .await
    .unwrap_or_default();
    if let Err(e) = discovery
        .discover_staged(Vec::new(), ips, DEFAULT_PORT, ProtocolType::Https, Duration::from_secs(1))
        .await
    {
        tracing::warn!("discovery scan failed: {e:#}");
    }
}

fn parse_host(host: &str) -> Option<IpAddr> {
    host.split('%').next()?.parse().ok()
}

/// Records a device and probes its extension port when it is new or moved.
pub(crate) fn saw_device(
    shared: &Arc<Shared>,
    fingerprint: &str,
    alias: &str,
    device_type: Option<DeviceType>,
    ip: IpAddr,
    port: u16,
    https: bool,
) {
    if fingerprint.eq_ignore_ascii_case(&shared.identity.fingerprint) {
        return;
    }
    // Without TLS a fingerprint is only a claim: plain-HTTP sightings must
    // never redirect a paired or HTTPS device to another address.
    if !https {
        let trusted = shared.trust.lock().unwrap().is_trusted(fingerprint);
        let known_https = shared.devices.lock().unwrap().get(fingerprint).is_some_and(|e| e.https);
        if trusted || known_https {
            tracing::warn!("ignored plain-HTTP announcement for {fingerprint}");
            return;
        }
    }
    let probe =
        shared.devices.lock().unwrap().upsert(fingerprint, alias, device_type, ip, port, https);
    shared.emit_devices();
    if probe && https {
        let shared = shared.clone();
        let fingerprint = fingerprint.to_string();
        tokio::spawn(async move {
            let ok = match ExtClient::new(
                &shared.identity,
                &fingerprint,
                ip,
                extension_port(port),
                Some(Duration::from_secs(3)),
            ) {
                Ok(client) => client.info().await.is_ok(),
                Err(_) => false,
            };
            if ok {
                shared.devices.lock().unwrap().set_feathersend(&fingerprint, true);
                shared.emit_devices();
            }
        });
    }
}

pub(crate) fn handle_discovery_event(shared: &Arc<Shared>, event: DiscoveryEvent) {
    match event {
        DiscoveryEvent::Discovered { device } | DiscoveryEvent::Updated { device } => {
            let Some(http) = device.channel.http() else { return };
            let Some(ip) = parse_host(&http.host) else { return };
            saw_device(
                shared,
                &device.fingerprint,
                &device.alias,
                device.device_type,
                ip,
                http.port,
                http.protocol == ProtocolType::Https,
            );
        }
        DiscoveryEvent::MulticastFailed => tracing::warn!("multicast stopped working"),
    }
}

/// "Add by IP": asks the device directly who it is.
pub(crate) async fn register_with(shared: &Arc<Shared>, ip: IpAddr, port: u16) {
    let result = async {
        let client = LsHttpClientV2::try_new(
            &shared.identity.key_pem,
            &shared.identity.cert_pem,
            None,
            Some(Duration::from_secs(5)),
        )?;
        let answer = client
            .register(ProtocolType::Https, &ip.to_string(), port, register_dto(shared))
            .await?;
        anyhow::Ok(answer)
    }
    .await;
    match result {
        Ok(answer) => {
            let fingerprint = answer.cert_fingerprint.unwrap_or(answer.body.fingerprint);
            saw_device(
                shared,
                &fingerprint,
                &answer.body.alias,
                answer.body.device_type,
                ip,
                port,
                true,
            );
        }
        Err(e) => shared.emit(UiEvent::SendFailed {
            device: ip.to_string(),
            problem: SendProblem::Offline,
            detail: format!("{e:#}"),
        }),
    }
}

pub(crate) async fn handle_server_event(shared: &Arc<Shared>, event: ServerEventV2) {
    match event {
        ServerEventV2::Register { ip, info } => saw_device(
            shared,
            &info.fingerprint,
            &info.alias,
            info.device_type,
            ip.ip,
            info.port,
            info.protocol == ProtocolType::Https,
        ),
        ServerEventV2::PrepareUpload {
            session_id,
            info,
            cert_fingerprint,
            files,
            decision_tx,
            ..
        } => {
            let shared = shared.clone();
            tokio::spawn(async move {
                let decision =
                    decide(&shared, &session_id, &info.alias, cert_fingerprint, &files).await;
                let _ = decision_tx.send(decision);
            });
        }
        ServerEventV2::FileUpload { session_id, file_id, file, target_tx } => {
            receive_file(shared, &session_id, &file_id, &file, target_tx);
        }
        ServerEventV2::SessionEnd { session_id, reason } => {
            let Some(session) = shared.ls_sessions.lock().unwrap().remove(&session_id) else {
                return;
            };
            match reason {
                SessionEndReasonV2::Cancelled => {
                    shared.transfers.set_state(shared, session.transfer, TransferState::Cancelled)
                }
                SessionEndReasonV2::Finished if session.failed > 0 => shared.transfers.set_state(
                    shared,
                    session.transfer,
                    TransferState::Failed(format!("{}/{}", session.failed, session.count)),
                ),
                SessionEndReasonV2::Finished => {
                    shared.transfers.set_state(shared, session.transfer, TransferState::Done);
                    shared.emit(UiEvent::Received {
                        from: session.from,
                        item: files::label(
                            [session.first_name.as_str()].into_iter(),
                            session.count,
                        ),
                    });
                }
            }
        }
        ServerEventV2::PrepareUploadAborted { session_id } => {
            if let Some(session) = shared.ls_sessions.lock().unwrap().remove(&session_id) {
                shared.transfers.set_state(shared, session.transfer, TransferState::Cancelled);
            }
        }
        ServerEventV2::CancelReceived { session_id, .. } => {
            let id = shared.ls_outgoing.lock().unwrap().get(&session_id).copied();
            if let Some(token) = id.and_then(|id| shared.transfers.token(id)) {
                token.cancel();
            }
        }
        ServerEventV2::ListenerFailed { error } => {
            tracing::error!("LocalSend server stopped: {error}");
        }
    }
}

fn is_text_message(files: &HashMap<String, FileDto>) -> Option<String> {
    let [file] = files.values().collect::<Vec<_>>()[..] else { return None };
    (file.file_type.starts_with("text")).then(|| file.preview.clone()).flatten()
}

async fn decide(
    shared: &Arc<Shared>,
    session_id: &str,
    alias: &str,
    cert_fingerprint: Option<String>,
    files: &HashMap<String, FileDto>,
) -> PrepareUploadDecisionV2 {
    // Identity is the TLS client certificate, never the claimed fingerprint.
    let fingerprint = cert_fingerprint.unwrap_or_default().to_ascii_uppercase();
    let (paired, auto_accept) = {
        let trust = shared.trust.lock().unwrap();
        let device = (!fingerprint.is_empty()).then(|| trust.get(&fingerprint)).flatten();
        (device.is_some(), device.is_some_and(|d| d.auto_accept_files))
    };
    let accept_unpaired = shared.settings.lock().unwrap().accept_from_unpaired;
    if shared.paused.load(Ordering::Relaxed) || (!paired && !accept_unpaired) {
        return PrepareUploadDecisionV2::Decline;
    }
    if let Some(text) = is_text_message(files) {
        shared.emit(UiEvent::IncomingText { from: alias.to_string(), text });
        return PrepareUploadDecisionV2::Accept(HashSet::new());
    }
    let mut names: Vec<&str> = files.values().map(|f| f.file_name.as_str()).collect();
    names.sort_unstable();
    let total: u64 = files.values().map(|f| f.size).sum();
    let accepted = auto_accept
        || shared
            .ask(|reply| UiEvent::IncomingTransfer {
                request: IncomingRequest {
                    from: alias.to_string(),
                    fingerprint: fingerprint.clone(),
                    paired,
                    count: files.len(),
                    total_bytes: total,
                    names: names.iter().take(10).map(|s| s.to_string()).collect(),
                    clipboard: false,
                },
                reply,
            })
            .await;
    if !accepted {
        return PrepareUploadDecisionV2::Decline;
    }
    let item = files::label(names.iter().copied(), files.len());
    let (transfer, cancel) = shared.transfers.create(shared, alias, &item, true, total);
    shared.ls_sessions.lock().unwrap().insert(
        session_id.to_string(),
        LsSession {
            transfer,
            from: alias.to_string(),
            folders: TransferFolders::default(),
            sizes: files.iter().map(|(id, f)| (id.clone(), f.size)).collect(),
            progress: HashMap::new(),
            failed: 0,
            first_name: names.first().map(|s| s.to_string()).unwrap_or_default(),
            count: files.len(),
        },
    );
    let watcher = shared.clone();
    let session = session_id.to_string();
    tokio::spawn(async move {
        cancel.cancelled().await;
        if watcher.ls_sessions.lock().unwrap().remove(&session).is_some() {
            watcher.ls_server().cancel_v2_session(&session).await;
        }
    });
    PrepareUploadDecisionV2::Accept(files.keys().cloned().collect())
}

fn receive_file(
    shared: &Arc<Shared>,
    session_id: &str,
    file_id: &str,
    file: &FileDto,
    target_tx: oneshot::Sender<FileUploadTarget>,
) {
    let download_dir = shared.download_dir();
    let placed = {
        let mut sessions = shared.ls_sessions.lock().unwrap();
        let Some(session) = sessions.get_mut(session_id) else { return };
        let placed = match sanitize_relative(&file.file_name) {
            Ok(components) => session
                .folders
                .place(&shared.names, &download_dir, components)
                .map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        };
        if placed.is_err() {
            session.failed += 1;
        }
        placed.map(|path| (path, session.transfer))
    };
    let (path, transfer) = match placed {
        Ok(placed) => placed,
        Err(e) => {
            tracing::warn!("rejected received name {:?}: {e}", file.file_name);
            return; // dropping target_tx fails this file only
        }
    };
    let (progress_tx, mut progress_rx) = mpsc::channel::<u64>(16);
    let (result_tx, result_rx) = oneshot::channel();
    let tracker = shared.clone();
    let (session, id) = (session_id.to_string(), file_id.to_string());
    tokio::spawn(async move {
        while let Some(bytes) = progress_rx.recv().await {
            let done = {
                let mut sessions = tracker.ls_sessions.lock().unwrap();
                let Some(s) = sessions.get_mut(&session) else { continue };
                s.progress.insert(id.clone(), bytes);
                s.progress.values().sum::<u64>()
            };
            tracker.transfers.progress(&tracker, transfer, done);
        }
    });
    let finisher = shared.clone();
    let (session, id) = (session_id.to_string(), file_id.to_string());
    let final_path = path.clone();
    tokio::spawn(async move {
        let result = result_rx.await.unwrap_or_else(|_| Err("upload aborted".into()));
        if let Err(e) = result {
            tracing::warn!("receiving {} failed: {e}", final_path.display());
            let _ = tokio::fs::remove_file(long_path(&final_path)).await;
            if let Some(s) = finisher.ls_sessions.lock().unwrap().get_mut(&session) {
                s.failed += 1;
            }
        } else if let Some(s) = finisher.ls_sessions.lock().unwrap().get_mut(&session) {
            let size = s.sizes.get(&id).copied().unwrap_or_default();
            s.progress.insert(id, size);
        }
        finisher.names.release(&final_path);
    });
    let _ = target_tx.send(FileUploadTarget::Path {
        path: long_path(&path),
        result_tx,
        progress_tx: Some(progress_tx),
    });
}

fn client_for(shared: &Shared, fingerprint: &str) -> Result<LsHttpClientV2, ClientError> {
    LsHttpClientV2::try_new(
        &shared.identity.key_pem,
        &shared.identity.cert_pem,
        Some(fingerprint.to_string()),
        None,
    )
}

fn problem(e: &ClientError) -> SendProblem {
    match e {
        ClientError::StatusCode(s) if s.status == 403 => SendProblem::Declined,
        ClientError::StatusCode(s) if s.status == 409 => SendProblem::Busy,
        ClientError::Reqwest(_) => SendProblem::Offline,
        _ => SendProblem::Other,
    }
}

/// Sends files with LocalSend v2 (to devices without a FeatherSend pairing).
pub(crate) async fn send_files(
    shared: &Arc<Shared>,
    fingerprint: &str,
    alias: &str,
    ip: IpAddr,
    port: u16,
    https: bool,
    local: Vec<LocalFile>,
) {
    let total: u64 = local.iter().map(|f| f.size).sum();
    let item = files::label(local.iter().map(|f| f.rel.as_str()), local.len());
    let (transfer, cancel) = shared.transfers.create(shared, alias, &item, false, total);
    let outcome =
        send_files_inner(shared, fingerprint, ip, port, https, local, transfer, cancel).await;
    finish_outgoing(shared, transfer, alias, outcome);
}

pub(crate) fn finish_outgoing(
    shared: &Shared,
    transfer: u64,
    alias: &str,
    outcome: Result<(), (SendProblem, String)>,
) {
    match outcome {
        Ok(()) => shared.transfers.set_state(shared, transfer, TransferState::Done),
        Err(_) if shared.transfers.state(transfer) == Some(TransferState::Cancelled) => {}
        Err((problem, detail)) => {
            shared.transfers.set_state(shared, transfer, TransferState::Failed(detail.clone()));
            shared.emit(UiEvent::SendFailed { device: alias.to_string(), problem, detail });
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn send_files_inner(
    shared: &Arc<Shared>,
    fingerprint: &str,
    ip: IpAddr,
    port: u16,
    https: bool,
    local: Vec<LocalFile>,
    transfer: u64,
    cancel: CancellationToken,
) -> Result<(), (SendProblem, String)> {
    let fail = |e: ClientError| (problem(&e), format!("{e:#}"));
    let client = client_for(shared, fingerprint).map_err(fail)?;
    let protocol = if https { ProtocolType::Https } else { ProtocolType::Http };
    let host = ip.to_string();
    let mut dtos = HashMap::new();
    let mut paths: HashMap<String, (PathBuf, u64)> = HashMap::new();
    for f in &local {
        let id = uuid::Uuid::new_v4().to_string();
        dtos.insert(
            id.clone(),
            FileDto {
                id: id.clone(),
                file_name: f.rel.clone(),
                size: f.size,
                file_type: mime_guess::from_path(&f.rel).first_or_octet_stream().to_string(),
                sha256: None,
                preview: None,
                metadata: FileMetadata::from_path(&f.path),
            },
        );
        paths.insert(id, (f.path.clone(), f.size));
    }
    let request = PrepareUploadRequestDtoV2 { info: register_dto(shared), files: dtos };
    let prepared = client
        .prepare_upload(protocol, &host, port, None, request, None, cancel.clone())
        .await
        .map_err(fail)?;
    let Some(response) = prepared.response else { return Ok(()) };
    shared.ls_outgoing.lock().unwrap().insert(response.session_id.clone(), transfer);
    let mut order: Vec<(&String, &String)> = response.files.iter().collect();
    order.sort_by_key(|(id, _)| paths.get(*id).map(|(p, _)| p.clone()));
    let mut sent_before = 0u64;
    let mut result = Ok(());
    for (file_id, token) in order {
        let Some((path, size)) = paths.get(file_id).cloned() else { continue };
        let progress_shared = shared.clone();
        let mut sent = 0u64;
        let base = sent_before;
        let stream = FileContent::Path(long_path(&path)).into_stream().map(move |chunk| {
            if let Ok(bytes) = &chunk {
                sent += bytes.len() as u64;
                progress_shared.transfers.progress(&progress_shared, transfer, base + sent);
            }
            chunk
        });
        let upload = client
            .upload(
                protocol,
                &host,
                port,
                None,
                &response.session_id,
                file_id,
                token,
                reqwest::Body::wrap_stream(stream),
                cancel.clone(),
            )
            .await;
        if let Err(e) = upload {
            let _ = client.cancel(protocol, &host, port, &response.session_id).await;
            result = Err(fail(e));
            break;
        }
        sent_before += size;
    }
    shared.ls_outgoing.lock().unwrap().remove(&response.session_id);
    result
}

/// Sends a LocalSend text message (long texts as a `.txt` file).
pub(crate) async fn send_text(
    shared: &Arc<Shared>,
    fingerprint: &str,
    alias: &str,
    ip: IpAddr,
    port: u16,
    https: bool,
    text: String,
) {
    if text.len() > MAX_PREVIEW_BYTES {
        let path = std::env::temp_dir()
            .join(format!("message-{}.txt", crate::util::file_stamp(crate::util::unix_now())));
        if std::fs::write(&path, &text).is_ok() {
            let local = vec![LocalFile {
                rel: path.file_name().unwrap().to_string_lossy().into(),
                path: path.clone(),
                size: text.len() as u64,
            }];
            send_files(shared, fingerprint, alias, ip, port, https, local).await;
            let _ = std::fs::remove_file(path);
        }
        return;
    }
    let result = async {
        let client = client_for(shared, fingerprint)?;
        let id = uuid::Uuid::new_v4().to_string();
        let dto = FileDto {
            id: id.clone(),
            file_name: format!("{id}.txt"),
            size: text.len() as u64,
            file_type: "text/plain".into(),
            sha256: None,
            preview: Some(text),
            metadata: None,
        };
        let protocol = if https { ProtocolType::Https } else { ProtocolType::Http };
        let request = PrepareUploadRequestDtoV2 {
            info: register_dto(shared),
            files: HashMap::from([(id, dto)]),
        };
        client
            .prepare_upload(
                protocol,
                &ip.to_string(),
                port,
                None,
                request,
                None,
                CancellationToken::new(),
            )
            .await
            .map(|_| ())
    }
    .await;
    if let Err(e) = result {
        shared.emit(UiEvent::SendFailed {
            device: alias.to_string(),
            problem: problem(&e),
            detail: format!("{e:#}"),
        });
    }
}
