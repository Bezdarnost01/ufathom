//! The subcommands.

mod control;
mod dump;
mod files;
mod inspect;
mod io;
mod probe;

use crate::args::Command;
use crate::error::Outcome;

/// Runs one subcommand.
pub fn run(command: &Command) -> Outcome {
    match command {
        Command::Ps(args) => inspect::ps(args),
        Command::InspectExe(args) => inspect::inspect(args),
        Command::Probe(args) => probe::probe(args),
        Command::Doctor(args) => probe::doctor(args),
        Command::Dump(args) => dump::dump(args),
        Command::Gen(args) => files::generate(args),
        Command::Usmap(args) => files::usmap(args),
        Command::CheckOracle(args) => files::check_oracle(args),
        Command::Diff(args) => files::diff_dumps(args),
        Command::Status(args) => control::status(args),
        Command::Send(args) => control::command(args),
    }
}
