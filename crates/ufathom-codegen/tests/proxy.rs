//! Real proxy DLLs: an SDK built with a `proxy-*` feature exports the same
//! names and ordinals as the system DLL, and calls made through it reach the
//! real functions.
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
use std::sync::Mutex;

use common::dumps::golden_dump;
use common::{PeExport, pe_export_table, run_cargo, run_cargo_unchecked};
use ufathom_codegen::{GenOptions, generate};

#[link(name = "kernel32")]
unsafe extern "system" {
    fn LoadLibraryW(name: *const u16) -> *mut c_void;
    fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
}

/// Builds share one target directory and one output file name, so only one
/// test at a time may build and copy.
static BUILD: Mutex<()> = Mutex::new(());

fn system_dll(name: &str) -> PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    PathBuf::from(root).join("System32").join(name)
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

/// A built proxy DLL, copied under the name of the system DLL into a folder
/// of its own. The folders live as long as this value.
struct Proxy {
    _sdk: tempfile::TempDir,
    _folder: tempfile::TempDir,
    dll: PathBuf,
}

/// Builds an SDK with `feature` and copies its DLL to `<folder>\<name>`.
///
/// The SDK folder has a space in its name, which the linker argument for the
/// `.def` file has to survive.
fn build_proxy(feature: &str, name: &str) -> Proxy {
    let _guard = BUILD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let sdk = tempfile::Builder::new()
        .prefix("proxy sdk ")
        .tempdir()
        .unwrap();
    generate(&golden_dump(), &GenOptions::default(), sdk.path()).unwrap();
    assert!(sdk.path().join("build.rs").is_file());
    assert!(
        sdk.path()
            .join("proxy")
            .join(name.replace(".dll", ".def"))
            .is_file()
    );
    let run = run_cargo(sdk.path(), &["build", "--lib", "--features", feature]);
    assert!(
        !run.stderr.contains("warning:"),
        "warnings while building {feature}:\n{}",
        run.stderr
    );
    let built = common::target_dir(sdk.path()).join("debug/ufathom_sdk.dll");
    let folder = tempfile::Builder::new()
        .prefix("proxy folder ")
        .tempdir()
        .unwrap();
    let dll = folder.path().join(name);
    std::fs::copy(&built, &dll).unwrap();
    Proxy {
        _sdk: sdk,
        _folder: folder,
        dll,
    }
}

/// The exports of the built DLL and of the system DLL it poses as, for the
/// export names and ordinals the system DLL has. The proxy may export more
/// (the Rust `DllMain`), never less.
fn assert_exports_match(proxy: &Proxy, system_name: &str) {
    let ours = pe_export_table(&std::fs::read(&proxy.dll).unwrap());
    let system = pe_export_table(&std::fs::read(system_dll(system_name)).unwrap());
    let missing: Vec<&PeExport> = system.iter().filter(|e| !ours.contains(e)).collect();
    assert!(
        missing.is_empty(),
        "the proxy does not export {missing:?}\nit exports {ours:?}"
    );
    let extra: Vec<&PeExport> = ours.iter().filter(|e| !system.contains(e)).collect();
    assert!(
        extra.iter().all(|e| e.name.as_deref() == Some("DllMain")),
        "the proxy exports more than the system DLL does: {extra:?}"
    );
}

/// Loads the proxy by full path. The proxy then loads the system DLL of the
/// same name by full path, which must give a different module.
fn load(proxy: &Proxy) -> *mut c_void {
    // SAFETY: a NUL-terminated full path.
    let module = unsafe { LoadLibraryW(wide(&proxy.dll).as_ptr()) };
    assert!(!module.is_null(), "cannot load {}", proxy.dll.display());
    module
}

fn export(module: *mut c_void, name: &str) -> usize {
    let name: Vec<u8> = name.bytes().chain(Some(0)).collect();
    // SAFETY: `module` is loaded and `name` is NUL-terminated.
    let address = unsafe { GetProcAddress(module, name.as_ptr()) };
    assert!(!address.is_null(), "{name:?} is not exported");
    address as usize
}

fn export_by_ordinal(module: *mut c_void, ordinal: usize) -> usize {
    // SAFETY: `module` is loaded; a pointer holding only a small value asks
    // for an ordinal.
    let address = unsafe { GetProcAddress(module, core::ptr::without_provenance(ordinal)) };
    assert!(!address.is_null(), "ordinal {ordinal} is not exported");
    address as usize
}

#[test]
fn the_version_proxy_exports_the_system_names_and_forwards_calls() {
    let proxy = build_proxy("proxy-version", "version.dll");
    assert_exports_match(&proxy, "version.dll");
    let module = load(&proxy);

    type SizeW = unsafe extern "system" fn(*const u16, *mut u32) -> u32;
    type InfoW = unsafe extern "system" fn(*const u16, u32, u32, *mut c_void) -> i32;
    type QueryW =
        unsafe extern "system" fn(*const c_void, *const u16, *mut *mut c_void, *mut u32) -> i32;
    // SAFETY: the exports have these signatures in the system version.dll.
    let (size_w, info_w, query_w) = unsafe {
        (
            core::mem::transmute::<usize, SizeW>(export(module, "GetFileVersionInfoSizeW")),
            core::mem::transmute::<usize, InfoW>(export(module, "GetFileVersionInfoW")),
            core::mem::transmute::<usize, QueryW>(export(module, "VerQueryValueW")),
        )
    };
    // The same function by ordinal is the same stub.
    assert_eq!(
        export_by_ordinal(module, 8),
        export(module, "GetFileVersionInfoSizeW")
    );

    let file = wide(&system_dll("kernel32.dll"));
    let mut handle = 0u32;
    // The first call resolves the export; the second uses the cached slot.
    for call in 0..2 {
        // SAFETY: a NUL-terminated path and a valid out pointer.
        let size = unsafe { size_w(file.as_ptr(), &raw mut handle) };
        assert!(size > 0, "no version resource on call {call}");
    }
    // SAFETY: as above.
    let size = unsafe { size_w(file.as_ptr(), &raw mut handle) };
    let mut block = vec![0u8; size as usize];
    // SAFETY: `block` holds `size` bytes.
    let ok = unsafe { info_w(file.as_ptr(), 0, size, block.as_mut_ptr().cast()) };
    assert_ne!(ok, 0);
    let root: Vec<u16> = "\\".encode_utf16().chain(Some(0)).collect();
    let mut info: *mut c_void = core::ptr::null_mut();
    let mut len = 0u32;
    // SAFETY: `block` was filled above; the out pointers are valid.
    let ok = unsafe {
        query_w(
            block.as_ptr().cast(),
            root.as_ptr(),
            &raw mut info,
            &raw mut len,
        )
    };
    assert_ne!(ok, 0);
    assert!(len >= 52, "VS_FIXEDFILEINFO is 52 bytes, got {len}");
    // SAFETY: `VerQueryValueW` pointed `info` into `block` at a VS_FIXEDFILEINFO.
    let signature = unsafe { info.cast::<u32>().read_unaligned() };
    assert_eq!(signature, 0xFEEF_04BD);
}

#[test]
fn the_dsound_proxy_exports_the_system_names_and_forwards_calls() {
    let proxy = build_proxy("proxy-dsound", "dsound.dll");
    assert_exports_match(&proxy, "dsound.dll");
    let module = load(&proxy);

    // `DirectSoundCreate` is imported by ordinal 1 by some games.
    assert_eq!(
        export_by_ordinal(module, 1),
        export(module, "DirectSoundCreate")
    );

    type Callback =
        unsafe extern "system" fn(*mut c_void, *const u16, *const u16, *mut c_void) -> i32;
    type EnumerateW = unsafe extern "system" fn(Callback, *mut c_void) -> i32;
    unsafe extern "system" fn count(
        _guid: *mut c_void,
        _description: *const u16,
        _module: *const u16,
        context: *mut c_void,
    ) -> i32 {
        // SAFETY: `context` is the `u32` the test passed.
        unsafe { *context.cast::<u32>() += 1 };
        1
    }
    // SAFETY: `DirectSoundEnumerateW` has this signature.
    let enumerate = unsafe {
        core::mem::transmute::<usize, EnumerateW>(export(module, "DirectSoundEnumerateW"))
    };
    for call in 0..2 {
        let mut devices = 0u32;
        // SAFETY: the callback and a pointer that outlives the call.
        let result = unsafe { enumerate(count, (&raw mut devices).cast()) };
        // S_OK, or a DSERR_* code on a machine with no audio device: the
        // call reached DirectSound either way.
        assert!(
            result == 0 || (result as u32) >> 16 == 0x8878,
            "call {call}: unexpected result {result:#x}"
        );
    }

    type CanUnload = unsafe extern "system" fn() -> i32;
    // SAFETY: `DllCanUnloadNow` has this signature.
    let can_unload =
        unsafe { core::mem::transmute::<usize, CanUnload>(export(module, "DllCanUnloadNow")) };
    // SAFETY: no arguments.
    let result = unsafe { can_unload() };
    assert!(
        result == 0 || result == 1,
        "DllCanUnloadNow gave {result:#x}"
    );
}

#[test]
fn the_dinput8_proxy_exports_the_system_names_and_forwards_calls() {
    let proxy = build_proxy("proxy-dinput8", "dinput8.dll");
    assert_exports_match(&proxy, "dinput8.dll");
    let module = load(&proxy);

    type CanUnload = unsafe extern "system" fn() -> i32;
    // SAFETY: `DllCanUnloadNow` has this signature.
    let can_unload =
        unsafe { core::mem::transmute::<usize, CanUnload>(export(module, "DllCanUnloadNow")) };
    for _ in 0..2 {
        // SAFETY: no arguments.
        let result = unsafe { can_unload() };
        assert!(
            result == 0 || result == 1,
            "DllCanUnloadNow gave {result:#x}"
        );
    }
}

#[test]
fn the_winmm_proxy_exports_the_system_names_and_ordinals() {
    let proxy = build_proxy("proxy-winmm", "winmm.dll");
    assert_exports_match(&proxy, "winmm.dll");
    let module = load(&proxy);

    // An export without a name is reachable by ordinal only.
    assert_ne!(export_by_ordinal(module, 2), 0);

    type TimeGetTime = unsafe extern "system" fn() -> u32;
    // SAFETY: `timeGetTime` has this signature.
    let time_get_time =
        unsafe { core::mem::transmute::<usize, TimeGetTime>(export(module, "timeGetTime")) };
    // SAFETY: no arguments.
    let (first, second) = unsafe { (time_get_time(), time_get_time()) };
    assert!(first > 0 && second >= first, "{first} then {second}");
}

#[test]
fn two_proxy_features_at_once_stop_the_build() {
    let _guard = BUILD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let sdk = tempfile::tempdir().unwrap();
    generate(&golden_dump(), &GenOptions::default(), sdk.path()).unwrap();
    let run = run_cargo_unchecked(
        sdk.path(),
        &["check", "--lib", "--features", "proxy-version proxy-dsound"],
    );
    assert!(!run.success, "two proxy features must not build");
    assert!(
        run.stderr.contains("enable only one `proxy-*` feature"),
        "{}",
        run.stderr
    );
}
