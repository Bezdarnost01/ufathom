//! Shared helper: a child process spawned by a test, that exposes a known
//! buffer to be read. The child is this very test binary, re-run with an
//! environment variable so that a hidden test acts as the host.

#![allow(
    dead_code,
    reason = "each integration test binary uses a different part of this module"
)]

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};

/// Environment variable that turns [`host_entry`] into the host.
const HOST_ENV: &str = "UFATHOM_MEM_TEST_HOST";

/// Name of the hidden test function that every test binary must define,
/// calling [`host_entry`].
const HOST_TEST_NAME: &str = "hidden_child_host";

/// Prefix of the line the host prints.
const HOST_LINE: &str = "UFATHOM_HOST";

/// How long to wait for the host to report.
const HOST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Length of the buffer the host exposes.
pub const MARKER_LEN: usize = 8192;

/// The text the host stores at the start of its buffer.
pub const MARKER_TEXT: &[u8] = b"ufathom-mem host marker\0";

/// The exact contents of the host's buffer.
pub fn marker_bytes() -> Vec<u8> {
    let mut bytes: Vec<u8> = (0..MARKER_LEN).map(|i| (i * 7 % 251) as u8).collect();
    bytes[..MARKER_TEXT.len()].copy_from_slice(MARKER_TEXT);
    bytes
}

/// Body of the hidden test. In a normal run it does nothing. When the parent
/// started this process as a host, it prints the buffer address and then waits
/// until its stdin is closed.
pub fn host_entry() {
    if std::env::var_os(HOST_ENV).is_none() {
        return;
    }
    let buffer = std::hint::black_box(marker_bytes());
    let mut stdout = std::io::stdout();
    writeln!(
        stdout,
        "{HOST_LINE} {:x} {}",
        buffer.as_ptr() as usize,
        buffer.len()
    )
    .ok();
    // Flushing matters: stdout is a pipe and the parent is waiting for this line.
    stdout.flush().ok();
    let mut sink = Vec::new();
    std::io::stdin().read_to_end(&mut sink).ok();
    // Keep the buffer alive until the parent closes stdin.
    std::hint::black_box(&buffer);
}

/// A running host. The child is killed and reaped on drop, even if the test panics.
pub struct Host {
    child: Child,
    stdin: Option<ChildStdin>,
    /// Address of the marker buffer inside the child.
    pub addr: u64,
}

impl Host {
    /// Spawns the host and waits until it has published its buffer address.
    pub fn spawn() -> Self {
        let exe = std::env::current_exe().expect("test binary path");
        let mut child = Command::new(exe)
            .args([HOST_TEST_NAME, "--exact", "--nocapture", "--test-threads=1"])
            .env(HOST_ENV, "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn host child");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("child stdout");
        let mut host = Self {
            child,
            stdin,
            addr: 0,
        };
        // A reader thread keeps draining stdout for the child's whole life, so the
        // child can never block on a full pipe, and lets the wait below time out
        // instead of hanging the test if the child never reports.
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut lines = BufReader::new(stdout);
            let mut line = String::new();
            let mut reported = false;
            loop {
                line.clear();
                match lines.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
                // The harness prints "test name ... " on the same line before our output.
                if !reported && let Some((_, rest)) = line.split_once(HOST_LINE) {
                    reported = true;
                    sender.send(rest.to_owned()).ok();
                }
            }
        });
        let rest = receiver
            .recv_timeout(HOST_TIMEOUT)
            .expect("host did not print its buffer address in time");
        let mut parts = rest.split_whitespace();
        host.addr = u64::from_str_radix(parts.next().expect("address"), 16).expect("hex address");
        let len: usize = parts.next().expect("length").parse().expect("length");
        assert_eq!(len, MARKER_LEN);
        host
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn child(&self) -> &Child {
        &self.child
    }

    /// Terminates the child and waits for it, so its pid is truly gone.
    pub fn kill(&mut self) {
        drop(self.stdin.take());
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        self.kill();
    }
}
