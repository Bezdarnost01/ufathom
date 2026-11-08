//! How a command ends, and the exit code that goes with it.

use std::process::ExitCode;

/// Why a command did not simply succeed.
#[derive(Debug)]
pub enum CliError {
    /// The arguments are wrong in a way `clap` cannot see. Exit code 2.
    Usage(String),
    /// The work failed. Exit code 1.
    Failure(anyhow::Error),
    /// A dump was made but is degraded, and the user did not accept that.
    /// Exit code 3.
    Degraded(String),
}

impl From<anyhow::Error> for CliError {
    fn from(error: anyhow::Error) -> Self {
        Self::Failure(error)
    }
}

/// A usage error with this message.
pub fn usage(message: impl Into<String>) -> CliError {
    CliError::Usage(message.into())
}

/// What every command returns.
pub type Outcome = Result<(), CliError>;

/// Exit code for success.
const EXIT_OK: u8 = 0;
/// Exit code for a failure.
const EXIT_FAILURE: u8 = 1;
/// Exit code for a usage error. It is also what `clap` uses for its own.
const EXIT_USAGE: u8 = 2;
/// Exit code for a degraded dump.
const EXIT_DEGRADED: u8 = 3;

/// Prints the error on stderr and returns the exit code for the outcome.
pub fn finish(outcome: Outcome) -> ExitCode {
    match outcome {
        Ok(()) => ExitCode::from(EXIT_OK),
        Err(CliError::Usage(message)) => {
            eprintln!("error: {message}\n\nFor more information, try '--help'.");
            ExitCode::from(EXIT_USAGE)
        }
        Err(CliError::Failure(error)) => {
            eprintln!("error: {error:#}");
            ExitCode::from(EXIT_FAILURE)
        }
        Err(CliError::Degraded(message)) => {
            eprintln!("warning: {message}");
            ExitCode::from(EXIT_DEGRADED)
        }
    }
}
