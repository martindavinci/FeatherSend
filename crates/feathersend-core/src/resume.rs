//! Bookkeeping for resumable extension transfers (spec §13): a `.fspart`
//! file next to the final destination plus a JSON sidecar in
//! `data\partial\`, keyed by sender, relative path and size.

use crate::paths::long_path;
use crate::util::{hex, sha256};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

pub const PART_SUFFIX: &str = ".fspart";
pub const MAX_AGE_SECS: u64 = 7 * 24 * 3600;
/// How long a receipt lets a retry through without asking. A retry comes
/// within minutes; the same files sent again later are a new transfer.
pub const RECEIPT_APPROVAL_SECS: u64 = 10 * 60;

pub fn transfer_key(sender_fp: &str, path: &str, size: u64) -> String {
    let material = format!("{}\0{path}\0{size}", sender_fp.to_ascii_uppercase());
    hex(&sha256(material.as_bytes()))[..32].to_string()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sidecar {
    pub sender_fp: String,
    pub path: String,
    pub size: u64,
    pub sha256: Option<String>,
    pub part_path: PathBuf,
    pub final_path: PathBuf,
    pub updated_at: u64,
    /// Received for the clipboard (consent is per purpose).
    #[serde(default)]
    pub clipboard: bool,
    /// Already complete at `final_path`: a retry of the same transfer must
    /// not receive it twice.
    #[serde(default)]
    pub completed: bool,
}

impl Sidecar {
    /// Whether the same files may arrive again without asking the user: the
    /// approval of an interrupted transfer stands until it is cancelled (then
    /// the sidecar is gone), the approval of a finished one soon runs out.
    pub fn approves_retry(&self, now: u64) -> bool {
        !self.completed || now.saturating_sub(self.updated_at) <= RECEIPT_APPROVAL_SECS
    }
}

pub struct PartialStore {
    dir: PathBuf,
}

impl PartialStore {
    pub fn new(dir: &Path) -> PartialStore {
        PartialStore { dir: dir.to_path_buf() }
    }

    fn sidecar_path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.json"))
    }

    /// The sidecar and the bytes already received: the part file length, or
    /// the full size for a completed file that is still at its destination.
    pub fn find(&self, key: &str) -> Option<(Sidecar, u64)> {
        let text = std::fs::read_to_string(self.sidecar_path(key)).ok()?;
        let sidecar: Sidecar = serde_json::from_str(&text).ok()?;
        if sidecar.completed {
            let len = std::fs::metadata(long_path(&sidecar.final_path)).ok()?.len();
            return (len == sidecar.size).then_some((sidecar.clone(), sidecar.size));
        }
        let len = std::fs::metadata(long_path(&sidecar.part_path)).ok()?.len();
        (len <= sidecar.size).then_some((sidecar, len))
    }

    /// Marks a file as received at `final_path` (kept as a receipt until the
    /// purge, so a retry of the same transfer skips it).
    pub fn complete(&self, key: &str, final_path: &Path, now: u64) {
        let Ok(text) = std::fs::read_to_string(self.sidecar_path(key)) else { return };
        let Ok(mut sidecar) = serde_json::from_str::<Sidecar>(&text) else { return };
        sidecar.completed = true;
        sidecar.final_path = final_path.to_path_buf();
        sidecar.updated_at = now;
        let _ = self.create(key, &sidecar);
    }

    pub fn create(&self, key: &str, sidecar: &Sidecar) -> io::Result<()> {
        let json = serde_json::to_string(sidecar).map_err(io::Error::other)?;
        crate::settings::write_atomic(&self.sidecar_path(key), json.as_bytes())
    }

    pub fn touch(&self, key: &str, now: u64) {
        if let Some((mut sidecar, _)) = self.find(key) {
            sidecar.updated_at = now;
            let _ = self.create(key, &sidecar);
        }
    }

    /// Forgets the sidecar; `delete_part` also removes the partial data.
    pub fn remove(&self, key: &str, delete_part: bool) {
        let path = self.sidecar_path(key);
        if delete_part
            && let Some(sidecar) = std::fs::read_to_string(&path)
                .ok()
                .and_then(|t| serde_json::from_str::<Sidecar>(&t).ok())
        {
            let _ = std::fs::remove_file(long_path(&sidecar.part_path));
        }
        let _ = std::fs::remove_file(path);
    }

    /// Forgets everything received from a sender, partial data included.
    /// Files already at their destination stay.
    pub fn remove_sender(&self, sender_fp: &str) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            let from_sender = std::fs::read_to_string(&path)
                .ok()
                .and_then(|t| serde_json::from_str::<Sidecar>(&t).ok())
                .is_some_and(|s| s.sender_fp.eq_ignore_ascii_case(sender_fp));
            if from_sender && let Some(key) = path.file_stem().and_then(|s| s.to_str()) {
                self.remove(key, true);
            }
        }
    }

    /// Removes sidecars (and their part files) not updated for `max_age`.
    pub fn purge_older_than(&self, max_age: u64, now: u64) -> usize {
        let Ok(entries) = std::fs::read_dir(&self.dir) else { return 0 };
        let mut removed = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "json") {
                continue;
            }
            let stale = std::fs::read_to_string(&path)
                .ok()
                .and_then(|t| serde_json::from_str::<Sidecar>(&t).ok())
                .is_none_or(|s| now.saturating_sub(s.updated_at) > max_age);
            if stale && let Some(key) = path.file_stem().and_then(|s| s.to_str()) {
                self.remove(key, true);
                removed += 1;
            }
        }
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sidecar(dir: &Path, updated_at: u64) -> Sidecar {
        Sidecar {
            sender_fp: "AA".into(),
            path: "a.bin".into(),
            size: 10,
            sha256: None,
            part_path: dir.join(format!("a.bin{PART_SUFFIX}")),
            final_path: dir.join("a.bin"),
            updated_at,
            clipboard: false,
            completed: false,
        }
    }

    #[test]
    fn a_receipt_approves_a_retry_only_for_a_while() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = sidecar(dir.path(), 1000);
        assert!(s.approves_retry(1000 + MAX_AGE_SECS), "an interrupted transfer stays approved");
        s.completed = true;
        assert!(s.approves_retry(1000 + RECEIPT_APPROVAL_SECS));
        assert!(!s.approves_retry(1001 + RECEIPT_APPROVAL_SECS));
    }

    #[test]
    fn remove_sender_keeps_other_senders() {
        let dir = tempfile::tempdir().unwrap();
        let store = PartialStore::new(dir.path());
        let mine = sidecar(dir.path(), 1);
        std::fs::write(&mine.part_path, b"1234").unwrap();
        store.create("mine", &mine).unwrap();
        let other_part = dir.path().join("other.fspart");
        std::fs::write(&other_part, b"1").unwrap();
        let other = Sidecar { sender_fp: "BB".into(), part_path: other_part, ..mine.clone() };
        store.create("other", &other).unwrap();

        store.remove_sender("aa");

        assert_eq!(store.find("mine"), None);
        assert!(!mine.part_path.exists());
        assert!(store.find("other").is_some());
    }

    #[test]
    fn key_stable_and_32_hex() {
        let k = transfer_key("aa", "x/y.txt", 5);
        assert_eq!(k, transfer_key("AA", "x/y.txt", 5));
        assert_eq!(k.len(), 32);
        assert_ne!(k, transfer_key("AA", "x/y.txt", 6));
    }

    #[test]
    fn find_returns_part_length() {
        let dir = tempfile::tempdir().unwrap();
        let store = PartialStore::new(dir.path());
        let s = sidecar(dir.path(), 1);
        std::fs::write(&s.part_path, b"1234").unwrap();
        store.create("k", &s).unwrap();
        assert_eq!(store.find("k"), Some((s, 4)));
    }

    #[test]
    fn missing_part_means_none() {
        let dir = tempfile::tempdir().unwrap();
        let store = PartialStore::new(dir.path());
        store.create("k", &sidecar(dir.path(), 1)).unwrap();
        assert_eq!(store.find("k"), None);
    }

    #[test]
    fn completed_receipt_found_while_final_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        let store = PartialStore::new(dir.path());
        let s = sidecar(dir.path(), 1);
        std::fs::write(&s.part_path, b"0123456789").unwrap();
        store.create("k", &s).unwrap();
        std::fs::rename(&s.part_path, &s.final_path).unwrap();
        store.complete("k", &s.final_path, 2);
        let (found, len) = store.find("k").unwrap();
        assert!(found.completed);
        assert_eq!(len, 10);
        std::fs::remove_file(&s.final_path).unwrap();
        assert!(store.find("k").is_none());
    }

    #[test]
    fn purge_removes_old_only() {
        let dir = tempfile::tempdir().unwrap();
        let store = PartialStore::new(dir.path());
        let old = sidecar(dir.path(), 0);
        std::fs::write(&old.part_path, b"1").unwrap();
        store.create("old", &old).unwrap();
        let mut fresh = sidecar(dir.path(), 1000);
        fresh.part_path = dir.path().join("b.fspart");
        std::fs::write(&fresh.part_path, b"1").unwrap();
        store.create("fresh", &fresh).unwrap();
        assert_eq!(store.purge_older_than(500, 1100), 1);
        assert!(!old.part_path.exists());
        assert!(store.find("fresh").is_some());
    }
}
