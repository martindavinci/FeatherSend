//! User settings and the data directory (installed or portable).

use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

/// A file with this name next to the exe switches to portable mode.
pub const PORTABLE_MARKER: &str = "portable.txt";
pub const MAX_ALIAS_CHARS: usize = 64;

/// Where FeatherSend keeps its files.
#[derive(Clone, Debug, PartialEq)]
pub struct DataDirs {
    pub root: PathBuf,
    pub settings_file: PathBuf,
    pub trusted_file: PathBuf,
    pub identity_file: PathBuf,
    pub logs_dir: PathBuf,
    pub partial_dir: PathBuf,
}

impl DataDirs {
    /// `<exe dir>\data` when the portable marker exists, else
    /// `%APPDATA%\FeatherSend` (or `<exe dir>\data` without `APPDATA`).
    pub fn resolve(exe_dir: &Path, appdata: Option<&Path>) -> DataDirs {
        let root = match appdata {
            Some(appdata) if !exe_dir.join(PORTABLE_MARKER).exists() => appdata.join("FeatherSend"),
            _ => exe_dir.join("data"),
        };
        DataDirs::at(root)
    }

    pub fn at(root: PathBuf) -> DataDirs {
        DataDirs {
            settings_file: root.join("settings.json"),
            trusted_file: root.join("trusted.json"),
            identity_file: root.join("identity").join("identity.pem"),
            logs_dir: root.join("logs"),
            partial_dir: root.join("partial"),
            root,
        }
    }

    pub fn ensure(&self) -> io::Result<()> {
        for dir in [&self.root, &self.logs_dir, &self.partial_dir] {
            std::fs::create_dir_all(dir)?;
        }
        if let Some(parent) = self.identity_file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub alias: String,
    pub download_dir: PathBuf,
    /// `"auto"`, `"en"` or `"it"`.
    pub language: String,
    pub start_with_windows: bool,
    pub check_updates: bool,
    pub clipboard_hotkey: String,
    /// Lets unpaired LocalSend devices ask to send (always with approval).
    pub accept_from_unpaired: bool,
    /// LocalSend port; the extension server always uses the next one
    /// ([`extension_port`]), so peers find it without announcing it.
    pub localsend_port: u16,
}

pub fn extension_port(localsend_port: u16) -> u16 {
    localsend_port.wrapping_add(1)
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            alias: String::new(),
            download_dir: PathBuf::new(),
            language: "auto".into(),
            start_with_windows: false,
            check_updates: true,
            clipboard_hotkey: "Ctrl+Alt+C".into(),
            accept_from_unpaired: true,
            localsend_port: 53317,
        }
    }
}

impl Settings {
    pub fn defaults(alias: String, download_dir: PathBuf) -> Settings {
        Settings { alias, download_dir, ..Settings::default() }.normalized(None)
    }

    /// Reads `path`. A missing file gives `defaults`; an unreadable or corrupt
    /// one is renamed to `settings.json.bak` and gives `defaults` too.
    pub fn load(path: &Path, defaults: &Settings) -> Settings {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return defaults.clone(),
            Err(e) => {
                tracing::warn!("cannot read {}: {e}", path.display());
                return defaults.clone();
            }
        };
        match serde_json::from_str::<Settings>(&text) {
            Ok(settings) => settings.normalized(Some(defaults)),
            Err(e) => {
                tracing::warn!("corrupt {}: {e}; using defaults", path.display());
                let _ = std::fs::rename(path, path.with_extension("json.bak"));
                defaults.clone()
            }
        }
    }

    /// Writes atomically (temporary file, then rename).
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let json = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        write_atomic(path, json.as_bytes())
    }

    fn normalized(mut self, defaults: Option<&Settings>) -> Settings {
        self.alias = self.alias.trim().chars().take(MAX_ALIAS_CHARS).collect();
        if let Some(defaults) = defaults {
            if self.alias.is_empty() {
                self.alias = defaults.alias.clone();
            }
            if self.download_dir.as_os_str().is_empty() {
                self.download_dir = defaults.download_dir.clone();
            }
        }
        if self.alias.is_empty() {
            self.alias = "FeatherSend".into();
        }
        if !matches!(self.language.as_str(), "auto" | "en" | "it") {
            self.language = "auto".into();
        }
        self
    }
}

pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> Settings {
        Settings::defaults("PC-UFFICIO".into(), PathBuf::from(r"C:\Users\u\Downloads\FeatherSend"))
    }

    #[test]
    fn portable_marker_selects_exe_dir() {
        let exe = tempfile::tempdir().unwrap();
        std::fs::write(exe.path().join(PORTABLE_MARKER), "").unwrap();
        let dirs = DataDirs::resolve(exe.path(), Some(Path::new(r"C:\AppData")));
        assert_eq!(dirs.root, exe.path().join("data"));
        assert_eq!(
            dirs.identity_file,
            exe.path().join("data").join("identity").join("identity.pem")
        );
    }

    #[test]
    fn appdata_used_without_marker() {
        let exe = tempfile::tempdir().unwrap();
        let dirs = DataDirs::resolve(exe.path(), Some(Path::new(r"C:\AppData")));
        assert_eq!(dirs.root, Path::new(r"C:\AppData").join("FeatherSend"));
        assert_eq!(dirs.settings_file, Path::new(r"C:\AppData\FeatherSend\settings.json"));
    }

    #[test]
    fn missing_settings_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let loaded = Settings::load(&dir.path().join("settings.json"), &defaults());
        assert_eq!(loaded, defaults());
        assert_eq!(loaded.clipboard_hotkey, "Ctrl+Alt+C");
        assert_eq!(loaded.localsend_port, 53317);
        assert_eq!(extension_port(loaded.localsend_port), 53318);
    }

    #[test]
    fn corrupt_settings_backed_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(Settings::load(&path, &defaults()), defaults());
        assert!(dir.path().join("settings.json.bak").exists());
        assert!(!path.exists());
    }

    #[test]
    fn unknown_keys_ignored_and_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{"alias":"Vecchio PC","future_option":42,"language":"it"}"#)
            .unwrap();
        let loaded = Settings::load(&path, &defaults());
        assert_eq!(loaded.alias, "Vecchio PC");
        assert_eq!(loaded.language, "it");
        assert_eq!(loaded.download_dir, defaults().download_dir);
        loaded.save(&path).unwrap();
        assert_eq!(Settings::load(&path, &defaults()), loaded);
    }

    #[test]
    fn alias_truncated_to_64_chars() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let long = "x".repeat(100);
        std::fs::write(&path, format!(r#"{{"alias":"{long}"}}"#)).unwrap();
        assert_eq!(Settings::load(&path, &defaults()).alias.chars().count(), MAX_ALIAS_CHARS);
    }
}
