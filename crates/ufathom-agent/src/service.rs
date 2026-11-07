//! The state machine behind the DLL: triggers in, dumps and control files out.
//!
//! A [`Service`] owns the [`Session`] and the control folder. The shell calls
//! [`Service::poll`] about every 50 to 100 ms from its worker thread, and
//! feeds hotkey presses to [`Service::trigger_dump`] and
//! [`Service::trigger_stop`]. Everything slow happens on other threads: a
//! dump runs on a thread of the session, and `poll` only looks at whether it
//! has ended.
//!
//! # States
//!
//! | State | Meaning | Next |
//! |---|---|---|
//! | `ready` | started, no dump yet | `dumping`, `stopping` |
//! | `dumping` | one dump is running | `idle`, `stopping` |
//! | `idle` | a dump has ended | `dumping`, `stopping` |
//! | `stopping` | stop requested: new commands are refused and a running dump is cancelled | `stopped` |
//! | `stopped` | everything is closed | |
//!
//! Only one dump runs at a time. A `dump` while `dumping` is answered `busy`.
//! The status file is rewritten on every state change.

use std::time::{Duration, Instant, SystemTime};

use crate::clock::rfc3339;
use crate::config::Config;
use crate::control::{ControlDir, is_settled};
use crate::error::{DumpError, ServiceError};
use crate::fsio::FileEntry;
use crate::paths::sanitize_component;
use crate::protocol::{
    Command, CommandResult, CurrentDump, DumpFailure, LastDump, Request, ResultStatus, State,
    StatusDocument, parse_request,
};
use crate::session::{DumpOutcome, Environment, RunningDump, Session, ShutdownReport};

/// How often the inbox is read. The shell may call `poll` more often.
pub const INBOX_SCAN_INTERVAL: Duration = Duration::from_millis(100);
/// Most command files handled in one poll, so a flood cannot starve the
/// shell's other work.
pub const MAX_COMMANDS_PER_POLL: usize = 16;
/// How long [`Service::shutdown`] waits for threads before it gives up on them.
pub const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(20);
/// The label of the dump that `auto_dump` starts.
pub const AUTO_DUMP_LABEL: &str = "auto";

/// Why a dump was started.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Origin {
    /// A command file with this id. Its result is published when the dump ends.
    Command(String),
    /// A hotkey.
    Hotkey,
    /// `auto_dump`.
    Auto,
}

struct Running {
    dump: RunningDump,
    origin: Origin,
}

/// The answer to a request to start a dump from outside a command file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DumpTrigger {
    /// The dump was started.
    Started,
    /// Another dump is running.
    Busy,
    /// The DLL is stopping and takes no new work.
    Stopping,
    /// The dump thread could not be created.
    Failed(String),
}

/// See the [module documentation](self).
pub struct Service {
    session: Option<Session>,
    control: ControlDir,
    config: Config,
    session_name: String,
    pid: u32,
    state: State,
    detail: Option<String>,
    running: Option<Running>,
    stop_requested: bool,
    auto_deadline: Option<Instant>,
    last_scan: Option<Instant>,
    dumps_completed: u64,
    dumps_failed: u64,
    last_dump: Option<LastDump>,
}

impl Service {
    /// Creates the session and the control folder, and publishes `ready`.
    ///
    /// # Errors
    ///
    /// [`ServiceError`] when the pool cannot be built or the control folder
    /// cannot be created. The session is shut down before returning.
    pub fn start(config: Config, environment: Environment) -> Result<Self, ServiceError> {
        let pid = environment.pid;
        let control = ControlDir::new(&config.output);
        let session = Session::new(config.clone(), environment)?;
        if let Err(source) = control.create() {
            let _ = session.shutdown(SHUTDOWN_TIMEOUT);
            return Err(ServiceError::Control {
                path: control.root().to_owned(),
                source,
            });
        }
        let auto_deadline = config
            .auto_dump
            .then(|| Instant::now() + Duration::from_secs_f64(config.trigger_delay_seconds));
        let session_name = session.name().to_owned();
        let service = Self {
            session: Some(session),
            control,
            config,
            session_name,
            pid,
            state: State::Ready,
            detail: None,
            running: None,
            stop_requested: false,
            auto_deadline,
            last_scan: None,
            dumps_completed: 0,
            dumps_failed: 0,
            last_dump: None,
        };
        service.publish_status();
        tracing::info!(
            "ready: session {}, output {}",
            service.session_name,
            service.config.output.display()
        );
        Ok(service)
    }

    /// Where the DLL is in its life.
    #[must_use]
    pub fn state(&self) -> State {
        self.state
    }

    /// The control folder.
    #[must_use]
    pub fn control(&self) -> &ControlDir {
        &self.control
    }

    /// The settings the service runs with.
    #[must_use]
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// A stop was requested and no dump is still running, so the shell can
    /// call [`shutdown`](Self::shutdown).
    #[must_use]
    pub fn should_unload(&self) -> bool {
        self.stop_requested && self.running.is_none()
    }

    /// The status document as it would be published now.
    #[must_use]
    pub fn status_document(&self) -> StatusDocument {
        StatusDocument {
            state: self.state,
            pid: self.pid,
            session: self.session_name.clone(),
            tool_version: env!("CARGO_PKG_VERSION").to_owned(),
            output: self.config.output.display().to_string(),
            dumps_completed: self.dumps_completed,
            dumps_failed: self.dumps_failed,
            current: self.running.as_ref().map(|running| CurrentDump {
                label: running.dump.label.clone(),
                started: rfc3339(running.dump.started),
            }),
            last_dump: self.last_dump.clone(),
            detail: self.detail.clone(),
            updated: rfc3339(SystemTime::now()),
        }
    }

    /// Does the periodic work: collects a finished dump, starts the automatic
    /// dump when its delay is over, and handles command files. Cheap when
    /// there is nothing to do; never blocks on a dump.
    pub fn poll(&mut self) {
        self.collect_finished_dump();
        self.start_auto_dump();
        let due = self
            .last_scan
            .is_none_or(|last| last.elapsed() >= INBOX_SCAN_INTERVAL);
        if due {
            self.last_scan = Some(Instant::now());
            self.handle_inbox();
        }
    }

    /// Starts a dump because a hotkey was pressed.
    pub fn trigger_dump(&mut self) -> DumpTrigger {
        self.begin_dump(None, None, Origin::Hotkey)
    }

    /// Asks the DLL to unload because a hotkey was pressed or `auto_unload`
    /// applies. Cancels a running dump.
    pub fn trigger_stop(&mut self) {
        self.request_stop("stop requested");
    }

    /// Stops everything: cancels a running dump, waits for it, drops the
    /// session so the pool's threads exit, joins them, and publishes
    /// `stopped`.
    ///
    /// The returned report says whether every thread ended. When it does not,
    /// the shell must leave the DLL loaded: unmapping code that a thread still
    /// runs would crash the host.
    pub fn shutdown(mut self) -> ShutdownReport {
        self.request_stop("shutdown");
        let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
        if let Some(running) = &self.running {
            running.dump.cancel();
        }
        while self
            .running
            .as_ref()
            .is_some_and(|running| !running.dump.is_finished())
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(5));
        }
        self.collect_finished_dump();
        let remaining = deadline.saturating_duration_since(Instant::now());
        let report = match self.session.take() {
            Some(session) => session.shutdown(remaining),
            None => ShutdownReport {
                pool_threads_joined: 0,
                pool_threads_left: 0,
                dump_threads_left: 0,
            },
        };
        self.state = State::Stopped;
        if !report.is_clean() {
            self.detail = Some(format!(
                "{} pool threads and {} dump threads did not end: the DLL stays loaded",
                report.pool_threads_left, report.dump_threads_left
            ));
            tracing::error!("{}", self.detail.as_deref().unwrap_or_default());
        }
        self.publish_status();
        tracing::info!("stopped");
        report
    }

    fn request_stop(&mut self, reason: &str) {
        if self.stop_requested {
            return;
        }
        tracing::info!("{reason}: stopping");
        self.stop_requested = true;
        self.state = State::Stopping;
        if let Some(running) = &self.running {
            running.dump.cancel();
        }
        self.publish_status();
    }

    fn begin_dump(
        &mut self,
        label: Option<&str>,
        formats: Option<Vec<crate::config::Format>>,
        origin: Origin,
    ) -> DumpTrigger {
        if self.stop_requested {
            return DumpTrigger::Stopping;
        }
        if self.running.is_some() {
            return DumpTrigger::Busy;
        }
        let Some(session) = &self.session else {
            return DumpTrigger::Stopping;
        };
        match session.spawn_dump(label, formats) {
            Ok(dump) => {
                tracing::info!("dump `{}` started ({origin:?})", dump.label);
                self.running = Some(Running { dump, origin });
                self.state = State::Dumping;
                self.publish_status();
                DumpTrigger::Started
            }
            Err(error) => {
                tracing::error!("cannot start a dump: {error}");
                DumpTrigger::Failed(error.to_string())
            }
        }
    }

    fn start_auto_dump(&mut self) {
        let Some(deadline) = self.auto_deadline else {
            return;
        };
        if Instant::now() < deadline || self.stop_requested || self.running.is_some() {
            return;
        }
        self.auto_deadline = None;
        if let DumpTrigger::Failed(message) =
            self.begin_dump(Some(AUTO_DUMP_LABEL), None, Origin::Auto)
        {
            tracing::error!("the automatic dump did not start: {message}");
            if self.config.auto_unload {
                self.request_stop("auto_unload after a failed automatic dump");
            }
        }
    }

    /// Takes the result of a dump that has ended, if there is one.
    fn collect_finished_dump(&mut self) {
        let finished = self
            .running
            .as_ref()
            .is_some_and(|running| running.dump.is_finished());
        if !finished {
            return;
        }
        let Some(Running { dump, origin }) = self.running.take() else {
            return;
        };
        let label = dump.label.clone();
        let result = dump.join();
        let now = rfc3339(SystemTime::now());
        let (status, message, summary, failure, last) = match result {
            Ok(outcome) => {
                self.dumps_completed += 1;
                let degraded = outcome.summary.degraded;
                let last = last_dump(
                    &label,
                    if degraded { "degraded" } else { "ok" },
                    Some(&outcome),
                    &now,
                );
                (
                    ResultStatus::Ok,
                    degraded.then(|| "the dump is degraded: many objects failed".to_owned()),
                    Some(outcome.summary),
                    None,
                    last,
                )
            }
            Err(error) => {
                self.dumps_failed += 1;
                let status = if matches!(error, DumpError::Cancelled) {
                    ResultStatus::Cancelled
                } else {
                    ResultStatus::Error
                };
                tracing::warn!("dump `{label}` ended without a result: {error}");
                let folder = error.folder().map(|path| path.display().to_string());
                let last = LastDump {
                    label: label.clone(),
                    result: error.kind().to_owned(),
                    folder: folder.clone(),
                    finished: now.clone(),
                };
                let failure = DumpFailure {
                    kind: error.kind().to_owned(),
                    message: error.to_string(),
                    folder,
                };
                (status, Some(error.to_string()), None, Some(failure), last)
            }
        };
        self.last_dump = Some(last);
        self.state = if self.stop_requested {
            State::Stopping
        } else {
            State::Idle
        };
        self.publish_status();
        if let Origin::Command(id) = &origin {
            let result = CommandResult {
                id: Some(id.clone()),
                command: Some("dump".to_owned()),
                status,
                message,
                state: self.state,
                dump: summary,
                failure,
                status_document: None,
                finished: now,
            };
            self.publish_result(id, &result);
        }
        if origin == Origin::Auto && self.config.auto_unload {
            self.request_stop("auto_unload");
        }
    }

    fn handle_inbox(&mut self) {
        let entries = match self.control.pending() {
            Ok(entries) => entries,
            Err(error) => {
                tracing::warn!("cannot read the inbox: {error}");
                return;
            }
        };
        for entry in entries.into_iter().take(MAX_COMMANDS_PER_POLL) {
            self.handle_file(&entry);
        }
    }

    fn handle_file(&mut self, entry: &FileEntry) {
        let text = match self.control.read_command(entry) {
            Ok(text) => text,
            Err(error) => {
                if !is_settled(entry, SystemTime::now()) {
                    return;
                }
                let message = format!("cannot read the command file: {error}");
                self.reject(entry, None, None, message);
                return;
            }
        };
        match parse_request(&text) {
            Ok(request) => {
                self.execute(request);
                self.archive(entry);
            }
            Err(error) => {
                if !is_settled(entry, SystemTime::now()) {
                    return;
                }
                self.reject(entry, error.id, error.command, error.message);
            }
        }
    }

    /// Answers a command file that is not a command, and archives it.
    fn reject(
        &mut self,
        entry: &FileEntry,
        id: Option<String>,
        command: Option<String>,
        message: String,
    ) {
        tracing::warn!("rejected command file {}: {message}", entry.name);
        let name = id.clone().unwrap_or_else(|| {
            let stem = entry.name.strip_suffix(".json").unwrap_or(&entry.name);
            sanitize_component(stem, "command")
        });
        let result = CommandResult {
            id,
            command,
            status: ResultStatus::Error,
            message: Some(message),
            state: self.state,
            dump: None,
            failure: None,
            status_document: None,
            finished: rfc3339(SystemTime::now()),
        };
        self.publish_result(&name, &result);
        self.archive(entry);
    }

    fn archive(&self, entry: &FileEntry) {
        if let Err(error) = self.control.archive_command(entry) {
            tracing::warn!("cannot archive {}: {error}", entry.name);
            // A command that cannot be moved would be handled again and again.
            if let Err(error) = crate::fsio::remove_file(&entry.path) {
                tracing::error!("cannot remove {}: {error}", entry.name);
            }
        }
    }

    fn execute(&mut self, request: Request) {
        let Request { id, command } = request;
        let name = command.name();
        let answer = |service: &Self, status, message: Option<String>| CommandResult {
            id: Some(id.clone()),
            command: Some(name.to_owned()),
            status,
            message,
            state: service.state,
            dump: None,
            failure: None,
            status_document: None,
            finished: rfc3339(SystemTime::now()),
        };
        match command {
            Command::Status => {
                let mut result = answer(self, ResultStatus::Ok, None);
                result.status_document = Some(self.status_document());
                self.publish_result(&id, &result);
            }
            Command::Stop => {
                let already = self.stop_requested;
                self.request_stop("stop command");
                let note = if already {
                    "already stopping"
                } else {
                    "stopping: a running dump is cancelled, then the DLL unloads"
                };
                let result = answer(self, ResultStatus::Ok, Some(note.to_owned()));
                self.publish_result(&id, &result);
            }
            Command::Dump { label, formats } => {
                match self.begin_dump(label.as_deref(), formats, Origin::Command(id.clone())) {
                    DumpTrigger::Started => {}
                    DumpTrigger::Busy => {
                        let busy = self
                            .running
                            .as_ref()
                            .map(|running| running.dump.label.clone())
                            .unwrap_or_default();
                        let message = format!("a dump is already running: `{busy}`");
                        let result = answer(self, ResultStatus::Busy, Some(message));
                        self.publish_result(&id, &result);
                    }
                    DumpTrigger::Stopping => {
                        let message = "the DLL is stopping and takes no new commands".to_owned();
                        let result = answer(self, ResultStatus::Error, Some(message));
                        self.publish_result(&id, &result);
                    }
                    DumpTrigger::Failed(message) => {
                        let result = answer(self, ResultStatus::Error, Some(message));
                        self.publish_result(&id, &result);
                    }
                }
            }
        }
    }

    fn publish_result(&self, name: &str, result: &CommandResult) {
        if let Err(error) = self.control.publish_result(name, result) {
            tracing::error!("cannot publish the result `{name}`: {error}");
        }
    }

    fn publish_status(&self) {
        if let Err(error) = self.control.write_status(&self.status_document()) {
            tracing::error!("cannot publish the status: {error}");
        }
    }
}

fn last_dump(label: &str, result: &str, outcome: Option<&DumpOutcome>, finished: &str) -> LastDump {
    LastDump {
        label: label.to_owned(),
        result: result.to_owned(),
        folder: outcome.map(|outcome| outcome.folder.display().to_string()),
        finished: finished.to_owned(),
    }
}
