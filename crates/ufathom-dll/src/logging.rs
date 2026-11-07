//! The DLL's own log file and the panic hook that writes to it.
//!
//! Nothing is ever written to stderr or stdout: the host process owns them.
//! The subscriber is not installed globally. It is set as the default of each
//! thread the DLL starts, so nothing of it outlives the DLL and unloading does
//! not leave a subscriber behind.

use std::fs::File;
use std::io::{self, Write};
use std::panic::PanicHookInfo;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tracing::Dispatch;
use tracing::level_filters::LevelFilter;
use tracing_subscriber::fmt::MakeWriter;
use ufathom_agent::fsio::open_append;

/// A log file that can be closed while writers still hold a handle to it.
#[derive(Clone)]
pub(crate) struct LogSink {
    file: Arc<Mutex<Option<File>>>,
}

impl LogSink {
    /// Opens the log for appending, creating its folder. `None` if it cannot
    /// be opened: the DLL then runs without a log.
    pub(crate) fn open(path: &Path) -> Option<Self> {
        let file = open_append(path).ok()?;
        Some(Self {
            file: Arc::new(Mutex::new(Some(file))),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Option<File>> {
        self.file.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Flushes and closes the file. Later writes are dropped silently.
    pub(crate) fn close(&self) {
        if let Some(mut file) = self.lock().take() {
            let _ = file.flush();
            let _ = file.sync_all();
        }
    }

    /// Writes a line without waiting: used by the panic hook, which may run
    /// while the thread that panicked holds the lock.
    fn write_line_without_waiting(&self, line: &str) {
        let Ok(mut guard) = self.file.try_lock() else {
            return;
        };
        if let Some(file) = guard.as_mut() {
            let _ = writeln!(file, "{line}");
            let _ = file.flush();
        }
    }

    /// A subscriber that writes events here, as a [`Dispatch`] to set as the
    /// default of the DLL's threads.
    pub(crate) fn dispatch(&self) -> Dispatch {
        Dispatch::new(
            tracing_subscriber::fmt()
                .with_writer(self.clone())
                .with_ansi(false)
                .with_target(false)
                .with_max_level(LevelFilter::INFO)
                .finish(),
        )
    }
}

/// Writes one event's formatted text to the file.
pub(crate) struct SinkWriter {
    sink: LogSink,
}

impl Write for SinkWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        match self.sink.lock().as_mut() {
            Some(file) => file.write(buffer),
            // Closed: pretend the bytes were written, so nothing reports an error.
            None => Ok(buffer.len()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.sink.lock().as_mut() {
            Some(file) => file.flush(),
            None => Ok(()),
        }
    }
}

impl<'a> MakeWriter<'a> for LogSink {
    type Writer = SinkWriter;

    fn make_writer(&'a self) -> Self::Writer {
        SinkWriter { sink: self.clone() }
    }
}

/// Replaces the panic hook with one that writes to the log, and puts the
/// default hook back when dropped.
///
/// The hook is installed first thing, silent: until [`set_sink`](Self::set_sink)
/// gives it a log there is nowhere to write, and it stays silent rather than
/// falling back to stderr, which belongs to the host.
///
/// The hook's code lives in this DLL, so it must not outlive it: the worker
/// drops this guard before it unloads.
pub(crate) struct PanicHook {
    sink: Arc<Mutex<Option<LogSink>>>,
}

impl PanicHook {
    /// Installs the hook, with no log yet.
    pub(crate) fn install() -> Self {
        let sink: Arc<Mutex<Option<LogSink>>> = Arc::new(Mutex::new(None));
        let hook_sink = Arc::clone(&sink);
        std::panic::set_hook(Box::new(move |info| {
            // Without waiting: the panic may have happened while this lock was held.
            let Ok(guard) = hook_sink.try_lock() else {
                return;
            };
            if let Some(sink) = guard.as_ref() {
                sink.write_line_without_waiting(&describe(info));
            }
        }));
        Self { sink }
    }

    /// Sets the log the hook writes to, or `None` to stop writing.
    pub(crate) fn set_sink(&self, sink: Option<LogSink>) {
        *self.sink.lock().unwrap_or_else(PoisonError::into_inner) = sink;
    }
}

impl Drop for PanicHook {
    fn drop(&mut self) {
        drop(std::panic::take_hook());
    }
}

fn describe(info: &PanicHookInfo<'_>) -> String {
    let message = info
        .payload()
        .downcast_ref::<&str>()
        .map(|text| (*text).to_owned())
        .or_else(|| info.payload().downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "no message".to_owned());
    let place = info
        .location()
        .map_or_else(String::new, |at| format!(" at {}:{}", at.file(), at.line()));
    let thread = std::thread::current();
    format!(
        "PANIC in thread `{}`: {message}{place}",
        thread.name().unwrap_or("unnamed")
    )
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code may panic on failure"
)]
mod tests {
    use super::*;

    #[test]
    fn writes_reach_the_file_until_it_is_closed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logs").join("a.log");
        let sink = LogSink::open(&path).unwrap();
        let mut writer = sink.make_writer();
        writer.write_all(b"first\n").unwrap();
        sink.close();
        writer.write_all(b"after close\n").unwrap();
        sink.close();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first\n");
    }

    #[test]
    fn a_panic_reaches_the_log_and_the_default_hook_is_restored() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("panic.log");
        let sink = LogSink::open(&path).unwrap();
        let hook = PanicHook::install();
        hook.set_sink(Some(sink.clone()));
        let caught = std::panic::catch_unwind(|| panic!("simulated failure"));
        assert!(caught.is_err());
        hook.set_sink(None);
        drop(hook);
        sink.close();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("PANIC"), "{text}");
        assert!(text.contains("simulated failure"), "{text}");
        assert!(text.contains("logging.rs"), "the place is named: {text}");
    }

    #[test]
    fn a_log_that_cannot_be_opened_is_none() {
        let dir = tempfile::tempdir().unwrap();
        // A folder where the file should be.
        assert!(LogSink::open(dir.path()).is_none());
    }

    #[test]
    fn a_held_lock_does_not_block_a_panic_line_and_later_lines_are_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.log");
        let sink = LogSink::open(&path).unwrap();
        // A held lock must not block the hook.
        let held = sink.lock();
        sink.write_line_without_waiting("dropped");
        drop(held);
        sink.write_line_without_waiting("kept");
        sink.close();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "kept\n");
    }
}
