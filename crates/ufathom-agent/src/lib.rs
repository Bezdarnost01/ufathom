//! Dump session logic behind the DLL: config, commands, output folders and identity.
//!
//! This crate is plain Rust with no FFI, so everything the DLL does except
//! calling the operating system can be tested without loading a DLL. The
//! shell (`ufathom-dll`) supplies a [`MemoryReader`](ufathom_mem::MemoryReader)
//! and the few facts only the process knows, in an [`Environment`], and drives a
//! [`Service`].
//!
//! - [`Config`] reads `ufathom.toml`. A broken file means defaults, never a
//!   failure to start.
//! - [`Session`] owns the thread pool and runs dumps: discovery on a page
//!   cache, reflection on the session's pool, then atomic writes of
//!   `dump.json`, `Mappings.usmap`, `sdk/`, `identity.json` and `ufathom.log` into
//!   `<output>/<build id>/<session>/<label>/`.
//! - [`Service`] is the state machine (`ready`, `dumping`, `idle`,
//!   `stopping`, `stopped`) that accepts triggers, serves the command files
//!   and publishes `status.json`. The file protocol is described in
//!   [`protocol`].
//! - [`HotkeyTracker`] decides when a sampled key counts as a press.
//!
//! # Safety model
//!
//! Nothing here touches the target except through the reader it was given, and
//! nothing writes outside the output folder. Every thread this crate starts
//! is kept and joined by [`Session::shutdown`]; the global rayon pool is never
//! used. A panic in a dump is contained and becomes an error.

#![forbid(unsafe_code)]

mod clock;
mod config;
mod control;
mod error;
pub mod fsio;
mod hashing;
mod hotkey;
mod identity;
mod output;
pub mod paths;
pub mod protocol;
mod sdk;
mod service;
mod session;

pub use clock::{compact, rfc3339};
pub use config::{
    CONFIG_FILE_NAME, Config, ConfigSource, DEFAULT_HOTKEY_DUMP, DEFAULT_HOTKEY_UNLOAD,
    DEFAULT_OUTPUT_FOLDER, DEFAULT_THREADS, DEFAULT_TRIGGER_DELAY_SECONDS, Format, LoadedConfig,
    MAX_THREADS, MAX_TRIGGER_DELAY_SECONDS, MIN_THREADS,
};
pub use control::{ControlDir, SETTLE_TIME, is_settled};
pub use error::{ConfigError, DumpError, HotkeyError, ServiceError, SessionError};
pub use hashing::{HashError, sha256_file};
pub use hotkey::{Hotkey, HotkeyTracker, is_own_process_foreground};
pub use identity::{DiscoveryRecord, EvidenceRecord, GlobalsRecord, Identity, TimingsMs};
pub use output::{
    CONTROL_FOLDER, DUMP_JSON, DUMP_LOG, IDENTITY_JSON, LOG_FOLDER, MAPPINGS_USMAP, SDK_FOLDER,
    build_id, control_folder, create_dump_folder, dump_label, session_name,
};
pub use service::{AUTO_DUMP_LABEL, DumpTrigger, SHUTDOWN_TIMEOUT, Service};
pub use session::{
    DumpOutcome, Environment, RunningDump, Session, ShutdownReport, TOOL_NAME, ThreadHook,
};
