//! Companion DLLs: a built SDK DLL reads `ufathom-companions.txt` from its own
//! folder and loads the older mod DLLs named there.
//!
//! The companion is a tiny real DLL built here. It creates a named event when
//! the loader attaches it and another when the loader detaches it, so a test
//! can see from the outside whether and when it was loaded.
#![cfg(windows)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "tests may panic"
)]

mod common;

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use common::dumps::golden_dump;
use common::{run_cargo, workspace_root};
use ufathom_codegen::{GenOptions, generate};

#[link(name = "kernel32")]
unsafe extern "system" {
    fn LoadLibraryW(name: *const u16) -> *mut c_void;
    fn GetModuleHandleW(name: *const u16) -> *mut c_void;
    fn OpenEventW(access: u32, inherit: i32, name: *const u16) -> *mut c_void;
    fn CloseHandle(handle: *mut c_void) -> i32;
}

/// `SYNCHRONIZE`, the least right that opens an existing event.
const SYNCHRONIZE: u32 = 0x0010_0000;

/// How long a positive check waits. The worker thread starts right after
/// `LoadLibrary` returns, so this is generous.
const LOAD_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a negative check waits before it believes nothing will happen.
const QUIET_PERIOD: Duration = Duration::from_millis(1500);

/// Builds share one target directory and output names, so one at a time.
static BUILD: Mutex<()> = Mutex::new(());

const FIXTURE_MANIFEST: &str = r#"[package]
name = "ufathom-companion-fixture"
version = "0.0.0"
edition = "2024"
publish = false

[lib]
crate-type = ["cdylib"]

[workspace]
"#;

const FIXTURE_CARGO_CONFIG: &str = r#"[target.x86_64-pc-windows-msvc]
rustflags = ["-C", "target-feature=+crt-static"]
"#;

/// A DLL that names an event after its own file name when attached, and
/// another, with `-unloaded` added, when detached.
const FIXTURE_SOURCE: &str = r#"use core::ffi::c_void;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetModuleFileNameW(module: *mut c_void, name: *mut u16, size: u32) -> u32;
    fn CreateEventW(
        attributes: *const c_void,
        manual: i32,
        initial: i32,
        name: *const u16,
    ) -> *mut c_void;
}

fn signal(module: *mut c_void, suffix: &str) {
    let mut path = [0u16; 520];
    let length = unsafe { GetModuleFileNameW(module, path.as_mut_ptr(), 520) } as usize;
    if length == 0 || length >= path.len() {
        return;
    }
    let start = path[..length]
        .iter()
        .rposition(|&unit| unit == u16::from(b'\\'))
        .map_or(0, |index| index + 1);
    let mut name = [0u16; 640];
    let mut used = 0;
    for unit in "ufathom-companion-test:"
        .encode_utf16()
        .chain(path[start..length].iter().copied())
        .chain(suffix.encode_utf16())
    {
        if used + 1 >= name.len() {
            return;
        }
        name[used] = unit;
        used += 1;
    }
    unsafe { CreateEventW(core::ptr::null(), 1, 0, name.as_ptr()) };
}

#[unsafe(no_mangle)]
pub extern "system" fn DllMain(module: *mut c_void, reason: u32, _reserved: *mut c_void) -> i32 {
    match reason {
        1 => signal(module, ""),
        0 => signal(module, "-unloaded"),
        _ => {}
    }
    1
}
"#;

fn wide(text: &std::ffi::OsStr) -> Vec<u16> {
    text.encode_wide().chain(Some(0)).collect()
}

/// The event the fixture creates for a DLL file name (`suffix` is empty for
/// attach, `-unloaded` for detach).
fn event_name(file_name: &str, suffix: &str) -> Vec<u16> {
    wide(format!("ufathom-companion-test:{file_name}{suffix}").as_ref())
}

fn event_exists(file_name: &str, suffix: &str) -> bool {
    let name = event_name(file_name, suffix);
    // SAFETY: a NUL-terminated name.
    let handle = unsafe { OpenEventW(SYNCHRONIZE, 0, name.as_ptr()) };
    if handle.is_null() {
        return false;
    }
    // SAFETY: the handle was just opened here.
    unsafe { CloseHandle(handle) };
    true
}

fn wait_until(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if condition() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn companion_loaded(file_name: &str) -> bool {
    wait_until(LOAD_TIMEOUT, || event_exists(file_name, ""))
}

/// Waits for the quiet period and tells whether the companion stayed unloaded.
fn companion_stays_unloaded(file_name: &str) -> bool {
    !wait_until(QUIET_PERIOD, || event_exists(file_name, ""))
}

fn build_in_temp_dir(
    prefix: &str,
    files: &[(&str, &str)],
    features: &[&str],
    output: &str,
) -> Vec<u8> {
    let _guard = BUILD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = tempfile::Builder::new().prefix(prefix).tempdir().unwrap();
    for (name, text) in files {
        let path = dir.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    let mut args = vec!["build", "--lib"];
    let joined = features.join(" ");
    if !features.is_empty() {
        args.extend(["--features", &joined]);
    }
    let run = run_cargo(dir.path(), &args);
    assert!(
        !run.stderr.contains("warning:"),
        "warnings while building {prefix}:\n{}",
        run.stderr
    );
    std::fs::read(workspace_root().join("target/sdk-check/debug").join(output)).unwrap()
}

/// The bytes of the companion test DLL.
fn fixture_dll() -> &'static [u8] {
    static DLL: OnceLock<Vec<u8>> = OnceLock::new();
    DLL.get_or_init(|| {
        build_in_temp_dir(
            "companion-fixture-",
            &[
                ("Cargo.toml", FIXTURE_MANIFEST),
                ("src/lib.rs", FIXTURE_SOURCE),
                (".cargo/config.toml", FIXTURE_CARGO_CONFIG),
            ],
            &[],
            "ufathom_companion_fixture.dll",
        )
    })
}

/// The bytes of a generated SDK DLL built with `features`.
fn sdk_dll(features: &[&str]) -> Vec<u8> {
    let _guard = BUILD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let sdk = tempfile::Builder::new()
        .prefix("companion sdk ")
        .tempdir()
        .unwrap();
    generate(&golden_dump(), &GenOptions::default(), sdk.path()).unwrap();
    let mut args = vec!["build", "--lib"];
    let joined = features.join(" ");
    if !features.is_empty() {
        args.extend(["--features", &joined]);
    }
    let run = run_cargo(sdk.path(), &args);
    assert!(
        !run.stderr.contains("warning:"),
        "warnings while building {features:?}:\n{}",
        run.stderr
    );
    std::fs::read(common::target_dir(sdk.path()).join("debug/ufathom_sdk.dll")).unwrap()
}

fn plain_sdk() -> &'static [u8] {
    static DLL: OnceLock<Vec<u8>> = OnceLock::new();
    DLL.get_or_init(|| sdk_dll(&[]))
}

fn hot_unload_sdk() -> &'static [u8] {
    static DLL: OnceLock<Vec<u8>> = OnceLock::new();
    DLL.get_or_init(|| sdk_dll(&["hot-unload"]))
}

fn proxy_sdk() -> &'static [u8] {
    static DLL: OnceLock<Vec<u8>> = OnceLock::new();
    DLL.get_or_init(|| sdk_dll(&["proxy-version"]))
}

/// A folder holding one SDK DLL, companion copies and the list file.
///
/// File names are unique per test: the loader treats two DLLs with one file
/// name as one module, and the events are named after the file.
struct Setup {
    root: tempfile::TempDir,
    sdk: PathBuf,
}

impl Setup {
    /// Puts `sdk` as `<root>\sdk\<name>` and writes `list` as the companion
    /// list there, unless it is `None`.
    fn new(sdk: &[u8], name: &str, list: Option<&str>) -> Self {
        let root = tempfile::Builder::new()
            .prefix("companion folder ")
            .tempdir()
            .unwrap();
        let folder = root.path().join("sdk");
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join(name);
        std::fs::write(&path, sdk).unwrap();
        if let Some(list) = list {
            std::fs::write(folder.join("ufathom-companions.txt"), list).unwrap();
        }
        Self { root, sdk: path }
    }

    /// The folder of the SDK DLL.
    fn folder(&self) -> PathBuf {
        self.sdk.parent().unwrap().to_owned()
    }

    /// Copies the companion test DLL to `relative` below the SDK folder, or
    /// below the root when `outside` is set. Returns the full path.
    fn add_companion(&self, relative: &str, outside: bool) -> PathBuf {
        let base = if outside {
            self.root.path().to_owned()
        } else {
            self.folder()
        };
        let path = base.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, fixture_dll()).unwrap();
        path
    }

    fn load_sdk(&self) -> *mut c_void {
        // SAFETY: a NUL-terminated full path.
        let module = unsafe { LoadLibraryW(wide(self.sdk.as_os_str()).as_ptr()) };
        assert!(!module.is_null(), "cannot load {}", self.sdk.display());
        module
    }
}

fn module_loaded(path: &Path) -> bool {
    let name = path.file_name().unwrap();
    // SAFETY: a NUL-terminated module name.
    !unsafe { GetModuleHandleW(wide(name).as_ptr()) }.is_null()
}

#[test]
fn listed_companions_load_in_a_plain_build() {
    let setup = Setup::new(
        plain_sdk(),
        "sdk_plain.dll",
        Some(
            "\u{feff}# the legacy mod and its helper\r\n\r\nplain_a.dll\r\nlegacy\\plain_b.dll\r\nafter-engine:plain_c.dll\r\n",
        ),
    );
    setup.add_companion("plain_a.dll", false);
    setup.add_companion("legacy/plain_b.dll", false);
    setup.add_companion("plain_c.dll", false);
    setup.load_sdk();

    assert!(companion_loaded("plain_a.dll"));
    assert!(companion_loaded("plain_b.dll"));
    // A build that does not wait for the engine loads these right after.
    assert!(companion_loaded("plain_c.dll"));
}

#[test]
fn without_a_list_nothing_is_loaded() {
    let setup = Setup::new(plain_sdk(), "sdk_nolist.dll", None);
    setup.add_companion("nolist_a.dll", false);
    setup.load_sdk();

    assert!(companion_stays_unloaded("nolist_a.dll"));
    assert!(module_loaded(&setup.sdk), "the SDK DLL itself must stay");
}

#[test]
fn a_refused_line_is_skipped_and_the_others_still_load() {
    let setup = Setup::new(plain_sdk(), "sdk_refused.dll", None);
    let escaped = setup.add_companion("refused_escaped.dll", true);
    let absolute = setup.add_companion("refused_absolute.dll", true);
    setup.add_companion("refused_ok.dll", false);
    let list = format!(
        "..\\refused_escaped.dll\r\n{}\r\nrefused_missing.dll\r\nbefore-engine:refused_ok.dll\r\nrefused_ok.dll\r\n",
        absolute.display()
    );
    std::fs::write(setup.folder().join("ufathom-companions.txt"), list).unwrap();
    setup.load_sdk();

    assert!(companion_loaded("refused_ok.dll"));
    // The refused lines come before the good one in the file, so they were
    // handled by now.
    assert!(!event_exists("refused_escaped.dll", ""));
    assert!(!event_exists("refused_absolute.dll", ""));
    assert!(!module_loaded(&escaped));
    assert!(!module_loaded(&absolute));
}

#[test]
fn a_proxy_build_holds_after_engine_companions_until_the_engine_is_up() {
    let setup = Setup::new(
        proxy_sdk(),
        "version.dll",
        Some("proxy_early.dll\nafter-engine:proxy_late.dll\n"),
    );
    setup.add_companion("proxy_early.dll", false);
    setup.add_companion("proxy_late.dll", false);
    setup.load_sdk();

    // The immediate one loads before the engine wait; this test process has
    // no engine, so the late one must not load.
    assert!(companion_loaded("proxy_early.dll"));
    assert!(companion_stays_unloaded("proxy_late.dll"));
}

/// Waits until the self-unloading SDK DLL has left the process.
fn wait_for_sdk_to_unload(setup: &Setup) {
    assert!(
        wait_until(LOAD_TIMEOUT, || !module_loaded(&setup.sdk)),
        "the hot-unload DLL did not unload"
    );
}

#[test]
fn hot_unload_keeps_companions_loaded_by_default() {
    let setup = Setup::new(
        hot_unload_sdk(),
        "sdk_keep.dll",
        Some("keep_a.dll\nkeep_b.dll\n"),
    );
    let first = setup.add_companion("keep_a.dll", false);
    let second = setup.add_companion("keep_b.dll", false);
    setup.load_sdk();

    assert!(companion_loaded("keep_a.dll"));
    assert!(companion_loaded("keep_b.dll"));
    wait_for_sdk_to_unload(&setup);
    std::thread::sleep(QUIET_PERIOD);
    assert!(module_loaded(&first) && module_loaded(&second));
    assert!(!event_exists("keep_a.dll", "-unloaded"));
    assert!(!event_exists("keep_b.dll", "-unloaded"));
}

#[test]
fn unload_on_exit_frees_the_companions_with_the_sdk_dll() {
    let setup = Setup::new(
        hot_unload_sdk(),
        "sdk_free.dll",
        Some("free_a.dll\nfree_b.dll\nunload-on-exit\n"),
    );
    let first = setup.add_companion("free_a.dll", false);
    let second = setup.add_companion("free_b.dll", false);
    setup.load_sdk();

    assert!(companion_loaded("free_a.dll"));
    assert!(companion_loaded("free_b.dll"));
    wait_for_sdk_to_unload(&setup);
    assert!(wait_until(LOAD_TIMEOUT, || {
        event_exists("free_a.dll", "-unloaded") && event_exists("free_b.dll", "-unloaded")
    }));
    assert!(!module_loaded(&first) && !module_loaded(&second));
}
