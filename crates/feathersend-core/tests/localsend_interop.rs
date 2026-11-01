//! FeatherSend against LocalSend's own core (the official implementation)
//! as the other side: the interop the phone apps rely on.

mod common;

use common::*;
use feathersend_core::backend::{Command, TransferState};
use localsend::http::client::{ClientError, LsHttpClientV2};
use localsend::http::dto_v2::{PrepareUploadRequestDtoV2, RegisterDtoV2};
use localsend::http::server::common::save::FileUploadTarget;
use localsend::http::server::v2::{PrepareUploadDecisionV2, ServerEventV2};
use localsend::http::server::web::WebConfig;
use localsend::http::server::{ServerConfigV2, TlsConfig, start_with_port};
use localsend::http::state::ClientInfo;
use localsend::model::discovery::{DeviceType, ProtocolType};
use localsend::model::transfer::{FileContent, FileDto};
use localsend::reqwest;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

fn register(id: usize, fingerprint: String) -> RegisterDtoV2 {
    RegisterDtoV2 {
        alias: format!("oracle{id}"),
        version: "2.2".into(),
        device_model: None,
        device_type: Some(DeviceType::Mobile),
        fingerprint,
        port: 1,
        protocol: ProtocolType::Https,
        download: false,
    }
}

fn file(id: &str, name: &str, size: u64) -> FileDto {
    FileDto {
        id: id.into(),
        file_name: name.into(),
        size,
        file_type: "text/plain".into(),
        sha256: None,
        preview: None,
        metadata: None,
    }
}

/// Sends `files` (name, content) from LocalSend's client with identity `id`.
async fn core_send(
    id: usize,
    claimed_fp: Option<String>,
    to: &Node,
    files: &[(&str, &[u8])],
) -> Result<(), ClientError> {
    let ident = identity(id);
    let client =
        LsHttpClientV2::try_new(&ident.key_pem, &ident.cert_pem, Some(to.fp.clone()), None)?;
    let dtos: HashMap<String, FileDto> = files
        .iter()
        .enumerate()
        .map(|(i, (name, data))| (i.to_string(), file(&i.to_string(), name, data.len() as u64)))
        .collect();
    let request = PrepareUploadRequestDtoV2 {
        info: register(id, claimed_fp.unwrap_or(ident.fingerprint)),
        files: dtos,
    };
    let prepared = client
        .prepare_upload(
            ProtocolType::Https,
            "127.0.0.1",
            to.port,
            None,
            request,
            None,
            CancellationToken::new(),
        )
        .await?;
    let response = prepared.response.expect("files accepted");
    for (file_id, token) in &response.files {
        let data = files[file_id.parse::<usize>().unwrap()].1.to_vec();
        let (tx, rx) = mpsc::channel(1);
        tx.send(data.into()).await.unwrap();
        drop(tx);
        let body = reqwest::Body::wrap_stream(FileContent::Stream(rx).into_stream());
        client
            .upload(
                ProtocolType::Https,
                "127.0.0.1",
                to.port,
                None,
                &response.session_id,
                file_id,
                token,
                body,
                CancellationToken::new(),
            )
            .await?;
    }
    Ok(())
}

#[tokio::test]
async fn send_folder_to_core_server() {
    // LocalSend's server as the receiver, accepting everything.
    let oracle = identity(3);
    let port = free_port_pair();
    let (events_tx, mut events) = mpsc::channel(16);
    let (_stop_tx, stop_rx) = oneshot::channel();
    let _server = start_with_port(
        port,
        Some(TlsConfig { cert: oracle.cert_pem.clone(), private_key: oracle.key_pem.clone() }),
        ClientInfo {
            alias: "oracle".into(),
            version: "2.2".into(),
            device_model: None,
            device_type: Some(DeviceType::Mobile),
            token: oracle.fingerprint.clone(),
        },
        None,
        Some(ServerConfigV2 { pin: None, verify_checksums: true, event_tx: events_tx }),
        WebConfig::default(),
        stop_rx,
    )
    .await
    .unwrap();
    let inbox = tempfile::tempdir().unwrap();
    let received = Arc::new(Mutex::new(Vec::new()));
    let (inbox_path, got) = (inbox.path().to_path_buf(), received.clone());
    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            match event {
                ServerEventV2::PrepareUpload { files, decision_tx, .. } => {
                    let _ = decision_tx
                        .send(PrepareUploadDecisionV2::Accept(files.keys().cloned().collect()));
                }
                ServerEventV2::FileUpload { file_id, file, target_tx, .. } => {
                    got.lock().unwrap().push(file.file_name.clone());
                    let (result_tx, _result_rx) = oneshot::channel();
                    let _ = target_tx.send(FileUploadTarget::Path {
                        path: inbox_path.join(file_id),
                        result_tx,
                        progress_tx: None,
                    });
                }
                _ => {}
            }
        }
    });

    let mut node = NodeSpec::new(0).start().await;
    node.handle.send(Command::AddByIp { ip: "127.0.0.1".parse().unwrap(), port: Some(port) });
    let fp = oracle.fingerprint.clone();
    node.wait_for(|r| match r {
        Rec::Devices(list) => list.iter().any(|d| d.fingerprint == fp).then_some(()),
        _ => None,
    })
    .await;
    let src = tempfile::tempdir().unwrap();
    let folder = make_folder(src.path());
    node.handle
        .send(Command::SendFiles { device: oracle.fingerprint.clone(), paths: vec![folder] });
    node.wait_for(|r| match r {
        Rec::Transfer(t) if t.state == TransferState::Done => Some(()),
        Rec::Transfer(t) if matches!(t.state, TransferState::Failed(_)) => panic!("{t:?}"),
        _ => None,
    })
    .await;
    let mut names = received.lock().unwrap().clone();
    names.sort();
    assert_eq!(names, ["Dir/a.txt", "Dir/sub/b.txt"]);
}

#[tokio::test]
async fn core_client_sends_to_backend() {
    let mut node = NodeSpec::new(0).start().await;
    core_send(1, None, &node, &[("Dir/a.txt", b"alpha"), ("Dir/sub/b.txt", b"beta")])
        .await
        .unwrap();
    node.wait_for(|r| matches!(r, Rec::Received(_)).then_some(())).await;
    assert_eq!(
        read_tree(&node.downloads()),
        [
            ("Dir/a.txt".to_string(), b"alpha".to_vec()),
            ("Dir/sub/b.txt".to_string(), b"beta".to_vec())
        ]
    );
}

#[tokio::test]
async fn text_message_received() {
    let mut node = NodeSpec::new(0).start().await;
    let ident = identity(1);
    let client =
        LsHttpClientV2::try_new(&ident.key_pem, &ident.cert_pem, Some(node.fp.clone()), None)
            .unwrap();
    let mut dto = file("m", "m.txt", 5);
    dto.preview = Some("ciao!".into());
    let request = PrepareUploadRequestDtoV2 {
        info: register(1, ident.fingerprint.clone()),
        files: HashMap::from([("m".to_string(), dto)]),
    };
    let result = client
        .prepare_upload(
            ProtocolType::Https,
            "127.0.0.1",
            node.port,
            None,
            request,
            None,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(result.response.is_none(), "message only: 204");
    let text = node
        .wait_for(|r| match r {
            Rec::Text(t) => Some(t.clone()),
            _ => None,
        })
        .await;
    assert_eq!(text, "ciao!");
}

#[tokio::test]
async fn spoofed_fingerprint_is_not_trusted() {
    // Node trusts identity 2 with auto-accept; identity 1 claims to be 2.
    let mut node = NodeSpec::new(0).trusting(2, true, false).answer(Answer::No).start().await;
    let err =
        core_send(1, Some(identity(2).fingerprint), &node, &[("a.txt", b"x")]).await.unwrap_err();
    assert!(matches!(err, ClientError::StatusCode(ref s) if s.status == 403), "{err:?}");
    let asked = node
        .wait_for(|r| match r {
            Rec::Incoming(req) => Some(req.clone()),
            _ => None,
        })
        .await;
    assert!(!asked.paired, "the claimed fingerprint must not count");
    assert!(read_tree(&node.downloads()).is_empty());
}

#[tokio::test]
async fn decision_times_out_with_decline() {
    let node = NodeSpec::new(0).answer(Answer::Never).timeout(Duration::from_secs(1)).start().await;
    let started = std::time::Instant::now();
    let err = core_send(1, None, &node, &[("a.txt", b"x")]).await.unwrap_err();
    assert!(matches!(err, ClientError::StatusCode(ref s) if s.status == 403), "{err:?}");
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[tokio::test]
async fn traversal_name_rejected() {
    let node = NodeSpec::new(0).start().await;
    let result = core_send(1, None, &node, &[("../evil.txt", b"x"), ("ok.txt", b"y")]).await;
    assert!(result.is_err(), "the bad file fails");
    assert!(!node.dir.path().join("evil.txt").exists());
    assert!(!node.downloads().parent().unwrap().join("evil.txt").exists());
}
