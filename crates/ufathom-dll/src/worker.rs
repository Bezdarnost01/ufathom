//! The worker thread: guard against a second copy, set up, serve, unload.

use std::io::Write as _;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use ufathom_agent::fsio::open_append;
use ufathom_agent::{
    Config, ConfigSource, DumpTrigger, Environment, HotkeyTracker, LOG_FOLDER, Service,
    is_own_process_foreground, rfc3339, session_name,
};
use ufathom_mem::SelfMemory;

use crate::logging::{LogSink, PanicHook};
use crate::os::{self, MutexError, NamedMutex};

/// How often the worker looks at the service and the keys.
const TICK: Duration = Duration::from_millis(50);

/// What the entry point does when the worker is done.
pub(crate) enum Exit {
    /// Everything this DLL started has ended: release the module and end the
    /// thread.
    Unload,
    /// Something may still run code of this DLL, or the module cannot be
    /// released safely: leave it loaded and just end the thread.
    Stay,
}

/// The whole life of the worker. Never panics out: a panic is turned into
/// [`Exit::Stay`].
pub(crate) fn run() -> Exit {
    os::lower_current_thread_priority();
    // Before anything that could panic: a panic must reach the log, never the
    // host's stderr.
    let hook = PanicHook::install();
    let pid = os::process_id();
    let mutex = match NamedMutex::acquire(&format!("Local\\ufathom-{pid}")) {
        Ok(mutex) => mutex,
        Err(MutexError::AlreadyHeld) => {
            refuse(pid);
            return Exit::Unload;
        }
        Err(MutexError::Os(code)) => {
            os::debug_log(&format!(
                "cannot create the per-process mutex (error {code}); not starting"
            ));
            return Exit::Unload;
        }
    };
    let exit = catch_unwind(AssertUnwindSafe(|| serve(pid, &hook))).unwrap_or(Exit::Stay);
    // Released after the service and the log are closed, just before the
    // module is. The panic hook goes first: its code is in this module.
    drop(hook);
    drop(mutex);
    exit
}

/// A second copy found the mutex held. It writes one line where the user can
/// find it and does nothing else.
fn refuse(pid: u32) {
    let dll = os::module_path(true);
    let message = format!(
        "another instance of ufathom is already loaded in process {pid}; this copy{} exits without doing anything",
        dll.as_ref()
            .map_or_else(String::new, |path| format!(" ({})", path.display()))
    );
    os::debug_log(&message);
    let Some(folder) = dll.as_deref().and_then(Path::parent) else {
        return;
    };
    let file = folder.join(format!("ufathom-refused-{pid}.log"));
    if let Ok(mut log) = open_append(&file) {
        let _ = writeln!(log, "{} {message}", rfc3339(SystemTime::now()));
    }
}

/// Loads the config, opens the log and serves until told to stop.
fn serve(pid: u32, hook: &PanicHook) -> Exit {
    let Some(game_exe) = os::module_path(false) else {
        os::debug_log("cannot find the path of the game executable; not starting");
        return Exit::Unload;
    };
    let dll_path = os::module_path(true).unwrap_or_default();
    let exe_dir = game_exe.parent().map(Path::to_owned).unwrap_or_default();
    let dll_dir = dll_path.parent().map(Path::to_owned).unwrap_or_default();
    let loaded = Config::load(&[exe_dir.as_path(), dll_dir.as_path()], &exe_dir);

    let log_file = loaded.config.output.join(LOG_FOLDER).join(format!(
        "ufathom-{}.log",
        session_name(SystemTime::now(), pid)
    ));
    let sink = LogSink::open(&log_file);
    hook.set_sink(sink.clone());

    let exit = match sink.as_ref().map(LogSink::dispatch) {
        Some(dispatch) => tracing::dispatcher::with_default(&dispatch, || {
            drive(
                &loaded.config,
                loaded.source,
                loaded.problem,
                game_exe,
                dll_path,
                pid,
                sink.as_ref(),
            )
        }),
        None => drive(
            &loaded.config,
            loaded.source,
            loaded.problem,
            game_exe,
            dll_path,
            pid,
            None,
        ),
    };
    hook.set_sink(None);
    if let Some(sink) = &sink {
        sink.close();
    }
    exit
}

/// Starts the service and runs it until it should unload, then shuts it down.
fn drive(
    config: &Config,
    source: ConfigSource,
    problem: Option<String>,
    game_exe: PathBuf,
    dll_path: PathBuf,
    pid: u32,
    sink: Option<&LogSink>,
) -> Exit {
    tracing::info!(
        "ufathom {} loaded in process {pid}; executable {}",
        env!("CARGO_PKG_VERSION"),
        game_exe.display()
    );
    if let Some(problem) = problem {
        tracing::error!("the config file was not used: {problem}; running with the defaults");
    }
    match &source {
        ConfigSource::File(path) => tracing::info!("config: {}", path.display()),
        ConfigSource::Defaults => tracing::info!("config: defaults"),
    }
    let environment = Environment {
        game_exe,
        dll_path,
        pid,
        reader: Arc::new(SelfMemory::new()),
        thread_start: Some(Arc::new(os::lower_current_thread_priority)),
        config_source: source,
    };
    let mut service = match Service::start(config.clone(), environment) {
        Ok(service) => service,
        Err(error) => {
            tracing::error!("cannot start: {error}");
            // The log may be the very thing that could not be created.
            os::debug_log(&format!("cannot start: {error}"));
            return Exit::Unload;
        }
    };

    let ended_normally = tick_loop(&mut service, pid);
    if !ended_normally {
        tracing::error!("the worker loop panicked; shutting down");
    }
    // The log is closed after the threads are joined and before `stopped` is
    // published, so a client that sees `stopped` finds the log complete.
    let report = service.shutdown_with(|| {
        if let Some(sink) = sink {
            sink.close();
        }
    });
    if report.is_clean() {
        Exit::Unload
    } else {
        Exit::Stay
    }
}

/// Polls the service and the hotkeys until the service should unload.
/// Returns `false` if the loop panicked.
fn tick_loop(service: &mut Service, pid: u32) -> bool {
    let dump_key = service.config().hotkey_dump;
    let unload_key = service.config().hotkey_unload;
    let mut dump_state = HotkeyTracker::new();
    let mut unload_state = HotkeyTracker::new();
    catch_unwind(AssertUnwindSafe(|| {
        loop {
            service.poll();
            if dump_key.is_some() || unload_key.is_some() {
                let foreground = is_own_process_foreground(os::foreground_process_id(), pid);
                if let Some(key) = dump_key {
                    let pressed = dump_state.update(os::key_is_down(key.virtual_key()), foreground);
                    if pressed {
                        on_dump_hotkey(service, key.to_string());
                    }
                }
                if let Some(key) = unload_key {
                    let pressed =
                        unload_state.update(os::key_is_down(key.virtual_key()), foreground);
                    if pressed {
                        tracing::info!("unload hotkey {key} pressed");
                        service.trigger_stop();
                    }
                }
            }
            if service.should_unload() {
                break;
            }
            std::thread::sleep(TICK);
        }
    }))
    .is_ok()
}

fn on_dump_hotkey(service: &mut Service, key: String) {
    match service.trigger_dump() {
        DumpTrigger::Started => tracing::info!("dump hotkey {key} pressed: dump started"),
        DumpTrigger::Busy => tracing::info!("dump hotkey {key} pressed: a dump is already running"),
        DumpTrigger::Stopping => tracing::info!("dump hotkey {key} pressed: the DLL is stopping"),
        DumpTrigger::Failed(message) => tracing::error!("dump hotkey {key}: {message}"),
    }
}
