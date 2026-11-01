//! Devices seen on the network plus paired ones, keyed by fingerprint.

use crate::trust::TrustStore;
use ::localsend::model::discovery::DeviceType;
use std::collections::HashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

pub const ONLINE_WINDOW: Duration = Duration::from_secs(120);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceKind {
    LocalSend,
    FeatherSend,
    Paired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceClass {
    Mobile,
    Desktop,
    Other,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DeviceView {
    pub fingerprint: String,
    pub alias: String,
    /// `ip:port` when the device has been seen.
    pub address: Option<String>,
    pub class: DeviceClass,
    pub kind: DeviceKind,
    pub feathersend: bool,
    pub online: bool,
    pub auto_accept: bool,
    pub auto_sync: bool,
}

#[derive(Clone, Debug)]
pub(crate) struct Entry {
    pub alias: String,
    pub device_type: Option<DeviceType>,
    pub ip: IpAddr,
    pub port: u16,
    pub https: bool,
    pub last_seen: Instant,
    /// Answers on the extension port with the same certificate.
    pub feathersend: bool,
}

impl Entry {
    pub fn online(&self) -> bool {
        self.last_seen.elapsed() < ONLINE_WINDOW
    }
}

#[derive(Default)]
pub(crate) struct Registry {
    map: HashMap<String, Entry>,
}

fn class(device_type: &Option<DeviceType>) -> DeviceClass {
    match device_type {
        Some(DeviceType::Mobile) => DeviceClass::Mobile,
        Some(DeviceType::Web) => DeviceClass::Other,
        _ => DeviceClass::Desktop,
    }
}

impl Registry {
    /// Records a sighting. Returns true when the device is new or moved, i.e.
    /// its FeatherSend capability should be (re)probed.
    pub fn upsert(
        &mut self,
        fingerprint: &str,
        alias: &str,
        device_type: Option<DeviceType>,
        ip: IpAddr,
        port: u16,
        https: bool,
    ) -> bool {
        let fingerprint = fingerprint.to_ascii_uppercase();
        match self.map.get_mut(&fingerprint) {
            Some(entry) => {
                let moved = entry.ip != ip || entry.port != port;
                entry.alias = alias.to_string();
                entry.device_type = device_type;
                entry.ip = ip;
                entry.port = port;
                entry.https = https;
                entry.last_seen = Instant::now();
                if moved {
                    entry.feathersend = false;
                }
                moved
            }
            None => {
                self.map.insert(
                    fingerprint,
                    Entry {
                        alias: alias.to_string(),
                        device_type,
                        ip,
                        port,
                        https,
                        last_seen: Instant::now(),
                        feathersend: false,
                    },
                );
                true
            }
        }
    }

    pub fn get(&self, fingerprint: &str) -> Option<&Entry> {
        self.map.get(&fingerprint.to_ascii_uppercase())
    }

    pub fn set_feathersend(&mut self, fingerprint: &str, value: bool) {
        if let Some(entry) = self.map.get_mut(&fingerprint.to_ascii_uppercase()) {
            entry.feathersend = value;
        }
    }

    pub fn views(&self, trust: &TrustStore) -> Vec<DeviceView> {
        let mut views: Vec<DeviceView> = self
            .map
            .iter()
            .map(|(fp, e)| {
                let trusted = trust.get(fp);
                DeviceView {
                    fingerprint: fp.clone(),
                    alias: e.alias.clone(),
                    address: Some(match e.ip {
                        IpAddr::V6(ip) => format!("[{ip}]:{}", e.port),
                        IpAddr::V4(ip) => format!("{ip}:{}", e.port),
                    }),
                    class: class(&e.device_type),
                    kind: match (trusted.is_some(), e.feathersend) {
                        (true, _) => DeviceKind::Paired,
                        (false, true) => DeviceKind::FeatherSend,
                        (false, false) => DeviceKind::LocalSend,
                    },
                    feathersend: e.feathersend,
                    online: e.online(),
                    auto_accept: trusted.is_some_and(|t| t.auto_accept_files),
                    auto_sync: trusted.is_some_and(|t| t.auto_sync_clipboard),
                }
            })
            .collect();
        for t in trust.list() {
            if !self.map.contains_key(&t.fingerprint.to_ascii_uppercase()) {
                views.push(DeviceView {
                    fingerprint: t.fingerprint.clone(),
                    alias: t.alias.clone(),
                    address: None,
                    class: DeviceClass::Desktop,
                    kind: DeviceKind::Paired,
                    feathersend: true,
                    online: false,
                    auto_accept: t.auto_accept_files,
                    auto_sync: t.auto_sync_clipboard,
                });
            }
        }
        views.sort_by(|a, b| {
            b.online
                .cmp(&a.online)
                .then_with(|| a.alias.to_lowercase().cmp(&b.alias.to_lowercase()))
        });
        views
    }

    /// Online paired FeatherSend devices with auto-sync on.
    pub fn auto_sync_targets(&self, trust: &TrustStore) -> Vec<(String, Entry)> {
        self.map
            .iter()
            .filter(|(fp, e)| {
                e.online() && e.feathersend && trust.get(fp).is_some_and(|t| t.auto_sync_clipboard)
            })
            .map(|(fp, e)| (fp.clone(), e.clone()))
            .collect()
    }
}
