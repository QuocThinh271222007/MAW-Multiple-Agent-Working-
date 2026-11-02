//! File logging with size-based rotation and secret redaction. The TUI owns the terminal, so
//! diagnostics never go to stdout/stderr while it runs.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const MAX_BYTES: u64 = 5 * 1024 * 1024;
const KEEP: usize = 3;

struct Inner {
    path: PathBuf,
    file: File,
    written: u64,
}

#[derive(Clone)]
pub struct RotatingLog(Arc<Mutex<Inner>>);

impl RotatingLog {
    pub fn open(path: &Path) -> io::Result<RotatingLog> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let written = file.metadata().map(|m| m.len()).unwrap_or(0);
        Ok(RotatingLog(Arc::new(Mutex::new(Inner {
            path: path.to_path_buf(),
            file,
            written,
        }))))
    }
}

fn rotate(inner: &mut Inner) -> io::Result<()> {
    for i in (1..KEEP).rev() {
        let from = inner.path.with_extension(format!("log.{i}"));
        let to = inner.path.with_extension(format!("log.{}", i + 1));
        if from.exists() {
            let _ = std::fs::rename(from, to);
        }
    }
    let _ = std::fs::rename(&inner.path, inner.path.with_extension("log.1"));
    inner.file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&inner.path)?;
    inner.written = 0;
    Ok(())
}

pub struct LogWriter(RotatingLog);

impl Write for LogWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let text = String::from_utf8_lossy(buf);
        let clean = maw_core::redact::redact(&text);
        let mut g = (self.0)
            .0
            .lock()
            .map_err(|_| io::Error::other("log mutex poisoned"))?;
        if g.written + clean.len() as u64 > MAX_BYTES {
            rotate(&mut g)?;
        }
        g.file.write_all(clean.as_bytes())?;
        g.written += clean.len() as u64;
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        (self.0)
            .0
            .lock()
            .map_err(|_| io::Error::other("log mutex poisoned"))?
            .file
            .flush()
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for RotatingLog {
    type Writer = LogWriter;
    fn make_writer(&'a self) -> Self::Writer {
        LogWriter(self.clone())
    }
}

/// Install the global subscriber. `RUST_LOG` overrides the default `info` level.
pub fn init(log_file: &Path, to_stderr: bool) {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,rmcp=warn,hyper=warn"));
    if to_stderr {
        let _ = tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(io::stderr)
            .try_init();
        return;
    }
    match RotatingLog::open(log_file) {
        Ok(w) => {
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_ansi(false)
                .with_writer(w)
                .try_init();
        }
        Err(_) => {
            // logging must never prevent MAW from starting
            let _ = tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_writer(io::sink)
                .try_init();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_and_rotates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("maw.log");
        let log = RotatingLog::open(&path).unwrap();
        let mut w = LogWriter(log.clone());
        w.write_all(b"auth Bearer supersecrettoken123 ok\n")
            .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("supersecrettoken123") && text.contains("[REDACTED]"));
        // force a rotation
        {
            let mut g = log.0.lock().unwrap();
            g.written = MAX_BYTES;
        }
        w.write_all(b"after rotation\n").unwrap();
        assert!(path.with_extension("log.1").exists());
        assert!(std::fs::read_to_string(&path)
            .unwrap()
            .contains("after rotation"));
    }
}
