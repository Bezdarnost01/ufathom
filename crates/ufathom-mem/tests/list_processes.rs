//! `list_processes` against the machine's real process list.
//!
//! The test inspects only the entry for the current process.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code may panic on failure; clippy.toml only exempts #[test] functions, not helpers"
)]
use ufathom_mem::list_processes;

#[test]
fn lists_the_current_process_with_its_path() {
    let processes = list_processes().unwrap();
    assert!(processes.len() > 1);
    let me = processes
        .iter()
        .find(|process| process.pid == std::process::id())
        .expect("the current process is listed");
    let exe = std::env::current_exe().unwrap();
    let exe_name = exe.file_name().unwrap().to_string_lossy().to_lowercase();
    // Linux truncates the name to 15 characters.
    let name = me.name.to_lowercase();
    assert!(
        exe_name.starts_with(&name) || name.starts_with(&exe_name),
        "name {name:?} does not match {exe_name:?}"
    );
    let path = me.path.as_deref().expect("own path is readable");
    assert_eq!(
        std::fs::canonicalize(path).unwrap(),
        std::fs::canonicalize(&exe).unwrap()
    );
}

#[test]
fn every_entry_is_well_formed() {
    let processes = list_processes().unwrap();
    // Processes that deny access are listed with no path, not dropped.
    assert!(processes.iter().all(|process| !process.name.is_empty()));
    let mut pids: Vec<_> = processes.iter().map(|process| process.pid).collect();
    pids.sort_unstable();
    pids.dedup();
    assert_eq!(pids.len(), processes.len(), "pids are unique");
}
