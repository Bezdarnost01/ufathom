//! Shared helpers for the DLL tests: locate the built DLL and `fake-host`, set
//! up a sandbox with its own copy of the DLL and a config, drive a host
//! through its stdin and the DLL through its command files.
//!
//! Every wait has a timeout, and a host is killed and reaped when its handle is
//! dropped, even if the test panics.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test helpers may panic on failure; each test binary uses a different part"
)]

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant, SystemTime};

use serde_json::Value;
use ufathom_agent::paths::extended_path;
use ufathom_model::Dump;
use ufathom_testkit::{Corruption, Fixture, FixtureBuilder, HostedPlacement, Placement, Profile};

/// Time allowed for the host to print its first line.
pub const START_TIMEOUT: Duration = Duration::from_secs(60);
/// Time allowed for a command to be answered when no dump is involved.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
/// Time allowed for a dump to finish.
pub const DUMP_TIMEOUT: Duration = Duration::from_secs(180);
/// Time allowed for the DLL to unload.
pub const UNLOAD_TIMEOUT: Duration = Duration::from_secs(30);
/// How often waits look again.
const POLL: Duration = Duration::from_millis(50);

/// The `target` directory of the build that is running these tests.
fn target_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("CARGO_TARGET_DIR") {
        return PathBuf::from(dir);
    }
    // target/<profile>/deps/<test>.exe
    let exe = std::env::current_exe().unwrap();
    exe.parent()
        .and_then(Path::parent)
        .and_then(Path::parent)
        .unwrap()
        .to_owned()
}

/// File name `cargo build -p ufathom-dll` gives the DLL. The library is not
/// called `ufathom`, so its PDB does not clash with the CLI binary's; the
/// sandboxes copy it as `ufathom.dll`, the name it is shipped under.
pub const BUILT_DLL_NAME: &str = "ufathom_dll.dll";

/// The built DLL. `UFATHOM_DLL` names one explicitly; otherwise the
/// newer of the release and debug builds is used. Fails with an instruction if
/// there is none, because these tests must test a real DLL.
pub fn built_dll() -> PathBuf {
    if let Some(path) = std::env::var_os("UFATHOM_DLL") {
        let path = PathBuf::from(path);
        assert!(
            path.is_file(),
            "UFATHOM_DLL={} does not exist",
            path.display()
        );
        return path;
    }
    let target = target_dir();
    let candidates: Vec<PathBuf> = ["release", "debug"]
        .iter()
        .map(|profile| target.join(profile).join(BUILT_DLL_NAME))
        .filter(|path| path.is_file())
        .collect();
    let newest = candidates
        .into_iter()
        .max_by_key(|path| fs::metadata(path).and_then(|m| m.modified()).ok());
    newest.unwrap_or_else(|| {
        panic!(
            "{BUILT_DLL_NAME} is not built under {}: run `cargo build -p ufathom-dll` (or with --release) first, or set UFATHOM_DLL",
            target.display()
        )
    })
}

/// A fixture size whose dump takes long enough to still be running when a
/// second command is read.
pub const LARGE_FILLER: usize = 60_000;

/// The built `fake-host.exe`, building it when only the library of the testkit
/// has been built.
pub fn fake_host_path() -> PathBuf {
    static PATH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    PATH.get_or_init(|| {
        let exe = std::env::current_exe().unwrap();
        let profile_dir = exe.parent().and_then(Path::parent).unwrap().to_owned();
        let path = profile_dir.join("fake-host.exe");
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
    })
    .clone()
}

/// A folder with its own copy of the DLL and a config beside it.
pub struct Sandbox {
    _dir: tempfile::TempDir,
    /// The copy of the DLL.
    pub dll: PathBuf,
    /// The output folder the config names.
    pub output: PathBuf,
}

impl Sandbox {
    /// A sandbox with the DLL copied as `ufathom.dll` and the default output
    /// folder. `extra` is added to the config.
    pub fn new(extra: &str) -> Self {
        Self::build("ufathom.dll", None, extra)
    }

    /// A sandbox with the DLL copied under `dll_name`.
    pub fn named(dll_name: &str, extra: &str) -> Self {
        Self::build(dll_name, None, extra)
    }

    /// A sandbox whose config names this output folder.
    pub fn with_output(output: PathBuf, extra: &str) -> Self {
        Self::build("ufathom.dll", Some(output), extra)
    }

    fn build(dll_name: &str, output: Option<PathBuf>, extra: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let dll = dir.path().join(dll_name);
        fs::copy(built_dll(), &dll).unwrap();
        let output = output.unwrap_or_else(|| dir.path().join("out"));
        let config = format!("output = '{}'\n{extra}\n", output.display());
        fs::write(dir.path().join("ufathom.toml"), config).unwrap();
        Self {
            _dir: dir,
            dll,
            output,
        }
    }

    /// A copy of `fake-host.exe` inside the sandbox, so a test can run a host
    /// whose executable folder it owns. Returns its path.
    pub fn copy_host_exe(&self) -> PathBuf {
        let folder = self._dir.path().join("game");
        fs::create_dir_all(&folder).unwrap();
        let exe = folder.join("Game-Win64-Shipping.exe");
        fs::copy(fake_host_path(), &exe).unwrap();
        exe
    }

    /// The control folder under the output folder.
    pub fn control(&self) -> Control {
        Control {
            root: self.output.join("control"),
        }
    }
}

/// The command files of one DLL instance.
pub struct Control {
    root: PathBuf,
}

impl Control {
    /// The control folder at `root`.
    pub fn at(root: PathBuf) -> Self {
        Self { root }
    }

    fn read_json(path: &Path) -> Option<Value> {
        let bytes = fs::read(extended_path(path)).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Writes a command file atomically (a temporary name, then a rename).
    pub fn send_raw(&self, file_name: &str, text: &str) {
        let inbox = self.root.join("inbox");
        fs::create_dir_all(extended_path(&inbox)).unwrap();
        let temp = inbox.join(format!(".{file_name}.tmp"));
        fs::write(extended_path(&temp), text).unwrap();
        fs::rename(extended_path(&temp), extended_path(&inbox.join(file_name))).unwrap();
    }

    /// Sends `{ "id": id, "command": command, ...fields }`, where `fields` is
    /// the text of zero or more `"key": value` pairs after a comma.
    pub fn send(&self, id: &str, command: &str, fields: &str) {
        let tail = if fields.is_empty() {
            String::new()
        } else {
            format!(",{fields}")
        };
        self.send_raw(
            &format!("{id}.json"),
            &format!(r#"{{"id":"{id}","command":"{command}"{tail}}}"#),
        );
    }

    /// Publishes several commands as close together as possible: every file is
    /// staged under a temporary name first, then all are renamed into place, so
    /// the DLL reads them in one poll. Each item is `(id, command, fields)` as
    /// for [`send`](Self::send).
    pub fn send_together(&self, commands: &[(&str, &str, &str)]) {
        let inbox = self.root.join("inbox");
        fs::create_dir_all(extended_path(&inbox)).unwrap();
        let mut staged = Vec::new();
        for (id, command, fields) in commands {
            let tail = if fields.is_empty() {
                String::new()
            } else {
                format!(",{fields}")
            };
            let temp = inbox.join(format!(".{id}.json.tmp"));
            fs::write(
                extended_path(&temp),
                format!(r#"{{"id":"{id}","command":"{command}"{tail}}}"#),
            )
            .unwrap();
            staged.push((temp, inbox.join(format!("{id}.json"))));
        }
        for (temp, target) in staged {
            fs::rename(extended_path(&temp), extended_path(&target)).unwrap();
        }
    }

    /// The result of command `id`, once it is published.
    pub fn wait_result(&self, id: &str, timeout: Duration) -> Value {
        let path = self.root.join("outbox").join(format!("{id}.json"));
        wait_for(&format!("the result of `{id}`"), timeout, || {
            Self::read_json(&path)
        })
    }

    /// The status document, if it exists and parses.
    pub fn status(&self) -> Option<Value> {
        Self::read_json(&self.root.join("status.json"))
    }

    /// The status document once its state is `state`.
    pub fn wait_state(&self, state: &str, timeout: Duration) -> Value {
        wait_for(&format!("state `{state}`"), timeout, || {
            self.status().filter(|doc| doc["state"] == state)
        })
    }

    /// The names of the files in the archive folder.
    pub fn archived(&self) -> Vec<String> {
        fs::read_dir(extended_path(&self.root.join("archive")))
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Calls `probe` until it returns `Some` or `timeout` passes, then panics.
pub fn wait_for<T>(what: &str, timeout: Duration, mut probe: impl FnMut() -> Option<T>) -> T {
    let start = Instant::now();
    loop {
        if let Some(found) = probe() {
            return found;
        }
        assert!(
            start.elapsed() < timeout,
            "timed out after {timeout:?} waiting for {what}"
        );
        std::thread::sleep(POLL);
    }
}

/// How a host is built: the image it hosts.
#[derive(Clone)]
pub struct HostSpec {
    pub profile: &'static str,
    pub seed: u64,
    pub filler: usize,
    pub corruptions: Vec<Corruption>,
    /// The host executable to run. `None` runs the built `fake-host.exe` in place.
    pub exe: Option<PathBuf>,
}

impl HostSpec {
    /// The default image: a case-preserving UE 4.25 build with some filler.
    pub fn new(filler: usize) -> Self {
        Self {
            profile: "ue4.25-cp",
            seed: 5,
            filler,
            corruptions: Vec::new(),
            exe: None,
        }
    }
}

/// What a `report` line of the host says.
#[derive(Debug, Clone)]
pub struct Report {
    pub worst_gap_ms: f64,
    pub ticks: u64,
    pub threads: usize,
    pub threads_before_load: usize,
    /// Threads that did not exist before the first load: id and the file name
    /// of the module holding their start address (`None` if it is in no mapped
    /// module).
    pub new_threads: Vec<(u64, Option<String>)>,
    /// `(file path, still mapped)` of every DLL the host loaded.
    pub dlls: Vec<(String, bool)>,
}

impl Report {
    fn parse(line: &Value) -> Option<Self> {
        let report = line.get("report")?;
        Some(Self {
            worst_gap_ms: report["worst_gap_ms"].as_f64()?,
            ticks: report["ticks"].as_u64()?,
            threads: usize::try_from(report["threads"].as_u64()?).ok()?,
            threads_before_load: usize::try_from(report["threads_before_load"].as_u64()?).ok()?,
            new_threads: report["new_threads"]
                .as_array()?
                .iter()
                .map(|thread| {
                    Some((
                        thread["id"].as_u64()?,
                        thread["module"].as_str().map(str::to_owned),
                    ))
                })
                .collect::<Option<_>>()?,
            dlls: report["dlls"]
                .as_array()?
                .iter()
                .map(|dll| Some((dll["path"].as_str()?.to_owned(), dll["loaded"].as_bool()?)))
                .collect::<Option<_>>()?,
        })
    }

    /// Whether the DLL at `path` is still mapped.
    pub fn is_loaded(&self, path: &Path) -> bool {
        let wanted = path.display().to_string();
        self.dlls
            .iter()
            .any(|(dll, loaded)| *loaded && *dll == wanted)
    }

    /// The threads that run, or ran, code of the DLL at `path`: their start
    /// address is inside that module, or inside no module at all, which after
    /// an unload means a thread is executing unmapped code.
    ///
    /// The threads the system's own loader starts on a `LoadLibrary` (they start
    /// in `ntdll.dll`) are not counted: they are not the DLL's and stay for the
    /// life of the process.
    pub fn dll_threads(&self, path: &Path) -> Vec<(u64, Option<String>)> {
        let name = path.file_name().unwrap().to_string_lossy().to_lowercase();
        self.new_threads
            .iter()
            .filter(|(_, module)| {
                module
                    .as_ref()
                    .is_none_or(|module| module.to_lowercase() == name)
            })
            .cloned()
            .collect()
    }
}

/// A running `fake-host`. Killed and reaped on drop.
pub struct Host {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    json: Value,
    spec: HostSpec,
}

impl Host {
    /// Starts a host with the DLLs in `loads` (each loaded in order) and waits
    /// for its first line.
    pub fn spawn(spec: &HostSpec, loads: &[&Path], report_stall: bool) -> Self {
        let mut command = Command::new(spec.exe.clone().unwrap_or_else(fake_host_path));
        command
            .args(["--profile", spec.profile])
            .args(["--seed", &spec.seed.to_string()])
            .args(["--filler", &spec.filler.to_string()]);
        for corruption in &spec.corruptions {
            command.args(["--corrupt", &format!("{corruption:?}")]);
        }
        for dll in loads {
            command.arg("--load").arg(dll);
        }
        if report_stall {
            command.arg("--report-stall");
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn fake-host");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("stdout is piped");
        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        let mut host = Self {
            child,
            stdin,
            lines,
            json: Value::Null,
            spec: spec.clone(),
        };
        let first = host
            .lines
            .recv_timeout(START_TIMEOUT)
            .expect("fake-host printed its first line in time");
        host.json = serde_json::from_str(&first).expect("the first line is JSON");
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

    /// The process id of the host.
    pub fn pid(&self) -> u32 {
        u32::try_from(self.u64(&["pid"])).unwrap()
    }

    /// The same image built locally from the addresses the host reported, with
    /// its truth.
    pub fn local_fixture(&self) -> Fixture {
        let placement = HostedPlacement {
            module_name: self.json["module"].as_str().unwrap().to_owned(),
            module_base: self.u64(&["module_base"]),
            text: self.u64(&["addresses", "text"]),
            rdata: self.u64(&["addresses", "rdata"]),
            data: self.u64(&["addresses", "data"]),
            heap: self.u64(&["addresses", "heap"]),
        };
        let mut builder = FixtureBuilder::new(Profile::by_name(self.spec.profile).unwrap())
            .seed(self.spec.seed)
            .filler_objects(self.spec.filler)
            .placement(Placement::Hosted(placement));
        for &corruption in &self.spec.corruptions {
            builder = builder.corrupt(corruption);
        }
        builder.build()
    }

    /// Whether the process is still running.
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Sends one line to the host's stdin.
    pub fn send_line(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().expect("stdin is open");
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
    }

    /// Waits for a host line that `matches`, skipping others.
    fn wait_line(&self, what: &str, timeout: Duration, matches: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + timeout;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self
                .lines
                .recv_timeout(left)
                .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
            if let Ok(value) = serde_json::from_str::<Value>(&line) {
                if matches(&value) {
                    return value;
                }
            }
        }
    }

    /// Asks the host to load one more DLL. Returns whether `LoadLibraryW`
    /// succeeded.
    pub fn load(&mut self, dll: &Path) -> bool {
        self.send_line(&format!("load {}", dll.display()));
        let line = self.wait_line("the load result", COMMAND_TIMEOUT, |v| {
            v.get("loaded").is_some()
        });
        line["loaded"]["ok"].as_bool().unwrap()
    }

    /// Asks for a report. It starts a new stall window.
    pub fn report(&mut self) -> Report {
        self.send_line("report");
        let line = self.wait_line("a report", COMMAND_TIMEOUT, |v| v.get("report").is_some());
        Report::parse(&line).expect("a well-formed report")
    }

    /// Waits until the DLL is no longer mapped and no thread that started in its
    /// code is left (see [`Report::dll_threads`]). Returns that report.
    pub fn wait_unloaded(&mut self, dll: &Path, timeout: Duration) -> Report {
        let start = Instant::now();
        loop {
            let report = self.report();
            if !report.is_loaded(dll) && report.dll_threads(dll).is_empty() {
                return report;
            }
            assert!(
                start.elapsed() < timeout,
                "the DLL did not unload cleanly within {timeout:?}: {report:?}"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Closes stdin, as a game exiting would, and reads the final line the host
    /// prints when it was started with `--report-stall`.
    pub fn close_and_read_final_report(&mut self) -> Option<Report> {
        drop(self.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let line = self.lines.recv_timeout(left).ok()?;
            if let Some(report) = serde_json::from_str::<Value>(&line)
                .ok()
                .and_then(|value| Report::parse(&value))
            {
                return Some(report);
            }
        }
    }

    /// Closes stdin and waits for the host to exit by itself.
    pub fn close_and_wait(&mut self) -> bool {
        drop(self.stdin.take());
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(30) {
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

/// Reads and parses a JSON file, through the extended path form.
pub fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(extended_path(path)).unwrap()).unwrap()
}

/// Compares every struct and enum of the dump in `folder` with the truth of
/// `fixture`. Returns the raw bytes of `dump.json`.
pub fn assert_dump_matches_truth(fixture: &Fixture, folder: &Path) -> Vec<u8> {
    let bytes = fs::read(extended_path(&folder.join("dump.json"))).unwrap();
    let dump = Dump::from_json(std::str::from_utf8(&bytes).unwrap()).unwrap();
    assert_eq!(
        dump.structs, fixture.truth.structs,
        "structs differ from the truth"
    );
    assert_eq!(
        dump.enums, fixture.truth.enums,
        "enums differ from the truth"
    );
    bytes
}

/// The folder a finished dump was written to, from its result.
pub fn dump_folder(result: &Value) -> PathBuf {
    PathBuf::from(
        result["dump"]["folder"]
            .as_str()
            .unwrap_or_else(|| panic!("a finished dump names its folder: {result}")),
    )
}

/// Seconds since the epoch, for unique-enough labels.
pub fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
