//! Logging to stderr, a log file and an in-memory ring buffer shown in the UI.

use parking_lot::Mutex;
use std::collections::VecDeque;
use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::EnvFilter;

const RING_LINES: usize = 600;

#[derive(Clone, Default)]
pub struct LogRing(Arc<Mutex<VecDeque<String>>>);

impl LogRing {
    pub fn lines(&self) -> Vec<String> {
        self.0.lock().iter().cloned().collect()
    }
}

#[derive(Clone)]
struct Tee {
    ring: LogRing,
    file: Option<Arc<Mutex<std::fs::File>>>,
}

struct TeeWriter {
    tee: Tee,
    buf: Vec<u8>,
}

impl Write for TeeWriter {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Drop for TeeWriter {
    fn drop(&mut self) {
        if self.buf.is_empty() {
            return;
        }
        let _ = std::io::stderr().write_all(&self.buf);
        if let Some(f) = &self.tee.file {
            let _ = f.lock().write_all(&self.buf);
        }
        let text = String::from_utf8_lossy(&self.buf);
        let mut ring = self.tee.ring.0.lock();
        for line in text.lines() {
            if ring.len() >= RING_LINES {
                ring.pop_front();
            }
            ring.push_back(line.to_string());
        }
    }
}

impl<'a> MakeWriter<'a> for Tee {
    type Writer = TeeWriter;
    fn make_writer(&'a self) -> Self::Writer {
        TeeWriter { tee: self.clone(), buf: Vec::with_capacity(256) }
    }
}

/// Install the global subscriber. `RUST_LOG` overrides the default filter.
pub fn init(log_dir: &Path) -> LogRing {
    let ring = LogRing::default();
    let file = std::fs::create_dir_all(log_dir).ok().and_then(|_| {
        let path = log_dir.join("kvmbridge.log");
        // Keep the previous run's log around for bug reports.
        if path.exists() {
            let _ = std::fs::rename(&path, log_dir.join("kvmbridge.previous.log"));
        }
        std::fs::OpenOptions::new().create(true).append(true).open(path).ok()
    });
    let tee = Tee { ring: ring.clone(), file: file.map(|f| Arc::new(Mutex::new(f))) };
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,kvmbridge_core=info,kvmbridge_platform=info,zbus=error,tao=warn,wry=warn"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).with_ansi(false).with_writer(tee).try_init();
    ring
}
