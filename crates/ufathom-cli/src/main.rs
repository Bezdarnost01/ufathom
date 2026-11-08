//! The `ufathom` command line tool.
//!
//! Every subcommand is a thin layer over the library crates: it opens a
//! target or reads a file, calls one library function and prints the result.
//! The only crate that prints, parses arguments or uses `anyhow` is this one.

#![forbid(unsafe_code)]
#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a command line tool: printing is its job"
)]

mod args;
mod commands;
mod error;
mod layout_file;
mod target;

use std::process::ExitCode;

use clap::Parser;
use tracing_subscriber::filter::LevelFilter;

use crate::args::Cli;

/// The log level for the `-v` and `-q` counts: only errors by default, one
/// step per flag.
///
/// The commands print what matters to the user themselves, warnings included,
/// so the libraries' own log lines would only repeat it.
fn log_level(verbose: u8, quiet: u8) -> LevelFilter {
    const LEVELS: [LevelFilter; 5] = [
        LevelFilter::OFF,
        LevelFilter::ERROR,
        LevelFilter::INFO,
        LevelFilter::DEBUG,
        LevelFilter::TRACE,
    ];
    const DEFAULT: usize = 1;
    let steps = usize::from(verbose).saturating_add(DEFAULT);
    let index = steps
        .saturating_sub(usize::from(quiet))
        .min(LEVELS.len() - 1);
    LEVELS.get(index).copied().unwrap_or(LevelFilter::ERROR)
}

fn main() -> ExitCode {
    // `clap` prints its own message and exits with 2 on a usage error, and
    // with 0 for --help and --version.
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_max_level(log_level(cli.verbose, cli.quiet))
        .with_writer(std::io::stderr)
        .without_time()
        .init();
    error::finish(commands::run(&cli.command))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test code may panic on failure"
)]
mod tests {
    use super::*;

    #[test]
    fn errors_are_the_default_and_each_flag_is_one_step() {
        assert_eq!(log_level(0, 0), LevelFilter::ERROR);
        assert_eq!(log_level(1, 0), LevelFilter::INFO);
        assert_eq!(log_level(2, 0), LevelFilter::DEBUG);
        assert_eq!(log_level(3, 0), LevelFilter::TRACE);
        assert_eq!(log_level(200, 0), LevelFilter::TRACE);
        assert_eq!(log_level(0, 1), LevelFilter::OFF);
        assert_eq!(log_level(0, 200), LevelFilter::OFF);
        assert_eq!(log_level(1, 1), LevelFilter::ERROR);
    }
}
