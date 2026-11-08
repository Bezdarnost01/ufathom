//! How much a dump disturbs the host: the worst gap between two ticks of the
//! host's own thread while the DLL dumps.
//!
//! The host's ticker sleeps 5 ms per tick and records the gap between
//! consecutive ticks, so an undisturbed host shows gaps of about 5 to 16 ms
//! (timer granularity and scheduling). The worst gap while the DLL dumps is
//! compared with the worst gap of the same host idle, and with a fixed bound.
//!
//! One dump of the synthetic image takes a few hundred milliseconds, so the
//! test dumps back to back until the DLL has been busy for several seconds: a
//! pool that starves the host would show up in some of those windows.
//!
//! These tests measure timing, so they run one at a time.

#![cfg(windows)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::print_stdout,
    reason = "test code may panic on failure and prints what it measured"
)]

mod common;

use std::sync::Mutex;
use std::time::Duration;

use common::{
    COMMAND_TIMEOUT, DUMP_TIMEOUT, Host, HostSpec, Sandbox, UNLOAD_TIMEOUT,
    assert_dump_matches_truth, dump_folder,
};

/// Image size of the stall tests.
const STALL_FILLER: usize = 60_000;

/// How long the DLL must have been dumping, in total, before the window closes.
const MIN_BUSY_MS: u64 = 3_000;

/// Most dumps one test runs, so a broken DLL cannot loop forever.
const MAX_DUMPS: usize = 80;

/// The most the host's tick may be late during a dump, in milliseconds.
///
/// A game that runs at 30 frames per second has a 33 ms frame, so 100 ms is
/// three dropped frames: a hitch a player would notice. A dump that stays under
/// it does not "block the game for seconds", which is the failure the criterion
/// is about. The bound is loose on purpose, because a shared CI machine can
/// delay any thread by tens of milliseconds, idle or not: the values measured
/// here (printed by these tests) are about 6 to 9 ms, against a 5 ms tick.
const MAX_STALL_MS: f64 = 100.0;

/// Serializes the tests of this file: two dumps at once would disturb each
/// other's measurement.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

fn measure(threads: usize) {
    let _alone = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let sandbox = Sandbox::new(&format!("threads = {threads}\n"));
    let mut host = Host::spawn(&HostSpec::new(STALL_FILLER), &[&sandbox.dll], true);
    let control = sandbox.control();
    control.wait_state("ready", COMMAND_TIMEOUT);

    // The idle host: the DLL is loaded and polling, no dump runs.
    let _ = host.report();
    std::thread::sleep(Duration::from_secs(2));
    let idle = host.report();

    // The same host while the DLL dumps, from just before the first command to
    // just after the last result.
    let _ = host.report();
    let mut dumps = 0;
    let mut busy_ms = 0;
    let mut last = None;
    while busy_ms < MIN_BUSY_MS && dumps < MAX_DUMPS {
        let id = format!("d{dumps}");
        control.send(&id, "dump", "");
        let result = control.wait_result(&id, DUMP_TIMEOUT);
        assert_eq!(result["status"], "ok", "{result}");
        busy_ms += result["dump"]["elapsed_ms"].as_u64().unwrap();
        dumps += 1;
        last = Some(result);
    }
    let during = host.report();
    let last = last.unwrap();

    println!(
        "stall, {threads} pool threads, {STALL_FILLER} filler objects: {dumps} dumps, \
         {busy_ms} ms busy in total; worst tick gap idle {:.1} ms, during the dumps {:.1} ms \
         (bound {MAX_STALL_MS} ms)",
        idle.worst_gap_ms, during.worst_gap_ms,
    );
    assert!(
        busy_ms >= MIN_BUSY_MS,
        "{MAX_DUMPS} dumps were busy for only {busy_ms} ms: too short to measure a stall"
    );
    assert!(
        during.worst_gap_ms < MAX_STALL_MS,
        "the host's tick was late by {:.1} ms during dumps (idle: {:.1} ms)",
        during.worst_gap_ms,
        idle.worst_gap_ms
    );
    assert!(during.ticks > idle.ticks, "the host must keep ticking");
    assert_dump_matches_truth(&host.local_fixture(), &dump_folder(&last));

    control.send("halt", "stop", "");
    control.wait_state("stopped", UNLOAD_TIMEOUT);
    host.wait_unloaded(&sandbox.dll, UNLOAD_TIMEOUT);

    // The final report the host prints when stdin closes (`--report-stall`).
    let final_report = host
        .close_and_read_final_report()
        .expect("the host prints a report when stdin closes");
    println!(
        "stall, {threads} pool threads: worst gap while unloading {:.1} ms",
        final_report.worst_gap_ms
    );
    assert!(final_report.worst_gap_ms < MAX_STALL_MS);
    assert!(host.close_and_wait());
}

#[test]
fn dumps_on_two_threads_do_not_stall_the_host() {
    measure(2);
}

#[test]
fn dumps_on_four_threads_do_not_stall_the_host() {
    measure(4);
}

#[test]
fn dumps_on_one_thread_do_not_stall_the_host() {
    measure(1);
}
