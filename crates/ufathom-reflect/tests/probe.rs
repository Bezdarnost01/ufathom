#![allow(
    clippy::print_stderr,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    missing_docs,
    reason = "probe"
)]
mod common;
use common::*;
use ufathom_reflect::{ReflectOptions, build_dump};
use ufathom_testkit::{Corruption, Profile};

#[test]
fn probe() {
    let profiles = [
        Profile::ue4_21_measured(),
        Profile::ue4_25_case_preserving_measured(),
        Profile::by_name("ue5.7").unwrap(),
    ];
    for profile in profiles {
        for c in Corruption::ALL {
            let fx = build_corrupt(profile.clone(), 7, 600, &[c]);
            let d = truth_discovery(&fx);
            match build_dump(&fx.memory, &d, &ReflectOptions::default()) {
                Err(e) => eprintln!("== {} {c:?}: ERR {e}", fx.profile.name),
                Ok(o) => {
                    eprintln!(
                        "== {} {c:?}: degraded {} failures {}/{} rereads {} structs {} enums {}",
                        fx.profile.name,
                        o.degraded,
                        o.stats.failures,
                        o.stats.objects,
                        o.stats.rereads,
                        o.dump.structs.len(),
                        o.dump.enums.len()
                    );
                    let mut shown = 0;
                    for dg in o
                        .dump
                        .diagnostics
                        .iter()
                        .filter(|d| d.severity == ufathom_model::Severity::Error)
                    {
                        if shown < 5 {
                            eprintln!("   {:?} {} :: {}", dg.severity, dg.path, dg.message);
                        }
                        shown += 1;
                    }
                    eprintln!(
                        "   total diags {} errors {}",
                        o.dump.diagnostics.len(),
                        shown
                    );
                }
            }
        }
    }
}
