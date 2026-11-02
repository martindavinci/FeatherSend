//! Log file `logs\feathersend.log`, rotated at 1 MiB (one previous file kept
//! as `feathersend.log.1`). A warning or error repeated within 10 minutes is
//! written once, then once more with a count: the LocalSend core logs every
//! failed multicast send, which filled 200 KB overnight on a sleeping PC.
//! Panics are written to the same file before the process aborts.

use crate::win::wide;
use feathersend_core::i18n::fill;
use feathersend_core::util::{rfc3339, unix_now};
use std::borrow::Cow;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

static LOG_FILE: OnceLock<PathBuf> = OnceLock::new();
static CRASH_TEXT: OnceLock<&'static str> = OnceLock::new();

const MAX_BYTES: u64 = 1024 * 1024;
const THROTTLE_WINDOW: Duration = Duration::from_secs(10 * 60);
/// Distinct repeated messages remembered at once (oldest forgotten first).
const THROTTLE_KEYS: usize = 32;

struct Seen {
    key: String,
    since: Instant,
    dropped: u32,
}

/// Drops repeats of a warning or error line (compared without the timestamp)
/// within a window; the first repeat after the window carries the count.
pub struct Throttle {
    window: Duration,
    seen: Vec<Seen>,
}

/// Lines look like `2026-11-02T03:14:15.123456Z  WARN target: message`; only
/// warnings and errors are throttled, keyed by everything after the time.
fn throttle_key(line: &str) -> Option<&str> {
    let (_, rest) = line.split_once(' ')?;
    let rest = rest.trim_start();
    (rest.starts_with("WARN") || rest.starts_with("ERROR")).then(|| rest.trim_end())
}

impl Throttle {
    pub fn new(window: Duration) -> Throttle {
        Throttle { window, seen: Vec::new() }
    }

    /// The text to write for `line`, or `None` to drop it.
    pub fn filter<'a>(&mut self, line: &'a str, now: Instant) -> Option<Cow<'a, str>> {
        let Some(key) = throttle_key(line) else { return Some(Cow::Borrowed(line)) };
        if let Some(seen) = self.seen.iter_mut().find(|s| s.key == key) {
            if now.saturating_duration_since(seen.since) < self.window {
                seen.dropped += 1;
                return None;
            }
            seen.since = now;
            return match std::mem::take(&mut seen.dropped) {
                0 => Some(Cow::Borrowed(line)),
                n => {
                    let body = line.trim_end_matches(['\r', '\n']);
                    Some(Cow::Owned(format!("{body} [{n} similar lines suppressed]\n")))
                }
            };
        }
        if self.seen.len() >= THROTTLE_KEYS
            && let Some(oldest) = (0..self.seen.len()).min_by_key(|&i| self.seen[i].since)
        {
            self.seen.swap_remove(oldest);
        }
        self.seen.push(Seen { key: key.to_string(), since: now, dropped: 0 });
        Some(Cow::Borrowed(line))
    }
}

fn open(path: &Path) -> (Option<File>, u64) {
    let file = OpenOptions::new().create(true).append(true).open(path).ok();
    let written = file.as_ref().and_then(|f| f.metadata().ok()).map(|m| m.len()).unwrap_or(0);
    (file, written)
}

struct RotatingFile {
    path: PathBuf,
    file: Option<File>,
    written: u64,
    throttle: Throttle,
}

impl RotatingFile {
    fn open(path: &Path) -> RotatingFile {
        let (file, written) = open(path);
        RotatingFile {
            path: path.to_path_buf(),
            file,
            written,
            throttle: Throttle::new(THROTTLE_WINDOW),
        }
    }

    fn rotate(&mut self) {
        self.file = None;
        let _ = std::fs::rename(&self.path, self.path.with_extension("log.1"));
        (self.file, self.written) = open(&self.path);
    }
}

/// tracing's fmt layer hands over one whole line per event.
impl Write for RotatingFile {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let text = String::from_utf8_lossy(buf);
        let Some(line) = self.throttle.filter(&text, Instant::now()) else { return Ok(buf.len()) };
        if self.written + line.len() as u64 > MAX_BYTES {
            self.rotate();
        }
        self.written += line.len() as u64;
        if let Some(file) = &mut self.file {
            file.write_all(line.as_bytes())?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.as_mut().map_or(Ok(()), |f| f.flush())
    }
}

pub fn init(dir: &Path) {
    let path = dir.join("feathersend.log");
    let writer = Mutex::new(RotatingFile::open(&path));
    let _ = tracing_subscriber::fmt()
        .with_writer(writer)
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .try_init();
    let _ = LOG_FILE.set(path);
    install_panic_hook();
}

/// The localized message shown after a panic (known once settings are read).
pub fn set_crash_text(text: &'static str) {
    let _ = CRASH_TEXT.set(text);
}

/// One log line for a panic, laid out like tracing's lines.
pub fn panic_line(time: &str, thread: &str, location: Option<&str>, message: &str) -> String {
    let at = location.map(|l| format!(" at {l}")).unwrap_or_default();
    format!("{time} ERROR feathersend::panic: thread '{thread}' panicked{at}: {message}\n")
}

/// Release builds abort on panic and a GUI exe has no console, so a panic
/// would vanish without a trace. The line goes straight to the file, not
/// through tracing: the panicking thread may hold the subscriber's lock.
fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let thread = std::thread::current();
        let location = info.location().map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()));
        let line = panic_line(
            &rfc3339(unix_now()),
            thread.name().unwrap_or("unnamed"),
            location.as_deref(),
            info.payload_as_str().unwrap_or("(no message)"),
        );
        let Some(path) = LOG_FILE.get() else { return };
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = file.write_all(line.as_bytes());
        }
        // Debug builds unwind (tokio survives a panicking task): no box there.
        if cfg!(panic = "abort")
            && let Some(text) = CRASH_TEXT.get()
        {
            show_crash_box(&fill(text, &[("path", &path.display().to_string())]));
        }
    }));
}

fn show_crash_box(text: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MB_ICONERROR, MB_SETFOREGROUND, MB_TOPMOST, MessageBoxW,
    };
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            wide(text).as_ptr(),
            wide("FeatherSend").as_ptr(),
            MB_ICONERROR | MB_SETFOREGROUND | MB_TOPMOST,
        )
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    const WARN: &str = "2026-11-02T03:14:15.000001Z  WARN localsend::multicast: send failed\n";
    const WARN_LATER: &str =
        "2026-11-02T03:14:45.000001Z  WARN localsend::multicast: send failed\n";

    #[test]
    fn info_lines_pass_through() {
        let mut throttle = Throttle::new(THROTTLE_WINDOW);
        let now = Instant::now();
        let info = "2026-11-02T03:14:15.000001Z  INFO feathersend: started\n";
        assert_eq!(throttle.filter(info, now).as_deref(), Some(info));
        assert_eq!(throttle.filter(info, now).as_deref(), Some(info));
    }

    #[test]
    fn repeated_warning_dropped_within_window() {
        let mut throttle = Throttle::new(THROTTLE_WINDOW);
        let now = Instant::now();
        assert_eq!(throttle.filter(WARN, now).as_deref(), Some(WARN));
        assert_eq!(throttle.filter(WARN_LATER, now + Duration::from_secs(30)), None);
        assert_eq!(throttle.filter(WARN, now + Duration::from_secs(60)), None);
    }

    #[test]
    fn warning_after_window_carries_count() {
        let mut throttle = Throttle::new(THROTTLE_WINDOW);
        let now = Instant::now();
        throttle.filter(WARN, now);
        throttle.filter(WARN, now + Duration::from_secs(1));
        throttle.filter(WARN, now + Duration::from_secs(2));
        let later = throttle.filter(WARN_LATER, now + THROTTLE_WINDOW).unwrap();
        assert_eq!(
            later,
            "2026-11-02T03:14:45.000001Z  WARN localsend::multicast: send failed \
             [2 similar lines suppressed]\n"
        );
        // The count restarts with the new window.
        assert_eq!(throttle.filter(WARN, now + THROTTLE_WINDOW + Duration::from_secs(1)), None);
    }

    #[test]
    fn different_messages_are_independent() {
        let mut throttle = Throttle::new(THROTTLE_WINDOW);
        let now = Instant::now();
        let other = "2026-11-02T03:14:15.000001Z ERROR feathersend: other\n";
        assert!(throttle.filter(WARN, now).is_some());
        assert!(throttle.filter(other, now).is_some());
        assert!(throttle.filter(other, now).is_none());
    }

    #[test]
    fn panic_line_has_thread_location_and_message() {
        assert_eq!(
            panic_line("2026-11-02T03:14:15Z", "feathersend-net", Some("src\\a.rs:10:5"), "boom"),
            "2026-11-02T03:14:15Z ERROR feathersend::panic: thread 'feathersend-net' panicked \
             at src\\a.rs:10:5: boom\n"
        );
        assert_eq!(
            panic_line("t", "main", None, "boom"),
            "t ERROR feathersend::panic: thread 'main' panicked: boom\n"
        );
    }

    #[test]
    fn oldest_message_forgotten_when_full() {
        let mut throttle = Throttle::new(THROTTLE_WINDOW);
        let now = Instant::now();
        throttle.filter(WARN, now);
        for i in 0..THROTTLE_KEYS {
            let line = format!("2026-11-02T03:14:15Z  WARN x: message {i}\n");
            throttle.filter(&line, now + Duration::from_secs(1));
        }
        assert!(throttle.filter(WARN, now + Duration::from_secs(2)).is_some());
    }
}
