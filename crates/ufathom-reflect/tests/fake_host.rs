//! `discover` and `build_dump` end to end through real operating system
//! reads: a child process (`fake-host`) hosts a synthetic image, and the dump
//! read back through `ProcessMemory` must equal the truth of an identical
//! local build.

#![cfg(windows)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code may panic on failure"
)]

mod common;

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{OnceLock, mpsc};
use std::time::{Duration, Instant};

use common::{assert_matches_truth, watchdog};
use rayon::ThreadPoolBuilder;
use ufathom_mem::ProcessMemory;
use ufathom_reflect::{ReflectOptions, ReflectOutput, build_dump};
use ufathom_scan::{DiscoverOptions, discover};
use ufathom_testkit::{Corruption, Fixture, FixtureBuilder, HostedPlacement, Placement, Profile};

const LINE_TIMEOUT: Duration = Duration::from_secs(60);
const DUMP_TIMEOUT: Duration = Duration::from_secs(180);
const EXIT_TIMEOUT: Duration = Duration::from_secs(30);

/// The built `fake-host` executable, building it when only the library of the
/// testkit has been built.
fn fake_host_path() -> PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(locate_fake_host).clone()
}

fn locate_fake_host() -> PathBuf {
    let path = ufathom_testkit::artifact_dir().join("fake-host.exe");
    if !path.exists() {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
        let status = Command::new(cargo)
            .args(["build", "-p", "ufathom-testkit", "--bin", "fake-host"])
            .status()
            .expect("run cargo build");
        assert!(status.success(), "cargo build of fake-host failed");
    }
    assert!(path.exists(), "fake-host not found at {}", path.display());
    path
}

/// A running host. The child is killed and reaped on drop, even if the test
/// panics.
struct Host {
    child: Child,
    stdin: Option<ChildStdin>,
    json: serde_json::Value,
}

impl Host {
    fn spawn(profile: &str, seed: u64, filler: usize, extra: &[&str]) -> Self {
        let mut child = Command::new(fake_host_path())
            .args([
                "--profile",
                profile,
                "--seed",
                &seed.to_string(),
                "--filler",
                &filler.to_string(),
            ])
            .args(extra)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn fake-host");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("stdout is piped");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            let _ = BufReader::new(stdout).read_line(&mut line);
            let _ = tx.send(line);
        });
        let mut host = Self {
            child,
            stdin,
            json: serde_json::Value::Null,
        };
        let line = rx
            .recv_timeout(LINE_TIMEOUT)
            .expect("fake-host printed its line in time");
        host.json = serde_json::from_str(&line).expect("one JSON line");
        host
    }

    fn u64(&self, path: &[&str]) -> u64 {
        let mut value = &self.json;
        for key in path {
            value = &value[*key];
        }
        value
            .as_u64()
            .unwrap_or_else(|| panic!("{path:?} is a number"))
    }

    /// Closes stdin and waits for the child to exit by itself.
    fn close_and_wait(&mut self) -> bool {
        drop(self.stdin.take());
        let start = Instant::now();
        while start.elapsed() < EXIT_TIMEOUT {
            if let Ok(Some(status)) = self.child.try_wait() {
                return status.success();
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    /// The same image built locally, from the addresses the host reported.
    fn local(
        &self,
        profile: &str,
        seed: u64,
        filler: usize,
        corruptions: &[Corruption],
    ) -> Fixture {
        let placement = HostedPlacement {
            module_name: self.json["module"].as_str().unwrap().to_owned(),
            module_base: self.u64(&["module_base"]),
            text: self.u64(&["addresses", "text"]),
            rdata: self.u64(&["addresses", "rdata"]),
            data: self.u64(&["addresses", "data"]),
            heap: self.u64(&["addresses", "heap"]),
        };
        let mut builder = FixtureBuilder::new(Profile::by_name(profile).unwrap())
            .seed(seed)
            .filler_objects(filler)
            .placement(Placement::Hosted(placement));
        for &corruption in corruptions {
            builder = builder.corrupt(corruption);
        }
        builder.build()
    }

    /// Solves and dumps the host's memory on a watched thread, on a pool.
    fn dump(&self) -> ReflectOutput {
        let pid = u32::try_from(self.u64(&["pid"])).unwrap();
        let module = self.json["module"].as_str().unwrap().to_owned();
        watchdog("dump of the host", DUMP_TIMEOUT, move || {
            let memory = ProcessMemory::open(pid).expect("open the child");
            let discovery = discover(
                &memory,
                &DiscoverOptions {
                    module: Some(module),
                    ..DiscoverOptions::default()
                },
            )
            .expect("discovery succeeded");
            let pool = ThreadPoolBuilder::new().num_threads(2).build().unwrap();
            let options = ReflectOptions {
                pool: Some(&pool),
                ..ReflectOptions::default()
            };
            build_dump(&memory, &discovery, &options).expect("the dump was built")
        })
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn a_hosted_name_pool_image_dumps_to_its_truth() {
    let mut host = Host::spawn("ue4.25-cp", 5, 800, &[]);
    let local = host.local("ue4.25-cp", 5, 800, &[]);
    let out = host.dump();
    assert!(!out.degraded);
    assert_eq!(out.stats.failures, 0);
    assert!(
        out.dump.diagnostics.is_empty(),
        "{:?}",
        out.dump.diagnostics
    );
    assert_matches_truth(&local, &out);
    assert!(
        host.close_and_wait(),
        "the host did not exit cleanly after the dump"
    );
}

#[test]
fn a_hosted_name_array_image_dumps_to_its_truth() {
    let mut host = Host::spawn("ue4.21", 6, 800, &[]);
    let local = host.local("ue4.21", 6, 800, &[]);
    let out = host.dump();
    assert!(!out.degraded);
    assert_eq!(out.stats.failures, 0);
    assert_matches_truth(&local, &out);
    assert!(host.close_and_wait());
}

#[test]
fn a_damaged_hosted_image_still_dumps_what_it_can() {
    let mut host = Host::spawn("ue4.25-cp", 7, 800, &["--corrupt", "ZeroedObjectHeaders"]);
    let out = host.dump();
    assert!(out.stats.failures >= 26, "{:?}", out.stats);
    assert!(!out.degraded);
    assert!(!out.dump.structs.is_empty());
    assert!(host.close_and_wait());
}
