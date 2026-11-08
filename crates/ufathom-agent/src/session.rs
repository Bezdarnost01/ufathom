//! One load of the DLL: its thread pool and the dumps it runs.
//!
//! A [`Session`] owns a [`rayon::ThreadPool`] that it built itself, of at most
//! [`Config::threads`] threads. The global rayon pool is never used: its
//! threads cannot be stopped, which would block unloading. Every thread this
//! crate starts is kept as a [`JoinHandle`], so [`Session::shutdown`] can wait
//! for all of them before the DLL's code is unmapped.
//!
//! A dump reads the target only through the [`MemoryReader`] the shell put in
//! the [`Environment`], and writes only under the output folder.

use std::any::Any;
use std::fmt::Display;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime};

use rayon::{ThreadPool, ThreadPoolBuilder};
use tracing::Dispatch;
use ufathom_mem::{MemoryReader, PageCache};
use ufathom_model::Severity;
use ufathom_reflect::{ReflectError, ReflectOptions, build_dump};
use ufathom_scan::{DiscoverOptions, Discovery, discover};
use ufathom_usmap::write_usmap;

use crate::clock;
use crate::config::{Config, ConfigSource, Format};
use crate::error::{DumpError, SessionError};
use crate::fsio;
use crate::hashing::{HashError, sha256_file};
use crate::identity::{
    DiscoveryRecord, FileIdentity, IDENTITY_FORMAT_VERSION, Identity, ReflectRecord, TimingsMs,
    describe_source,
};
use crate::output::{
    DUMP_JSON, DUMP_LOG, IDENTITY_JSON, MAPPINGS_USMAP, build_id, create_dump_folder, dump_label,
    session_name,
};
use crate::protocol::DumpSummary;

/// A function run on every thread the session starts, before it does any
/// work. The DLL shell uses it to lower the thread's priority.
pub type ThreadHook = Arc<dyn Fn() + Send + Sync>;

/// The tool name written to identity records.
pub const TOOL_NAME: &str = "ufathom";

/// How often shutdown checks whether threads have finished.
const JOIN_POLL: Duration = Duration::from_millis(5);

/// Everything the session needs from the process it runs in.
#[derive(Clone)]
pub struct Environment {
    /// Full path of the game executable.
    pub game_exe: PathBuf,
    /// Full path of this DLL.
    pub dll_path: PathBuf,
    /// Process id of the host.
    pub pid: u32,
    /// The only way the dump reads the target. In the DLL this is
    /// `SelfMemory`.
    pub reader: Arc<dyn MemoryReader>,
    /// Run first on every thread the session starts.
    pub thread_start: Option<ThreadHook>,
    /// Where the config came from, for the identity record.
    pub config_source: ConfigSource,
}

/// The result of a dump that wrote its files.
#[derive(Debug, Clone)]
pub struct DumpOutcome {
    /// The label of the dump folder, as requested or generated.
    pub label: String,
    /// The folder that was created.
    pub folder: PathBuf,
    /// Counts and file names.
    pub summary: DumpSummary,
}

/// What [`Session::shutdown`] could and could not stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShutdownReport {
    /// Pool threads that ended and were joined.
    pub pool_threads_joined: usize,
    /// Pool threads that were still running at the deadline.
    pub pool_threads_left: usize,
    /// Dump threads that were still running at the deadline.
    pub dump_threads_left: usize,
}

impl ShutdownReport {
    /// Every thread this crate started has ended, so it is safe to unmap
    /// the code.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.pool_threads_left == 0 && self.dump_threads_left == 0
    }
}

/// Hashes of the two files, computed once per session.
#[derive(Default)]
struct Hashes {
    game: Option<String>,
    dll: Option<String>,
}

/// State shared between the session and its dump threads.
struct Shared {
    config: Config,
    environment: Environment,
    name: String,
    pool: ThreadPool,
    workers: Arc<Mutex<Vec<JoinHandle<()>>>>,
    hashes: Mutex<Hashes>,
    dump_counter: AtomicU64,
}

/// One load of the DLL. See the [module documentation](self).
pub struct Session {
    shared: Arc<Shared>,
}

/// A dump running on its own thread.
pub struct RunningDump {
    /// The label the dump folder will carry.
    pub label: String,
    /// When the dump started.
    pub started: SystemTime,
    cancel: Arc<AtomicBool>,
    handle: JoinHandle<Result<DumpOutcome, DumpError>>,
}

impl RunningDump {
    /// Asks the dump to stop. It ends between objects.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// The dump thread has ended and [`join`](Self::join) will not block.
    #[must_use]
    pub fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }

    /// Waits for the dump and returns its result.
    pub fn join(self) -> Result<DumpOutcome, DumpError> {
        self.handle.join().unwrap_or_else(|payload| {
            Err(DumpError::Panicked {
                message: panic_message(payload.as_ref()),
            })
        })
    }
}

impl Session {
    /// Creates a session with its own thread pool of `config.threads`
    /// threads.
    ///
    /// # Errors
    ///
    /// [`SessionError::Pool`] when the pool or one of its threads cannot be
    /// created.
    pub fn new(config: Config, environment: Environment) -> Result<Self, SessionError> {
        let started = SystemTime::now();
        let name = session_name(started, environment.pid);
        let workers: Arc<Mutex<Vec<JoinHandle<()>>>> = Arc::new(Mutex::new(Vec::new()));
        let pool = build_pool(config.threads, &environment, &workers)?;
        Ok(Self {
            shared: Arc::new(Shared {
                config,
                environment,
                name,
                pool,
                workers,
                hashes: Mutex::new(Hashes::default()),
                dump_counter: AtomicU64::new(0),
            }),
        })
    }

    /// The session folder name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.shared.name
    }

    /// The settings the session runs with.
    #[must_use]
    pub fn config(&self) -> &Config {
        &self.shared.config
    }

    /// The process the session runs in.
    #[must_use]
    pub fn environment(&self) -> &Environment {
        &self.shared.environment
    }

    /// Runs a dump on the calling thread and the session pool, and writes its
    /// files.
    ///
    /// `label` names the dump folder; `None` numbers it. `formats` defaults to
    /// the config's. `cancel` is checked between steps and between objects;
    /// once set the dump ends with [`DumpError::Cancelled`] and writes nothing.
    /// Panics inside the dump are contained and returned as
    /// [`DumpError::Panicked`].
    ///
    /// Each call reads the target through a new page cache, so a repeated dump
    /// never sees pages of an earlier one.
    ///
    /// # Errors
    ///
    /// [`DumpError`]. When discovery or reflection fails, a folder with
    /// `identity.json` and `ufathom.log` is still written and named in the
    /// error.
    pub fn run_dump(
        &self,
        label: Option<&str>,
        formats: Option<&[Format]>,
        cancel: &AtomicBool,
    ) -> Result<DumpOutcome, DumpError> {
        let label = self.shared.next_label(label);
        let formats =
            formats.map_or_else(|| self.shared.config.formats.clone(), <[Format]>::to_vec);
        self.shared.run_contained(&label, &formats, cancel)
    }

    /// Starts a dump on its own thread and returns at once.
    ///
    /// The thread runs the same dump as [`run_dump`](Self::run_dump) and its
    /// result is read with [`RunningDump::join`] once
    /// [`is_finished`](RunningDump::is_finished).
    ///
    /// # Errors
    ///
    /// The operating system's error when the thread cannot be created.
    pub fn spawn_dump(
        &self,
        label: Option<&str>,
        formats: Option<Vec<Format>>,
    ) -> std::io::Result<RunningDump> {
        let label = self.shared.next_label(label);
        let formats = formats.unwrap_or_else(|| self.shared.config.formats.clone());
        let cancel = Arc::new(AtomicBool::new(false));
        let shared = Arc::clone(&self.shared);
        let thread_cancel = Arc::clone(&cancel);
        let thread_label = label.clone();
        let dispatch = tracing::dispatcher::get_default(Dispatch::clone);
        let hook = self.shared.environment.thread_start.clone();
        let handle = std::thread::Builder::new()
            .name("ufathom-dump".to_owned())
            .spawn(move || {
                let _logging = tracing::dispatcher::set_default(&dispatch);
                if let Some(hook) = hook {
                    let _ = catch_unwind(AssertUnwindSafe(|| hook()));
                }
                shared.run_contained(&thread_label, &formats, &thread_cancel)
            })?;
        Ok(RunningDump {
            label,
            started: SystemTime::now(),
            cancel,
            handle,
        })
    }

    /// Drops the pool and waits for every thread this session started.
    ///
    /// Dump threads must have ended or been cancelled; they are waited for up
    /// to `timeout`. Then the pool is dropped, which tells its threads to
    /// exit, and they are joined within the same deadline. A thread that does
    /// not end in time is reported, not waited for: the caller must then keep
    /// the DLL loaded.
    #[must_use]
    pub fn shutdown(self, timeout: Duration) -> ShutdownReport {
        let deadline = Instant::now() + timeout;
        let mut shared = self.shared;
        loop {
            match Arc::try_unwrap(shared) {
                Ok(owned) => return owned.stop_pool(deadline),
                Err(still_shared) => {
                    shared = still_shared;
                    if Instant::now() >= deadline {
                        let workers = lock(&shared.workers).len();
                        return ShutdownReport {
                            pool_threads_joined: 0,
                            pool_threads_left: workers,
                            dump_threads_left: Arc::strong_count(&shared).saturating_sub(1),
                        };
                    }
                    std::thread::sleep(JOIN_POLL);
                }
            }
        }
    }
}

/// Builds the pool so every thread is started by us and kept as a handle.
fn build_pool(
    threads: usize,
    environment: &Environment,
    workers: &Arc<Mutex<Vec<JoinHandle<()>>>>,
) -> Result<ThreadPool, SessionError> {
    let dispatch = tracing::dispatcher::get_default(Dispatch::clone);
    let hook = environment.thread_start.clone();
    let handles = Arc::clone(workers);
    let built = ThreadPoolBuilder::new()
        .num_threads(threads)
        .thread_name(|index| format!("ufathom-pool-{index}"))
        .panic_handler(|payload| {
            tracing::error!(
                "a pool task panicked outside containment: {}",
                panic_message(payload.as_ref())
            );
        })
        .spawn_handler(move |worker| {
            let dispatch = dispatch.clone();
            let hook = hook.clone();
            let mut builder = std::thread::Builder::new();
            if let Some(name) = worker.name() {
                builder = builder.name(name.to_owned());
            }
            if let Some(size) = worker.stack_size() {
                builder = builder.stack_size(size);
            }
            let handle = builder.spawn(move || {
                let _logging = tracing::dispatcher::set_default(&dispatch);
                if let Some(hook) = hook {
                    let _ = catch_unwind(AssertUnwindSafe(|| hook()));
                }
                if let Err(payload) = catch_unwind(AssertUnwindSafe(|| worker.run())) {
                    tracing::error!(
                        "a pool thread panicked: {}",
                        panic_message(payload.as_ref())
                    );
                }
            })?;
            lock(&handles).push(handle);
            Ok(())
        })
        .build();
    built.map_err(|error| {
        // Threads started before the failure were told to stop by rayon.
        let _ = join_workers(workers, Instant::now() + Duration::from_secs(5));
        SessionError::Pool(error.to_string())
    })
}

/// Joins what has ended by `deadline`. Returns `(joined, left)`.
fn join_workers(workers: &Mutex<Vec<JoinHandle<()>>>, deadline: Instant) -> (usize, usize) {
    let mut pending = std::mem::take(&mut *lock(workers));
    let mut joined = 0;
    while !pending.is_empty() {
        let (done, running): (Vec<_>, Vec<_>) =
            pending.into_iter().partition(JoinHandle::is_finished);
        for handle in done {
            let _ = handle.join();
            joined += 1;
        }
        pending = running;
        if pending.is_empty() || Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(JOIN_POLL);
    }
    let left = pending.len();
    // Handles of threads that are still running are dropped, not joined: the
    // caller sees `left > 0` and keeps the DLL loaded.
    drop(pending);
    (joined, left)
}

/// A poisoned lock only means another thread panicked; the data is still
/// valid for the plain collections kept here.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn panic_message(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|text| (*text).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a panic with no message".to_owned())
}

/// Lines for `ufathom.log` of one dump, also sent to `tracing`.
struct RunLog {
    started: Instant,
    lines: Vec<String>,
}

impl RunLog {
    fn new() -> Self {
        Self {
            started: Instant::now(),
            lines: Vec::new(),
        }
    }

    fn info(&mut self, message: impl Display) {
        tracing::info!("{message}");
        self.push("INFO", &message);
    }

    fn warn(&mut self, message: impl Display) {
        tracing::warn!("{message}");
        self.push("WARN", &message);
    }

    fn error(&mut self, message: impl Display) {
        tracing::error!("{message}");
        self.push("ERROR", &message);
    }

    fn push(&mut self, level: &str, message: &dyn Display) {
        let millis = self.started.elapsed().as_millis();
        self.lines
            .push(format!("[+{millis:>7} ms] {level:<5} {message}"));
    }

    fn text(&self) -> String {
        let mut text = self.lines.join("\n");
        text.push('\n');
        text
    }
}

/// What a dump has learned so far, for the record written at its end.
struct Run<'a> {
    label: &'a str,
    formats: &'a [Format],
    captured: SystemTime,
    log: RunLog,
    game: FileIdentity,
    dll: FileIdentity,
    timings: TimingsMs,
    started: Instant,
}

impl Shared {
    fn next_label(&self, requested: Option<&str>) -> String {
        let sequence = self.dump_counter.fetch_add(1, Ordering::Relaxed) + 1;
        dump_label(requested, sequence)
    }

    /// Runs the dump and turns a panic into an error.
    fn run_contained(
        &self,
        label: &str,
        formats: &[Format],
        cancel: &AtomicBool,
    ) -> Result<DumpOutcome, DumpError> {
        match catch_unwind(AssertUnwindSafe(|| self.execute(label, formats, cancel))) {
            Ok(result) => result,
            Err(payload) => {
                let message = panic_message(payload.as_ref());
                tracing::error!("the dump panicked: {message}");
                Err(DumpError::Panicked { message })
            }
        }
    }

    fn execute(
        &self,
        label: &str,
        formats: &[Format],
        cancel: &AtomicBool,
    ) -> Result<DumpOutcome, DumpError> {
        check(cancel)?;
        let mut run = Run {
            label,
            formats,
            captured: SystemTime::now(),
            log: RunLog::new(),
            game: FileIdentity {
                path: self.environment.game_exe.display().to_string(),
                sha256: None,
                hash_error: None,
            },
            dll: FileIdentity {
                path: self.environment.dll_path.display().to_string(),
                sha256: None,
                hash_error: None,
            },
            timings: TimingsMs::default(),
            started: Instant::now(),
        };
        run.log.info(format!(
            "dump `{label}` started in process {} ({} pool threads)",
            self.environment.pid, self.config.threads
        ));

        self.hash_files(&mut run, cancel)?;
        check(cancel)?;

        let step = Instant::now();
        let discovered = self.discover(cancel);
        run.timings.discover = millis(step.elapsed());
        check(cancel)?;
        let discovery = match discovered {
            Ok(discovery) => discovery,
            Err(message) => {
                run.log.error(format!("discovery failed: {message}"));
                let folder = self.write_failure(&mut run, None, &message);
                return Err(DumpError::Discovery { message, folder });
            }
        };
        run.log.info(format!(
            "layout solved: engine {}, {} evidence records, {} warnings",
            discovery.engine.claimed.as_deref().unwrap_or("unknown"),
            discovery.evidence.len(),
            discovery.warnings.len()
        ));
        for warning in &discovery.warnings {
            run.log.warn(format!("discovery: {warning}"));
        }

        let step = Instant::now();
        let options = ReflectOptions {
            pool: Some(&self.pool),
            cancel: Some(cancel),
            ..ReflectOptions::default()
        };
        let reflected = build_dump(&*self.environment.reader, &discovery, &options);
        run.timings.reflect = millis(step.elapsed());
        let output = match reflected {
            Ok(output) => output,
            Err(ReflectError::Cancelled) => return Err(DumpError::Cancelled),
            Err(error) => {
                let message = error.to_string();
                run.log.error(format!("reflection failed: {message}"));
                let folder = self.write_failure(&mut run, Some(&discovery), &message);
                return Err(DumpError::Reflect { message, folder });
            }
        };
        check(cancel)?;
        let errors = output
            .dump
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.severity == Severity::Error)
            .count();
        run.log.info(format!(
            "reflected {} objects: {} classes and structs, {} enums, {} functions, {} failures, {} diagnostics ({errors} errors){}",
            output.stats.objects,
            output.stats.classes + output.stats.structs,
            output.stats.enums,
            output.stats.functions,
            output.stats.failures,
            output.dump.diagnostics.len(),
            if output.degraded { ", DEGRADED" } else { "" }
        ));

        self.write_outputs(&mut run, &discovery, &output)
    }

    fn hash_files(&self, run: &mut Run<'_>, cancel: &AtomicBool) -> Result<(), DumpError> {
        let step = Instant::now();
        let mut guard = lock(&self.hashes);
        let hashes = &mut *guard;
        hash_one(
            &mut hashes.game,
            &mut run.game,
            &self.environment.game_exe,
            "game executable",
            &mut run.log,
            cancel,
        )?;
        hash_one(
            &mut hashes.dll,
            &mut run.dll,
            &self.environment.dll_path,
            "DLL",
            &mut run.log,
            cancel,
        )?;
        drop(guard);
        run.timings.hash = millis(step.elapsed());
        Ok(())
    }

    /// Solves the layout on a page cache that lives only for this call.
    fn discover(&self, cancel: &AtomicBool) -> Result<Discovery, String> {
        if cancel.load(Ordering::Relaxed) {
            return Err("cancelled".to_owned());
        }
        let cache = PageCache::new(&*self.environment.reader);
        let options = DiscoverOptions {
            module: self.config.module.clone(),
            pointer_transform: self.config.pointer_transform.clone(),
            pool: Some(&self.pool),
            ..DiscoverOptions::default()
        };
        discover(&cache, &options).map_err(|error| error.to_string())
    }

    /// Writes the folder of a dump that could not be built: the log and an
    /// identity record with the error. Returns the folder, if it was created.
    fn write_failure(
        &self,
        run: &mut Run<'_>,
        discovery: Option<&Discovery>,
        message: &str,
    ) -> Option<PathBuf> {
        let claimed = discovery.and_then(|found| found.engine.claimed.clone());
        let folder = self.create_folder(run, claimed.as_deref()).ok()?;
        let mut files = Vec::new();
        let identity = self.identity(run, discovery, None, Some(message), &files);
        if let Err(error) = write_log_and_identity(run, &folder, &identity, &mut files) {
            tracing::error!("cannot write the failure record: {error}");
        }
        Some(folder)
    }

    fn create_folder(&self, run: &Run<'_>, claimed: Option<&str>) -> std::io::Result<PathBuf> {
        let build = build_id(claimed, run.game.sha256.as_deref());
        create_dump_folder(&self.config.output, &build, &self.name, run.label)
    }

    fn write_outputs(
        &self,
        run: &mut Run<'_>,
        discovery: &Discovery,
        output: &ufathom_reflect::ReflectOutput,
    ) -> Result<DumpOutcome, DumpError> {
        let step = Instant::now();
        let claimed = discovery.engine.claimed.clone();
        let folder = self
            .create_folder(run, claimed.as_deref())
            .map_err(|error| DumpError::Output {
                message: format!("cannot create the dump folder: {error}"),
                folder: None,
            })?;
        run.log.info(format!("writing to {}", folder.display()));

        let mut files: Vec<String> = Vec::new();
        let written = self.write_dump_files(run, &folder, output, &mut files);
        run.timings.write = millis(step.elapsed());
        if let Err(message) = written {
            run.log.error(&message);
            let identity =
                self.identity(run, Some(discovery), Some(output), Some(&message), &files);
            if let Err(error) = write_log_and_identity(run, &folder, &identity, &mut files) {
                tracing::error!("cannot write the failure record: {error}");
            }
            return Err(DumpError::Output {
                message,
                folder: Some(folder),
            });
        }

        let degraded = output.degraded;
        run.log.info(if degraded {
            "dump finished, degraded"
        } else {
            "dump finished"
        });
        let identity = self.identity(run, Some(discovery), Some(output), None, &files);
        write_log_and_identity(run, &folder, &identity, &mut files).map_err(|message| {
            DumpError::Output {
                message,
                folder: Some(folder.clone()),
            }
        })?;

        let summary = DumpSummary {
            folder: folder.display().to_string(),
            files,
            engine: claimed,
            structs: output.stats.classes + output.stats.structs,
            enums: output.stats.enums,
            functions: output.stats.functions,
            objects: output.stats.objects,
            failures: output.stats.failures,
            diagnostics: u64::try_from(output.dump.diagnostics.len()).unwrap_or(u64::MAX),
            degraded,
            elapsed_ms: millis(run.started.elapsed()),
        };
        Ok(DumpOutcome {
            label: run.label.to_owned(),
            folder,
            summary,
        })
    }

    fn write_dump_files(
        &self,
        run: &mut Run<'_>,
        folder: &Path,
        output: &ufathom_reflect::ReflectOutput,
        files: &mut Vec<String>,
    ) -> Result<(), String> {
        if run.formats.contains(&Format::Json) {
            let json = output
                .dump
                .to_json()
                .map_err(|error| format!("cannot serialize {DUMP_JSON}: {error}"))?;
            write_file(run, folder, DUMP_JSON, json.as_bytes(), files)?;
        }
        if run.formats.contains(&Format::Usmap) {
            let usmap = write_usmap(&output.dump);
            write_file(run, folder, MAPPINGS_USMAP, &usmap, files)?;
        }
        Ok(())
    }

    fn identity(
        &self,
        run: &Run<'_>,
        discovery: Option<&Discovery>,
        output: Option<&ufathom_reflect::ReflectOutput>,
        error: Option<&str>,
        files: &[String],
    ) -> Identity {
        let degraded = output.is_some_and(|output| output.degraded);
        let result = if error.is_some() {
            "failed"
        } else if degraded {
            "degraded"
        } else {
            "ok"
        };
        let mut timings = run.timings.clone();
        timings.total = millis(run.started.elapsed());
        Identity {
            format_version: IDENTITY_FORMAT_VERSION,
            tool: TOOL_NAME.to_owned(),
            tool_version: env!("CARGO_PKG_VERSION").to_owned(),
            captured_at: clock::rfc3339(run.captured),
            session: self.name.clone(),
            label: run.label.to_owned(),
            result: result.to_owned(),
            error: error.map(str::to_owned),
            pid: self.environment.pid,
            game: run.game.clone(),
            dll: run.dll.clone(),
            engine: discovery.map(|found| found.engine.clone()),
            config_source: describe_source(&self.environment.config_source),
            config: self.config.clone(),
            discovery: discovery.map(DiscoveryRecord::new),
            warnings: discovery
                .map(|found| found.warnings.clone())
                .unwrap_or_default(),
            reflect: output.map(|output| {
                ReflectRecord::new(
                    &output.stats,
                    output.dump.diagnostics.len(),
                    output.degraded,
                )
            }),
            files: files.to_vec(),
            timings_ms: timings,
        }
    }

    /// Drops the pool and joins its threads. Called with the last reference.
    fn stop_pool(self, deadline: Instant) -> ShutdownReport {
        let Self { pool, workers, .. } = self;
        // Dropping the pool tells its threads to exit once they run out of work.
        drop(pool);
        let (joined, left) = join_workers(&workers, deadline);
        ShutdownReport {
            pool_threads_joined: joined,
            pool_threads_left: left,
            dump_threads_left: 0,
        }
    }
}

/// Writes `ufathom.log` and then `identity.json`. The identity comes last, so
/// a folder that has one is a folder whose dump ran to its end.
fn write_log_and_identity(
    run: &mut Run<'_>,
    folder: &Path,
    identity: &Identity,
    files: &mut Vec<String>,
) -> Result<(), String> {
    let log = run.log.text();
    write_file(run, folder, DUMP_LOG, log.as_bytes(), files)?;
    let mut identity = identity.clone();
    identity.files.clone_from(files);
    identity.files.push(IDENTITY_JSON.to_owned());
    let json = serde_json::to_string_pretty(&identity)
        .map_err(|error| format!("cannot serialize {IDENTITY_JSON}: {error}"))?;
    write_file(run, folder, IDENTITY_JSON, json.as_bytes(), files)
}

fn write_file(
    run: &mut Run<'_>,
    folder: &Path,
    name: &str,
    bytes: &[u8],
    files: &mut Vec<String>,
) -> Result<(), String> {
    fsio::write_atomic(&folder.join(name), bytes)
        .map_err(|error| format!("cannot write {name}: {error}"))?;
    run.log
        .push("INFO", &format!("wrote {name} ({} bytes)", bytes.len()));
    files.push(name.to_owned());
    Ok(())
}

/// Hashes one file unless the session already has its hash, and records the
/// result in `identity`. A file that cannot be read is a warning, not a
/// failure: the dump goes on with the hash marked missing.
fn hash_one(
    slot: &mut Option<String>,
    identity: &mut FileIdentity,
    path: &Path,
    what: &str,
    log: &mut RunLog,
    cancel: &AtomicBool,
) -> Result<(), DumpError> {
    if slot.is_none() {
        match sha256_file(path, cancel) {
            Ok(hash) => *slot = Some(hash),
            Err(HashError::Cancelled) => return Err(DumpError::Cancelled),
            Err(HashError::Io(error)) => {
                identity.hash_error = Some(error.to_string());
                log.warn(format!(
                    "cannot hash the {what} {}: {error}",
                    path.display()
                ));
            }
        }
    }
    identity.sha256.clone_from(slot);
    Ok(())
}

fn check(cancel: &AtomicBool) -> Result<(), DumpError> {
    if cancel.load(Ordering::Relaxed) {
        Err(DumpError::Cancelled)
    } else {
        Ok(())
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
