//! Extension transfers and clipboard between FeatherSend nodes, including
//! resume after an interruption.

mod common;

use common::*;
use feathersend_core::backend::{Command, TransferState};
use feathersend_core::clip::{ClipContent, MIME_TEXT};
use feathersend_core::ext::client::ExtClient;
use feathersend_core::ext::dto::{ExtFile, PrepareReq, Purpose};
use feathersend_core::settings::extension_port;
use feathersend_core::util::{hex, sha256};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

fn client(from: usize, to: &Node) -> ExtClient {
    ExtClient::new(
        &identity(from),
        &to.fp,
        "127.0.0.1".parse().unwrap(),
        extension_port(to.port),
        Some(Duration::from_secs(30)),
    )
    .unwrap()
}

#[tokio::test]
async fn folder_transfer_between_backends() {
    let mut a = NodeSpec::new(0).trusting(1, false, false).start().await;
    let mut b = NodeSpec::new(1).trusting(0, false, false).start().await;
    a.add(&b, true).await;
    let src = tempfile::tempdir().unwrap();
    a.handle
        .send(Command::SendFiles { device: b.fp.clone(), paths: vec![make_folder(src.path())] });
    a.wait_for(|r| match r {
        Rec::Transfer(t) if t.state == TransferState::Done => Some(()),
        Rec::Transfer(t) if matches!(t.state, TransferState::Failed(_) | TransferState::Paused) => {
            panic!("{t:?}")
        }
        _ => None,
    })
    .await;
    let asked = b
        .wait_for(|r| match r {
            Rec::Incoming(req) => Some(req.clone()),
            _ => None,
        })
        .await;
    assert!(asked.paired);
    b.wait_for(|r| matches!(r, Rec::Received(_)).then_some(())).await;
    assert_eq!(
        read_tree(&b.downloads()),
        [
            ("Dir/a.txt".to_string(), b"alpha".to_vec()),
            ("Dir/sub/b.txt".to_string(), b"beta".to_vec())
        ]
    );
}

#[tokio::test]
async fn resume_after_interrupt() {
    let b = NodeSpec::new(1).trusting(0, true, false).start().await;
    let src = tempfile::tempdir().unwrap();
    let path = src.path().join("big.bin");
    let data: Vec<u8> = (0..3_000_000u32).map(|i| (i * 7 % 251) as u8).collect();
    std::fs::write(&path, &data).unwrap();
    let request = PrepareReq {
        purpose: Purpose::Files,
        files: vec![ExtFile {
            id: "f".into(),
            path: "big.bin".into(),
            size: data.len() as u64,
            sha256: Some(hex(&sha256(&data))),
        }],
    };
    let sender = client(0, &b);

    // First attempt, interrupted after about 1 MB.
    let first = sender.prepare(&request).await.unwrap();
    assert_eq!(first.offsets["f"], 0);
    let cancel = CancellationToken::new();
    let (seen, stop) = (Arc::new(AtomicU64::new(0)), cancel.clone());
    let progress_seen = seen.clone();
    let result = sender
        .upload(
            &first.transfer_id,
            "f",
            0,
            &path,
            Arc::new(move |n| {
                progress_seen.store(n, Ordering::Relaxed);
                if n >= 1_000_000 {
                    stop.cancel();
                }
            }),
            cancel,
        )
        .await;
    assert!(result.is_err());
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Second attempt starts where the receiver stopped.
    let second = sender.prepare(&request).await.unwrap();
    let offset = second.offsets["f"];
    assert!(offset > 0 && offset < data.len() as u64, "offset {offset}");
    let part = b.downloads().join("big.bin.fspart");
    assert_eq!(std::fs::metadata(&part).unwrap().len(), offset);
    sender
        .upload(&second.transfer_id, "f", offset, &path, Arc::new(|_| {}), CancellationToken::new())
        .await
        .unwrap();
    let finish = sender.finish(&second.transfer_id).await.unwrap();
    assert_eq!(finish.completed, ["f"]);
    assert_eq!(std::fs::read(b.downloads().join("big.bin")).unwrap(), data);
    assert!(!part.exists());
}

#[tokio::test]
async fn retry_skips_completed_files_and_keeps_folder() {
    let b = NodeSpec::new(1).trusting(0, true, false).start().await;
    let src = tempfile::tempdir().unwrap();
    let a_path = src.path().join("a.txt");
    let b_path = src.path().join("b.bin");
    std::fs::write(&a_path, b"alpha").unwrap();
    let big: Vec<u8> = (0..2_000_000u32).map(|i| (i % 253) as u8).collect();
    std::fs::write(&b_path, &big).unwrap();
    let request = PrepareReq {
        purpose: Purpose::Files,
        files: vec![
            ExtFile {
                id: "a".into(),
                path: "Dir/a.txt".into(),
                size: 5,
                sha256: Some(hex(&sha256(b"alpha"))),
            },
            ExtFile {
                id: "b".into(),
                path: "Dir/b.bin".into(),
                size: big.len() as u64,
                sha256: Some(hex(&sha256(&big))),
            },
        ],
    };
    let sender = client(0, &b);
    let first = sender.prepare(&request).await.unwrap();
    sender
        .upload(&first.transfer_id, "a", 0, &a_path, Arc::new(|_| {}), CancellationToken::new())
        .await
        .unwrap();
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    let _ = sender
        .upload(
            &first.transfer_id,
            "b",
            0,
            &b_path,
            Arc::new(move |n| {
                if n >= 500_000 {
                    stop.cancel();
                }
            }),
            cancel,
        )
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let second = sender.prepare(&request).await.unwrap();
    assert!(!second.offsets.contains_key("a"), "a is already complete");
    let offset = second.offsets["b"];
    assert!(offset > 0);
    sender
        .upload(
            &second.transfer_id,
            "b",
            offset,
            &b_path,
            Arc::new(|_| {}),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let finish = sender.finish(&second.transfer_id).await.unwrap();
    let mut completed = finish.completed.clone();
    completed.sort();
    assert_eq!(completed, ["a", "b"]);
    let tree = read_tree(&b.downloads());
    let names: Vec<&str> = tree.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["Dir/a.txt", "Dir/b.bin"], "no duplicate, same folder");
}

#[tokio::test]
async fn hash_mismatch_fails_file() {
    let b = NodeSpec::new(1).trusting(0, true, false).start().await;
    let src = tempfile::tempdir().unwrap();
    let path = src.path().join("x.txt");
    std::fs::write(&path, b"hello").unwrap();
    let sender = client(0, &b);
    let prepared = sender
        .prepare(&PrepareReq {
            purpose: Purpose::Files,
            files: vec![ExtFile {
                id: "x".into(),
                path: "x.txt".into(),
                size: 5,
                sha256: Some("00".repeat(32)),
            }],
        })
        .await
        .unwrap();
    let err = sender
        .upload(&prepared.transfer_id, "x", 0, &path, Arc::new(|_| {}), CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(err.status, 422);
    let finish = sender.finish(&prepared.transfer_id).await.unwrap();
    assert_eq!(finish.failed, ["x"]);
    assert!(!b.downloads().join("x.txt").exists());
}

#[tokio::test]
async fn clipboard_text_auto_sync() {
    let mut b = NodeSpec::new(1).trusting(0, false, true).answer(Answer::No).start().await;
    client(0, &b).clipboard(MIME_TEXT, "copiato".as_bytes().to_vec()).await.unwrap();
    let content = b
        .wait_for(|r| match r {
            Rec::Clipboard(c) => Some(c.clone()),
            Rec::IncomingClipboard => panic!("auto-sync must not ask"),
            _ => None,
        })
        .await;
    assert_eq!(content, ClipContent::Text("copiato".into()));
}

#[tokio::test]
async fn clipboard_rejected_from_unpaired() {
    let b = NodeSpec::new(1).start().await;
    let err = client(2, &b).clipboard(MIME_TEXT, b"x".to_vec()).await.unwrap_err();
    assert_eq!(err.status, 403);
}

#[tokio::test]
async fn clipboard_declined_by_user() {
    let b = NodeSpec::new(1).trusting(0, false, false).answer(Answer::No).start().await;
    let err = client(0, &b).clipboard(MIME_TEXT, b"x".to_vec()).await.unwrap_err();
    assert_eq!(err.status, 409);
}
