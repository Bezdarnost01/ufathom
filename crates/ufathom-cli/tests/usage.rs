//! Usage errors, `--help` and the exit codes that need no target.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code may panic on failure"
)]

mod common;

use common::{arg, run};

#[test]
fn help_lists_every_subcommand_and_documents_the_exit_codes() {
    let help = run(&["--help"]).expect_code(0);
    for name in [
        "ps",
        "inspect-exe",
        "probe",
        "doctor",
        "dump",
        "gen",
        "usmap",
        "check-oracle",
        "diff",
        "status",
        "command",
    ] {
        assert!(
            help.stdout
                .lines()
                .any(|line| line.trim_start().starts_with(name)),
            "{name} is missing from --help:\n{}",
            help.stdout
        );
    }
    help.stdout_has("Exit codes:")
        .stdout_has("0  success")
        .stdout_has("1  failure")
        .stdout_has("2  usage error")
        .stdout_has("3  a dump finished but is degraded")
        .stdout_has("--pid N, --process NAME or --minidump PATH");
    // The program is called ufathom, not ufathom.exe, in its own usage line.
    help.stdout_has("Usage: ufathom ");
}

#[test]
fn the_version_flag_names_the_tool() {
    run(&["--version"]).expect_code(0).stdout_has("ufathom ");
}

#[test]
fn no_arguments_is_a_usage_error() {
    run::<&str>(&[]).expect_code(2).stderr_has("Usage:");
}

#[test]
fn an_unknown_subcommand_is_a_usage_error() {
    run(&["frobnicate"]).expect_code(2).stderr_has("frobnicate");
}

#[test]
fn a_target_is_required_and_only_one_may_be_given() {
    for command in ["probe", "doctor", "dump"] {
        let mut args = vec![command];
        if command == "dump" {
            args.extend(["-o", "out.json"]);
        }
        run(&args)
            .expect_code(2)
            .stderr_has("--pid")
            .stderr_has("--minidump");
    }
    run(&["probe", "--pid", "1", "--minidump", "x.dmp"])
        .expect_code(2)
        .stderr_has("cannot be used with");
    run(&["probe", "--pid", "1", "--process", "x"]).expect_code(2);
    run(&["probe", "--pid", "not-a-number"]).expect_code(2);
}

#[test]
fn a_bad_hint_is_a_usage_error_and_no_process_is_touched() {
    // pid 0 is never opened: the hint is checked first.
    let run = run(&["probe", "--pid", "0", "--hint", "banana"]).expect_code(2);
    run.stderr_has("--hint").stderr_has("4.25");
}

#[test]
fn the_two_runtime_flags_exclude_each_other() {
    let dir = tempfile::tempdir().unwrap();
    let dump = arg(&dir.path().join("d.json"));
    let out = arg(&dir.path().join("sdk"));
    run(&[
        "gen",
        &dump,
        "-o",
        &out,
        "--runtime-path",
        "a",
        "--runtime-version",
        "0.1",
    ])
    .expect_code(2)
    .stderr_has("cannot be used with");
}

#[test]
fn a_runtime_path_without_a_crate_is_a_usage_error() {
    let dir = tempfile::tempdir().unwrap();
    let dump = arg(&dir.path().join("d.json"));
    let out = arg(&dir.path().join("sdk"));
    run(&["gen", &dump, "-o", &out, "--runtime-path", &arg(dir.path())])
        .expect_code(2)
        .stderr_has("no Cargo.toml");
}

#[test]
fn dump_checks_its_arguments_before_it_touches_the_target() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("d.json");
    // pid 0 cannot be opened: reaching it would be exit code 1.
    let out_arg = arg(&out);
    let dump = |extra: &[&str]| {
        let mut args = vec!["dump", "--pid", "0", "-o", out_arg.as_str()];
        args.extend_from_slice(extra);
        run(&args)
    };
    dump(&["--runtime-version", "0.1"])
        .expect_code(2)
        .stderr_has("--sdk");
    dump(&["--threads", "0"])
        .expect_code(2)
        .stderr_has("--threads");
    assert!(!out.exists());
}

#[test]
fn command_arguments_are_checked_before_anything_is_written() {
    let dir = tempfile::tempdir().unwrap();
    let control = dir.path().join("control");
    for sub in ["inbox", "outbox"] {
        std::fs::create_dir_all(control.join(sub)).unwrap();
    }
    let out = arg(dir.path());
    run(&["command", &out, "bogus"]).expect_code(2);
    run(&["command", &out, "dump", "--formats", "xml"])
        .expect_code(2)
        .stderr_has("rust-sdk");
    run(&["command", &out, "status", "--label", "x"])
        .expect_code(2)
        .stderr_has("dump");
    run(&["command", &out, "status", "--wait", "-1"]).expect_code(2);
    run(&["command", &out, "status", "--wait", "nan"]).expect_code(2);
    assert_eq!(
        std::fs::read_dir(control.join("inbox")).unwrap().count(),
        0,
        "a refused request must not reach the inbox"
    );
}

#[test]
fn a_missing_file_is_a_failure_that_names_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("nothing.json");
    let name = "nothing.json";
    run(&["inspect-exe", &arg(&missing)])
        .expect_code(1)
        .stderr_has("cannot inspect")
        .stderr_has(name);
    run(&["usmap", &arg(&missing), "-o", &arg(&dir.path().join("o"))])
        .expect_code(1)
        .stderr_has("cannot read the dump file")
        .stderr_has(name);
    run(&["diff", &arg(&missing), &arg(&missing)])
        .expect_code(1)
        .stderr_has(name);
    run(&["status", &arg(dir.path())])
        .expect_code(1)
        .stderr_has("no control folder");
}

#[test]
fn a_file_that_is_not_a_dump_is_refused_with_the_reason() {
    let dir = tempfile::tempdir().unwrap();
    let wrong_version = dir.path().join("future.json");
    std::fs::write(&wrong_version, r#"{"format_version": 999}"#).unwrap();
    run(&[
        "usmap",
        &arg(&wrong_version),
        "-o",
        &arg(&dir.path().join("o")),
    ])
    .expect_code(1)
    .stderr_has("not a usable dump")
    .stderr_has("999");
    let junk = dir.path().join("junk.json");
    std::fs::write(&junk, "<html>").unwrap();
    run(&["diff", &arg(&junk), &arg(&junk)])
        .expect_code(1)
        .stderr_has("not a usable dump");
}
