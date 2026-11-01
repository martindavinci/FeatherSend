//! Paired devices, keyed by certificate fingerprint (`trusted.json`).

use crate::identity::fingerprints_equal;
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrustedDevice {
    pub fingerprint: String,
    pub alias: String,
    /// RFC 3339 UTC.
    pub paired_at: String,
    #[serde(default)]
    pub auto_accept_files: bool,
    #[serde(default)]
    pub auto_sync_clipboard: bool,
}

#[derive(Debug)]
pub struct TrustStore {
    path: PathBuf,
    devices: Vec<TrustedDevice>,
}

impl TrustStore {
    /// A missing or corrupt file gives an empty store (a corrupt file is kept
    /// as `trusted.json.bak`).
    pub fn load(path: &Path) -> TrustStore {
        let devices = match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                tracing::warn!("corrupt {}: {e}", path.display());
                let _ = std::fs::rename(path, path.with_extension("json.bak"));
                Vec::new()
            }),
            Err(_) => Vec::new(),
        };
        TrustStore { path: path.to_path_buf(), devices }
    }

    pub fn save(&self) -> io::Result<()> {
        let json = serde_json::to_string_pretty(&self.devices).map_err(io::Error::other)?;
        crate::settings::write_atomic(&self.path, json.as_bytes())
    }

    pub fn get(&self, fingerprint: &str) -> Option<&TrustedDevice> {
        self.devices.iter().find(|d| fingerprints_equal(&d.fingerprint, fingerprint))
    }

    pub fn is_trusted(&self, fingerprint: &str) -> bool {
        self.get(fingerprint).is_some()
    }

    /// Adds or replaces the device with the same fingerprint.
    pub fn upsert(&mut self, mut device: TrustedDevice) {
        device.fingerprint = device.fingerprint.to_ascii_uppercase();
        match self.devices.iter_mut().find(|d| d.fingerprint == device.fingerprint) {
            Some(existing) => *existing = device,
            None => self.devices.push(device),
        }
    }

    pub fn remove(&mut self, fingerprint: &str) -> bool {
        let before = self.devices.len();
        self.devices.retain(|d| !fingerprints_equal(&d.fingerprint, fingerprint));
        self.devices.len() != before
    }

    pub fn set_flags(
        &mut self,
        fingerprint: &str,
        auto_accept: Option<bool>,
        auto_sync: Option<bool>,
    ) -> bool {
        let Some(device) =
            self.devices.iter_mut().find(|d| fingerprints_equal(&d.fingerprint, fingerprint))
        else {
            return false;
        };
        if let Some(v) = auto_accept {
            device.auto_accept_files = v;
        }
        if let Some(v) = auto_sync {
            device.auto_sync_clipboard = v;
        }
        true
    }

    pub fn list(&self) -> &[TrustedDevice] {
        &self.devices
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(fp: &str) -> TrustedDevice {
        TrustedDevice {
            fingerprint: fp.into(),
            alias: "PC".into(),
            paired_at: "2026-11-01T10:00:00Z".into(),
            auto_accept_files: false,
            auto_sync_clipboard: false,
        }
    }

    #[test]
    fn roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trusted.json");
        let mut store = TrustStore::load(&path);
        store.upsert(device("AA"));
        assert!(store.set_flags("aa", Some(true), None));
        store.save().unwrap();
        let loaded = TrustStore::load(&path);
        assert_eq!(loaded.list().len(), 1);
        assert!(loaded.get("AA").unwrap().auto_accept_files);
        assert!(!loaded.get("AA").unwrap().auto_sync_clipboard);
    }

    #[test]
    fn lookup_case_insensitive() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = TrustStore::load(&dir.path().join("t.json"));
        store.upsert(device("abcd"));
        assert!(store.is_trusted("ABCD"));
        store.upsert(device("ABCD"));
        assert_eq!(store.list().len(), 1);
    }

    #[test]
    fn remove_unknown_false() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = TrustStore::load(&dir.path().join("t.json"));
        assert!(!store.remove("FF"));
        store.upsert(device("FF"));
        assert!(store.remove("ff"));
        assert!(!store.set_flags("FF", Some(true), None));
    }

    #[test]
    fn corrupt_file_empty_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trusted.json");
        std::fs::write(&path, "[{").unwrap();
        assert!(TrustStore::load(&path).list().is_empty());
        assert!(dir.path().join("trusted.json.bak").exists());
    }
}
