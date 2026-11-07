//! Damaged images: the host keeps running, the dump ends with a result or a
//! named error in the outbox, and the DLL still unloads cleanly.

#![cfg(windows)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::print_stdout,
    reason = "test code may panic on failure and prints what each corruption led to"
)]

mod common;

use std::fs;

use common::{
    COMMAND_TIMEOUT, DUMP_TIMEOUT, Host, HostSpec, Sandbox, UNLOAD_TIMEOUT, dump_folder, read_json,
};
use ufathom_agent::paths::extended_path;
use ufathom_model::Dump;
use ufathom_testkit::Corruption;

fn check(corruption: Corruption) {
    let name = format!("{corruption:?}");
    let sandbox = Sandbox::new("");
    let mut spec = HostSpec::new(300);
    spec.corruptions = vec![corruption];
    let mut host = Host::spawn(&spec, &[&sandbox.dll], false);
    let control = sandbox.control();
    control.wait_state("ready", COMMAND_TIMEOUT);
    let before = host.report();

    control.send("d", "dump", "");
    let result = control.wait_result("d", DUMP_TIMEOUT);
    match result["status"].as_str().unwrap() {
        "ok" => {
            // A dump that was possible is complete and well formed.
            let folder = dump_folder(&result);
            let json = fs::read_to_string(extended_path(&folder.join("dump.json"))).unwrap();
            let dump = Dump::from_json(&json)
                .unwrap_or_else(|error| panic!("{name}: dump.json does not parse: {error}"));
            assert!(
                !dump.structs.is_empty(),
                "{name}: an empty dump is not a result"
            );
            let identity = read_json(&folder.join("identity.json"));
            assert!(
                identity["result"] == "ok" || identity["result"] == "degraded",
                "{name}: {identity}"
            );
        }
        "error" => {
            // A named error: a stable kind and a reason that says what failed.
            let failure = &result["failure"];
            let kind = failure["kind"].as_str().unwrap_or_default();
            assert!(
                ["discovery", "reflect", "output", "panic"].contains(&kind),
                "{name}: unnamed failure {result}"
            );
            assert!(
                failure["message"].as_str().is_some_and(|m| m.len() > 10),
                "{name}: the error does not say what failed: {result}"
            );
            if let Some(folder) = failure["folder"].as_str() {
                let identity = read_json(&std::path::Path::new(folder).join("identity.json"));
                assert_eq!(identity["result"], "failed", "{name}");
            }
        }
        other => panic!("{name}: unexpected status {other}: {result}"),
    }
    println!(
        "{name}: {} {}",
        result["status"],
        result["failure"]["message"]
            .as_str()
            .or_else(|| result["dump"]["folder"].as_str())
            .unwrap_or_default()
    );

    // The host never noticed.
    assert!(host.is_running(), "{name}: the host died");
    let during = host.report();
    assert!(
        during.ticks > before.ticks,
        "{name}: the host stopped ticking"
    );
    // The status says the DLL is alive and idle, then it unloads on request.
    assert_eq!(control.status().unwrap()["state"], "idle", "{name}");
    control.send("halt", "stop", "");
    control.wait_state("stopped", UNLOAD_TIMEOUT);
    let after = host.wait_unloaded(&sandbox.dll, UNLOAD_TIMEOUT);
    assert!(
        after.dll_threads(&sandbox.dll).is_empty(),
        "{name}: a thread of the DLL is left: {after:?}"
    );
    assert!(host.is_running(), "{name}: the host died while unloading");
    assert!(
        host.close_and_wait(),
        "{name}: the host did not exit cleanly"
    );
}

macro_rules! corruption_tests {
    ($($test:ident => $variant:ident),* $(,)?) => {
        $(
            #[test]
            fn $test() {
                check(Corruption::$variant);
            }
        )*

        #[test]
        fn every_corruption_has_a_test() {
            let tested = [$(Corruption::$variant),*];
            for corruption in Corruption::ALL {
                assert!(
                    tested.contains(&corruption),
                    "{corruption:?} has no DLL test: add it to the list"
                );
            }
        }
    };
}

corruption_tests! {
    null_object_slots => NullObjectSlots,
    class_chain_cycle => ClassChainCycle,
    super_chain_cycle => SuperChainCycle,
    property_list_cycle => PropertyListCycle,
    huge_array_counts => HugeArrayCounts,
    dangling_pointers => DanglingPointers,
    unmapped_names_block => UnmappedNamesBlock,
    name_index_out_of_range => NameIndexOutOfRange,
    non_utf8_names => NonUtf8Names,
    truncated_chunk_table => TruncatedChunkTable,
    zeroed_object_headers => ZeroedObjectHeaders,
    wrong_element_size => WrongElementSize,
    overlapping_field => OverlappingField,
    object_freed_and_reused => ObjectFreedAndReused,
}
