//! Shared helpers: run the built `ufathom` binary, build fixtures and dumps,
//! and host a synthetic engine in a `fake-host` child process.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test helpers may panic on failure; each test binary uses a different part"
)]

use std::path::{Path, PathBuf};
use std::process::Command;

use ufathom_model::Dump;
use ufathom_reflect::{ReflectOptions, build_dump};
use ufathom_scan::{DiscoverOptions, discover};
use ufathom_testkit::{Fixture, FixtureBuilder, Profile};

/// What one run of the binary did.
#[derive(Debug)]
pub struct Run {
    /// The exit code. A process killed by a signal has none, and fails the test.
    pub code: i32,
    /// Standard output.
    pub stdout: String,
    /// Standard error.
    pub stderr: String,
}

impl Run {
    /// Panics with the whole run unless the exit code is `code`.
    pub fn expect_code(self, code: i32) -> Self {
        assert_eq!(self.code, code, "{self:#?}");
        self
    }

    /// Panics unless `text` is in standard output.
    pub fn stdout_has(&self, text: &str) -> &Self {
        assert!(self.stdout.contains(text), "no {text:?} in {self:#?}");
        self
    }

    /// Panics unless `text` is in standard error.
    pub fn stderr_has(&self, text: &str) -> &Self {
        assert!(self.stderr.contains(text), "no {text:?} in {self:#?}");
        self
    }

    /// Standard output as JSON.
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.stdout)
            .unwrap_or_else(|error| panic!("stdout is not JSON ({error}): {self:#?}"))
    }
}

/// Runs the built binary with `args`.
pub fn run<S: AsRef<std::ffi::OsStr>>(args: &[S]) -> Run {
    let output = Command::new(env!("CARGO_BIN_EXE_ufathom"))
        .args(args)
        .output()
        .expect("run ufathom");
    Run {
        code: output.status.code().expect("ufathom exited normally"),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// Runs the built binary with `args` and one string of standard input.
pub fn run_with_input<S: AsRef<std::ffi::OsStr>>(args: &[S], input: &str) -> Run {
    let mut child = Command::new(env!("CARGO_BIN_EXE_ufathom"))
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn ufathom");
    let mut stdin = child.stdin.take().expect("stdin is piped");
    std::io::Write::write_all(&mut stdin, input.as_bytes()).expect("write the selection");
    drop(stdin);
    let output = child.wait_with_output().expect("wait for ufathom");
    Run {
        code: output.status.code().expect("ufathom exited normally"),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// A path as the text a command line carries.
pub fn arg(path: &Path) -> String {
    path.to_str().expect("test paths are UTF-8").to_owned()
}

/// A synthetic image of a profile, seeded so it is the same every time.
pub fn fixture(profile: &str, seed: u64, fillers: usize) -> Fixture {
    FixtureBuilder::new(Profile::by_name(profile).expect("a known profile"))
        .seed(seed)
        .filler_objects(fillers)
        .build()
}

/// The dump of a fixture, made by the library.
pub fn dump_of(fixture: &Fixture) -> Dump {
    let discovery = discover(&fixture.memory, &DiscoverOptions::default()).expect("discovery");
    build_dump(&fixture.memory, &discovery, &ReflectOptions::default())
        .expect("a dump")
        .dump
}

/// Writes the dump of a fixture to `path` as `dump.json` text.
pub fn write_dump(fixture: &Fixture, path: &Path) -> Dump {
    let dump = dump_of(fixture);
    std::fs::write(path, dump.to_json().expect("JSON")).expect("write the dump");
    dump
}

/// The `crates/ufathom-runtime` folder of this workspace.
pub fn runtime_crate() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../ufathom-runtime")
}

#[cfg(windows)]
#[allow(
    unused_imports,
    reason = "each test binary uses a different part of the helpers"
)]
pub use host::{Host, fake_host_path};

#[cfg(windows)]
mod host {
    use std::io::{BufRead, BufReader};
    use std::path::PathBuf;
    use std::process::{Child, ChildStdin, Command, Stdio};
    use std::sync::{OnceLock, mpsc};
    use std::time::{Duration, Instant};

    use ufathom_testkit::{
        Corruption, Fixture, FixtureBuilder, HostedPlacement, Placement, Profile,
    };

    const LINE_TIMEOUT: Duration = Duration::from_secs(60);
    const EXIT_TIMEOUT: Duration = Duration::from_secs(30);

    /// The built `fake-host` executable, building it when only the CLI has
    /// been built.
    pub fn fake_host_path() -> PathBuf {
        static PATH: OnceLock<PathBuf> = OnceLock::new();
        PATH.get_or_init(|| {
            // target/<profile>/deps/<test>.exe
            let exe = std::env::current_exe().unwrap();
            let profile_dir = exe.parent().and_then(|deps| deps.parent()).unwrap();
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

    /// A running synthetic engine. The child is killed and reaped on drop,
    /// even if the test panics.
    pub struct Host {
        child: Child,
        stdin: Option<ChildStdin>,
        json: serde_json::Value,
        profile: String,
        seed: u64,
        filler: usize,
        corruptions: Vec<Corruption>,
    }

    impl Host {
        /// Starts a host of `profile` with `filler` filler objects.
        pub fn spawn(profile: &str, seed: u64, filler: usize) -> Self {
            Self::spawn_with(profile, seed, filler, &[])
        }

        /// Starts a host whose image has the given damage.
        pub fn spawn_with(
            profile: &str,
            seed: u64,
            filler: usize,
            corruptions: &[Corruption],
        ) -> Self {
            let mut command = Command::new(fake_host_path());
            command
                .args(["--profile", profile])
                .args(["--seed", &seed.to_string()])
                .args(["--filler", &filler.to_string()]);
            for corruption in corruptions {
                command.args(["--corrupt", &format!("{corruption:?}")]);
            }
            let mut child = command
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
                profile: profile.to_owned(),
                seed,
                filler,
                corruptions: corruptions.to_vec(),
            };
            let line = rx
                .recv_timeout(LINE_TIMEOUT)
                .expect("fake-host printed its line in time");
            host.json = serde_json::from_str(&line).expect("one JSON line");
            host
        }

        /// The process id of the host.
        pub fn pid(&self) -> u32 {
            u32::try_from(self.u64(&["pid"])).unwrap()
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

        /// The same image built locally, from the addresses the host
        /// reported: what its dump must equal.
        pub fn local(&self) -> Fixture {
            let placement = HostedPlacement {
                module_name: self.json["module"].as_str().unwrap().to_owned(),
                module_base: self.u64(&["module_base"]),
                text: self.u64(&["addresses", "text"]),
                rdata: self.u64(&["addresses", "rdata"]),
                data: self.u64(&["addresses", "data"]),
                heap: self.u64(&["addresses", "heap"]),
            };
            let mut builder = FixtureBuilder::new(Profile::by_name(&self.profile).unwrap())
                .seed(self.seed)
                .filler_objects(self.filler)
                .placement(Placement::Hosted(placement));
            for &corruption in &self.corruptions {
                builder = builder.corrupt(corruption);
            }
            builder.build()
        }

        /// Closes stdin and waits for the child to exit by itself.
        pub fn close_and_wait(&mut self) -> bool {
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
}
