//! The command line, as `clap` derive types.

use std::path::PathBuf;

use clap::{ArgAction, Args, Parser, Subcommand};

/// Text at the end of `--help`.
const EXIT_CODES: &str = "\
Exit codes:
  0  success
  1  failure: the target, a file or the pipeline failed (the message says what)
  2  usage error: bad arguments or a missing required option
  3  a dump finished but is degraded (dump accepts --allow-degraded)

A <target> is --pid N, --process NAME or --minidump PATH, plus the optional
--module, --layout and --hint. ufathom only reads: a process is opened with
read access and nothing is written to it.";

/// The `ufathom` command line.
#[derive(Debug, Parser)]
#[command(
    name = "ufathom",
    bin_name = "ufathom",
    version,
    about = "Dump an Unreal Engine game's reflection data and generate a Rust SDK from it",
    after_help = EXIT_CODES,
    arg_required_else_help = true
)]
pub struct Cli {
    /// More log output on stderr: -v for progress, -vv for detail, -vvv for everything.
    #[arg(short, long, action = ArgAction::Count, global = true)]
    pub verbose: u8,
    /// Less log output on stderr: -q for errors only, -qq for none.
    #[arg(short, long, action = ArgAction::Count, global = true)]
    pub quiet: u8,
    /// What to do.
    #[command(subcommand)]
    pub command: Command,
}

/// The subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// List processes and flag the probable Unreal Engine targets.
    ///
    /// A process is flagged when its executable file carries an engine version
    /// in its version resource, an engine build string, or an engine marker
    /// string. This is a hint, not proof. Executables under the Windows folder
    /// are not looked at.
    Ps(PsArgs),
    /// Show what a static look at an executable file finds, without running it.
    InspectExe(InspectArgs),
    /// Solve the engine layout of a target and print it with the evidence.
    Probe(ProbeArgs),
    /// Probe a target and print a health report: how well each field is
    /// established, every warning, and what is missing.
    Doctor(DoctorArgs),
    /// Dump a target's types to a JSON file, and optionally a usmap and a Rust SDK.
    Dump(DumpArgs),
    /// Generate a Rust SDK crate from a dump file.
    Gen(GenArgs),
    /// Write a usmap file from a dump file.
    Usmap(UsmapArgs),
    /// Check a dump file against a usmap file made by another tool.
    CheckOracle(CheckOracleArgs),
    /// Compare two dump files for porting between builds.
    Diff(DiffArgs),
    /// Read the status file of a running ufathom.dll.
    Status(StatusArgs),
    /// Send a command to a running ufathom.dll.
    #[command(name = "command")]
    Send(CommandArgs),
}

/// `ufathom ps`.
#[derive(Debug, Args)]
pub struct PsArgs {
    /// List every process, not only the probable Unreal Engine targets.
    #[arg(long)]
    pub all: bool,
}

/// `ufathom inspect-exe`.
#[derive(Debug, Args)]
pub struct InspectArgs {
    /// The executable file.
    pub file: PathBuf,
    /// Print JSON instead of text.
    #[arg(long)]
    pub json: bool,
}

/// How to pick the target. Exactly one is required.
#[derive(Debug, Args)]
#[group(required = true, multiple = false)]
pub struct Selector {
    /// Process id of a running process.
    #[arg(long, value_name = "N")]
    pub pid: Option<u32>,
    /// Executable name of a running process, such as `Game.exe`. Refused when
    /// several processes match.
    #[arg(long, value_name = "NAME")]
    pub process: Option<String>,
    /// A full-memory minidump file.
    #[arg(long, value_name = "PATH")]
    pub minidump: Option<PathBuf>,
}

/// A target and the settings that steer discovery.
#[derive(Debug, Args)]
pub struct TargetArgs {
    /// Which target.
    #[command(flatten)]
    pub selector: Selector,
    /// File name of the module that holds the engine globals. Defaults to the
    /// first .exe module.
    #[arg(long, value_name = "NAME")]
    pub module: Option<String>,
    /// A TOML or JSON file with `[overrides]` (dotted field names and offsets)
    /// and `pointer_transform` (steps that decode the object array pointer).
    #[arg(long, value_name = "FILE")]
    pub layout: Option<PathBuf>,
    /// An engine version you believe the target is, such as 4.25. It only
    /// orders candidates; the evidence still decides.
    #[arg(long, value_name = "VERSION")]
    pub hint: Option<String>,
}

/// `ufathom probe`.
#[derive(Debug, Args)]
pub struct ProbeArgs {
    /// The target.
    #[command(flatten)]
    pub target: TargetArgs,
    /// Print JSON instead of text.
    #[arg(long)]
    pub json: bool,
}

/// `ufathom doctor`.
#[derive(Debug, Args)]
pub struct DoctorArgs {
    /// The target.
    #[command(flatten)]
    pub target: TargetArgs,
}

/// Where the generated crate finds `ufathom-runtime`. Nothing is published
/// yet, so one of the two must be given.
#[derive(Debug, Args)]
#[group(multiple = false)]
pub struct RuntimeArgs {
    /// Depend on `ufathom-runtime` at this path (the `crates/ufathom-runtime`
    /// folder of a checkout).
    #[arg(long, value_name = "PATH")]
    pub runtime_path: Option<PathBuf>,
    /// Depend on `ufathom-runtime` at this version requirement from a registry.
    #[arg(long, value_name = "VERSION")]
    pub runtime_version: Option<String>,
}

/// `ufathom dump`.
#[derive(Debug, Args)]
pub struct DumpArgs {
    /// The target.
    #[command(flatten)]
    pub target: TargetArgs,
    /// Where to write the dump (JSON). Missing folders are created and an
    /// existing file is replaced.
    #[arg(short, long, value_name = "FILE")]
    pub output: PathBuf,
    /// Also write a usmap file.
    #[arg(long, value_name = "FILE")]
    pub usmap: Option<PathBuf>,
    /// Also generate a Rust SDK crate into this folder. Needs --runtime-path
    /// or --runtime-version.
    #[arg(long, value_name = "DIR")]
    pub sdk: Option<PathBuf>,
    /// The runtime dependency of the SDK.
    #[command(flatten)]
    pub runtime: RuntimeArgs,
    /// Threads used to read the target. Defaults to the number of CPUs.
    #[arg(long, value_name = "N")]
    pub threads: Option<usize>,
    /// Exit with 0 even when the dump is degraded (more than 5% of the objects
    /// could not be dumped). The files are written either way.
    #[arg(long)]
    pub allow_degraded: bool,
}

/// `ufathom gen`.
#[derive(Debug, Args)]
pub struct GenArgs {
    /// The dump file.
    pub dump: PathBuf,
    /// The folder of the generated crate.
    #[arg(short, long, value_name = "DIR")]
    pub output: PathBuf,
    /// The runtime dependency of the SDK.
    #[command(flatten)]
    pub runtime: RuntimeArgs,
}

/// `ufathom usmap`.
#[derive(Debug, Args)]
pub struct UsmapArgs {
    /// The dump file.
    pub dump: PathBuf,
    /// Where to write the usmap file.
    #[arg(short, long, value_name = "FILE")]
    pub output: PathBuf,
}

/// `ufathom check-oracle`.
#[derive(Debug, Args)]
pub struct CheckOracleArgs {
    /// The dump file.
    pub dump: PathBuf,
    /// The usmap file to check it against.
    pub usmap: PathBuf,
    /// Print JSON instead of text.
    #[arg(long)]
    pub json: bool,
}

/// `ufathom diff`.
#[derive(Debug, Args)]
pub struct DiffArgs {
    /// The older dump file.
    pub old: PathBuf,
    /// The newer dump file.
    pub new: PathBuf,
    /// Print JSON instead of text.
    #[arg(long)]
    pub json: bool,
}

/// `ufathom status`.
#[derive(Debug, Args)]
pub struct StatusArgs {
    /// The output folder of the DLL (the one that holds `control/`).
    pub output_dir: PathBuf,
}

/// What `ufathom command` can send.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum CommandName {
    /// Ask for the status document.
    Status,
    /// Start a dump.
    Dump,
    /// Cancel any dump and unload the DLL.
    Stop,
}

impl CommandName {
    /// The name the DLL's command files use.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Dump => "dump",
            Self::Stop => "stop",
        }
    }
}

/// `ufathom command`.
#[derive(Debug, Args)]
pub struct CommandArgs {
    /// The output folder of the DLL (the one that holds `control/`).
    pub output_dir: PathBuf,
    /// The command.
    pub command: CommandName,
    /// The name of the dump folder (`dump` only).
    #[arg(long, value_name = "LABEL")]
    pub label: Option<String>,
    /// Formats the dump writes (`dump` only): json, usmap, rust-sdk. Defaults
    /// to the DLL's config.
    #[arg(long, value_name = "FORMAT", value_delimiter = ',')]
    pub formats: Vec<String>,
    /// Wait up to this many seconds for the result, and print it. Without it
    /// the command is queued and ufathom returns at once.
    #[arg(long, value_name = "SECONDS")]
    pub wait: Option<f64>,
}
