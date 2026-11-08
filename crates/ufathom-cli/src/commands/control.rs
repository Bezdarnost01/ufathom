//! `ufathom status` and `ufathom command`: talking to a running ufathom.dll
//! through its control folder.
//!
//! The file protocol and its types belong to `ufathom-agent`; this module
//! only writes a request file and reads the documents back.

use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, anyhow};
use serde_json::json;
use ufathom_agent::protocol::{CommandResult, ResultStatus, State, StatusDocument, parse_request};
use ufathom_agent::{ControlDir, Format, fsio};

use crate::args::{CommandArgs, CommandName, StatusArgs};
use crate::error::{CliError, Outcome, usage};

/// How often `--wait` looks for the result.
const POLL: Duration = Duration::from_millis(100);

/// The control folder of `output_dir`, which must exist.
fn control_of(output_dir: &Path) -> Result<ControlDir, CliError> {
    let control = ControlDir::new(output_dir);
    if !control.root().is_dir() {
        return Err(CliError::Failure(anyhow!(
            "there is no control folder at {}; give the output folder of the running ufathom.dll \
             (the one that holds control/), and check that the DLL is loaded",
            control.root().display()
        )));
    }
    Ok(control)
}

/// `ufathom status`.
pub fn status(args: &StatusArgs) -> Outcome {
    let control = control_of(&args.output_dir)?;
    let path = control.status_path();
    let text = std::fs::read_to_string(&path).map_err(|error| {
        anyhow!(
            "cannot read the status file {}: {error}; the DLL writes it when it starts, so it may not be loaded",
            path.display()
        )
    })?;
    let document: StatusDocument = serde_json::from_str(&text)
        .with_context(|| format!("{} is not a status document", path.display()))?;
    print_status(&document);
    Ok(())
}

fn print_status(document: &StatusDocument) {
    println!("state:         {}", state_name(document.state));
    println!("process:       {}", document.pid);
    println!("session:       {}", document.session);
    println!("tool version:  {}", document.tool_version);
    println!("output:        {}", document.output);
    println!(
        "dumps:         {} completed, {} failed",
        document.dumps_completed, document.dumps_failed
    );
    if let Some(current) = &document.current {
        println!(
            "running:       {} (since {})",
            current.label, current.started
        );
    }
    if let Some(last) = &document.last_dump {
        println!(
            "last dump:     {} ended {}: {}{}",
            last.label,
            last.finished,
            last.result,
            last.folder
                .as_deref()
                .map_or_else(String::new, |folder| format!(" in {folder}"))
        );
    }
    if let Some(detail) = &document.detail {
        println!("note:          {detail}");
    }
    println!("updated:       {}", document.updated);
}

/// The request file text for `args`, checked with the agent's own parser so a
/// request the DLL would refuse never leaves this process.
fn request_text(id: &str, args: &CommandArgs) -> Result<String, CliError> {
    let name = args.command.name();
    let is_dump = args.command == CommandName::Dump;
    if !is_dump && (args.label.is_some() || !args.formats.is_empty()) {
        return Err(usage(
            "--label and --formats only apply to the dump command",
        ));
    }
    let mut request = serde_json::Map::new();
    request.insert("id".to_owned(), json!(id));
    request.insert("command".to_owned(), json!(name));
    if let Some(label) = &args.label {
        request.insert("label".to_owned(), json!(label));
    }
    if !args.formats.is_empty() {
        let mut formats = Vec::new();
        for text in &args.formats {
            let Some(format) = Format::from_name(text) else {
                return Err(usage(format!(
                    "unknown format {text:?}: use json, usmap or rust-sdk"
                )));
            };
            formats.push(format.name());
        }
        request.insert("formats".to_owned(), json!(formats));
    }
    let text = serde_json::Value::Object(request).to_string();
    parse_request(&text).map_err(|error| usage(error.message))?;
    Ok(text)
}

/// A request id no earlier run of this tool used.
fn new_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis());
    format!("cli-{}-{millis}", std::process::id())
}

/// `ufathom command`.
pub fn command(args: &CommandArgs) -> Outcome {
    let wait = match args.wait {
        None => None,
        Some(seconds) => Some(
            Duration::try_from_secs_f64(seconds)
                .map_err(|_| usage("--wait must be a number of seconds, 0 or more"))?,
        ),
    };
    let id = new_id();
    let text = request_text(&id, args)?;
    let control = control_of(&args.output_dir)?;
    let inbox = control.inbox();
    if !inbox.is_dir() {
        return Err(CliError::Failure(anyhow!(
            "there is no inbox at {}; is the DLL's control folder complete?",
            inbox.display()
        )));
    }
    let request_path = inbox.join(format!("{id}.json"));
    fsio::write_atomic(&request_path, text.as_bytes())
        .with_context(|| format!("cannot write the command to {}", request_path.display()))?;

    let result_path = control.outbox().join(format!("{id}.json"));
    let Some(wait) = wait else {
        println!("queued `{}` as {id}", args.command.name());
        println!("the result will be {}", result_path.display());
        return Ok(());
    };

    let result = wait_for_result(&result_path, wait).map_err(|error| {
        // Where the request is tells why there is no answer.
        let why = if request_path.exists() {
            "the request is still in the inbox, so the DLL is not reading it: it may not be loaded, or may have stopped"
        } else {
            "the DLL took the request but has not answered; a dump may still be running"
        };
        CliError::Failure(anyhow!(
            "{error} within {:.0} s; {why}. `ufathom status` shows the DLL's state",
            wait.as_secs_f64()
        ))
    })?;
    print_result(&result);
    judge(&result)
}

/// Waits for the result file, up to `limit`.
fn wait_for_result(path: &Path, limit: Duration) -> Result<CommandResult, String> {
    let started = Instant::now();
    loop {
        // The DLL publishes by renaming, so a file that is there is whole. One
        // that does not parse is read again until the time is up.
        let attempt = std::fs::read_to_string(path)
            .map_err(|error| error.to_string())
            .and_then(|text| {
                serde_json::from_str::<CommandResult>(&text).map_err(|error| error.to_string())
            });
        match attempt {
            Ok(result) => return Ok(result),
            Err(reason) if started.elapsed() >= limit => {
                return Err(if path.exists() {
                    format!("the result {} is not readable ({reason})", path.display())
                } else {
                    "no result arrived".to_owned()
                });
            }
            Err(_) => std::thread::sleep(POLL.min(limit.saturating_sub(started.elapsed()))),
        }
    }
}

fn print_result(result: &CommandResult) {
    println!(
        "result:        {} ({})",
        status_name(result.status),
        result.command.as_deref().unwrap_or("unknown command")
    );
    println!("state:         {}", state_name(result.state));
    if let Some(message) = &result.message {
        println!("message:       {message}");
    }
    if let Some(dump) = &result.dump {
        println!("folder:        {}", dump.folder);
        println!("files:         {}", dump.files.join(", "));
        println!(
            "engine:        {}",
            dump.engine
                .as_deref()
                .unwrap_or("not claimed by the binary")
        );
        println!(
            "types:         {} classes and structs, {} enums, {} functions from {} objects",
            dump.structs, dump.enums, dump.functions, dump.objects
        );
        println!(
            "problems:      {} objects failed, {} diagnostics; degraded: {}",
            dump.failures,
            dump.diagnostics,
            if dump.degraded { "YES" } else { "no" }
        );
        if let Some(sdk) = &dump.sdk {
            println!(
                "SDK:           {} packages, {} classes, {} script structs, {} enums, {} functions, {} items not exact, {} files",
                sdk.packages,
                sdk.classes,
                sdk.script_structs,
                sdk.enums,
                sdk.functions,
                sdk.degraded,
                sdk.files
            );
        }
        println!("time:          {} ms", dump.elapsed_ms);
    }
    if let Some(failure) = &result.failure {
        println!("failure:       {}: {}", failure.kind, failure.message);
        if let Some(folder) = &failure.folder {
            println!("record in:     {folder}");
        }
    }
    if let Some(document) = &result.status_document {
        print_status(document);
    }
}

/// The exit code for what the DLL answered.
fn judge(result: &CommandResult) -> Outcome {
    match result.status {
        ResultStatus::Ok => {
            if result.dump.as_ref().is_some_and(|dump| dump.degraded) {
                Err(CliError::Degraded(
                    "the dump is degraded: more than 5% of the objects could not be dumped. \
                     The files were written; read the diagnostics before trusting them"
                        .to_owned(),
                ))
            } else {
                Ok(())
            }
        }
        ResultStatus::Error | ResultStatus::Busy | ResultStatus::Cancelled => {
            Err(CliError::Failure(anyhow!(
                "the DLL answered `{}`{}",
                status_name(result.status),
                result
                    .message
                    .as_deref()
                    .map_or_else(String::new, |message| format!(": {message}"))
            )))
        }
    }
}

/// The name of a result status, as the DLL writes it.
fn status_name(status: ResultStatus) -> &'static str {
    match status {
        ResultStatus::Ok => "ok",
        ResultStatus::Error => "error",
        ResultStatus::Busy => "busy",
        ResultStatus::Cancelled => "cancelled",
    }
}

/// The name of a state, as the DLL writes it.
fn state_name(state: State) -> &'static str {
    match state {
        State::Ready => "ready",
        State::Dumping => "dumping",
        State::Idle => "idle",
        State::Stopping => "stopping",
        State::Stopped => "stopped",
    }
}
