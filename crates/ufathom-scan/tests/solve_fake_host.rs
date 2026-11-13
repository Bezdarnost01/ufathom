//! `discover` end to end through real operating system reads: a child process
//! (`fake-host`) hosts a synthetic image, and the layout read back through
//! `ProcessMemory` must be the one the host reports.

#![cfg(windows)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::print_stderr,
    reason = "test code may panic on failure"
)]

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use ufathom_mem::ProcessMemory;
use ufathom_model::EngineLayout;
use ufathom_model::layout::{Location, NameTableLayout};
use ufathom_scan::{DiscoverOptions, Discovery, discover};

const LINE_TIMEOUT: Duration = Duration::from_secs(60);
const DISCOVER_TIMEOUT: Duration = Duration::from_secs(120);
const EXIT_TIMEOUT: Duration = Duration::from_secs(30);

/// The built `fake-host` executable, building it when only the library of the
/// testkit has been built.
fn fake_host_path() -> PathBuf {
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
    fn spawn(profile: &str, seed: u64, filler: usize) -> Self {
        let mut child = Command::new(fake_host_path())
            .args([
                "--profile",
                profile,
                "--seed",
                &seed.to_string(),
                "--filler",
                &filler.to_string(),
            ])
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
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn absolute(host: &Host, location: &Location) -> u64 {
    match location {
        Location::Absolute(addr) => *addr,
        Location::Rva { rva, .. } => host.u64(&["module_base"]) + rva,
    }
}

fn discover_guarded(pid: u32, module: String) -> Discovery {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let memory = ProcessMemory::open(pid).expect("open the child");
        let options = DiscoverOptions {
            module: Some(module),
            ..DiscoverOptions::default()
        };
        let _ = tx.send(discover(&memory, &options));
    });
    rx.recv_timeout(DISCOVER_TIMEOUT)
        .expect("discovery finished in time")
        .expect("discovery succeeded")
}

fn check(profile: &str, seed: u64) {
    let mut host = Host::spawn(profile, seed, 400);
    let want: EngineLayout = serde_json::from_value(host.json["layout"].clone()).unwrap();
    let module = host.json["module"].as_str().unwrap().to_owned();
    let pid = u32::try_from(host.u64(&["pid"])).unwrap();
    let d = discover_guarded(pid, module);
    let got = &d.layout;
    let name = profile;

    let (g, w) = (&got.objects, &want.objects);
    let (gb, wb) = (absolute(&host, &g.location), absolute(&host, &w.location));
    assert_eq!(
        gb + u64::from(g.objects),
        wb + u64::from(w.objects),
        "{name} objects"
    );
    assert_eq!(
        gb + u64::from(g.num_elements),
        wb + u64::from(w.num_elements),
        "{name} num"
    );
    assert_eq!(g.kind, w.kind, "{name}");
    assert_eq!(
        (g.item_size, g.item_object, g.item_serial),
        (w.item_size, w.item_object, w.item_serial),
        "{name}"
    );
    match (&got.names, &want.names) {
        (NameTableLayout::Pool(g), NameTableLayout::Pool(w)) => {
            let (gb, wb) = (absolute(&host, &g.location), absolute(&host, &w.location));
            assert_eq!(
                gb + u64::from(g.blocks),
                wb + u64::from(w.blocks),
                "{name} blocks"
            );
            assert_eq!(
                (g.stride, g.block_offset_bits, &g.entry),
                (w.stride, w.block_offset_bits, &w.entry),
                "{name}"
            );
        }
        (NameTableLayout::Array(g), NameTableLayout::Array(w)) => {
            assert_eq!(
                absolute(&host, &g.location),
                absolute(&host, &w.location),
                "{name}"
            );
            assert_eq!(
                (g.indirect, g.num_elements, g.num_chunks),
                (w.indirect, w.num_elements, w.num_chunks),
                "{name}"
            );
            assert_eq!(g.entry, w.entry, "{name}");
        }
        other => panic!("{name}: {other:?}"),
    }
    assert_eq!(got.fname, want.fname, "{name} fname");
    assert_eq!(got.uobject, want.uobject, "{name} uobject");
    assert_eq!(got.ufield, want.ufield, "{name}");
    assert_eq!(
        got.ustruct.super_struct, want.ustruct.super_struct,
        "{name}"
    );
    assert_eq!(got.ustruct.children, want.ustruct.children, "{name}");
    assert_eq!(
        got.ustruct.child_properties, want.ustruct.child_properties,
        "{name}"
    );
    assert_eq!(
        got.ustruct.properties_size, want.ustruct.properties_size,
        "{name}"
    );
    assert_eq!(got.ufunction.flags, want.ufunction.flags, "{name}");
    assert_eq!(got.properties.system, want.properties.system, "{name}");
    assert_eq!(got.properties.size, want.properties.size, "{name}");
    assert_eq!(
        got.properties.offset_internal, want.properties.offset_internal,
        "{name}"
    );

    let globals = &host.json["globals"];
    assert_eq!(
        d.globals.gworld,
        vec![globals["gworld_rva"].as_u64().unwrap()],
        "{name}"
    );
    assert_eq!(
        d.globals.gengine,
        vec![globals["gengine_rva"].as_u64().unwrap()],
        "{name}"
    );
    assert_eq!(
        d.globals.process_event_index.map(u64::from),
        globals["process_event_index"].as_u64(),
        "{name}"
    );
    assert_eq!(
        d.globals.process_event_rva,
        globals["process_event_rva"].as_u64(),
        "{name}"
    );

    // The child exits cleanly once its stdin closes: discovery left it alone.
    assert!(
        host.close_and_wait(),
        "{name}: the host did not exit cleanly"
    );
}

#[test]
fn a_pool_layout_is_read_through_real_os_reads() {
    check("ue4.25-cp", 5);
}

#[test]
fn an_array_layout_is_read_through_real_os_reads() {
    check("ue4.21", 6);
}
