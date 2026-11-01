//! Log file `logs\feathersend.log`, rotated at 1 MiB (one previous file kept
//! as `feathersend.log.1`).

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const MAX_BYTES: u64 = 1024 * 1024;

struct RotatingFile {
    path: PathBuf,
    file: Option<File>,
    written: u64,
}

impl RotatingFile {
    fn open(path: &Path) -> RotatingFile {
        let file = OpenOptions::new().create(true).append(true).open(path).ok();
        let written = file.as_ref().and_then(|f| f.metadata().ok()).map(|m| m.len()).unwrap_or(0);
        RotatingFile { path: path.to_path_buf(), file, written }
    }

    fn rotate(&mut self) {
        self.file = None;
        let _ = std::fs::rename(&self.path, self.path.with_extension("log.1"));
        *self = RotatingFile::open(&self.path);
    }
}

impl Write for RotatingFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.written + buf.len() as u64 > MAX_BYTES {
            self.rotate();
        }
        self.written += buf.len() as u64;
        match &mut self.file {
            Some(file) => file.write(buf),
            None => Ok(buf.len()),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.as_mut().map_or(Ok(()), |f| f.flush())
    }
}

pub fn init(dir: &Path) {
    let writer = Mutex::new(RotatingFile::open(&dir.join("feathersend.log")));
    let _ = tracing_subscriber::fmt()
        .with_writer(writer)
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .try_init();
}
