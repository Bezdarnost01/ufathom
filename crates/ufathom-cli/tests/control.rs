//! `status` and `command` against a fake control folder: a test thread plays
//! the DLL, reading the inbox and publishing results with the agent's own
//! types.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code may panic on failure"
)]

mod common;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use common::{arg, run};
use ufathom_agent::protocol::{
    Command, CommandResult, CurrentDump, DumpFailure, DumpSummary, LastDump, Request, ResultStatus,
    SdkSummary, State, StatusDocument, parse_request,
};
use ufathom_agent::{ControlDir, Format};

const TIMEOUT: Duration = Duration::from_secs(30);

fn control_in(output: &Path) -> ControlDir {
    let control = ControlDir::new(output);
    control.create().unwrap();
    control
}

fn document(state: State) -> StatusDocument {
    StatusDocument {
        state,
        pid: 4321,
        session: "20260101-000000-p4321".to_owned(),
        tool_version: "0.1.0".to_owned(),
        output: "out".to_owned(),
        dumps_completed: 2,
        dumps_failed: 1,
        current: None,
        last_dump: None,
        detail: None,
        updated: "2026-01-01T00:00:00Z".to_owned(),
    }
}

fn base_result(request: &Request, status: ResultStatus) -> CommandResult {
    CommandResult {
        id: Some(request.id.clone()),
        command: Some(request.command.name().to_owned()),
        status,
        message: None,
        state: State::Idle,
        dump: None,
        failure: None,
        status_document: None,
        finished: "2026-01-01T00:00:01Z".to_owned(),
    }
}

fn summary(degraded: bool) -> DumpSummary {
    DumpSummary {
        folder: "dumps/ue4.25.1-abcd1234/session/menu".to_owned(),
        files: vec!["dump.json".to_owned(), "sdk/".to_owned()],
        engine: Some("4.25.1".to_owned()),
        structs: 25,
        enums: 2,
        functions: 13,
        objects: 365,
        failures: u64::from(degraded) * 30,
        diagnostics: 0,
        degraded,
        sdk: Some(SdkSummary {
            folder: "dumps/ue4.25.1-abcd1234/session/menu/sdk".to_owned(),
            packages: 17,
            classes: 18,
            script_structs: 7,
            enums: 2,
            functions: 13,
            functions_skipped: 0,
            degraded: 4,
            files: 19,
        }),
        elapsed_ms: 321,
    }
}

/// A thread that plays the DLL for one command.
struct Dll {
    handle: JoinHandle<Request>,
}

impl Dll {
    /// Waits for one command, then `act` decides what the DLL does with it.
    fn serve(
        control: &ControlDir,
        act: impl FnOnce(&ControlDir, &Request) + Send + 'static,
    ) -> Self {
        let control = control.clone();
        let handle = std::thread::spawn(move || {
            let started = Instant::now();
            loop {
                let pending = control.pending().unwrap();
                if let Some(entry) = pending.first() {
                    let text = control.read_command(entry).unwrap();
                    let request = parse_request(&text).expect("the CLI sent a valid command");
                    act(&control, &request);
                    return request;
                }
                assert!(started.elapsed() < TIMEOUT, "no command arrived");
                std::thread::sleep(Duration::from_millis(10));
            }
        });
        Self { handle }
    }

    /// Answers every command with `result`, and archives the command file.
    fn answer(
        control: &ControlDir,
        result: impl FnOnce(&Request) -> CommandResult + Send + 'static,
    ) -> Self {
        Self::serve(control, move |control, request| {
            let entry = control.pending().unwrap().remove(0);
            control
                .publish_result(&request.id, &result(request))
                .unwrap();
            control.archive_command(&entry).unwrap();
        })
    }

    fn request(self) -> Request {
        self.handle.join().unwrap()
    }
}

#[test]
fn status_prints_the_document_the_dll_published() {
    let dir = tempfile::tempdir().unwrap();
    let control = control_in(dir.path());
    let mut status = document(State::Dumping);
    status.current = Some(CurrentDump {
        label: "menu".to_owned(),
        started: "2026-01-01T00:00:00Z".to_owned(),
    });
    status.last_dump = Some(LastDump {
        label: "dump-001".to_owned(),
        result: "ok".to_owned(),
        folder: Some("some/folder".to_owned()),
        finished: "2026-01-01T00:00:00Z".to_owned(),
    });
    status.detail = Some("stays loaded".to_owned());
    control.write_status(&status).unwrap();

    run(&["status", &arg(dir.path())])
        .expect_code(0)
        .stdout_has("state:         dumping")
        .stdout_has("process:       4321")
        .stdout_has("2 completed, 1 failed")
        .stdout_has("running:       menu")
        .stdout_has("last dump:     dump-001")
        .stdout_has("some/folder")
        .stdout_has("stays loaded");
}

#[test]
fn status_without_a_status_file_says_the_dll_may_not_be_loaded() {
    let dir = tempfile::tempdir().unwrap();
    control_in(dir.path());
    run(&["status", &arg(dir.path())])
        .expect_code(1)
        .stderr_has("status file")
        .stderr_has("may not be loaded");
}

#[test]
fn status_refuses_a_file_that_is_not_a_status_document() {
    let dir = tempfile::tempdir().unwrap();
    let control = control_in(dir.path());
    std::fs::write(control.status_path(), "{\"hello\": 1}").unwrap();
    run(&["status", &arg(dir.path())])
        .expect_code(1)
        .stderr_has("not a status document");
}

#[test]
fn a_status_command_waits_for_its_result_and_prints_the_document() {
    let dir = tempfile::tempdir().unwrap();
    let control = control_in(dir.path());
    let dll = Dll::answer(&control, |request| {
        let mut result = base_result(request, ResultStatus::Ok);
        result.status_document = Some(document(State::Idle));
        result
    });
    run(&["command", &arg(dir.path()), "status", "--wait", "30"])
        .expect_code(0)
        .stdout_has("result:        ok (status)")
        .stdout_has("state:         idle")
        .stdout_has("2 completed, 1 failed");
    let request = dll.request();
    assert_eq!(request.command, Command::Status);
    assert!(request.id.starts_with("cli-"), "{}", request.id);
}

#[test]
fn a_dump_command_carries_its_label_and_formats_and_prints_the_summary() {
    let dir = tempfile::tempdir().unwrap();
    let control = control_in(dir.path());
    let dll = Dll::answer(&control, |request| {
        let mut result = base_result(request, ResultStatus::Ok);
        result.dump = Some(summary(false));
        result
    });
    let result = run(&[
        "command",
        &arg(dir.path()),
        "dump",
        "--label",
        "menu",
        "--formats",
        "json,Rust-SDK",
        "--wait",
        "30",
    ])
    .expect_code(0);
    result
        .stdout_has("folder:        dumps/ue4.25.1-abcd1234/session/menu")
        .stdout_has("engine:        4.25.1")
        .stdout_has("25 classes and structs, 2 enums, 13 functions from 365 objects")
        .stdout_has("degraded: no")
        .stdout_has("SDK:           17 packages, 18 classes, 7 script structs")
        .stdout_has("321 ms");
    assert_eq!(
        dll.request().command,
        Command::Dump {
            label: Some("menu".to_owned()),
            formats: Some(vec![Format::Json, Format::RustSdk]),
        }
    );
}

#[test]
fn a_degraded_dump_result_exits_3() {
    let dir = tempfile::tempdir().unwrap();
    let control = control_in(dir.path());
    Dll::answer(&control, |request| {
        let mut result = base_result(request, ResultStatus::Ok);
        result.dump = Some(summary(true));
        result
    });
    run(&["command", &arg(dir.path()), "dump", "--wait", "30"])
        .expect_code(3)
        .stdout_has("degraded: YES")
        .stderr_has("degraded");
}

#[test]
fn busy_error_and_cancelled_results_are_failures_with_the_dlls_reason() {
    for (status, word, message) in [
        (
            ResultStatus::Busy,
            "busy",
            "a dump is already running: `one`",
        ),
        (ResultStatus::Error, "error", "formats must name one"),
        (
            ResultStatus::Cancelled,
            "cancelled",
            "the dump was cancelled",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let control = control_in(dir.path());
        Dll::answer(&control, move |request| {
            let mut result = base_result(request, status);
            result.message = Some(message.to_owned());
            result.state = State::Dumping;
            result
        });
        run(&["command", &arg(dir.path()), "dump", "--wait", "30"])
            .expect_code(1)
            .stdout_has(&format!("result:        {word} (dump)"))
            .stdout_has("state:         dumping")
            .stderr_has(&format!("answered `{word}`"))
            .stderr_has(message);
    }
}

#[test]
fn a_failed_dump_prints_where_the_failure_record_is() {
    let dir = tempfile::tempdir().unwrap();
    let control = control_in(dir.path());
    Dll::answer(&control, |request| {
        let mut result = base_result(request, ResultStatus::Error);
        result.failure = Some(DumpFailure {
            kind: "discovery".to_owned(),
            message: "no evidence for uobject.name".to_owned(),
            folder: Some("dumps/failed".to_owned()),
        });
        result
    });
    run(&["command", &arg(dir.path()), "dump", "--wait", "30"])
        .expect_code(1)
        .stdout_has("failure:       discovery: no evidence for uobject.name")
        .stdout_has("record in:     dumps/failed");
}

#[test]
fn without_wait_the_command_is_queued_and_the_cli_returns_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let control = control_in(dir.path());
    let result = run(&["command", &arg(dir.path()), "stop"]).expect_code(0);
    result
        .stdout_has("queued `stop` as cli-")
        .stdout_has("outbox");
    let pending = control.pending().unwrap();
    assert_eq!(pending.len(), 1);
    let request = parse_request(&control.read_command(&pending[0]).unwrap()).unwrap();
    assert_eq!(request.command, Command::Stop);
    assert!(
        pending[0].name.starts_with(&request.id),
        "the file is named by the id: {}",
        pending[0].name
    );
}

#[test]
fn waiting_with_nobody_reading_says_the_request_is_still_in_the_inbox() {
    let dir = tempfile::tempdir().unwrap();
    let control = control_in(dir.path());
    run(&["command", &arg(dir.path()), "status", "--wait", "0.4"])
        .expect_code(1)
        .stderr_has("no result arrived")
        .stderr_has("still in the inbox")
        .stderr_has("ufathom status");
    assert_eq!(
        control.pending().unwrap().len(),
        1,
        "the request stays queued"
    );
}

#[test]
fn waiting_when_the_dll_took_the_request_but_never_answers_says_so() {
    let dir = tempfile::tempdir().unwrap();
    let control = control_in(dir.path());
    // The DLL archives the command, as it does when a dump starts, and the
    // dump never ends within the wait.
    Dll::serve(&control, |control, _| {
        let entry = control.pending().unwrap().remove(0);
        control.archive_command(&entry).unwrap();
    });
    run(&["command", &arg(dir.path()), "dump", "--wait", "0.6"])
        .expect_code(1)
        .stderr_has("took the request but has not answered");
}

#[test]
fn a_result_that_cannot_be_read_is_reported_after_the_wait() {
    let dir = tempfile::tempdir().unwrap();
    let control = control_in(dir.path());
    Dll::serve(&control, |control, request| {
        let entry = control.pending().unwrap().remove(0);
        std::fs::write(
            control.outbox().join(format!("{}.json", request.id)),
            "{ truncated",
        )
        .unwrap();
        control.archive_command(&entry).unwrap();
    });
    run(&["command", &arg(dir.path()), "status", "--wait", "0.6"])
        .expect_code(1)
        .stderr_has("is not readable");
}

#[test]
fn a_command_to_a_folder_without_a_control_folder_is_a_failure() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nothing-here");
    run(&["command", &arg(&missing), "status"])
        .expect_code(1)
        .stderr_has("no control folder");
    assert!(
        !missing.exists(),
        "ufathom must not create the DLL's folders"
    );
}

#[test]
fn the_wait_ends_as_soon_as_the_result_is_there() {
    let dir = tempfile::tempdir().unwrap();
    let control = control_in(dir.path());
    let done = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&done);
    Dll::answer(&control, move |request| {
        flag.store(true, Ordering::SeqCst);
        base_result(request, ResultStatus::Ok)
    });
    let started = Instant::now();
    run(&["command", &arg(dir.path()), "stop", "--wait", "60"]).expect_code(0);
    assert!(done.load(Ordering::SeqCst));
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "a 60 s wait must not be used up when the result is already there"
    );
}

#[test]
fn the_cli_talks_to_a_real_service_end_to_end() {
    use ufathom_agent::{Config, ConfigSource, Environment, Service};

    let fx = common::fixture("ue4.25-cp", 11, 200);
    let dir = tempfile::tempdir().unwrap();
    let game = dir.path().join("game.exe");
    let dll = dir.path().join("ufathom.dll");
    std::fs::write(&game, b"not an executable").unwrap();
    std::fs::write(&dll, b"not a dll").unwrap();
    let output = dir.path().join("out");
    let mut config = Config::defaults(dir.path());
    config.output = output.clone();
    config.threads = 2;
    let service = Service::start(
        config,
        Environment {
            game_exe: game,
            dll_path: dll,
            pid: 4321,
            reader: Arc::new(fx.memory.clone()),
            thread_start: None,
            config_source: ConfigSource::Defaults,
        },
    )
    .unwrap();

    // The service is polled on its own thread, as the DLL's worker does.
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let worker = std::thread::spawn(move || {
        let mut service = service;
        while !flag.load(Ordering::SeqCst) {
            service.poll();
            std::thread::sleep(Duration::from_millis(10));
        }
        service.shutdown().is_clean()
    });

    let out = arg(&output);
    run(&["status", &out])
        .expect_code(0)
        .stdout_has("state:         ready");
    let dump = run(&[
        "command",
        &out,
        "dump",
        "--label",
        "e2e",
        "--formats",
        "json,rust-sdk",
        "--wait",
        "60",
    ])
    .expect_code(0);
    dump.stdout_has("result:        ok (dump)")
        .stdout_has("SDK:")
        .stdout_has("degraded: no");
    let folder = dump
        .stdout
        .lines()
        .find_map(|line| line.strip_prefix("folder:        "))
        .expect("the result names the folder");
    let folder = std::path::PathBuf::from(folder);
    assert!(folder.ends_with("e2e"), "{}", folder.display());
    let written =
        ufathom_model::Dump::from_json(&std::fs::read_to_string(folder.join("dump.json")).unwrap())
            .unwrap();
    assert_eq!(written.structs, fx.truth.structs);
    assert!(folder.join("sdk/Cargo.toml").is_file());
    assert!(!folder.join("Mappings.usmap").exists());

    run(&["status", &out])
        .expect_code(0)
        .stdout_has("state:         idle")
        .stdout_has("1 completed, 0 failed")
        .stdout_has("last dump:     e2e");
    run(&["command", &out, "stop", "--wait", "30"])
        .expect_code(0)
        .stdout_has("result:        ok (stop)");

    stop.store(true, Ordering::SeqCst);
    assert!(worker.join().unwrap(), "the service shuts down cleanly");
}
