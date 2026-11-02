//! The local IPv4 interfaces and when discovery must be restarted. The
//! LocalSend core binds one multicast socket per interface address, once, at
//! start: after sleep, a Wi-Fi reconnect or a new DHCP address those sockets
//! point at addresses that are gone (sends fail with os error 10065) and the
//! new address has none. [`NetWatch`] turns interface snapshots and hints
//! from Windows into restart decisions.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

/// Address changes come in bursts (adapter wake-up, DHCP): act once quiet.
pub(crate) const SETTLE: Duration = Duration::from_secs(3);
/// At most one restart per this interval: each one re-announces and may
/// scan the subnets.
pub(crate) const MIN_GAP: Duration = Duration::from_secs(30);

/// Why the GUI thinks the network changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetworkHint {
    /// An IPv4 address was added or removed (`NotifyAddrChange`).
    AddressChange,
    /// The PC woke up from sleep or hibernation (`WM_POWERBROADCAST`).
    Resume,
}

impl NetworkHint {
    pub fn reason(self) -> &'static str {
        match self {
            NetworkHint::AddressChange => "address change",
            NetworkHint::Resume => "resume from sleep",
        }
    }
}

/// One IPv4 address of a non-loopback interface.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct NetIf {
    pub name: String,
    pub ip: Ipv4Addr,
    /// False when Windows reports the interface down or not present.
    pub up: bool,
}

/// The current interfaces (blocking: `GetAdaptersAddresses` on Windows).
pub(crate) fn snapshot() -> Vec<NetIf> {
    use if_addrs::IfOperStatus;
    let Ok(interfaces) = if_addrs::get_if_addrs() else { return Vec::new() };
    interfaces
        .into_iter()
        .filter(|i| !i.is_loopback())
        .filter_map(|i| match i.ip() {
            IpAddr::V4(ip) => Some(NetIf {
                up: !matches!(
                    i.oper_status,
                    IfOperStatus::Down | IfOperStatus::NotPresent | IfOperStatus::LowerLayerDown
                ),
                name: i.name,
                ip,
            }),
            IpAddr::V6(_) => None,
        })
        .collect()
}

/// The address of the interface that holds the default route: what another
/// device on the LAN most likely reaches. Connecting a UDP socket only looks
/// up the route (192.0.2.1 is a documentation address); nothing is sent.
pub(crate) fn primary_ipv4() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    socket.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
    match socket.local_addr().ok()? {
        SocketAddr::V4(local) if !local.ip().is_unspecified() => Some(*local.ip()),
        _ => None,
    }
}

/// Addresses of the interfaces that are up, sorted, without duplicates.
pub(crate) fn usable(ifs: &[NetIf]) -> Vec<Ipv4Addr> {
    let mut addrs: Vec<Ipv4Addr> = ifs.iter().filter(|i| i.up).map(|i| i.ip).collect();
    addrs.sort();
    addrs.dedup();
    addrs
}

/// Interface filter patterns (exact addresses) of interfaces that are down,
/// so the core binds no socket to them.
pub(crate) fn down_addresses(ifs: &[NetIf]) -> Vec<String> {
    ifs.iter().filter(|i| !i.up).map(|i| i.ip.to_string()).collect()
}

/// `addrs` with `primary` first when it is one of them.
pub(crate) fn ordered(addrs: &[Ipv4Addr], primary: Option<Ipv4Addr>) -> Vec<Ipv4Addr> {
    let mut out: Vec<Ipv4Addr> = primary.filter(|p| addrs.contains(p)).into_iter().collect();
    out.extend(addrs.iter().filter(|a| Some(**a) != primary));
    out
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Decision {
    /// The addresses or the primary one changed: show them again.
    pub changed: bool,
    /// Restart discovery now, for this reason.
    pub restart: Option<&'static str>,
}

#[derive(Default)]
pub(crate) struct NetWatch {
    addrs: Option<Vec<Ipv4Addr>>,
    primary: Option<Ipv4Addr>,
    pending: Option<(Instant, &'static str)>,
    last_restart: Option<Instant>,
}

impl NetWatch {
    /// A hint from Windows: restart once quiet, even if the addresses look
    /// unchanged (a Wi-Fi drop and reconnect with the same address still
    /// loses the multicast group memberships).
    pub fn hint(&mut self, reason: &'static str, now: Instant) {
        self.pending = Some((now, reason));
    }

    /// Records the usable addresses seen at `now`. The first call only
    /// records them: discovery was just started with them.
    pub fn poll(
        &mut self,
        addrs: Vec<Ipv4Addr>,
        primary: Option<Ipv4Addr>,
        now: Instant,
    ) -> Decision {
        let moved = self.addrs.as_ref().is_some_and(|old| *old != addrs);
        let changed = self.addrs.is_none() || moved || self.primary != primary;
        if moved {
            self.pending = Some((now, "interfaces changed"));
        }
        self.addrs = Some(addrs);
        self.primary = primary;
        let restart = match self.pending {
            Some((since, reason))
                if now.saturating_duration_since(since) >= SETTLE
                    && self
                        .last_restart
                        .is_none_or(|last| now.saturating_duration_since(last) >= MIN_GAP) =>
            {
                Some(reason)
            }
            _ => None,
        };
        if restart.is_some() {
            self.pending = None;
            self.last_restart = Some(now);
        }
        Decision { changed, restart }
    }

    /// The last usable addresses and the primary one.
    pub fn addresses(&self) -> (Vec<Ipv4Addr>, Option<Ipv4Addr>) {
        (self.addrs.clone().unwrap_or_default(), self.primary)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(last: u8) -> Ipv4Addr {
        Ipv4Addr::new(192, 168, 1, last)
    }

    fn secs(s: u64) -> Duration {
        Duration::from_secs(s)
    }

    #[test]
    fn first_poll_reports_change_without_restart() {
        let mut watch = NetWatch::default();
        let d = watch.poll(vec![ip(20)], Some(ip(20)), Instant::now());
        assert_eq!(d, Decision { changed: true, restart: None });
    }

    #[test]
    fn changed_addresses_restart_once_settled() {
        let mut watch = NetWatch::default();
        let t0 = Instant::now();
        watch.poll(vec![ip(20)], Some(ip(20)), t0);
        let d = watch.poll(vec![ip(21)], Some(ip(21)), t0 + secs(60));
        assert_eq!(d, Decision { changed: true, restart: None }, "not settled yet");
        let d = watch.poll(vec![ip(21)], Some(ip(21)), t0 + secs(63));
        assert_eq!(d, Decision { changed: false, restart: Some("interfaces changed") });
        let d = watch.poll(vec![ip(21)], Some(ip(21)), t0 + secs(90));
        assert_eq!(d.restart, None, "only once");
    }

    #[test]
    fn flapping_addresses_wait_until_quiet() {
        let mut watch = NetWatch::default();
        let t0 = Instant::now();
        watch.poll(vec![ip(20)], None, t0);
        watch.poll(vec![], None, t0 + secs(60));
        watch.poll(vec![ip(20)], None, t0 + secs(62));
        assert_eq!(watch.poll(vec![ip(20)], None, t0 + secs(64)).restart, None);
        assert!(watch.poll(vec![ip(20)], None, t0 + secs(65)).restart.is_some());
    }

    #[test]
    fn losing_every_address_also_restarts() {
        // Closes the sockets of the vanished interface: no more failing sends.
        let mut watch = NetWatch::default();
        let t0 = Instant::now();
        watch.poll(vec![ip(20)], None, t0);
        watch.poll(vec![], None, t0 + secs(60));
        assert!(watch.poll(vec![], None, t0 + secs(63)).restart.is_some());
    }

    #[test]
    fn hint_restarts_with_unchanged_addresses() {
        let mut watch = NetWatch::default();
        let t0 = Instant::now();
        watch.poll(vec![ip(20)], None, t0);
        watch.hint(NetworkHint::Resume.reason(), t0 + secs(100));
        assert_eq!(watch.poll(vec![ip(20)], None, t0 + secs(101)).restart, None);
        let d = watch.poll(vec![ip(20)], None, t0 + secs(103));
        assert_eq!(d, Decision { changed: false, restart: Some("resume from sleep") });
    }

    #[test]
    fn restarts_are_rate_limited() {
        let mut watch = NetWatch::default();
        let t0 = Instant::now();
        watch.poll(vec![ip(20)], None, t0);
        watch.poll(vec![ip(21)], None, t0 + secs(60));
        assert!(watch.poll(vec![ip(21)], None, t0 + secs(63)).restart.is_some());
        watch.poll(vec![ip(22)], None, t0 + secs(65));
        assert_eq!(watch.poll(vec![ip(22)], None, t0 + secs(70)).restart, None);
        assert!(watch.poll(vec![ip(22)], None, t0 + secs(93)).restart.is_some());
    }

    #[test]
    fn hints_are_rate_limited_too() {
        let mut watch = NetWatch::default();
        let t0 = Instant::now();
        watch.poll(vec![ip(20)], None, t0);
        watch.hint("a", t0);
        assert!(watch.poll(vec![ip(20)], None, t0 + secs(3)).restart.is_some());
        watch.hint("b", t0 + secs(5));
        assert_eq!(watch.poll(vec![ip(20)], None, t0 + secs(10)).restart, None);
        assert_eq!(watch.poll(vec![ip(20)], None, t0 + secs(33)).restart, Some("b"));
    }

    #[test]
    fn primary_change_only_updates_the_display() {
        let mut watch = NetWatch::default();
        let t0 = Instant::now();
        watch.poll(vec![ip(20), ip(30)], Some(ip(20)), t0);
        let d = watch.poll(vec![ip(20), ip(30)], Some(ip(30)), t0 + secs(60));
        assert_eq!(d, Decision { changed: true, restart: None });
        assert_eq!(watch.poll(vec![ip(20), ip(30)], Some(ip(30)), t0 + secs(99)).restart, None);
    }

    #[test]
    fn usable_skips_down_interfaces() {
        let ifs = [
            NetIf { name: "Wi-Fi".into(), ip: ip(30), up: true },
            NetIf { name: "Ethernet".into(), ip: ip(9), up: false },
            NetIf { name: "VirtualBox".into(), ip: Ipv4Addr::new(192, 168, 56, 1), up: true },
            NetIf { name: "Wi-Fi".into(), ip: ip(30), up: true },
        ];
        assert_eq!(usable(&ifs), [ip(30), Ipv4Addr::new(192, 168, 56, 1)]);
    }

    #[test]
    fn down_interfaces_become_exact_filter_patterns() {
        let ifs = [
            NetIf { name: "Wi-Fi".into(), ip: ip(30), up: true },
            NetIf { name: "Ethernet".into(), ip: ip(9), up: false },
        ];
        assert_eq!(down_addresses(&ifs), ["192.168.1.9"]);
    }

    #[test]
    fn ordered_puts_the_primary_first() {
        let addrs = [ip(5), ip(20), ip(30)];
        assert_eq!(ordered(&addrs, Some(ip(20))), [ip(20), ip(5), ip(30)]);
        assert_eq!(ordered(&addrs, Some(ip(99))), addrs, "unknown primary ignored");
        assert_eq!(ordered(&addrs, None), addrs);
    }

    #[test]
    fn snapshot_has_no_loopback() {
        assert!(snapshot().iter().all(|i| !i.ip.is_loopback()));
    }
}
