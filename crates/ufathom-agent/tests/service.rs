//! The service end to end against synthetic images, without a DLL: command
//! files, the state machine, outputs and shutdown.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code may panic on failure"
)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use ufathom_agent::protocol::State;
use ufathom_agent::{Config, ConfigSource, Environment, Format, Service, fsio};
use ufathom_mem::{MemError, MemoryReader, MemoryRegion, ModuleInfo, Snapshot};
use ufathom_model::Dump;
use ufathom_testkit::{Fixture, FixtureBuilder, Profile};

const WAIT: Duration = Duration::from_secs(60);

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

/// A reader that blocks every read until its gate is opened, so a dump can be
/// held in the `dumping` state for as long as a test needs.
struct GatedReader {
    inner: Arc<dyn MemoryReader>,
    open: Mutex<bool>,
    changed: Condvar,
    reads: AtomicUsize,
}

impl GatedReader {
    fn new(inner: Arc<dyn MemoryReader>, open: bool) -> Arc<Self> {
        Arc::new(Self {
            inner,
            open: Mutex::new(open),
            changed: Condvar::new(),
            reads: AtomicUsize::new(0),
        })
    }

    fn set_open(&self, value: bool) {
        *self.open.lock().unwrap() = value;
        self.changed.notify_all();
    }

    fn wait_for_a_read(&self) {
        let start = Instant::now();
        while self.reads.load(Ordering::SeqCst) == 0 {
            assert!(start.elapsed() < WAIT, "the dump never read the target");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl MemoryReader for GatedReader {
    fn read_exact(&self, addr: u64, buf: &mut [u8]) -> Result<(), MemError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let mut open = self.open.lock().unwrap();
        while !*open {
            let (guard, timeout) = self
                .changed
                .wait_timeout(open, Duration::from_secs(120))
                .unwrap();
            open = guard;
            assert!(!timeout.timed_out(), "the gate was never opened");
        }
        drop(open);
        self.inner.read_exact(addr, buf)
    }

    fn regions(&self) -> Result<Vec<MemoryRegion>, MemError> {
        self.inner.regions()
    }

    fn modules(&self) -> Result<Vec<ModuleInfo>, MemError> {
        self.inner.modules()
    }
}

/// A reader that panics, to prove a panic in a dump does not escape.
struct PanickingReader;

impl MemoryReader for PanickingReader {
    fn read_exact(&self, _addr: u64, _buf: &mut [u8]) -> Result<(), MemError> {
        panic!("simulated bug in a reader");
    }

    fn regions(&self) -> Result<Vec<MemoryRegion>, MemError> {
        panic!("simulated bug in a reader");
    }

    fn modules(&self) -> Result<Vec<ModuleInfo>, MemError> {
        panic!("simulated bug in a reader");
    }
}

struct Rig {
    dir: tempfile::TempDir,
    output: PathBuf,
    game: PathBuf,
    dll: PathBuf,
    fixture: Fixture,
}

impl Rig {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let game = dir.path().join("Game-Win64-Shipping.exe");
        let dll = dir.path().join("ufathom.dll");
        std::fs::write(&game, b"not really an executable").unwrap();
        std::fs::write(&dll, b"not really a dll").unwrap();
        let fixture = FixtureBuilder::new(Profile::by_name("ue4.25-cp").unwrap())
            .seed(11)
            .filler_objects(200)
            .build();
        Self {
            output: dir.path().join("out"),
            dir,
            game,
            dll,
            fixture,
        }
    }

    fn config(&self) -> Config {
        let mut config = Config::defaults(self.dir.path());
        config.output = self.output.clone();
        config.threads = 2;
        config
    }

    fn environment(&self, reader: Arc<dyn MemoryReader>) -> Environment {
        Environment {
            game_exe: self.game.clone(),
            dll_path: self.dll.clone(),
            pid: 4321,
            reader,
            thread_start: None,
            config_source: ConfigSource::Defaults,
        }
    }

    fn image(&self) -> Arc<dyn MemoryReader> {
        Arc::new(self.fixture.memory.clone())
    }

    fn start(&self, config: Config, reader: Arc<dyn MemoryReader>) -> Service {
        Service::start(config, self.environment(reader)).unwrap()
    }
}

fn poll_until(service: &mut Service, what: &str, mut done: impl FnMut(&mut Service) -> bool) {
    let start = Instant::now();
    loop {
        service.poll();
        if done(service) {
            return;
        }
        assert!(start.elapsed() < WAIT, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn send(service: &Service, file: &str, text: &str) {
    fsio::write_atomic(&service.control().inbox().join(file), text.as_bytes()).unwrap();
}

fn send_command(service: &Service, id: &str, body: &str) {
    let text = if body.is_empty() {
        format!(r#"{{"id":"{id}"}}"#)
    } else {
        format!(r#"{{"id":"{id}",{body}}}"#)
    };
    send(service, &format!("{id}.json"), &text);
}

fn result_path(service: &Service, id: &str) -> PathBuf {
    service.control().outbox().join(format!("{id}.json"))
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn wait_result(service: &mut Service, id: &str) -> Value {
    let path = result_path(service, id);
    poll_until(service, &format!("result {id}"), |_| path.exists());
    read_json(&path)
}

fn status(service: &Service) -> Value {
    read_json(&service.control().status_path())
}

fn count_dirs(path: &Path) -> usize {
    std::fs::read_dir(path)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|e| e.path().is_dir())
                .count()
        })
        .unwrap_or(0)
}

#[test]
fn start_publishes_ready_and_makes_the_control_folders() -> TestResult {
    let rig = Rig::new();
    let service = rig.start(rig.config(), rig.image());
    assert_eq!(service.state(), State::Ready);
    let doc = status(&service);
    assert_eq!(doc["state"], "ready");
    assert_eq!(doc["pid"], 4321);
    assert_eq!(doc["dumps_completed"], 0);
    for folder in ["inbox", "outbox", "archive"] {
        assert!(service.control().root().join(folder).is_dir(), "{folder}");
    }
    assert!(service.shutdown().is_clean());
    Ok(())
}

#[test]
fn a_dump_command_writes_the_folder_and_publishes_a_result() -> TestResult {
    let rig = Rig::new();
    let mut service = rig.start(rig.config(), rig.image());
    send_command(&service, "d1", r#""command":"dump","label":"menu""#);
    let result = wait_result(&mut service, "d1");
    assert_eq!(result["status"], "ok", "{result}");
    assert_eq!(result["id"], "d1");
    let folder = PathBuf::from(result["dump"]["folder"].as_str().unwrap());
    assert!(folder.ends_with("menu"), "{}", folder.display());
    // <output>/<build id>/<session>/<label>
    let session = folder.parent().unwrap();
    let build = session.parent().unwrap();
    assert_eq!(build.parent().unwrap(), rig.output);
    assert!(
        build
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(&format!(
                "ue{}-",
                rig.fixture.engine.claimed.as_deref().unwrap()
            )),
        "{}",
        build.display()
    );
    assert!(
        session
            .file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with("-p4321"),
        "{}",
        session.display()
    );
    for file in [
        "dump.json",
        "Mappings.usmap",
        "identity.json",
        "ufathom.log",
    ] {
        assert!(folder.join(file).is_file(), "{file}");
    }

    let dump = Dump::from_json(&std::fs::read_to_string(folder.join("dump.json"))?)?;
    assert_eq!(dump.structs, rig.fixture.truth.structs);
    assert_eq!(dump.enums, rig.fixture.truth.enums);

    assert_eq!(service.state(), State::Idle);
    let doc = status(&service);
    assert_eq!(doc["state"], "idle");
    assert_eq!(doc["dumps_completed"], 1);
    assert_eq!(doc["last_dump"]["label"], "menu");
    assert!(doc["current"].is_null());

    let archived = std::fs::read_dir(service.control().archive())?.count();
    assert_eq!(archived, 1);
    assert!(service.control().pending()?.is_empty());
    assert!(service.shutdown().is_clean());
    Ok(())
}

#[test]
fn identity_names_the_run_the_files_and_the_settings() -> TestResult {
    let rig = Rig::new();
    let mut service = rig.start(rig.config(), rig.image());
    send_command(&service, "d1", r#""command":"dump""#);
    let result = wait_result(&mut service, "d1");
    let folder = PathBuf::from(result["dump"]["folder"].as_str().unwrap());
    assert!(folder.ends_with("dump-001"), "{}", folder.display());
    let identity = read_json(&folder.join("identity.json"));
    assert_eq!(identity["result"], "ok");
    assert_eq!(identity["pid"], 4321);
    assert_eq!(identity["tool"], "ufathom");
    assert_eq!(
        identity["game"]["path"],
        rig.game.display().to_string().as_str()
    );
    // SHA-256 of "not really an executable".
    let game_hash = identity["game"]["sha256"].as_str().unwrap();
    assert_eq!(game_hash.len(), 64);
    assert!(identity["dll"]["sha256"].is_string());
    assert!(identity["captured_at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(
        identity["engine"]["claimed"].as_str(),
        rig.fixture.engine.claimed.as_deref()
    );
    assert!(identity["discovery"]["layout"].is_object());
    assert!(
        !identity["discovery"]["evidence"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert_eq!(identity["config"]["threads"], 2);
    assert_eq!(identity["config_source"], "defaults");
    assert!(identity["timings_ms"]["total"].is_u64());
    let files: Vec<_> = identity["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f.as_str().unwrap().to_owned())
        .collect();
    for expected in [
        "dump.json",
        "Mappings.usmap",
        "ufathom.log",
        "identity.json",
    ] {
        assert!(files.iter().any(|f| f == expected), "{files:?}");
    }
    let build = folder
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .file_name()
        .unwrap();
    assert!(
        build.to_string_lossy().ends_with(&game_hash[..8]),
        "the build id must carry the short hash of the executable"
    );
    let log = std::fs::read_to_string(folder.join("ufathom.log"))?;
    assert!(log.contains("dump finished"), "{log}");
    assert!(service.shutdown().is_clean());
    Ok(())
}

#[test]
fn a_repeated_dump_gives_an_identical_dump_in_a_new_folder() -> TestResult {
    let rig = Rig::new();
    let mut service = rig.start(rig.config(), rig.image());
    let mut folders = Vec::new();
    for id in ["a", "b"] {
        send_command(&service, id, r#""command":"dump","label":"same""#);
        let result = wait_result(&mut service, id);
        assert_eq!(result["status"], "ok", "{result}");
        folders.push(PathBuf::from(result["dump"]["folder"].as_str().unwrap()));
    }
    assert_ne!(folders[0], folders[1]);
    assert_eq!(folders[0].file_name().unwrap(), "same");
    assert_eq!(folders[1].file_name().unwrap(), "same-2");
    assert_eq!(
        std::fs::read(folders[0].join("dump.json"))?,
        std::fs::read(folders[1].join("dump.json"))?,
        "two dumps of one image must be byte-identical"
    );
    assert_eq!(
        std::fs::read(folders[0].join("Mappings.usmap"))?,
        std::fs::read(folders[1].join("Mappings.usmap"))?
    );
    assert_eq!(status(&service)["dumps_completed"], 2);
    assert!(service.shutdown().is_clean());
    Ok(())
}

#[test]
fn formats_can_be_chosen_per_dump() -> TestResult {
    let rig = Rig::new();
    let mut service = rig.start(rig.config(), rig.image());
    send_command(&service, "j", r#""command":"dump","formats":["json"]"#);
    let result = wait_result(&mut service, "j");
    let folder = PathBuf::from(result["dump"]["folder"].as_str().unwrap());
    assert!(folder.join("dump.json").is_file());
    assert!(!folder.join("Mappings.usmap").exists());
    assert!(folder.join("identity.json").is_file());
    assert!(service.shutdown().is_clean());
    Ok(())
}

#[test]
fn a_dump_while_dumping_is_busy_and_does_not_disturb_the_first() -> TestResult {
    let rig = Rig::new();
    let gate = GatedReader::new(rig.image(), false);
    let mut service = rig.start(rig.config(), gate.clone());
    send_command(&service, "first", r#""command":"dump","label":"one""#);
    poll_until(&mut service, "the dump to start", |s| {
        s.state() == State::Dumping
    });
    gate.wait_for_a_read();
    assert_eq!(status(&service)["state"], "dumping");
    assert_eq!(status(&service)["current"]["label"], "one");

    send_command(&service, "second", r#""command":"dump","label":"two""#);
    let busy = wait_result(&mut service, "second");
    assert_eq!(busy["status"], "busy", "{busy}");
    assert_eq!(busy["state"], "dumping");
    assert!(!result_path(&service, "first").exists());

    gate.set_open(true);
    let first = wait_result(&mut service, "first");
    assert_eq!(first["status"], "ok", "{first}");
    assert_eq!(service.state(), State::Idle);
    assert_eq!(count_dirs(&rig.output.join("control").join("inbox")), 0);
    assert!(service.shutdown().is_clean());
    Ok(())
}

#[test]
fn stop_cancels_a_running_dump_then_shutdown_publishes_stopped() -> TestResult {
    let rig = Rig::new();
    let gate = GatedReader::new(rig.image(), false);
    let mut service = rig.start(rig.config(), gate.clone());
    send_command(&service, "run", r#""command":"dump""#);
    poll_until(&mut service, "the dump to start", |s| {
        s.state() == State::Dumping
    });
    gate.wait_for_a_read();

    send_command(&service, "halt", r#""command":"stop""#);
    let stop = wait_result(&mut service, "halt");
    assert_eq!(stop["status"], "ok");
    assert_eq!(service.state(), State::Stopping);
    assert_eq!(status(&service)["state"], "stopping");
    assert!(!service.should_unload(), "the dump is still running");

    // New commands are refused while stopping.
    send_command(&service, "late", r#""command":"dump""#);
    let late = wait_result(&mut service, "late");
    assert_eq!(late["status"], "error", "{late}");
    assert!(late["message"].as_str().unwrap().contains("stopping"));

    gate.set_open(true);
    poll_until(&mut service, "the cancelled dump to be collected", |s| {
        s.should_unload()
    });
    let run = read_json(&result_path(&service, "run"));
    assert_eq!(run["status"], "cancelled", "{run}");
    assert_eq!(
        std::fs::read_dir(&rig.output)?
            .filter_map(Result::ok)
            .filter(|e| e.file_name() != "control")
            .count(),
        0,
        "a cancelled dump must write no dump folder"
    );

    let status_path = service.control().status_path();
    let report = service.shutdown();
    assert!(report.is_clean(), "{report:?}");
    assert_eq!(report.pool_threads_joined, 2);
    assert_eq!(read_json(&status_path)["state"], "stopped");
    Ok(())
}

#[test]
fn stop_with_nothing_running_can_unload_at_once() -> TestResult {
    let rig = Rig::new();
    let mut service = rig.start(rig.config(), rig.image());
    send_command(&service, "halt", r#""command":"stop""#);
    let stop = wait_result(&mut service, "halt");
    assert_eq!(stop["status"], "ok");
    assert!(service.should_unload());
    let status_path = service.control().status_path();
    assert!(service.shutdown().is_clean());
    assert_eq!(read_json(&status_path)["state"], "stopped");
    Ok(())
}

#[test]
fn shutdown_cancels_a_dump_that_is_still_running() -> TestResult {
    let rig = Rig::new();
    let gate = GatedReader::new(rig.image(), false);
    let mut service = rig.start(rig.config(), gate.clone());
    send_command(&service, "run", r#""command":"dump""#);
    poll_until(&mut service, "the dump to start", |s| {
        s.state() == State::Dumping
    });
    gate.wait_for_a_read();
    let opener = {
        let gate = gate.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            gate.set_open(true);
        })
    };
    let report = service.shutdown();
    opener.join().unwrap();
    assert!(report.is_clean(), "{report:?}");
    let run = read_json(&rig.output.join("control/outbox/run.json"));
    assert_eq!(run["status"], "cancelled", "{run}");
    Ok(())
}

#[test]
fn garbage_commands_get_error_results_and_are_archived() -> TestResult {
    let rig = Rig::new();
    let mut service = rig.start(rig.config(), rig.image());
    send(&service, "bad-json.json", "{ this is not json");
    send(&service, "array.json", "[1, 2, 3]");
    send(&service, "no-id.json", r#"{"command":"status"}"#);
    send(&service, "empty.json", "");
    send_command(&service, "weird", r#""command":"explode""#);
    send_command(&service, "badopts", r#""command":"dump","formats":["xml"]"#);
    send(
        &service,
        "bad-id.json",
        r#"{"id":"../x","command":"status"}"#,
    );
    let wait_for = [
        "bad-json", "array", "no-id", "empty", "weird", "badopts", "bad-id",
    ];
    for name in wait_for {
        let path = result_path(&service, name);
        poll_until(&mut service, name, |_| path.exists());
        let result = read_json(&path);
        assert_eq!(result["status"], "error", "{name}: {result}");
        assert!(result["message"].is_string(), "{name}");
    }
    assert_eq!(read_json(&result_path(&service, "weird"))["id"], "weird");
    assert!(read_json(&result_path(&service, "bad-json"))["id"].is_null());
    assert!(service.control().pending()?.is_empty());
    assert_eq!(std::fs::read_dir(service.control().archive())?.count(), 7);
    assert_eq!(
        service.state(),
        State::Ready,
        "garbage must not change state"
    );
    assert!(service.shutdown().is_clean());
    Ok(())
}

#[test]
fn an_oversized_command_file_is_refused() -> TestResult {
    let rig = Rig::new();
    let mut service = rig.start(rig.config(), rig.image());
    let huge = format!(
        r#"{{"id":"big","command":"status","pad":"{}"}}"#,
        "x".repeat(70_000)
    );
    send(&service, "big.json", &huge);
    let result = wait_result(&mut service, "big");
    assert_eq!(result["status"], "error");
    assert!(service.shutdown().is_clean());
    Ok(())
}

#[test]
fn a_half_written_command_is_waited_for_not_rejected() -> TestResult {
    let rig = Rig::new();
    let mut service = rig.start(rig.config(), rig.image());
    let path = service.control().inbox().join("slow.json");
    std::fs::write(&path, r#"{"id":"slow","comma"#)?;
    for _ in 0..5 {
        service.poll();
        std::thread::sleep(Duration::from_millis(30));
    }
    assert!(
        !result_path(&service, "slow").exists(),
        "rejected too early"
    );
    std::fs::write(&path, r#"{"id":"slow","command":"status"}"#)?;
    let result = wait_result(&mut service, "slow");
    assert_eq!(result["status"], "ok", "{result}");
    assert!(service.shutdown().is_clean());
    Ok(())
}

#[test]
fn status_reports_the_service() -> TestResult {
    let rig = Rig::new();
    let mut service = rig.start(rig.config(), rig.image());
    send_command(&service, "s", r#""command":"status""#);
    let result = wait_result(&mut service, "s");
    assert_eq!(result["status"], "ok");
    assert_eq!(result["status_document"]["state"], "ready");
    assert_eq!(result["status_document"]["pid"], 4321);
    assert!(service.shutdown().is_clean());
    Ok(())
}

#[test]
fn a_target_that_cannot_be_solved_fails_with_a_named_error_and_a_record() -> TestResult {
    let rig = Rig::new();
    let empty: Arc<dyn MemoryReader> = Arc::new(Snapshot::default());
    let mut service = rig.start(rig.config(), empty);
    send_command(&service, "d", r#""command":"dump","label":"broken""#);
    let result = wait_result(&mut service, "d");
    assert_eq!(result["status"], "error", "{result}");
    assert_eq!(result["failure"]["kind"], "discovery");
    let folder = PathBuf::from(result["failure"]["folder"].as_str().unwrap());
    let identity = read_json(&folder.join("identity.json"));
    assert_eq!(identity["result"], "failed");
    assert!(identity["error"].is_string());
    assert!(folder.join("ufathom.log").is_file());
    assert!(!folder.join("dump.json").exists());
    assert_eq!(
        service.state(),
        State::Idle,
        "a failed dump must not stop the service"
    );
    assert_eq!(status(&service)["dumps_failed"], 1);
    assert!(service.shutdown().is_clean());
    Ok(())
}

#[test]
fn a_panic_in_the_reader_is_contained() -> TestResult {
    let rig = Rig::new();
    let mut service = rig.start(rig.config(), Arc::new(PanickingReader));
    send_command(&service, "d", r#""command":"dump""#);
    let result = wait_result(&mut service, "d");
    assert_eq!(result["status"], "error", "{result}");
    assert!(!result["failure"]["kind"].is_null());
    // The service still works afterwards.
    send_command(&service, "s", r#""command":"status""#);
    assert_eq!(wait_result(&mut service, "s")["status"], "ok");
    assert_eq!(service.state(), State::Idle);
    assert!(service.shutdown().is_clean());
    Ok(())
}

#[test]
fn auto_dump_runs_once_after_its_delay_without_any_command() -> TestResult {
    let rig = Rig::new();
    let mut config = rig.config();
    config.auto_dump = true;
    config.trigger_delay_seconds = 0.4;
    let mut service = rig.start(config, rig.image());
    let started = Instant::now();
    poll_until(&mut service, "the automatic dump", |s| {
        s.state() == State::Idle
    });
    assert!(
        started.elapsed() >= Duration::from_millis(350),
        "the dump ran before its delay"
    );
    let doc = status(&service);
    assert_eq!(doc["dumps_completed"], 1);
    assert_eq!(doc["last_dump"]["label"], "auto");
    assert_eq!(doc["last_dump"]["result"], "ok");
    // It does not repeat.
    for _ in 0..20 {
        service.poll();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(status(&service)["dumps_completed"], 1);
    assert!(!service.should_unload());
    assert!(service.shutdown().is_clean());
    Ok(())
}

#[test]
fn auto_dump_is_off_by_default() -> TestResult {
    let rig = Rig::new();
    let mut config = rig.config();
    config.trigger_delay_seconds = 0.0;
    let mut service = rig.start(config, rig.image());
    for _ in 0..20 {
        service.poll();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(service.state(), State::Ready);
    assert!(service.shutdown().is_clean());
    Ok(())
}

#[test]
fn auto_unload_asks_to_unload_after_the_automatic_dump() -> TestResult {
    let rig = Rig::new();
    let mut config = rig.config();
    config.auto_dump = true;
    config.auto_unload = true;
    config.trigger_delay_seconds = 0.0;
    let mut service = rig.start(config, rig.image());
    poll_until(&mut service, "auto unload", |s| s.should_unload());
    assert_eq!(status(&service)["last_dump"]["result"], "ok");
    assert!(service.shutdown().is_clean());
    Ok(())
}

#[test]
fn a_hotkey_dump_and_stop_use_the_same_state_machine() -> TestResult {
    let rig = Rig::new();
    let gate = GatedReader::new(rig.image(), false);
    let mut service = rig.start(rig.config(), gate.clone());
    assert_eq!(service.trigger_dump(), ufathom_agent::DumpTrigger::Started);
    assert_eq!(service.trigger_dump(), ufathom_agent::DumpTrigger::Busy);
    gate.wait_for_a_read();
    service.trigger_stop();
    assert_eq!(service.state(), State::Stopping);
    assert_eq!(service.trigger_dump(), ufathom_agent::DumpTrigger::Stopping);
    gate.set_open(true);
    poll_until(&mut service, "the cancelled dump", |s| s.should_unload());
    assert!(service.shutdown().is_clean());
    Ok(())
}

#[test]
fn the_thread_hook_runs_on_every_thread_the_session_starts() -> TestResult {
    let rig = Rig::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let mut environment = rig.environment(rig.image());
    let counter = Arc::clone(&calls);
    environment.thread_start = Some(Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    }));
    let mut service = Service::start(rig.config(), environment)?;
    // The pool's threads are started with the session; the dump thread with the dump.
    let started = Instant::now();
    while calls.load(Ordering::SeqCst) < 2 {
        assert!(
            started.elapsed() < WAIT,
            "the pool threads never ran the hook"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    send_command(&service, "d", r#""command":"dump""#);
    wait_result(&mut service, "d");
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    assert!(service.shutdown().is_clean());
    Ok(())
}

#[test]
fn a_missing_game_file_is_a_warning_and_the_dump_still_works() -> TestResult {
    let rig = Rig::new();
    std::fs::remove_file(&rig.game)?;
    let mut service = rig.start(rig.config(), rig.image());
    send_command(&service, "d", r#""command":"dump""#);
    let result = wait_result(&mut service, "d");
    assert_eq!(result["status"], "ok", "{result}");
    let folder = PathBuf::from(result["dump"]["folder"].as_str().unwrap());
    let identity = read_json(&folder.join("identity.json"));
    assert!(identity["game"]["sha256"].is_null());
    assert!(identity["game"]["hash_error"].is_string());
    let build = folder
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .file_name()
        .unwrap();
    assert!(build.to_string_lossy().ends_with("-nohash"), "{build:?}");
    assert!(service.shutdown().is_clean());
    Ok(())
}

#[cfg(windows)]
#[test]
fn an_output_path_over_260_characters_works() -> TestResult {
    let rig = Rig::new();
    let mut config = rig.config();
    let mut output = rig.dir.path().to_owned();
    while output.as_os_str().len() < 300 {
        output.push("a-very-long-folder-name-to-pass-the-limit");
    }
    config.output = output.clone();
    let mut service = rig.start(config, rig.image());
    send_command(&service, "d", r#""command":"dump""#);
    let result = wait_result(&mut service, "d");
    assert_eq!(result["status"], "ok", "{result}");
    let folder = PathBuf::from(result["dump"]["folder"].as_str().unwrap());
    assert!(folder.as_os_str().len() > 260);
    let dump = fsio::read_text_limited(&folder.join("dump.json"), 1 << 28)?;
    assert!(Dump::from_json(&dump).is_ok());
    assert!(service.shutdown().is_clean());
    Ok(())
}

#[test]
fn defaults_are_what_the_architecture_says() {
    let config = Config::defaults(Path::new("game"));
    assert!(!config.auto_dump);
    assert_eq!(config.formats, vec![Format::Json, Format::Usmap]);
}
