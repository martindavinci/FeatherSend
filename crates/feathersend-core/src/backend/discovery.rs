//! LocalSend discovery that can be restarted. The core binds its multicast
//! sockets once, per interface address, and never re-enumerates: FeatherSend
//! stops it and starts a fresh one when the network changes (decided by
//! [`super::netwatch::NetWatch`]). The core itself stays unmodified.

use super::{Shared, localsend, netwatch};
use ::localsend::discovery::{DiscoveryConfig, DiscoveryHandle};
use ::localsend::util::interface::InterfaceFilter;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

/// Builds the core's configuration for one run, with the interfaces to skip.
pub(crate) type MakeConfig = Box<dyn Fn(InterfaceFilter) -> DiscoveryConfig + Send + Sync>;

struct Running {
    handle: Arc<DiscoveryHandle>,
    stop: oneshot::Sender<()>,
}

pub(crate) struct Runner {
    make: MakeConfig,
    running: Mutex<Option<Running>>,
    /// Serializes restarts and the final stop.
    gate: tokio::sync::Mutex<()>,
}

impl Runner {
    pub fn new(make: MakeConfig) -> Runner {
        Runner { make, running: Mutex::new(None), gate: tokio::sync::Mutex::new(()) }
    }

    pub fn current(&self) -> Option<Arc<DiscoveryHandle>> {
        self.running.lock().unwrap().as_ref().map(|r| r.handle.clone())
    }

    /// Stops the running discovery, waits until its sockets are closed and
    /// starts a new one, which binds the interfaces as they are now.
    pub async fn restart(&self, filter: InterfaceFilter) -> Arc<DiscoveryHandle> {
        let _gate = self.gate.lock().await;
        self.stop_running().await;
        let (stop, stop_rx) = oneshot::channel();
        let handle = Arc::new(::localsend::discovery::start((self.make)(filter), stop_rx).await);
        if let Some(e) = handle.multicast_error() {
            tracing::warn!("multicast unavailable, HTTP discovery only: {e:#}");
        }
        *self.running.lock().unwrap() = Some(Running { handle: handle.clone(), stop });
        handle
    }

    pub async fn stop(&self) {
        let _gate = self.gate.lock().await;
        self.stop_running().await;
    }

    async fn stop_running(&self) {
        let running = self.running.lock().unwrap().take();
        if let Some(Running { handle, stop }) = running {
            let _ = stop.send(());
            handle.wait_stopped().await;
        }
    }
}

/// Interfaces Windows reports down get no socket.
pub(crate) fn filter_for(ifs: &[netwatch::NetIf]) -> InterfaceFilter {
    let down = netwatch::down_addresses(ifs);
    InterfaceFilter { whitelist: None, blacklist: (!down.is_empty()).then_some(down) }
}

/// Re-reads the interfaces; restarts discovery (and re-announces) when
/// [`netwatch::NetWatch`] says so. Runs on the 10 s network tick and after
/// hints; overlapping checks are skipped.
pub(crate) async fn check_network(shared: Arc<Shared>) {
    if shared.net_busy.swap(true, Ordering::AcqRel) {
        return;
    }
    let (ifs, decision) = shared.poll_network().await;
    if let Some(reason) = decision.restart
        && shared.discovery_enabled
    {
        let addrs: Vec<String> = netwatch::usable(&ifs).iter().map(|a| a.to_string()).collect();
        tracing::info!("restarting discovery ({reason}); interfaces: [{}]", addrs.join(", "));
        shared.discovery.restart(filter_for(&ifs)).await;
        let rescan = shared.clone();
        tokio::spawn(async move { localsend::rescan(&rescan).await });
    }
    shared.net_busy.store(false, Ordering::Release);
}

/// Records a hint and checks again once it has settled.
pub(crate) fn hint(shared: &Arc<Shared>, reason: &'static str) {
    shared.net.lock().unwrap().hint(reason, Instant::now());
    let shared = shared.clone();
    tokio::spawn(async move {
        tokio::time::sleep(netwatch::SETTLE + Duration::from_millis(100)).await;
        check_network(shared).await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;
    use ::localsend::discovery::{DEFAULT_DISCOVERY_TIMEOUT, DeviceIdentity};
    use ::localsend::model::discovery::{DeviceType, PROTOCOL_VERSION_V2, ProtocolType};
    use ::localsend::multicast::{self, MulticastConfig, MulticastDevice, MulticastEvent};
    use std::net::Ipv4Addr;
    use tokio::sync::mpsc;

    /// Not LocalSend's group, so real devices on the network stay out.
    const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 169);

    fn device(fingerprint: &str) -> MulticastDevice {
        MulticastDevice {
            alias: fingerprint.to_lowercase(),
            version: PROTOCOL_VERSION_V2.into(),
            device_model: None,
            device_type: Some(DeviceType::Desktop),
            fingerprint: fingerprint.into(),
            port: 1,
            protocol: ProtocolType::Https,
            download: false,
        }
    }

    #[tokio::test]
    async fn restarted_discovery_announces_again() {
        let port = 55_000 + (std::process::id() % 1000) as u16;
        let (heard_tx, mut heard) = mpsc::channel(64);
        let (_listener_stop, stop_rx) = oneshot::channel();
        let listener = multicast::start(
            MulticastConfig {
                group: GROUP,
                group_v6: None,
                port,
                interface_filter: InterfaceFilter::default(),
                device: device("LISTENER"),
                event_tx: heard_tx,
            },
            stop_rx,
        )
        .await;
        if listener.is_err() {
            eprintln!("skipped: no interface can carry multicast here");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let id = Identity::load_or_generate(&dir.path().join("id.pem")).unwrap();
        let runner = Runner::new(Box::new(move |filter| DiscoveryConfig {
            group: GROUP,
            group_v6: None,
            port,
            interface_filter: filter,
            device: device("RUNNER"),
            identity: DeviceIdentity {
                cert_pem: id.cert_pem.clone(),
                private_key_pem: id.key_pem.clone(),
            },
            timeout: DEFAULT_DISCOVERY_TIMEOUT,
            event_tx: None,
        }));

        let first = runner.restart(InterfaceFilter::default()).await;
        let second = runner.restart(InterfaceFilter::default()).await;
        assert!(!Arc::ptr_eq(&first, &second));
        assert!(second.multicast_error().is_none(), "the port was released and bound again");
        assert!(runner.current().is_some_and(|h| Arc::ptr_eq(&h, &second)));

        second.announce().await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            match tokio::time::timeout_at(deadline, heard.recv()).await {
                Ok(Some(MulticastEvent::Discovered { message, .. }))
                    if message.fingerprint == "RUNNER" =>
                {
                    break;
                }
                Ok(Some(_)) => {}
                _ => panic!("no announcement from the restarted discovery"),
            }
        }

        runner.stop().await;
        assert!(runner.current().is_none());
    }

    #[test]
    fn down_interfaces_are_blacklisted() {
        let ifs = [
            netwatch::NetIf { name: "Wi-Fi".into(), ip: Ipv4Addr::new(10, 0, 0, 2), up: true },
            netwatch::NetIf { name: "LAN".into(), ip: Ipv4Addr::new(10, 1, 0, 2), up: false },
        ];
        let filter = filter_for(&ifs);
        assert_eq!(filter.blacklist, Some(vec!["10.1.0.2".to_string()]));
        assert!(filter.is_ignored(&["10.1.0.2".parse().unwrap()]));
        assert!(!filter.is_ignored(&["10.0.0.2".parse().unwrap()]));
        assert_eq!(filter_for(&ifs[..1]).blacklist, None);
    }
}
