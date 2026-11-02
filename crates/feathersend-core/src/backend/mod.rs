//! The network backend the GUI drives: it owns the LocalSend v2 server and
//! discovery (through LocalSend's core), the extension server, the trust store
//! and every transfer. The GUI sends [`Command`]s and receives [`UiEvent`]s;
//! anything that needs the user's decision carries a [`Reply`].

mod devices;
mod discovery;
mod ext_handler;
mod files;
mod localsend;
mod netwatch;
mod outgoing;
mod transfers;
mod upload;

pub use devices::{DeviceClass, DeviceKind, DeviceView};
pub use netwatch::NetworkHint;
pub use transfers::{TransferState, TransferView};

use crate::clip::ClipContent;
use crate::ext::server::{ExtServer, ExtServerHandle};
use crate::identity::Identity;
use crate::pairing::PendingPairings;
use crate::paths::NameReservations;
use crate::resume::{MAX_AGE_SECS, PartialStore};
use crate::settings::{DataDirs, Settings, extension_port};
use crate::trust::TrustStore;
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

pub type Reply<T> = oneshot::Sender<T>;
pub type EventSink = Arc<dyn Fn(UiEvent) + Send + Sync>;

pub enum Command {
    /// Files and/or folders; folders are sent with their structure.
    SendFiles {
        device: String,
        paths: Vec<PathBuf>,
    },
    /// A text message (LocalSend message to any device).
    SendText {
        device: String,
        text: String,
    },
    /// The current clipboard to one device (hotkey / button).
    SendClipboard {
        device: String,
        content: ClipContent,
    },
    /// A clipboard change, for every online paired device with auto-sync on.
    SyncClipboard {
        content: ClipContent,
    },
    Pair {
        device: String,
    },
    Unpair {
        fingerprint: String,
    },
    SetDeviceFlags {
        fingerprint: String,
        auto_accept: Option<bool>,
        auto_sync: Option<bool>,
    },
    AddByIp {
        ip: IpAddr,
        port: Option<u16>,
    },
    CancelTransfer {
        id: u64,
    },
    ResumeTransfer {
        id: u64,
    },
    SetPaused(bool),
    ApplySettings(Settings),
    Rescan,
    /// Windows reported a network change: rebind discovery once it settles.
    NetworkChanged(NetworkHint),
}

/// A pending incoming transfer, for the approval prompt.
#[derive(Clone, Debug)]
pub struct IncomingRequest {
    pub from: String,
    pub fingerprint: String,
    pub paired: bool,
    pub count: usize,
    pub total_bytes: u64,
    /// At most the first 10 names.
    pub names: Vec<String>,
    pub clipboard: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PairOutcome {
    Paired,
    Failed,
    NotFeatherSend,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendProblem {
    Declined,
    Busy,
    Offline,
    Other,
}

/// How other devices reach this one ("Add by IP" on the other side).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdentityView {
    pub alias: String,
    /// IPv4 addresses of the interfaces that are up, default route first.
    pub addresses: Vec<Ipv4Addr>,
    /// `addresses[0]` is the default-route interface's address.
    pub primary: bool,
    /// The LocalSend port (the extension port is this + 1).
    pub port: u16,
}

pub enum UiEvent {
    Devices(Vec<DeviceView>),
    /// This device's name, addresses and port (at start and on changes).
    Identity(IdentityView),
    IncomingTransfer {
        request: IncomingRequest,
        reply: Reply<bool>,
    },
    IncomingText {
        from: String,
        text: String,
    },
    IncomingClipboard {
        from: String,
        content: ClipContent,
        reply: Reply<bool>,
    },
    /// Write this to the clipboard (accepted, or auto-synced).
    ClipboardReceived {
        from: String,
        content: ClipContent,
    },
    PairCode {
        alias: String,
        code: String,
        reply: Reply<bool>,
    },
    PairResult {
        alias: String,
        outcome: PairOutcome,
    },
    Transfer(TransferView),
    Received {
        from: String,
        item: String,
    },
    SendFailed {
        device: String,
        problem: SendProblem,
        detail: String,
    },
}

pub struct BackendConfig {
    pub dirs: DataDirs,
    pub settings: Settings,
    pub identity: Identity,
    /// Multicast + subnet discovery; off in tests.
    pub discovery: bool,
    /// How long incoming requests wait for the user (spec: 120 s).
    pub decision_timeout: Duration,
    /// Waits between automatic retries of an interrupted extension transfer.
    pub retry_delays: Vec<Duration>,
}

impl BackendConfig {
    pub fn new(dirs: DataDirs, settings: Settings, identity: Identity) -> BackendConfig {
        BackendConfig {
            dirs,
            settings,
            identity,
            discovery: true,
            decision_timeout: Duration::from_secs(120),
            retry_delays: [2, 5, 15].map(Duration::from_secs).to_vec(),
        }
    }
}

pub(crate) struct Shared {
    pub identity: Identity,
    pub settings: Mutex<Settings>,
    pub trust: Mutex<TrustStore>,
    pub names: NameReservations,
    pub partial: PartialStore,
    pub sink: EventSink,
    pub decision_timeout: Duration,
    pub retry_delays: Vec<Duration>,
    pub ls_port: u16,
    pub devices: Mutex<devices::Registry>,
    pub transfers: transfers::TransferTable,
    pub pairing: Mutex<PendingPairings>,
    /// Pairing id → (created, the local user's decision).
    pub pair_waits: Mutex<HashMap<String, (u64, oneshot::Receiver<bool>)>>,
    pub ls_server: Arc<::localsend::http::server::ServerHandle>,
    pub ls_sessions: Mutex<HashMap<String, localsend::LsSession>>,
    /// Outgoing LocalSend session id → transfer id (for peer cancellation).
    pub ls_outgoing: Mutex<HashMap<String, u64>>,
    pub ext_incoming: Mutex<HashMap<String, ext_handler::ExtIncoming>>,
    pub paused: AtomicBool,
    pub text_limit: Mutex<localsend::RateLimit>,
    pub discovery: discovery::Runner,
    /// False in tests: no multicast, no network checks.
    pub discovery_enabled: bool,
    pub net: Mutex<netwatch::NetWatch>,
    /// A network check is running.
    pub net_busy: AtomicBool,
}

impl Shared {
    pub fn emit(&self, event: UiEvent) {
        (self.sink)(event)
    }

    pub fn ls_server(&self) -> &::localsend::http::server::ServerHandle {
        &self.ls_server
    }

    pub fn alias(&self) -> String {
        self.settings.lock().unwrap().alias.clone()
    }

    pub fn download_dir(&self) -> PathBuf {
        self.settings.lock().unwrap().download_dir.clone()
    }

    pub fn emit_devices(&self) {
        let trust = self.trust.lock().unwrap();
        let views = self.devices.lock().unwrap().views(&trust);
        drop(trust);
        self.emit(UiEvent::Devices(views));
    }

    pub fn emit_identity(&self) {
        let (addrs, primary) = self.net.lock().unwrap().addresses();
        let addresses = netwatch::ordered(&addrs, primary);
        let primary = primary.is_some() && addresses.first() == primary.as_ref();
        self.emit(UiEvent::Identity(IdentityView {
            alias: self.alias(),
            addresses,
            primary,
            port: self.ls_port,
        }));
    }

    /// Reads the interfaces, records them and returns the decision (the
    /// enumeration blocks, so it runs on the blocking pool).
    pub async fn poll_network(&self) -> (Vec<netwatch::NetIf>, netwatch::Decision) {
        let (ifs, primary) =
            tokio::task::spawn_blocking(|| (netwatch::snapshot(), netwatch::primary_ipv4()))
                .await
                .unwrap_or_default();
        let decision = self.net.lock().unwrap().poll(
            netwatch::usable(&ifs),
            primary,
            std::time::Instant::now(),
        );
        if decision.changed {
            self.emit_identity();
        }
        (ifs, decision)
    }

    /// Asks the user through `make_event` and waits at most the decision
    /// timeout; no answer counts as "no".
    pub async fn ask(&self, make_event: impl FnOnce(Reply<bool>) -> UiEvent) -> bool {
        let (tx, rx) = oneshot::channel();
        self.emit(make_event(tx));
        matches!(tokio::time::timeout(self.decision_timeout, rx).await, Ok(Ok(true)))
    }
}

pub struct BackendHandle {
    cmd_tx: mpsc::UnboundedSender<Command>,
    stop_tx: Option<oneshot::Sender<()>>,
    task: tokio::task::JoinHandle<()>,
    ls_port: u16,
}

impl BackendHandle {
    pub fn send(&self, command: Command) {
        let _ = self.cmd_tx.send(command);
    }

    /// A clonable sender for other threads (the GUI).
    pub fn sender(&self) -> mpsc::UnboundedSender<Command> {
        self.cmd_tx.clone()
    }

    pub fn localsend_port(&self) -> u16 {
        self.ls_port
    }

    pub async fn shutdown(mut self) {
        if let Some(stop) = self.stop_tx.take() {
            let _ = stop.send(());
        }
        let _ = (&mut self.task).await;
    }
}

pub struct Backend;

impl Backend {
    /// Starts the servers (and discovery) on the current tokio runtime.
    pub async fn start(cfg: BackendConfig, sink: EventSink) -> anyhow::Result<BackendHandle> {
        cfg.dirs.ensure()?;
        let partial = PartialStore::new(&cfg.dirs.partial_dir);
        let purged = partial.purge_older_than(MAX_AGE_SECS, crate::util::unix_now());
        if purged > 0 {
            tracing::info!("purged {purged} stale partial transfers");
        }
        outgoing::purge_clipboard_temp();
        let trust = TrustStore::load(&cfg.dirs.trusted_file);

        let (ls_events_tx, ls_events_rx) = mpsc::channel(64);
        let (ls_stop_tx, ls_stop_rx) = oneshot::channel();
        let ls_server = localsend::start_server(
            &cfg.identity,
            &cfg.settings,
            cfg.settings.localsend_port,
            ls_events_tx,
            ls_stop_rx,
        )
        .await?;
        let ls_port = ls_server.port();
        let ls_server = Arc::new(ls_server);

        let (discovery_tx, discovery_rx) = mpsc::channel(64);
        let runner = discovery::Runner::new(localsend::discovery_config(
            &cfg.settings.alias,
            &cfg.identity,
            ls_port,
            discovery_tx,
        ));

        let shared = Arc::new(Shared {
            identity: cfg.identity.clone(),
            settings: Mutex::new(cfg.settings.clone()),
            trust: Mutex::new(trust),
            names: NameReservations::default(),
            partial,
            sink,
            decision_timeout: cfg.decision_timeout,
            retry_delays: cfg.retry_delays.clone(),
            ls_port,
            devices: Mutex::new(devices::Registry::default()),
            transfers: transfers::TransferTable::default(),
            pairing: Mutex::new(PendingPairings::default()),
            pair_waits: Mutex::new(HashMap::new()),
            ls_server: ls_server.clone(),
            ls_sessions: Mutex::new(HashMap::new()),
            ls_outgoing: Mutex::new(HashMap::new()),
            ext_incoming: Mutex::new(HashMap::new()),
            paused: AtomicBool::new(false),
            text_limit: Mutex::new(localsend::RateLimit::for_text_messages()),
            discovery: runner,
            discovery_enabled: cfg.discovery,
            net: Mutex::new(netwatch::NetWatch::default()),
            net_busy: AtomicBool::new(false),
        });

        let ext_server = match ExtServer::start(
            extension_port(ls_port),
            &cfg.identity,
            Arc::new(ext_handler::Handler(shared.clone())),
        )
        .await
        {
            Ok(server) => server,
            Err(e) => {
                let _ = ls_stop_tx.send(());
                ls_server.wait_stopped().await;
                return Err(e);
            }
        };

        // The first snapshot also tells discovery which interfaces are down.
        let (ifs, _) = shared.poll_network().await;
        if cfg.discovery {
            shared.discovery.restart(discovery::filter_for(&ifs)).await;
            let rescan = shared.clone();
            tokio::spawn(async move { localsend::rescan(&rescan).await });
        }

        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
        let (stop_tx, stop_rx) = oneshot::channel();
        let task = tokio::spawn(run(
            shared.clone(),
            cmd_rx,
            ls_events_rx,
            discovery_rx,
            stop_rx,
            Stoppers { ls_stop_tx, ext_server },
        ));
        shared.emit_devices();
        Ok(BackendHandle { cmd_tx, stop_tx: Some(stop_tx), task, ls_port })
    }
}

struct Stoppers {
    ls_stop_tx: oneshot::Sender<()>,
    ext_server: ExtServerHandle,
}

async fn run(
    shared: Arc<Shared>,
    mut cmd_rx: mpsc::UnboundedReceiver<Command>,
    mut ls_events: mpsc::Receiver<::localsend::http::server::v2::ServerEventV2>,
    mut discovery_events: mpsc::Receiver<::localsend::discovery::DiscoveryEvent>,
    mut stop_rx: oneshot::Receiver<()>,
    stoppers: Stoppers,
) {
    let mut tick = tokio::time::interval(Duration::from_secs(30));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Interfaces are re-read every 10 s: a GetAdaptersAddresses call, about
    // a millisecond. Hints from Windows only make it react sooner.
    let mut net_tick = tokio::time::interval(Duration::from_secs(10));
    net_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    net_tick.tick().await;
    loop {
        tokio::select! {
            _ = &mut stop_rx => break,
            Some(cmd) = cmd_rx.recv() => handle_command(&shared, cmd),
            Some(event) = ls_events.recv() => localsend::handle_server_event(&shared, event).await,
            Some(event) = discovery_events.recv() => localsend::handle_discovery_event(&shared, event),
            _ = tick.tick() => {
                if let Some(discovery) = shared.discovery.current() {
                    tokio::spawn(async move { discovery.announce().await });
                }
                shared.emit_devices();
            }
            _ = net_tick.tick(), if shared.discovery_enabled => {
                tokio::spawn(discovery::check_network(shared.clone()));
            }
        }
    }
    shared.discovery.stop().await;
    stoppers.ext_server.stop().await;
    let _ = stoppers.ls_stop_tx.send(());
    let _ = tokio::time::timeout(Duration::from_secs(2), shared.ls_server.wait_stopped()).await;
}

fn handle_command(shared: &Arc<Shared>, cmd: Command) {
    let shared = shared.clone();
    match cmd {
        Command::SendFiles { device, paths } => {
            tokio::spawn(async move { outgoing::send_files(&shared, &device, paths, false).await });
        }
        Command::SendText { device, text } => {
            tokio::spawn(async move { outgoing::send_text(&shared, &device, text).await });
        }
        Command::SendClipboard { device, content } => {
            tokio::spawn(async move { outgoing::send_clipboard(&shared, &device, content).await });
        }
        Command::SyncClipboard { content } => {
            tokio::spawn(async move { outgoing::sync_clipboard(&shared, content).await });
        }
        Command::Pair { device } => {
            tokio::spawn(async move { outgoing::pair(&shared, &device).await });
        }
        Command::Unpair { fingerprint } => {
            let mut trust = shared.trust.lock().unwrap();
            if trust.remove(&fingerprint)
                && let Err(e) = trust.save()
            {
                tracing::warn!("cannot save trusted devices: {e}");
            }
            drop(trust);
            ext_handler::drop_peer_transfers(&shared, &fingerprint);
            shared.emit_devices();
        }
        Command::SetDeviceFlags { fingerprint, auto_accept, auto_sync } => {
            let mut trust = shared.trust.lock().unwrap();
            if trust.set_flags(&fingerprint, auto_accept, auto_sync)
                && let Err(e) = trust.save()
            {
                tracing::warn!("cannot save trusted devices: {e}");
            }
            drop(trust);
            shared.emit_devices();
        }
        Command::AddByIp { ip, port } => {
            tokio::spawn(async move {
                let port = port.unwrap_or(::localsend::multicast::DEFAULT_PORT);
                localsend::register_with(&shared, ip, port).await;
            });
        }
        Command::CancelTransfer { id } => {
            shared.transfers.cancel(&shared, id);
            ext_handler::withdraw_cancelled(&shared, id);
        }
        Command::ResumeTransfer { id } => {
            if let Some(job) = shared.transfers.take_resume(id) {
                tokio::spawn(async move { outgoing::resume(&shared, id, job).await });
            }
        }
        Command::SetPaused(paused) => shared.paused.store(paused, Ordering::Relaxed),
        Command::ApplySettings(settings) => {
            *shared.settings.lock().unwrap() = settings;
        }
        Command::Rescan => {
            tokio::spawn(async move { localsend::rescan(&shared).await });
        }
        Command::NetworkChanged(hint) => {
            if shared.discovery_enabled {
                discovery::hint(&shared, hint.reason());
            }
        }
    }
}
