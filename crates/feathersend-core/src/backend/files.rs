//! Local files picked for sending: folders are expanded with their root
//! folder name in the relative path (`Photos/2024/a.jpg`).

use crate::paths::long_path;
use crate::util::hex;
use sha2::{Digest, Sha256};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct LocalFile {
    pub path: PathBuf,
    /// Relative name with `/` separators.
    pub rel: String,
    pub size: u64,
}

/// Expands files and folders. Symbolic links and junctions are skipped (no
/// loops, nothing outside the picked folder).
pub(crate) fn collect(paths: &[PathBuf]) -> io::Result<Vec<LocalFile>> {
    let mut out = Vec::new();
    for path in paths {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "path without a name"))?;
        let meta = std::fs::symlink_metadata(long_path(path))?;
        if meta.is_dir() {
            walk(path, name, &mut out)?;
        } else if meta.is_file() {
            out.push(LocalFile { path: path.clone(), rel: name.to_string(), size: meta.len() });
        }
    }
    make_unique(&mut out);
    Ok(out)
}

/// Two picked items with the same name (from different folders) get
/// `name (1).ext`: the receiver identifies files by relative name.
fn make_unique(files: &mut [LocalFile]) {
    let mut seen = std::collections::HashSet::new();
    for f in files.iter_mut() {
        let mut rel = f.rel.clone();
        let mut n = 0;
        while !seen.insert(rel.to_lowercase()) {
            n += 1;
            rel = match f.rel.rfind('.') {
                Some(dot) if dot > f.rel.rfind('/').map_or(0, |s| s + 1) => {
                    format!("{} ({n}){}", &f.rel[..dot], &f.rel[dot..])
                }
                _ => format!("{} ({n})", f.rel),
            };
        }
        f.rel = rel;
    }
}

fn walk(dir: &Path, rel: &str, out: &mut Vec<LocalFile>) -> io::Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(long_path(dir))?.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let Some(name) = entry.file_name().to_str().map(String::from) else { continue };
        let meta = entry.file_type()?;
        let path = dir.join(&name);
        let child = format!("{rel}/{name}");
        if meta.is_symlink() {
            continue;
        } else if meta.is_dir() {
            walk(&path, &child, out)?;
        } else if meta.is_file() {
            let size = entry.metadata()?.len();
            out.push(LocalFile { path, rel: child, size });
        }
    }
    Ok(())
}

/// Streaming SHA-256 (blocking: run on `spawn_blocking`).
pub(crate) fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = std::fs::File::open(long_path(path))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

/// A short, language-neutral label: the single name, or the first name and
/// how many more.
pub(crate) fn label<'a>(mut names: impl Iterator<Item = &'a str>, count: usize) -> String {
    let first = names.next().unwrap_or_default();
    let top = first.split('/').next().unwrap_or(first);
    match count {
        0 | 1 => first.to_string(),
        n => format!("{top} (+{})", n - 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_expand_with_root_name() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Photos");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("a.txt"), "aa").unwrap();
        std::fs::write(root.join("sub").join("b.txt"), "b").unwrap();
        let single = dir.path().join("c.txt");
        std::fs::write(&single, "ccc").unwrap();
        let files = collect(&[root, single]).unwrap();
        let rels: Vec<_> = files.iter().map(|f| (f.rel.as_str(), f.size)).collect();
        assert_eq!(rels, [("Photos/a.txt", 2), ("Photos/sub/b.txt", 1), ("c.txt", 3)]);
    }

    #[test]
    fn same_names_from_different_folders_become_unique() {
        let dir = tempfile::tempdir().unwrap();
        for sub in ["x", "y"] {
            std::fs::create_dir(dir.path().join(sub)).unwrap();
            std::fs::write(dir.path().join(sub).join("a.txt"), sub).unwrap();
        }
        let files =
            collect(&[dir.path().join("x").join("a.txt"), dir.path().join("y").join("a.txt")])
                .unwrap();
        let rels: Vec<_> = files.iter().map(|f| f.rel.as_str()).collect();
        assert_eq!(rels, ["a.txt", "a (1).txt"]);
    }

    #[test]
    fn hashes_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x");
        std::fs::write(&path, "abc").unwrap();
        assert_eq!(
            sha256_file(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn labels() {
        assert_eq!(label(["a.txt"].into_iter(), 1), "a.txt");
        assert_eq!(label(["Photos/a.jpg", "Photos/b.jpg"].into_iter(), 2), "Photos (+1)");
    }
}
