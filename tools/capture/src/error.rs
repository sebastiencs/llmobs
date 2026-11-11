//! Errors from reading a scenario, running a capture, and writing it.
//!
//! Messages name files, case paths, keys, statuses, and indexes. They never include
//! prompt or response text, timestamp values, environment values, or span payloads.

use std::path::PathBuf;

/// A scenario file could not be read, parsed, or accepted.
#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    /// `scenario.json` could not be read.
    #[error("failed to read {path}: {source}")]
    ScenarioIo {
        /// Path of the file that could not be read.
        path: PathBuf,
        /// The filesystem error.
        source: std::io::Error,
    },

    /// `scenario.json` is not the scenario JSON this crate accepts.
    #[error("invalid scenario JSON in {path}: {source}")]
    ScenarioJson {
        /// Path of the file that failed to parse.
        path: PathBuf,
        /// The JSON error. Serde may quote the offending line.
        source: serde_json::Error,
    },

    /// The scenario parsed, and a field broke a capture rule.
    #[error("invalid scenario {case}: {reason}")]
    ScenarioInvalid {
        /// `group/case`, or the text that failed to parse as one.
        case: String,
        /// Field path and rule. Never prompt or response text.
        reason: String,
    },

    /// A shifted instant lands on `ingest_time` or later. The run has to finish
    /// inside the 10 s window.
    #[error("capture spans {span_ms} ms and reaches ingest time")]
    WindowExceeded {
        /// Furthest shifted instant, in whole milliseconds after the first span.
        span_ms: i64,
    },

    /// An event time or a timestamp attribute is earlier than the first span.
    #[error("instant is before the first span")]
    InstantBeforeStart,

    /// A listed timestamp attribute could not be parsed. The message names the key.
    #[error("timestamp attribute {key} could not be parsed")]
    TimestampAttribute {
        /// Attribute key that failed to parse.
        key: String,
    },

    /// A string or `server.port` still carries a machine-specific value.
    ///
    /// `locations` are paths such as
    /// `resourceSpans[0].scopeSpans[1].spans[3].attributes["url.full"]`.
    /// The message lists those paths and no attribute values.
    #[error("leak at {}", join_locations(.locations))]
    Leak {
        /// Where the check found a machine-specific value.
        locations: Vec<String>,
    },

    /// `input.json` would be larger than 256 KiB.
    #[error("input.json is {bytes} bytes, over the 256 KiB limit")]
    TooLarge {
        /// Encoded size in bytes, including the trailing newline.
        bytes: usize,
    },

    /// `input.json` or `case.json` could not be encoded.
    #[error("failed to encode {file}")]
    Encode {
        /// `input.json` or `case.json`.
        file: &'static str,
    },

    /// A fixture file could not be written.
    #[error("failed to write {path}: {source}")]
    WriteIo {
        /// Path of the file or directory.
        path: PathBuf,
        /// The filesystem error.
        source: std::io::Error,
    },

    /// A fixture file could not be read.
    #[error("failed to read {path}: {source}")]
    ReadIo {
        /// Path of the file.
        path: PathBuf,
        /// The filesystem error.
        source: std::io::Error,
    },

    /// The same `group/case` is under both scenario trees.
    #[error("case {case} is in both the Python and TypeScript scenarios")]
    DuplicateCase {
        /// `group/case`.
        case: String,
    },

    /// `run` was given a case discovery did not find.
    #[error("case {case} was not found")]
    CaseNotFound {
        /// The requested `group/case`.
        case: String,
    },

    /// The program argv was empty.
    #[error("program command is empty")]
    ProgramCommand,

    /// The program was still running after 120 s and was killed.
    #[error("program exceeded 120s")]
    ProgramTimeout,

    /// The program exited with a non-zero status, or with a signal.
    #[error("program exited with {}", exit_status(.code))]
    ProgramFailed {
        /// Exit status, or `None` when the process ended on a signal.
        code: Option<i32>,
    },

    /// Binding a port, spawning, or waiting on the program failed.
    #[error("failed to {step}: {source}")]
    Process {
        /// What was being attempted. A fixed phrase, not a path or argv.
        step: &'static str,
        /// The operating-system error.
        source: std::io::Error,
    },

    /// `HOME` was unset, so the leak check has no home directory to look for.
    #[error("HOME is not set")]
    Home,

    /// Neither `/etc/hostname` nor `HOSTNAME` yielded a name.
    #[error("hostname is unavailable")]
    Hostname,

    /// The meta file path could not be passed in the environment.
    #[error("meta file path is not utf-8")]
    MetaPath,

    /// `context.ingest_time` was present and was not an RFC 3339 timestamp.
    #[error("context.ingest_time could not be parsed")]
    IngestTime,

    /// The mock, the receiver, the span count, or the meta file failed.
    ///
    /// `reasons` lists every failure from that step. The strings name counts,
    /// routes, and library names, not payloads or versions.
    #[error("capture {case} failed: {}", join_reasons(.reasons))]
    CaptureIncomplete {
        /// `group/case`.
        case: String,
        /// Everything that failed during collection.
        reasons: Vec<String>,
    },

    /// `--check` found no committed `case.json` or `input.json`.
    #[error("{case} is not captured yet")]
    NotCaptured {
        /// `group/case`.
        case: String,
    },

    /// `--check` found differences outside the masked timing fields.
    ///
    /// `paths` holds at most 20 JSON paths and no values.
    #[error("{case} differs at {}", join_locations(.paths))]
    CheckFailed {
        /// `group/case`.
        case: String,
        /// Where the committed fixture and the new capture differ.
        paths: Vec<String>,
    },

    /// A fixture file was not JSON this crate could parse. The message names the file.
    #[error("failed to parse {file}")]
    FixtureJson {
        /// `case.json` or `input.json`.
        file: &'static str,
    },
}

fn join_locations(locations: &[String]) -> String {
    locations.join(", ")
}

fn join_reasons(reasons: &[String]) -> String {
    reasons.join("; ")
}

fn exit_status(code: &Option<i32>) -> String {
    match code {
        Some(code) => format!("status {code}"),
        None => "no status".to_owned(),
    }
}
