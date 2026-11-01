//! Turning names received from peers into safe paths below the download
//! folder (spec §11). Peers are untrusted: every received name goes through
//! [`sanitize_relative`] before it touches the disk.

use std::collections::HashSet;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use unicode_normalization::UnicodeNormalization;

pub const MAX_COMPONENTS: usize = 32;

#[derive(Debug, PartialEq, Eq)]
pub enum PathError {
    Traversal,
    Absolute,
    TooDeep,
    Empty,
}

impl std::fmt::Display for PathError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PathError::Traversal => "path contains '..'",
            PathError::Absolute => "absolute path",
            PathError::TooDeep => "too many path components",
            PathError::Empty => "empty path",
        })
    }
}

impl std::error::Error for PathError {}

const RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Splits a received relative name (`/` or `\` separated) into safe
/// components. Rejects traversal, absolute/drive/UNC paths and more than
/// [`MAX_COMPONENTS`] components; cleans each component for Windows.
pub fn sanitize_relative(name: &str) -> Result<Vec<String>, PathError> {
    let trimmed = name.trim();
    if trimmed.starts_with('/') || trimmed.starts_with('\\') {
        return Err(PathError::Absolute);
    }
    let bytes = trimmed.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic() {
        return Err(PathError::Absolute);
    }
    let mut out = Vec::new();
    for raw in trimmed.split(['/', '\\']) {
        if raw.is_empty() || raw == "." {
            continue;
        }
        if raw.trim() == ".." {
            return Err(PathError::Traversal);
        }
        out.push(clean_component(raw));
        if out.len() > MAX_COMPONENTS {
            return Err(PathError::TooDeep);
        }
    }
    if out.is_empty() {
        return Err(PathError::Empty);
    }
    Ok(out)
}

fn clean_component(raw: &str) -> String {
    let mut s: String = raw
        .nfc()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '|' | '?' | '*' => '_',
            c if (c as u32) < 0x20 => '_',
            c => c,
        })
        .collect();
    while s.ends_with('.') || s.ends_with(' ') {
        s.pop();
    }
    let stem = s.split('.').next().unwrap_or("");
    if RESERVED.iter().any(|r| r.eq_ignore_ascii_case(stem.trim_end())) {
        s.insert(0, '_');
    }
    if s.is_empty() { "_".into() } else { s }
}

/// Reserves final destinations so that concurrent receives never pick the
/// same name (the disk check alone races: files are created later).
/// Windows file names are case-insensitive, so are the reservations.
#[derive(Default)]
pub struct NameReservations {
    taken: Mutex<HashSet<String>>,
}

fn key(path: &Path) -> String {
    path.to_string_lossy().to_lowercase()
}

/// `<path>.fspart`: the partial file next to a final destination.
pub fn part_path(final_path: &Path) -> PathBuf {
    let mut s = final_path.as_os_str().to_os_string();
    s.push(crate::resume::PART_SUFFIX);
    PathBuf::from(s)
}

impl NameReservations {
    /// Reserves and creates a new top-level folder `dir/name`, or
    /// `dir/name (n)` when that exists or is reserved: a received folder never
    /// merges into an existing one. Returns the chosen folder name.
    pub fn reserve_folder(&self, dir: &Path, name: &str) -> io::Result<String> {
        let mut taken = self.taken.lock().unwrap();
        let chosen = (0u32..)
            .map(|n| if n == 0 { name.to_string() } else { format!("{name} ({n})") })
            .find(|candidate| {
                let path = dir.join(candidate);
                !taken.contains(&key(&path)) && !long_path(&path).exists()
            })
            .expect("an unused name exists");
        let path = dir.join(&chosen);
        taken.insert(key(&path));
        drop(taken);
        std::fs::create_dir_all(long_path(&path))?;
        Ok(chosen)
    }

    /// Reserves `dir/components…` for a file, renaming the file name on
    /// collision to `name (n).ext`. A name is taken when the file or its
    /// `.fspart` exists, so a partial never truncates an unrelated file.
    /// Creates the parent directories.
    pub fn reserve_file(&self, dir: &Path, components: &[String]) -> io::Result<PathBuf> {
        let (file, parents) = components.split_last().ok_or(io::ErrorKind::InvalidInput)?;
        let parent = parents.iter().fold(dir.to_path_buf(), |p, c| p.join(c));
        std::fs::create_dir_all(long_path(&parent))?;
        let mut taken = self.taken.lock().unwrap();
        let path = (0u32..)
            .map(|n| parent.join(if n == 0 { file.clone() } else { numbered(file, n) }))
            .find(|candidate| {
                !taken.contains(&key(candidate))
                    && !long_path(candidate).exists()
                    && !long_path(&part_path(candidate)).exists()
            })
            .expect("an unused name exists");
        taken.insert(key(&path));
        Ok(path)
    }

    pub fn release(&self, path: &Path) {
        self.taken.lock().unwrap().remove(&key(path));
    }
}

fn numbered(name: &str, n: u32) -> String {
    match name.rfind('.') {
        Some(dot) if dot > 0 => format!("{} ({n}){}", &name[..dot], &name[dot..]),
        _ => format!("{name} ({n})"),
    }
}

/// Maps the top-level folder of each received path to a freshly reserved
/// folder, once per transfer, so all files of one folder transfer land in the
/// same new folder.
#[derive(Default)]
pub struct TransferFolders {
    map: std::collections::HashMap<String, String>,
}

impl TransferFolders {
    /// Records that received top-level folder `original` lives in `actual`
    /// (from an earlier attempt of the same transfer), so a retry keeps
    /// filling the same folder.
    pub fn remember(&mut self, original: &str, actual: &str) {
        self.map.entry(original.to_string()).or_insert_with(|| actual.to_string());
    }

    /// Reserves the final path for one received file of this transfer.
    pub fn place(
        &mut self,
        names: &NameReservations,
        dir: &Path,
        mut components: Vec<String>,
    ) -> io::Result<PathBuf> {
        if components.len() > 1 {
            let top = match self.map.get(&components[0]) {
                Some(top) => top.clone(),
                None => {
                    let top = names.reserve_folder(dir, &components[0])?;
                    self.map.insert(components[0].clone(), top.clone());
                    top
                }
            };
            components[0] = top;
        }
        names.reserve_file(dir, &components)
    }
}

/// Adds the `\\?\` prefix to absolute Windows paths so paths over 260
/// characters work on Windows 7 (no long-path opt-in there).
pub fn long_path(p: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let s = p.as_os_str().to_string_lossy();
        if p.is_absolute() && !s.starts_with(r"\\") {
            return PathBuf::from(format!(r"\\?\{s}"));
        }
    }
    p.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(name: &str) -> Vec<String> {
        sanitize_relative(name).unwrap()
    }

    #[test]
    fn dotdot_rejected() {
        assert_eq!(sanitize_relative("../evil.exe"), Err(PathError::Traversal));
        assert_eq!(sanitize_relative("a/../../b"), Err(PathError::Traversal));
        assert_eq!(sanitize_relative("a\\..\\b"), Err(PathError::Traversal));
    }

    #[test]
    fn absolute_and_drive_rejected() {
        for name in ["/etc/passwd", r"C:\x", "c:x", r"\\server\s", r"\\?\C:\x"] {
            assert_eq!(sanitize_relative(name), Err(PathError::Absolute), "{name}");
        }
    }

    #[test]
    fn depth_over_32_rejected() {
        assert_eq!(sanitize_relative(&"a/".repeat(33)), Err(PathError::TooDeep));
        assert_eq!(ok(&"a/".repeat(32)).len(), 32);
    }

    #[test]
    fn empty_rejected() {
        assert_eq!(sanitize_relative("./"), Err(PathError::Empty));
        assert_eq!(sanitize_relative(""), Err(PathError::Empty));
    }

    #[test]
    fn reserved_names_prefixed() {
        assert_eq!(ok("CON.txt"), ["_CON.txt"]);
        assert_eq!(ok("lpt9"), ["_lpt9"]);
        assert_eq!(ok("console.txt"), ["console.txt"]);
    }

    #[test]
    fn illegal_chars_replaced() {
        assert_eq!(ok("ab:c*?.txt"), ["ab_c__.txt"]);
        // `a:b` is a drive-relative path on Windows, not a name with a colon.
        assert_eq!(sanitize_relative("a:b.txt"), Err(PathError::Absolute));
        assert_eq!(ok("x\u{1}y"), ["x_y"]);
    }

    #[test]
    fn trailing_dots_spaces_trimmed() {
        assert_eq!(ok("name. . "), ["name"]);
        assert_eq!(ok("x/..."), ["x", "_"]);
    }

    #[test]
    fn nfc_normalized() {
        assert_eq!(ok("cafe\u{301}.txt"), ["caf\u{e9}.txt"]);
    }

    #[test]
    fn folder_structure_kept() {
        assert_eq!(ok("Photos/2024/a.jpg"), ["Photos", "2024", "a.jpg"]);
        assert_eq!(ok(r"Photos\2024\.\a.jpg"), ["Photos", "2024", "a.jpg"]);
    }

    #[test]
    fn reserve_unique_is_race_free() {
        let dir = tempfile::tempdir().unwrap();
        let r = NameReservations::default();
        let a = r.reserve_file(dir.path(), &["a.txt".into()]).unwrap();
        let b = r.reserve_file(dir.path(), &["a.txt".into()]).unwrap();
        assert_eq!(a, dir.path().join("a.txt"));
        assert_eq!(b, dir.path().join("a (1).txt"));
        r.release(&a);
        std::fs::write(&a, "x").unwrap();
        let c = r.reserve_file(dir.path(), &["a.txt".into()]).unwrap();
        assert_eq!(c, dir.path().join("a (2).txt"));
    }

    #[test]
    fn reservations_ignore_case() {
        let dir = tempfile::tempdir().unwrap();
        let r = NameReservations::default();
        let a = r.reserve_file(dir.path(), &["Foo.txt".into()]).unwrap();
        let b = r.reserve_file(dir.path(), &["foo.TXT".into()]).unwrap();
        assert_eq!(a, dir.path().join("Foo.txt"));
        assert_eq!(b, dir.path().join("foo (1).TXT"));
    }

    #[test]
    fn existing_partial_name_is_not_reused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("report.fspart"), "someone's file").unwrap();
        let r = NameReservations::default();
        let p = r.reserve_file(dir.path(), &["report".into()]).unwrap();
        assert_eq!(p, dir.path().join("report (1)"));
    }

    #[test]
    fn top_level_folder_collision() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("Photos")).unwrap();
        let r = NameReservations::default();
        let mut transfer = TransferFolders::default();
        let a = transfer.place(&r, dir.path(), ok("Photos/a.jpg")).unwrap();
        let b = transfer.place(&r, dir.path(), ok("Photos/sub/b.jpg")).unwrap();
        assert_eq!(a, dir.path().join("Photos (1)").join("a.jpg"));
        assert_eq!(b, dir.path().join("Photos (1)").join("sub").join("b.jpg"));
        assert!(b.parent().unwrap().is_dir());
        let mut other = TransferFolders::default();
        let c = other.place(&r, dir.path(), ok("Photos/a.jpg")).unwrap();
        assert_eq!(c, dir.path().join("Photos (2)").join("a.jpg"));
    }
}
