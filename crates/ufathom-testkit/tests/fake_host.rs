//! `fake-host` end to end: spawn it, read its JSON line, read its memory with
//! `ProcessMemory` and compare against an identical in-process build.

#![cfg(windows)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::cast_possible_truncation,
    reason = "test code may panic on failure; clippy.toml only exempts #[test] functions, not helpers"
)]

use std::io::{BufRead, BufReader};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use ufathom_mem::{MemoryReader, MemoryReaderExt, ProcessMemory};
use ufathom_model::EngineLayout;
use ufathom_testkit::{Corruption, FixtureBuilder, HostedPlacement, PartKind, Placement, Profile};

const LINE_TIMEOUT: Duration = Duration::from_secs(60);
const EXIT_TIMEOUT: Duration = Duration::from_secs(30);

/// A running host. The child is killed and reaped on drop, even if the test
/// panics.
struct Host {
    child: Child,
    stdin: Option<ChildStdin>,
    json: serde_json::Value,
}

impl Host {
    fn spawn(profile: &str, seed: u64, filler: usize, extra: &[&str]) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_fake-host"))
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
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn hosted_image_matches_an_identical_local_build_byte_for_byte() {
    let mut host = Host::spawn("ue4.25-cp", 5, 800, &[]);
    let memory =
        ProcessMemory::open(u32::try_from(host.u64(&["pid"])).unwrap()).expect("open the child");

    // Rebuild the same fixture locally from the reported addresses.
    let placement = HostedPlacement {
        module_name: host.json["module"].as_str().unwrap().to_owned(),
        module_base: host.u64(&["module_base"]),
        text: host.u64(&["addresses", "text"]),
        rdata: host.u64(&["addresses", "rdata"]),
        data: host.u64(&["addresses", "data"]),
        heap: host.u64(&["addresses", "heap"]),
    };
    let local = FixtureBuilder::new(Profile::by_name("ue4.25-cp").unwrap())
        .seed(5)
        .filler_objects(800)
        .placement(Placement::Hosted(placement))
        .build();

    let layout: EngineLayout = serde_json::from_value(host.json["layout"].clone()).unwrap();
    assert_eq!(layout, local.layout, "the host reports the expected layout");

    let counter = local.anchors.data.frame_counter;
    for part in &local.parts {
        let mut remote = vec![0u8; part.bytes.len()];
        memory
            .read_exact(part.addr, &mut remote)
            .expect("the part is readable in the child");
        if part.kind == PartKind::Data {
            // The ticker owns the counter slot; everything else must match.
            let at = (counter - part.addr) as usize;
            remote[at..at + 8].fill(0);
            let mut expected = part.bytes.clone();
            expected[at..at + 8].fill(0);
            assert_eq!(remote, expected, "{:?}", part.kind);
        } else {
            assert_eq!(remote, part.bytes, "{:?} at {:#x}", part.kind, part.addr);
        }
    }

    // The gaps between heap parts are really unmapped, and the reserved tail is not readable.
    assert!(
        memory
            .read_u8(host.u64(&["addresses", "unmapped"]))
            .is_err()
    );
    let heap = host.u64(&["addresses", "heap"]);
    assert!(
        memory.read_u8(heap).is_err(),
        "the heap range starts with a gap"
    );

    // The module list holds the real executable, and the globals sit inside its image.
    let modules = memory.modules().unwrap();
    let exe = modules
        .iter()
        .find(|m| {
            m.name
                .eq_ignore_ascii_case(host.json["module"].as_str().unwrap())
        })
        .expect("the exe is in the module list");
    assert_eq!(exe.base, host.u64(&["module_base"]));
    let gworld_slot = exe.base + host.u64(&["globals", "gworld_rva"]);
    assert!(gworld_slot >= exe.base && gworld_slot < exe.base + exe.size);
    assert_eq!(
        memory.read_u64(gworld_slot).unwrap(),
        host.u64(&["addresses", "world"])
    );
    let writable = memory
        .regions()
        .unwrap()
        .into_iter()
        .find(|r| r.contains(gworld_slot))
        .expect("the slot is in a region");
    assert!(
        writable.perms.write,
        "GWorld sits in a writable section of the exe"
    );

    assert!(host.close_and_wait(), "the host exits when stdin closes");
}

#[test]
fn the_counter_ticks_like_a_running_game() {
    let mut host = Host::spawn("ue4.21-measured", 1, 100, &[]);
    let memory = ProcessMemory::open(u32::try_from(host.u64(&["pid"])).unwrap()).unwrap();
    let slot = host.u64(&["addresses", "frame_counter"]);
    let first = memory.read_u64(slot).unwrap();
    let start = Instant::now();
    let advanced = loop {
        let now = memory.read_u64(slot).unwrap();
        if now > first {
            break true;
        }
        if start.elapsed() > Duration::from_secs(10) {
            break false;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(advanced, "the counter did not advance");
    assert!(host.close_and_wait());
}

#[test]
fn a_corrupted_image_is_hosted_with_the_corruption_applied() {
    let host = Host::spawn("ue4.25-cp", 2, 200, &["--corrupt", "UnmappedNamesBlock"]);
    let memory = ProcessMemory::open(u32::try_from(host.u64(&["pid"])).unwrap()).unwrap();
    let layout: EngineLayout = serde_json::from_value(host.json["layout"].clone()).unwrap();
    let ufathom_model::layout::NameTableLayout::Pool(pool) = layout.names else {
        panic!("a pool is expected");
    };
    let ufathom_model::layout::Location::Rva { rva, .. } = pool.location else {
        panic!("an RVA is expected");
    };
    let base = host.u64(&["module_base"]);
    // The only name block is the last one, so its pointer is the unmapped address.
    let block = memory
        .read_u64(base + rva + u64::from(pool.blocks))
        .unwrap();
    assert_eq!(block, host.u64(&["addresses", "unmapped"]));
    assert!(memory.read_u8(block).is_err());
    let _ = Corruption::UnmappedNamesBlock;
}
