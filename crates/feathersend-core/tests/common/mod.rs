//! Test harness: in-process FeatherSend nodes on loopback, with scripted
//! answers to prompts and a log of the events they emitted.

#![allow(dead_code)]

use feathersend_core::backend::*;
use feathersend_core::clip::ClipContent;
use feathersend_core::identity::Identity;
use feathersend_core::settings::{DataDirs, Settings};
use feathersend_core::trust::TrustedDevice;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use tokio::sync::mpsc;

/// What a node's user answers to prompts.
#[derive(Clone, Copy, PartialEq)]
pub enum Answer {
    Yes,
    No,
    Never,
}

#[derive(Clone, Debug)]
pub enum Rec {
    Devices(Vec<DeviceView>),
    Transfer(TransferView),
    Text(String),
    Clipboard(ClipContent),
    PairCode(String),
    PairResult(PairOutcome),
    Received(String),
    SendFailed(SendProblem),
    Incoming(IncomingRequest),
    IncomingClipboard,
}

pub fn identity(n: usize) -> Identity {
    static IDS: OnceLock<Vec<Identity>> = OnceLock::new();
    IDS.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("fs-it-ids-{}", std::process::id()));
        (0..4).map(|i| Identity::load_or_generate(&dir.join(format!("{i}.pem"))).unwrap()).collect()
    })[n]
        .clone()
}

/// A port `p` such that `p` and `p + 1` are free right now. Parallel tests
/// take distinct pairs from a per-process range, so they never collide.
pub fn free_port_pair() -> u16 {
    use std::sync::atomic::{AtomicU16, Ordering};
    static NEXT: OnceLock<AtomicU16> = OnceLock::new();
    let next =
        NEXT.get_or_init(|| AtomicU16::new(20_000 + (std::process::id() % 400) as u16 * 100));
    loop {
        let p = next.fetch_add(2, Ordering::Relaxed);
        let free = |port: u16| {
            std::net::TcpListener::bind(("0.0.0.0", port)).is_ok()
                && std::net::UdpSocket::bind(("0.0.0.0", port)).is_ok()
        };
        if free(p) && free(p + 1) {
            return p;
        }
    }
}

pub struct Node {
    pub handle: BackendHandle,
    pub dir: tempfile::TempDir,
    pub fp: String,
    pub port: u16,
    pub events: mpsc::UnboundedReceiver<Rec>,
}

impl Node {
    pub fn downloads(&self) -> PathBuf {
        self.dir.path().join("downloads")
    }

    pub async fn wait_for<T>(&mut self, mut pick: impl FnMut(&Rec) -> Option<T>) -> T {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let rec = tokio::time::timeout_at(deadline, self.events.recv())
                .await
                .expect("timed out waiting for an event")
                .expect("event channel closed");
            if let Some(found) = pick(&rec) {
                return found;
            }
        }
    }

    /// Adds `other` by IP and waits until it shows up with the given kind.
    pub async fn add(&mut self, other: &Node, feathersend: bool) {
        self.handle
            .send(Command::AddByIp { ip: "127.0.0.1".parse().unwrap(), port: Some(other.port) });
        let fp = other.fp.clone();
        self.wait_for(|r| match r {
            Rec::Devices(list) => list
                .iter()
                .any(|d| d.fingerprint == fp && d.online && d.feathersend == feathersend)
                .then_some(()),
            _ => None,
        })
        .await;
    }
}

pub struct NodeSpec {
    pub id: usize,
    pub answer: Answer,
    pub trusted: Vec<TrustedDevice>,
    pub decision_timeout: Duration,
}

impl NodeSpec {
    pub fn new(id: usize) -> NodeSpec {
        NodeSpec {
            id,
            answer: Answer::Yes,
            trusted: Vec::new(),
            decision_timeout: Duration::from_secs(10),
        }
    }

    pub fn answer(mut self, answer: Answer) -> NodeSpec {
        self.answer = answer;
        self
    }

    pub fn trusting(mut self, id: usize, auto_accept: bool, auto_sync: bool) -> NodeSpec {
        self.trusted.push(TrustedDevice {
            fingerprint: identity(id).fingerprint,
            alias: format!("node{id}"),
            paired_at: "2026-11-01T00:00:00Z".into(),
            auto_accept_files: auto_accept,
            auto_sync_clipboard: auto_sync,
        });
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> NodeSpec {
        self.decision_timeout = timeout;
        self
    }

    pub async fn start(self) -> Node {
        let dir = tempfile::tempdir().unwrap();
        let dirs = DataDirs::at(dir.path().join("data"));
        dirs.ensure().unwrap();
        if !self.trusted.is_empty() {
            std::fs::write(&dirs.trusted_file, serde_json::to_string(&self.trusted).unwrap())
                .unwrap();
        }
        let port = free_port_pair();
        let mut settings =
            Settings::defaults(format!("node{}", self.id), dir.path().join("downloads"));
        settings.localsend_port = port;
        let mut cfg = BackendConfig::new(dirs, settings, identity(self.id));
        cfg.discovery = false;
        cfg.decision_timeout = self.decision_timeout;
        cfg.retry_delays = vec![Duration::from_millis(100)];
        let (tx, rx) = mpsc::unbounded_channel();
        let answer = self.answer;
        let sink: EventSink = Arc::new(move |event| {
            let respond = |reply: Reply<bool>| match answer {
                Answer::Yes => drop(reply.send(true)),
                Answer::No => drop(reply.send(false)),
                Answer::Never => std::mem::forget(reply),
            };
            let rec = match event {
                UiEvent::Devices(list) => Rec::Devices(list),
                UiEvent::Transfer(view) => Rec::Transfer(view),
                UiEvent::IncomingText { text, .. } => Rec::Text(text),
                UiEvent::ClipboardReceived { content, .. } => Rec::Clipboard(content),
                UiEvent::PairCode { code, reply, .. } => {
                    respond(reply);
                    Rec::PairCode(code)
                }
                UiEvent::PairResult { outcome, .. } => Rec::PairResult(outcome),
                UiEvent::Received { item, .. } => Rec::Received(item),
                UiEvent::SendFailed { problem, .. } => Rec::SendFailed(problem),
                UiEvent::IncomingTransfer { request, reply } => {
                    respond(reply);
                    Rec::Incoming(request)
                }
                UiEvent::IncomingClipboard { reply, .. } => {
                    respond(reply);
                    Rec::IncomingClipboard
                }
            };
            let _ = tx.send(rec);
        });
        let handle = Backend::start(cfg, sink).await.unwrap();
        Node { handle, dir, fp: identity(self.id).fingerprint, port, events: rx }
    }
}

pub fn read_tree(root: &std::path::Path) -> Vec<(String, Vec<u8>)> {
    fn walk(root: &std::path::Path, dir: &std::path::Path, out: &mut Vec<(String, Vec<u8>)>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else {
                let rel = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
                out.push((rel, std::fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    if root.exists() {
        walk(root, root, &mut out);
    }
    out.sort();
    out
}

pub fn make_folder(root: &std::path::Path) -> PathBuf {
    let dir = root.join("Dir");
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("a.txt"), b"alpha").unwrap();
    std::fs::write(dir.join("sub").join("b.txt"), b"beta").unwrap();
    dir
}
