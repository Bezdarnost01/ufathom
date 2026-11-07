//! A second copy of the DLL, under another file name, is refused and the first
//! keeps working.

#![cfg(windows)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code may panic on failure"
)]

mod common;

use std::fs;
use std::time::Duration;

use common::{
    COMMAND_TIMEOUT, DUMP_TIMEOUT, Host, HostSpec, Sandbox, UNLOAD_TIMEOUT,
    assert_dump_matches_truth, dump_folder, wait_for,
};

#[test]
fn a_second_copy_is_refused_and_the_first_keeps_working() {
    let first = Sandbox::named("ufathom.dll", "");
    let second = Sandbox::named("ufathom_second_copy.dll", "");
    let mut host = Host::spawn(&HostSpec::new(300), &[&first.dll], false);
    let control = first.control();
    control.wait_state("ready", COMMAND_TIMEOUT);

    // Load the second copy while the first runs.
    assert!(
        host.load(&second.dll),
        "LoadLibraryW of the second copy failed"
    );

    // It leaves by itself: no longer mapped, none of its threads left.
    let report = {
        let start = std::time::Instant::now();
        loop {
            let report = host.report();
            if !report.is_loaded(&second.dll) && report.dll_threads(&second.dll).is_empty() {
                break report;
            }
            assert!(
                start.elapsed() < UNLOAD_TIMEOUT,
                "the second copy was not refused: {report:?}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    };
    assert!(
        report.is_loaded(&first.dll),
        "the first copy must stay loaded"
    );
    assert_eq!(report.dll_threads(&first.dll).len(), 3, "{report:?}");

    // It touched nothing of its own: no output folder, no control files.
    assert!(
        !second.output.exists(),
        "the refused copy created {}",
        second.output.display()
    );
    // It said why, where the user can find it.
    let refusal = second
        .dll
        .parent()
        .unwrap()
        .join(format!("ufathom-refused-{}.log", host.pid()));
    let text = wait_for("the refusal log", COMMAND_TIMEOUT, || {
        fs::read_to_string(&refusal).ok()
    });
    assert!(text.contains("already loaded"), "{text}");

    // The first copy never noticed.
    assert_eq!(control.status().unwrap()["state"], "ready");
    control.send("d", "dump", "");
    let result = control.wait_result("d", DUMP_TIMEOUT);
    assert_eq!(result["status"], "ok", "{result}");
    assert_dump_matches_truth(&host.local_fixture(), &dump_folder(&result));

    control.send("halt", "stop", "");
    control.wait_state("stopped", UNLOAD_TIMEOUT);
    let after = host.wait_unloaded(&first.dll, UNLOAD_TIMEOUT);
    assert!(after.dll_threads(&first.dll).is_empty(), "{after:?}");
    assert!(host.is_running());
}

#[test]
fn the_same_copy_can_run_again_once_the_first_instance_is_gone() {
    // The mutex is released at unload, so a new load is a first load again.
    let sandbox = Sandbox::named("ufathom.dll", "");
    let other = Sandbox::named("ufathom_other_name.dll", "");
    let mut host = Host::spawn(&HostSpec::new(200), &[&sandbox.dll], false);
    let control = sandbox.control();
    control.wait_state("ready", COMMAND_TIMEOUT);
    control.send("halt", "stop", "");
    control.wait_state("stopped", UNLOAD_TIMEOUT);
    host.wait_unloaded(&sandbox.dll, UNLOAD_TIMEOUT);

    assert!(host.load(&other.dll));
    let second = other.control();
    second.wait_state("ready", COMMAND_TIMEOUT);
    second.send("d", "dump", "");
    assert_eq!(second.wait_result("d", DUMP_TIMEOUT)["status"], "ok");
}
