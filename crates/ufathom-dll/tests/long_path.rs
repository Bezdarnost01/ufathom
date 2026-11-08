//! A folder path over 260 characters works end to end.

#![cfg(windows)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code may panic on failure"
)]

mod common;

use common::{
    COMMAND_TIMEOUT, DUMP_TIMEOUT, Host, HostSpec, Sandbox, UNLOAD_TIMEOUT,
    assert_dump_matches_truth, dump_folder, read_json,
};
use ufathom_agent::paths::extended_path;

/// The temporary folder of a test with enough nested folders that every file
/// under it has a path over 260 characters.
fn long_output() -> std::path::PathBuf {
    let mut path = std::env::temp_dir().join("ufathom-long-path-test");
    for index in 0.. {
        if path.as_os_str().len() > 280 {
            break;
        }
        path.push(format!("a-deliberately-long-folder-name-{index:02}"));
    }
    path
}

#[test]
fn a_dump_works_from_an_output_path_over_260_characters() {
    let output = long_output();
    assert!(output.as_os_str().len() > 260);
    let sandbox = Sandbox::with_output(output.clone(), "");
    let mut host = Host::spawn(&HostSpec::new(300), &[&sandbox.dll], false);
    let control = sandbox.control();
    control.wait_state("ready", COMMAND_TIMEOUT);

    control.send("d", "dump", "");
    let result = control.wait_result("d", DUMP_TIMEOUT);
    assert_eq!(result["status"], "ok", "{result}");
    let folder = dump_folder(&result);
    assert!(
        folder.join("dump.json").as_os_str().len() > 260,
        "the test path is not long enough: {}",
        folder.display()
    );
    assert_dump_matches_truth(&host.local_fixture(), &folder);
    for file in ["Mappings.usmap", "identity.json", "ufathom.log"] {
        assert!(extended_path(&folder.join(file)).is_file(), "{file}");
    }
    assert_eq!(read_json(&folder.join("identity.json"))["result"], "ok");

    control.send("halt", "stop", "");
    control.wait_state("stopped", UNLOAD_TIMEOUT);
    host.wait_unloaded(&sandbox.dll, UNLOAD_TIMEOUT);

    // The sandbox does not own this folder, so remove it here.
    let root = std::env::temp_dir().join("ufathom-long-path-test");
    let _ = std::fs::remove_dir_all(extended_path(&root));
}
