//! The real `ufathom.dll`, loaded by `fake-host`: dump, repeat, unload,
//! garbage commands and the automatic dump.

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
use std::path::PathBuf;
use std::time::Duration;

use common::{
    COMMAND_TIMEOUT, DUMP_TIMEOUT, Host, HostSpec, Sandbox, UNLOAD_TIMEOUT,
    assert_dump_matches_truth, dump_folder, read_json, wait_for,
};
use ufathom_agent::paths::extended_path;
use ufathom_agent::sha256_file;

/// A host with the DLL of `sandbox` loaded, and the DLL ready.
fn started(sandbox: &Sandbox, spec: &HostSpec) -> Host {
    let host = Host::spawn(spec, &[&sandbox.dll], false);
    sandbox.control().wait_state("ready", COMMAND_TIMEOUT);
    host
}

#[test]
fn a_dump_equals_the_truth_of_the_image() {
    let sandbox = Sandbox::new("");
    let spec = HostSpec::new(800);
    let mut host = started(&sandbox, &spec);
    let control = sandbox.control();

    control.send("first", "dump", r#""label":"first""#);
    let result = control.wait_result("first", DUMP_TIMEOUT);
    assert_eq!(result["status"], "ok", "{result}");
    let folder = dump_folder(&result);
    assert!(folder.ends_with("first"), "{}", folder.display());
    assert_dump_matches_truth(&host.local_fixture(), &folder);

    for file in [
        "dump.json",
        "Mappings.usmap",
        "identity.json",
        "ufathom.log",
    ] {
        assert!(extended_path(&folder.join(file)).is_file(), "{file}");
    }
    let identity = read_json(&folder.join("identity.json"));
    assert_eq!(identity["result"], "ok");
    assert_eq!(identity["pid"], u64::from(host.pid()));
    assert_eq!(
        identity["dll"]["sha256"].as_str().unwrap(),
        sha256_file(&sandbox.dll, &std::sync::atomic::AtomicBool::new(false)).unwrap()
    );
    assert_eq!(identity["game"]["sha256"].as_str().unwrap().len(), 64);
    assert!(identity["discovery"]["evidence"].as_array().unwrap().len() > 10);
    assert_eq!(identity["config"]["threads"], 2);
    assert!(host.is_running());
}

#[test]
fn a_second_dump_without_reloading_is_identical_and_in_a_new_folder() {
    let sandbox = Sandbox::new("");
    let mut host = started(&sandbox, &HostSpec::new(500));
    let control = sandbox.control();
    let mut folders = Vec::new();
    let mut bytes = Vec::new();
    for id in ["one", "two"] {
        control.send(id, "dump", "");
        let result = control.wait_result(id, DUMP_TIMEOUT);
        assert_eq!(result["status"], "ok", "{result}");
        let folder = dump_folder(&result);
        bytes.push(assert_dump_matches_truth(&host.local_fixture(), &folder));
        folders.push(folder);
    }
    assert_ne!(
        folders[0], folders[1],
        "the second dump needs its own folder"
    );
    assert_eq!(folders[0].parent(), folders[1].parent(), "same session");
    assert_eq!(
        bytes[0], bytes[1],
        "dump.json differs between two dumps of one image"
    );
    assert_eq!(
        fs::read(extended_path(&folders[0].join("Mappings.usmap"))).unwrap(),
        fs::read(extended_path(&folders[1].join("Mappings.usmap"))).unwrap()
    );
    let status = control.status().unwrap();
    assert_eq!(status["dumps_completed"], 2);
    assert!(host.is_running());
}

#[test]
fn stop_unloads_the_dll_and_leaves_no_thread_behind() {
    let sandbox = Sandbox::new("");
    let mut host = started(&sandbox, &HostSpec::new(300));
    let control = sandbox.control();

    // Use the DLL first, so its pool and dump threads have existed.
    control.send("d", "dump", "");
    assert_eq!(control.wait_result("d", DUMP_TIMEOUT)["status"], "ok");
    let before = host.report();
    assert!(before.is_loaded(&sandbox.dll));
    // The worker thread and the two pool threads of the default config.
    assert_eq!(
        before.dll_threads(&sandbox.dll).len(),
        3,
        "while loaded: {before:?}"
    );

    control.send("halt", "stop", "");
    assert_eq!(control.wait_result("halt", COMMAND_TIMEOUT)["status"], "ok");
    control.wait_state("stopped", UNLOAD_TIMEOUT);

    // `stopped` is published after the log is closed, so the log is already
    // complete the moment a client sees it.
    let logs = sandbox.output.join("logs");
    let log = fs::read_dir(extended_path(&logs))
        .unwrap()
        .filter_map(Result::ok)
        .next()
        .expect("a session log exists");
    let text = fs::read_to_string(log.path()).unwrap();
    assert!(text.contains("loaded in process"), "{text}");
    assert!(text.contains("pool threads joined; stopped"), "{text}");
    assert!(!text.contains("PANIC"), "{text}");

    let after = host.wait_unloaded(&sandbox.dll, UNLOAD_TIMEOUT);
    assert!(!after.is_loaded(&sandbox.dll), "{after:?}");
    assert!(
        after.dll_threads(&sandbox.dll).is_empty(),
        "a thread of the DLL is left: {after:?}"
    );
    assert!(after.ticks > before.ticks, "the host must keep running");
    assert!(host.is_running());
    assert!(host.close_and_wait(), "the host did not exit cleanly");
}

#[test]
fn the_dll_can_be_loaded_again_after_it_unloaded() {
    let sandbox = Sandbox::new("");
    let mut host = started(&sandbox, &HostSpec::new(300));
    let control = sandbox.control();
    control.send("halt", "stop", "");
    control.wait_state("stopped", UNLOAD_TIMEOUT);
    host.wait_unloaded(&sandbox.dll, UNLOAD_TIMEOUT);

    assert!(
        host.load(&sandbox.dll),
        "LoadLibraryW failed the second time"
    );
    // The new instance publishes `ready` over the old `stopped`.
    control.wait_state("ready", COMMAND_TIMEOUT);
    control.send("d", "dump", "");
    let result = control.wait_result("d", DUMP_TIMEOUT);
    assert_eq!(result["status"], "ok", "{result}");
    assert_dump_matches_truth(&host.local_fixture(), &dump_folder(&result));
}

#[test]
fn garbage_commands_get_error_results_and_the_dll_keeps_working() {
    let sandbox = Sandbox::new("");
    let mut host = started(&sandbox, &HostSpec::new(300));
    let control = sandbox.control();

    control.send_raw("not-json.json", "{ this is { not json");
    control.send_raw("array.json", "[1, 2, 3]");
    control.send_raw("empty.json", "");
    control.send("weird", "self-destruct", "");
    control.send("bad-options", "dump", r#""formats":["xml"]"#);
    control.send_raw("bad-id.json", r#"{"id":"../../escape","command":"status"}"#);
    control.send_raw("binary.json", "\u{0}\u{1}\u{2}");

    for name in [
        "not-json",
        "array",
        "empty",
        "weird",
        "bad-options",
        "bad-id",
        "binary",
    ] {
        let result = control.wait_result(name, COMMAND_TIMEOUT);
        assert_eq!(result["status"], "error", "{name}: {result}");
        assert!(
            !result["message"].as_str().unwrap_or_default().is_empty(),
            "{name}: an error result states the reason"
        );
    }
    assert_eq!(
        control.wait_result("weird", COMMAND_TIMEOUT)["command"],
        "self-destruct"
    );
    assert!(!sandbox.output.join("escape.json").exists());
    assert!(
        !sandbox
            .output
            .parent()
            .unwrap()
            .join("escape.json")
            .exists()
    );

    // None of that disturbed the state, and the DLL still serves.
    assert_eq!(control.status().unwrap()["state"], "ready");
    control.send("ok", "status", "");
    assert_eq!(control.wait_result("ok", COMMAND_TIMEOUT)["status"], "ok");
    assert!(host.is_running());
}

#[test]
fn a_dump_while_dumping_is_busy() {
    // Both commands are published together, so the DLL reads them in one poll,
    // in name order: the first starts a dump and the second finds it running.
    let sandbox = Sandbox::new("");
    let mut host = started(&sandbox, &HostSpec::new(common::LARGE_FILLER));
    let control = sandbox.control();

    control.send_together(&[
        ("a-first", "dump", r#""label":"long""#),
        ("b-second", "dump", r#""label":"second""#),
    ]);
    let busy = control.wait_result("b-second", COMMAND_TIMEOUT);
    assert_eq!(busy["status"], "busy", "{busy}");
    assert_eq!(busy["state"], "dumping");

    let first = control.wait_result("a-first", DUMP_TIMEOUT);
    assert_eq!(first["status"], "ok", "{first}");
    assert_dump_matches_truth(&host.local_fixture(), &dump_folder(&first));
    assert!(host.is_running());
}

#[test]
fn a_stop_read_together_with_a_dump_cancels_it_and_writes_nothing() {
    let sandbox = Sandbox::new("");
    let mut host = started(&sandbox, &HostSpec::new(common::LARGE_FILLER));
    let control = sandbox.control();

    control.send_together(&[("a-run", "dump", ""), ("b-halt", "stop", "")]);
    assert_eq!(
        control.wait_result("b-halt", COMMAND_TIMEOUT)["status"],
        "ok"
    );
    let run = control.wait_result("a-run", UNLOAD_TIMEOUT);
    assert_eq!(run["status"], "cancelled", "{run}");
    assert!(
        run["dump"].is_null(),
        "a cancelled dump has no result: {run}"
    );
    control.wait_state("stopped", UNLOAD_TIMEOUT);
    let after = host.wait_unloaded(&sandbox.dll, UNLOAD_TIMEOUT);
    assert!(after.dll_threads(&sandbox.dll).is_empty(), "{after:?}");
    let folders: Vec<_> = fs::read_dir(extended_path(&sandbox.output))
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        folders
            .iter()
            .all(|name| name == "control" || name == "logs"),
        "a cancelled dump must write no dump folder: {folders:?}"
    );
}

#[test]
fn stop_while_dumping_cancels_the_dump_and_unloads() {
    let sandbox = Sandbox::new("");
    let mut host = started(&sandbox, &HostSpec::new(common::LARGE_FILLER));
    let control = sandbox.control();

    control.send("a-run", "dump", "");
    control.wait_state("dumping", COMMAND_TIMEOUT);
    control.send("b-halt", "stop", "");
    let stop = control.wait_result("b-halt", COMMAND_TIMEOUT);
    assert_eq!(stop["status"], "ok", "{stop}");
    let run = control.wait_result("a-run", UNLOAD_TIMEOUT);
    assert!(
        matches!(run["status"].as_str(), Some("cancelled" | "ok")),
        "a stopped dump is cancelled, or finished just before: {run}"
    );
    control.wait_state("stopped", UNLOAD_TIMEOUT);
    let after = host.wait_unloaded(&sandbox.dll, UNLOAD_TIMEOUT);
    assert!(after.dll_threads(&sandbox.dll).is_empty(), "{after:?}");
    assert!(host.is_running());
}

#[test]
fn auto_dump_produces_a_dump_without_any_command() {
    let sandbox = Sandbox::new("auto_dump = true\ntrigger_delay_seconds = 1.0\n");
    let host = Host::spawn(&HostSpec::new(400), &[&sandbox.dll], false);
    let control = sandbox.control();
    control.wait_state("ready", COMMAND_TIMEOUT);

    let done = wait_for("the automatic dump", DUMP_TIMEOUT, || {
        control
            .status()
            .filter(|doc| doc["dumps_completed"] == 1 && doc["state"] == "idle")
    });
    assert_eq!(done["last_dump"]["label"], "auto");
    assert_eq!(done["last_dump"]["result"], "ok");
    let folder = PathBuf::from(done["last_dump"]["folder"].as_str().unwrap());
    assert_dump_matches_truth(&host.local_fixture(), &folder);
    assert!(
        control.archived().is_empty(),
        "no command was involved: {:?}",
        control.archived()
    );

    // It runs once, not repeatedly.
    std::thread::sleep(Duration::from_millis(1500));
    assert_eq!(control.status().unwrap()["dumps_completed"], 1);
}

#[test]
fn auto_unload_unloads_the_dll_after_the_automatic_dump() {
    let sandbox = Sandbox::new(
        "auto_dump = true\ntrigger_delay_seconds = 0.5\nauto_unload = true\nthreads = 1\n",
    );
    let mut host = Host::spawn(&HostSpec::new(300), &[&sandbox.dll], false);
    let control = sandbox.control();
    let stopped = control.wait_state("stopped", DUMP_TIMEOUT);
    assert_eq!(stopped["last_dump"]["result"], "ok", "{stopped}");
    let after = host.wait_unloaded(&sandbox.dll, UNLOAD_TIMEOUT);
    assert!(after.dll_threads(&sandbox.dll).is_empty(), "{after:?}");
    assert_dump_matches_truth(
        &host.local_fixture(),
        &PathBuf::from(stopped["last_dump"]["folder"].as_str().unwrap()),
    );
}

#[test]
fn a_bad_config_means_defaults_and_the_dll_still_starts() {
    let sandbox = Sandbox::new("threads = 99\nthis_key_does_not_exist = true\n");
    let mut spec = HostSpec::new(100);
    spec.exe = Some(sandbox.copy_host_exe());
    let exe_dir = spec.exe.as_ref().unwrap().parent().unwrap().to_owned();
    let mut host = Host::spawn(&spec, &[&sandbox.dll], false);

    // The broken file names the output folder, but it is refused as a whole, so
    // the DLL falls back to the default: a `ufathom` folder beside the executable.
    let control = common::Control::at(exe_dir.join("ufathom").join("control"));
    let status = control.wait_state("ready", COMMAND_TIMEOUT);
    assert_eq!(status["pid"], u64::from(host.pid()));
    assert!(
        !sandbox.output.exists(),
        "the refused config must not be used"
    );

    // The problem is in the log, and the defaults are in effect: a dump works.
    control.send("d", "dump", "");
    let result = control.wait_result("d", DUMP_TIMEOUT);
    assert_eq!(result["status"], "ok", "{result}");
    let identity = read_json(&dump_folder(&result).join("identity.json"));
    assert_eq!(identity["config_source"], "defaults");
    assert_eq!(identity["config"]["threads"], 2);
    let log_folder = exe_dir.join("ufathom").join("logs");
    let log = fs::read_dir(extended_path(&log_folder))
        .unwrap()
        .filter_map(Result::ok)
        .next()
        .expect("a session log exists");
    let text = fs::read_to_string(log.path()).unwrap();
    assert!(text.contains("config file was not used"), "{text}");
    assert!(text.contains("threads"), "{text}");

    control.send("halt", "stop", "");
    control.wait_state("stopped", UNLOAD_TIMEOUT);
    host.wait_unloaded(&sandbox.dll, UNLOAD_TIMEOUT);
}

#[test]
fn a_config_beside_the_executable_wins_over_one_beside_the_dll() {
    let sandbox = Sandbox::new("");
    let mut spec = HostSpec::new(100);
    let exe = sandbox.copy_host_exe();
    let exe_output = exe.parent().unwrap().join("chosen-output");
    fs::write(
        exe.parent().unwrap().join("ufathom.toml"),
        format!("output = '{}'\nthreads = 1\n", exe_output.display()),
    )
    .unwrap();
    spec.exe = Some(exe);
    let mut host = Host::spawn(&spec, &[&sandbox.dll], false);

    let control = common::Control::at(exe_output.join("control"));
    control.wait_state("ready", COMMAND_TIMEOUT);
    assert!(!sandbox.output.exists(), "the DLL's own config must lose");
    control.send("d", "dump", "");
    let result = control.wait_result("d", DUMP_TIMEOUT);
    let identity = read_json(&dump_folder(&result).join("identity.json"));
    assert_eq!(identity["config"]["threads"], 1);
    control.send("halt", "stop", "");
    control.wait_state("stopped", UNLOAD_TIMEOUT);
    host.wait_unloaded(&sandbox.dll, UNLOAD_TIMEOUT);
}
