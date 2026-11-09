//! Loads OTLP fixtures for the validation test and for Track B's golden test.
//!
//! See `fixtures/otlp/README.md`.

use std::fs;
use std::path::{Path, PathBuf};

use llmobs_core::ids::ProjectId;
use llmobs_core::span::Span;
use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use serde::Deserialize;

/// Root of `fixtures/otlp`, next to the workspace `Cargo.toml`.
pub const FIXTURES_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/otlp");

/// Default `ingest.max_attribute_bytes` from spec 02 § Limits.
pub const DEFAULT_MAX_ATTRIBUTE_BYTES: u64 = 8_388_608;
/// Default `media.min_bytes` from spec 02 § Limits.
pub const DEFAULT_MEDIA_MIN_BYTES: u64 = 4_096;
/// Default `media.max_bytes` from spec 02 § Limits.
pub const DEFAULT_MEDIA_MAX_BYTES: u64 = 33_554_432;
/// Project id used when `case.json` does not set `context.project_id`.
pub const DEFAULT_PROJECT_ID: &str = "01900000-0000-7000-8000-000000000001";
/// Ingest time used when `case.json` does not set `context.ingest_time`.
pub const DEFAULT_INGEST_TIME: &str = "2026-01-01T00:00:00.000000000Z";

/// Group directories the layout test accepts. Create one only when it has a case.
pub const GROUPS: &[&str] = &[
    "genai",
    "openinference",
    "openllmetry",
    "vercel",
    "langfuse",
    "laminar",
    "google_adk",
    "llmobs",
    "edge",
];

const CASE_FILES: &[&str] = &["case.json", "expected.json", "input.json"];

/// One fixture directory and the three JSON files parsed.
#[derive(Debug, Clone)]
pub struct Case {
    /// Group directory name, for example `genai`.
    pub group: String,
    /// Case directory name.
    pub name: String,
    /// Absolute path of the case directory.
    pub dir: PathBuf,
    /// `case.json`, with defaults applied.
    pub meta: CaseMeta,
    /// `input.json` as an OTLP export request.
    pub input: ExportTraceServiceRequest,
    /// `expected.json`.
    pub expected: Expected,
}

/// `case.json` after defaults are filled in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaseMeta {
    /// What the case exercises.
    pub description: String,
    /// Hand-written or captured.
    pub origin: Origin,
    /// Present exactly when [`Origin::Captured`].
    pub source: Option<Source>,
    /// Spec sections this case pins.
    pub spec_refs: Vec<String>,
    /// Ingest limits, including defaults.
    pub settings: Settings,
    /// Project and ingest time, including defaults.
    pub context: Context,
    /// Free text from the case author.
    pub notes: Option<String>,
}

/// Where the case came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// Written from spec 02 before normalization code existed.
    HandWritten,
    /// Recorded by the capture tool.
    Captured,
}

/// Provenance of a captured case. `captured_at` is RFC 3339.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// Instrumentation package name.
    pub instrumentation: String,
    /// Instrumentation package version.
    pub instrumentation_version: String,
    /// Language of the captured program.
    pub language: String,
    /// Library name to version.
    pub libraries: std::collections::BTreeMap<String, String>,
    /// When the capture ran, RFC 3339.
    pub captured_at: String,
}

/// Per-case ingest limits. Missing keys in `case.json` use the defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    /// `ingest.max_attribute_bytes`.
    pub max_attribute_bytes: u64,
    /// `media.min_bytes`.
    pub media_min_bytes: u64,
    /// `media.max_bytes`.
    pub media_max_bytes: u64,
}

/// Project and clock used while normalizing the case.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Context {
    /// Partition key written onto every span.
    pub project_id: ProjectId,
    /// Server receive time, nanoseconds since the Unix epoch.
    pub ingest_time: i64,
}

/// `expected.json`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expected {
    /// Spans the normalizer must accept, in comparison order.
    pub spans: Vec<Span>,
    /// Blobs the normalizer must extract, sorted by `sha256`.
    pub media: Vec<ExpectedMedia>,
    /// Input spans the normalizer must reject.
    pub rejected_spans: u32,
}

/// One extracted blob. The bytes live in `input.json`, not here.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExpectedMedia {
    /// Lowercase hex SHA-256 of the decoded bytes.
    pub sha256: String,
    /// MIME type stored with the blob.
    pub mime_type: String,
    /// Decoded size.
    pub size_bytes: u64,
}

/// One row of `prices.json`. Prices are USD per 1M tokens.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PriceRow {
    /// Display name.
    pub model_name: String,
    /// Anchored, case-insensitive regex matched against the span model.
    pub match_pattern: String,
    /// When set, the row matches only this provider.
    pub provider: Option<String>,
    /// Row applies at span start times at or after this instant.
    #[serde(with = "llmobs_core::time_serde")]
    pub valid_from: i64,
    /// USD per 1M uncached input tokens.
    pub price_input: Option<f64>,
    /// USD per 1M output tokens. Null on embedding-only rows.
    pub price_output: Option<f64>,
    /// USD per 1M cache-read tokens.
    pub price_cache_read: Option<f64>,
    /// USD per 1M cache-write tokens.
    pub price_cache_write: Option<f64>,
    /// USD per 1M reasoning tokens. Null means use `price_output`.
    pub price_reasoning: Option<f64>,
    /// Long-context tiers. Fixtures ship `null`.
    pub tiers: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CaseFile {
    description: String,
    origin: Origin,
    #[serde(default)]
    source: Option<Source>,
    spec_refs: Vec<String>,
    #[serde(default)]
    settings: SettingsFile,
    #[serde(default)]
    context: ContextFile,
    #[serde(default)]
    notes: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsFile {
    max_attribute_bytes: Option<u64>,
    media_min_bytes: Option<u64>,
    media_max_bytes: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContextFile {
    project_id: Option<ProjectId>,
    ingest_time: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Rfc3339 {
    #[serde(with = "llmobs_core::time_serde")]
    t: i64,
}

/// Loads every case under [`FIXTURES_DIR`].
///
/// # Panics
///
/// Panics with the case path when a fixture file is missing or does not parse.
pub fn load_all_cases() -> Vec<Case> {
    let root = PathBuf::from(FIXTURES_DIR);
    let mut cases = Vec::new();
    for group_entry in list_dir(&root) {
        if !group_entry.path().is_dir() {
            continue;
        }
        let group = entry_name(&group_entry);
        if !GROUPS.contains(&group.as_str()) {
            continue;
        }
        for case_entry in list_dir(&group_entry.path()) {
            if !case_entry.path().is_dir() {
                continue;
            }
            let name = entry_name(&case_entry);
            cases.push(load_case(group.clone(), name, case_entry.path()));
        }
    }
    cases.sort_by(|left, right| (&left.group, &left.name).cmp(&(&right.group, &right.name)));
    cases
}

/// Loads `prices.json`.
///
/// # Panics
///
/// Panics when the file is missing or is not an array of price rows.
pub fn load_prices() -> Vec<PriceRow> {
    let path = PathBuf::from(FIXTURES_DIR).join("prices.json");
    let text = read_text(&path);
    serde_json::from_str(&text).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

/// Sorts spans by `(trace_id, start_time, span_id)`, the fixture comparison order.
pub fn sort_spans(spans: &mut [Span]) {
    spans.sort_by(|left, right| {
        (&left.trace_id, left.start_time, &left.span_id).cmp(&(
            &right.trace_id,
            right.start_time,
            &right.span_id,
        ))
    });
}

/// Pretty-prints `value` with 2-space indentation and one trailing newline.
///
/// Key order is the order already stored on `value` (`serde_json` `preserve_order`).
pub fn canonical_json(value: &serde_json::Value) -> String {
    let mut text = match serde_json::to_string_pretty(value) {
        Ok(text) => text,
        // `serde_json::Value` contains only JSON data, so this cannot fail.
        Err(err) => panic!("a JSON value always serializes: {err}"),
    };
    text.push('\n');
    text
}

fn load_case(group: String, name: String, dir: PathBuf) -> Case {
    for file_name in CASE_FILES {
        let path = dir.join(file_name);
        if !path.is_file() {
            panic!("{}: missing {file_name}", dir.display());
        }
    }
    let meta = load_meta(&dir.join("case.json"));
    let input = parse_json(&dir.join("input.json"));
    let expected = parse_json(&dir.join("expected.json"));
    Case {
        group,
        name,
        dir,
        meta,
        input,
        expected,
    }
}

fn load_meta(path: &Path) -> CaseMeta {
    let file: CaseFile = parse_json(path);
    if let Some(source) = &file.source {
        parse_rfc3339(&source.captured_at).unwrap_or_else(|err| {
            panic!("{}: captured_at: {err}", path.display());
        });
    }
    let defaults = default_context();
    let ingest_time = match file.context.ingest_time {
        Some(text) => parse_rfc3339(&text)
            .unwrap_or_else(|err| panic!("{}: ingest_time: {err}", path.display())),
        None => defaults.ingest_time,
    };
    CaseMeta {
        description: file.description,
        origin: file.origin,
        source: file.source,
        spec_refs: file.spec_refs,
        settings: Settings {
            max_attribute_bytes: file
                .settings
                .max_attribute_bytes
                .unwrap_or(DEFAULT_MAX_ATTRIBUTE_BYTES),
            media_min_bytes: file
                .settings
                .media_min_bytes
                .unwrap_or(DEFAULT_MEDIA_MIN_BYTES),
            media_max_bytes: file
                .settings
                .media_max_bytes
                .unwrap_or(DEFAULT_MEDIA_MAX_BYTES),
        },
        context: Context {
            project_id: file.context.project_id.unwrap_or(defaults.project_id),
            ingest_time,
        },
        notes: file.notes,
    }
}

fn default_context() -> Context {
    let Ok(project_id) = DEFAULT_PROJECT_ID.parse() else {
        panic!("default project id is a valid uuid");
    };
    let Ok(ingest_time) = parse_rfc3339(DEFAULT_INGEST_TIME) else {
        panic!("default ingest time is RFC 3339");
    };
    Context {
        project_id,
        ingest_time,
    }
}

fn parse_rfc3339(text: &str) -> Result<i64, String> {
    // Deserialize from text, not from `Value`: time_serde borrows a `&str`.
    let quoted = serde_json::to_string(text).map_err(|err| err.to_string())?;
    let payload = format!(r#"{{"t":{quoted}}}"#);
    serde_json::from_str::<Rfc3339>(&payload)
        .map(|stamp| stamp.t)
        .map_err(|err| err.to_string())
}

fn parse_json<T: for<'de> Deserialize<'de>>(path: &Path) -> T {
    let text = read_text(path);
    serde_json::from_str(&text).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

fn read_text(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|err| panic!("{}: {err}", path.display()))
}

fn list_dir(path: &Path) -> Vec<fs::DirEntry> {
    let mut entries: Vec<_> = fs::read_dir(path)
        .unwrap_or_else(|err| panic!("{}: {err}", path.display()))
        .map(|entry| entry.unwrap_or_else(|err| panic!("{}: {err}", path.display())))
        .collect();
    entries.sort_by_key(entry_name);
    entries
}

fn entry_name(entry: &fs::DirEntry) -> String {
    entry.file_name().to_string_lossy().into_owned()
}
