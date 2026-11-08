//! The command files a client drops into the inbox, and the documents the
//! DLL publishes in return.
//!
//! # Commands
//!
//! A command is a JSON object in `<output>/control/inbox/<anything>.json`:
//!
//! ```json
//! { "id": "run-1", "command": "dump", "label": "menu", "formats": ["json"] }
//! ```
//!
//! - `id`: required. 1 to 100 characters from `A-Z a-z 0-9 . _ -`, not
//!   starting with a dot. It names the result file.
//! - `command`: `status`, `dump` or `stop`, in any letter case.
//! - `label` (`dump` only): the name of the dump folder. Sanitized.
//! - `formats` (`dump` only): a list of `json` and `usmap`. Defaults to the
//!   config's formats.
//!
//! Unknown extra fields are ignored so a newer client can talk to an older
//! DLL. The client should write the file under a temporary name and rename
//! it, since a half-written file is read as malformed once it is a moment
//! old.
//!
//! # Results
//!
//! `<output>/control/outbox/<id>.json` holds a [`CommandResult`]. A `dump`
//! result is published when the dump ends, not when it starts; a client
//! waits for the file. `status` and `stop` are answered at once.

use serde::Serialize;
use serde_json::Value;

use crate::config::Format;

/// Longest accepted command id.
pub const MAX_ID_LEN: usize = 100;
/// Longest accepted dump label before sanitizing.
pub const MAX_LABEL_LEN: usize = 200;
/// Largest command file that is read.
pub const MAX_COMMAND_BYTES: u64 = 64 * 1024;

/// What a client can ask for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    /// Report the current status.
    Status,
    /// Run a dump.
    Dump {
        /// Folder name for the dump, before sanitizing.
        label: Option<String>,
        /// Formats to write. `None` uses the config's.
        formats: Option<Vec<Format>>,
    },
    /// Cancel any running dump and unload the DLL.
    Stop,
}

impl Command {
    /// The command's name as written in a command file.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Dump { .. } => "dump",
            Self::Stop => "stop",
        }
    }
}

/// A well-formed command with its id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// The client's id, validated.
    pub id: String,
    /// What was asked.
    pub command: Command,
}

/// Why a command file is not a command. It becomes an error result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestError {
    /// The id, when it could be read and is valid, so the result can be
    /// matched to the request.
    pub id: Option<String>,
    /// The command name, when it could be read.
    pub command: Option<String>,
    /// What is wrong.
    pub message: String,
}

/// Parses the text of a command file.
///
/// # Errors
///
/// [`RequestError`] for anything that is not valid JSON, not an object, has
/// no valid `id`, an unknown or missing `command`, or a malformed `label` or
/// `formats`.
pub fn parse_request(text: &str) -> Result<Request, RequestError> {
    let fail = |id: Option<&str>, command: Option<&str>, message: String| RequestError {
        id: id.map(str::to_owned),
        command: command.map(str::to_owned),
        message,
    };
    let value: Value = serde_json::from_str(text)
        .map_err(|error| fail(None, None, format!("not valid JSON: {error}")))?;
    let Some(object) = value.as_object() else {
        return Err(fail(
            None,
            None,
            "a command must be a JSON object".to_owned(),
        ));
    };

    let id = match object.get("id") {
        Some(Value::String(id)) => id.clone(),
        Some(Value::Number(number)) if number.is_u64() => number.to_string(),
        Some(_) => return Err(fail(None, None, "`id` must be a string".to_owned())),
        None => return Err(fail(None, None, "`id` is missing".to_owned())),
    };
    if !is_valid_id(&id) {
        return Err(fail(
            None,
            None,
            format!(
                "`id` must be 1 to {MAX_ID_LEN} characters from A-Z a-z 0-9 . _ - and must not start with a dot"
            ),
        ));
    }

    let name = match object.get("command") {
        Some(Value::String(name)) => name.trim().to_ascii_lowercase(),
        Some(_) => {
            return Err(fail(
                Some(&id),
                None,
                "`command` must be a string".to_owned(),
            ));
        }
        None => return Err(fail(Some(&id), None, "`command` is missing".to_owned())),
    };
    let command = match name.as_str() {
        "status" => Command::Status,
        "stop" => Command::Stop,
        "dump" => parse_dump(object).map_err(|message| fail(Some(&id), Some("dump"), message))?,
        other => {
            return Err(fail(
                Some(&id),
                Some(other),
                format!("unknown command `{other}`: use status, dump or stop"),
            ));
        }
    };
    Ok(Request { id, command })
}

fn parse_dump(object: &serde_json::Map<String, Value>) -> Result<Command, String> {
    let label = match object.get("label") {
        None | Some(Value::Null) => None,
        Some(Value::String(label)) if label.chars().count() <= MAX_LABEL_LEN => Some(label.clone()),
        Some(Value::String(_)) => {
            return Err(format!("`label` is longer than {MAX_LABEL_LEN} characters"));
        }
        Some(_) => return Err("`label` must be a string".to_owned()),
    };
    let formats = match object.get("formats") {
        None | Some(Value::Null) => None,
        Some(Value::Array(items)) => {
            let mut formats = Vec::new();
            for item in items {
                let format = match item.as_str().map(str::to_ascii_lowercase).as_deref() {
                    Some("json") => Format::Json,
                    Some("usmap") => Format::Usmap,
                    _ => return Err(format!("unknown format {item}: use json or usmap")),
                };
                if !formats.contains(&format) {
                    formats.push(format);
                }
            }
            if formats.is_empty() {
                return Err("`formats` must name at least one format".to_owned());
            }
            Some(formats)
        }
        Some(_) => return Err("`formats` must be a list".to_owned()),
    };
    Ok(Command::Dump { label, formats })
}

/// Whether `id` is acceptable as a command id and a result file name.
#[must_use]
pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && !id.starts_with('.')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// Where the DLL is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Started and waiting, no dump yet.
    Ready,
    /// A dump is running.
    Dumping,
    /// At least one dump has ended, waiting.
    Idle,
    /// A stop was requested and the DLL is shutting down.
    Stopping,
    /// Everything is closed. The DLL may already be gone.
    Stopped,
}

/// The outcome class of a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ResultStatus {
    /// The command did what was asked.
    Ok,
    /// The command was refused or failed. See `message`.
    Error,
    /// A dump is already running.
    Busy,
    /// A dump was cancelled before it finished.
    Cancelled,
}

/// What a finished dump produced.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DumpSummary {
    /// The folder, in the form a user can open.
    pub folder: String,
    /// Names of the files written into it.
    pub files: Vec<String>,
    /// The engine version the binary claims, if it claims one.
    pub engine: Option<String>,
    /// Classes and script structs in the dump.
    pub structs: u64,
    /// Enums in the dump.
    pub enums: u64,
    /// Functions in the dump.
    pub functions: u64,
    /// Occupied slots of the object array.
    pub objects: u64,
    /// Objects that could not be dumped completely.
    pub failures: u64,
    /// Diagnostics recorded in the dump.
    pub diagnostics: u64,
    /// More than the allowed share of the objects failed.
    pub degraded: bool,
    /// Wall time of the whole dump.
    pub elapsed_ms: u64,
}

/// How a dump failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DumpFailure {
    /// A short stable name, see [`DumpError::kind`](crate::DumpError::kind).
    pub kind: String,
    /// The reason, as text.
    pub message: String,
    /// The folder with the failure record, when one was written.
    pub folder: Option<String>,
}

/// The document written to `outbox/<id>.json`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CommandResult {
    /// The client's id. `None` when the command file had no usable id.
    pub id: Option<String>,
    /// The command name, when it could be read.
    pub command: Option<String>,
    /// The outcome class.
    pub status: ResultStatus,
    /// A reason or a note, in English.
    pub message: Option<String>,
    /// The state of the DLL when the result was written.
    pub state: State,
    /// Set for a dump that finished.
    pub dump: Option<DumpSummary>,
    /// Set for a dump that failed.
    pub failure: Option<DumpFailure>,
    /// For `status`: the full status document.
    pub status_document: Option<StatusDocument>,
    /// When the result was written.
    pub finished: String,
}

/// The last dump that ended, for the status document.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LastDump {
    /// The label it was asked for or given.
    pub label: String,
    /// `ok`, `degraded`, `cancelled` or the failure kind.
    pub result: String,
    /// Its folder, when one exists.
    pub folder: Option<String>,
    /// When it ended.
    pub finished: String,
}

/// The dump that is running, for the status document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CurrentDump {
    /// The label it was asked for or given.
    pub label: String,
    /// When it started.
    pub started: String,
}

/// The document kept up to date in `<output>/control/status.json`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StatusDocument {
    /// Where the DLL is in its life.
    pub state: State,
    /// Process that hosts the DLL.
    pub pid: u32,
    /// Name of this session's folder under each build folder.
    pub session: String,
    /// Version of the tool.
    pub tool_version: String,
    /// The output folder.
    pub output: String,
    /// Dumps that finished with a result.
    pub dumps_completed: u64,
    /// Dumps that ended in an error or were cancelled.
    pub dumps_failed: u64,
    /// The running dump, if any.
    pub current: Option<CurrentDump>,
    /// The last dump that ended, if any.
    pub last_dump: Option<LastDump>,
    /// A note about the state, such as why the DLL did not unload.
    pub detail: Option<String>,
    /// When this document was written.
    pub updated: String,
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
    fn status_and_stop_parse() {
        let status = parse_request(r#"{"id":"a1","command":"status"}"#).unwrap();
        assert_eq!(status.id, "a1");
        assert_eq!(status.command, Command::Status);
        let stop = parse_request(r#"{"id":"a2","command":"STOP"}"#).unwrap();
        assert_eq!(stop.command, Command::Stop);
    }

    #[test]
    fn a_dump_with_no_options_uses_defaults() {
        let request = parse_request(r#"{"id":"d","command":"dump"}"#).unwrap();
        assert_eq!(
            request.command,
            Command::Dump {
                label: None,
                formats: None
            }
        );
    }

    #[test]
    fn a_dump_reads_label_and_formats() {
        let request = parse_request(
            r#"{"id":"d","command":"dump","label":"menu","formats":["usmap","JSON","usmap"]}"#,
        )
        .unwrap();
        assert_eq!(
            request.command,
            Command::Dump {
                label: Some("menu".to_owned()),
                formats: Some(vec![Format::Usmap, Format::Json]),
            }
        );
    }

    #[test]
    fn a_numeric_id_is_accepted_as_text() {
        assert_eq!(
            parse_request(r#"{"id":17,"command":"status"}"#).unwrap().id,
            "17"
        );
    }

    #[test]
    fn unknown_extra_fields_are_ignored() {
        assert!(parse_request(r#"{"id":"a","command":"status","future":{"x":1}}"#).is_ok());
    }

    #[test]
    fn malformed_json_is_an_error_without_an_id() {
        let error = parse_request("{not json").unwrap_err();
        assert!(error.id.is_none());
        assert!(error.message.contains("JSON"), "{}", error.message);
        assert!(parse_request("").is_err());
        assert!(parse_request("[1,2]").is_err());
        assert!(parse_request("null").is_err());
    }

    #[test]
    fn a_missing_or_bad_id_is_an_error() {
        assert!(parse_request(r#"{"command":"status"}"#).is_err());
        assert!(parse_request(r#"{"id":"","command":"status"}"#).is_err());
        assert!(parse_request(r#"{"id":".hidden","command":"status"}"#).is_err());
        assert!(parse_request(r#"{"id":"a/b","command":"status"}"#).is_err());
        assert!(parse_request(r#"{"id":"..\\x","command":"status"}"#).is_err());
        assert!(parse_request(r#"{"id":-1,"command":"status"}"#).is_err());
        assert!(parse_request(r#"{"id":true,"command":"status"}"#).is_err());
        let long = format!(r#"{{"id":"{}","command":"status"}}"#, "a".repeat(101));
        assert!(parse_request(&long).is_err());
    }

    #[test]
    fn an_unknown_command_keeps_the_id_so_the_result_can_be_matched() {
        let error = parse_request(r#"{"id":"x","command":"explode"}"#).unwrap_err();
        assert_eq!(error.id.as_deref(), Some("x"));
        assert_eq!(error.command.as_deref(), Some("explode"));
        assert!(error.message.contains("explode"));
    }

    #[test]
    fn a_missing_command_is_an_error() {
        let error = parse_request(r#"{"id":"x"}"#).unwrap_err();
        assert_eq!(error.id.as_deref(), Some("x"));
        assert!(parse_request(r#"{"id":"x","command":5}"#).is_err());
    }

    #[test]
    fn bad_dump_options_are_errors() {
        for text in [
            r#"{"id":"x","command":"dump","label":5}"#,
            r#"{"id":"x","command":"dump","formats":"json"}"#,
            r#"{"id":"x","command":"dump","formats":[]}"#,
            r#"{"id":"x","command":"dump","formats":["xml"]}"#,
            r#"{"id":"x","command":"dump","formats":[1]}"#,
        ] {
            let error = parse_request(text).unwrap_err();
            assert_eq!(error.id.as_deref(), Some("x"), "{text}");
            assert_eq!(error.command.as_deref(), Some("dump"), "{text}");
        }
        let long = format!(
            r#"{{"id":"x","command":"dump","label":"{}"}}"#,
            "a".repeat(201)
        );
        assert!(parse_request(&long).is_err());
    }

    #[test]
    fn states_serialize_in_lower_case() {
        let json = serde_json::to_string(&[
            State::Ready,
            State::Dumping,
            State::Idle,
            State::Stopping,
            State::Stopped,
        ])
        .unwrap();
        assert_eq!(json, r#"["ready","dumping","idle","stopping","stopped"]"#);
    }
}
