//! Compares the normalizer with `fixtures/otlp`.
//!
//! Pending cases are the ones this phase cannot produce yet. They must fail.
//! `LLMOBS_BLESS=1` rewrites `expected.json` and is not used in CI.

#[allow(dead_code, unreachable_pub)]
mod support;

use std::fs;

use llmobs_core::media::MediaBlob;
use llmobs_otel::{IngestContext, IngestLimits, Normalized, PriceTable, decode_json, normalize};
use serde_json::{Map, Value};
use support::{Case, ExpectedMedia, canonical_json, load_all_cases, load_prices, sort_spans};

/// Cases whose expected output needs a later sub-phase.
const PENDING: &[&str] = &[];

const COST_FIELDS: &[&str] = &["cost_input", "cost_output", "cost_total"];
/// Absolute tolerance for the three cost fields. Spec fixtures README.
const COST_EPSILON: f64 = 1e-12;

#[test]
fn golden() {
    let loaded = load_all_cases();
    assert!(
        loaded.failures.is_empty(),
        "fixtures failed to load:\n{}",
        loaded.failures.join("\n")
    );
    let bless = std::env::var_os("LLMOBS_BLESS").is_some_and(|value| value == "1");
    let mut failures = Vec::new();
    for case in &loaded.cases {
        match run_case(case, bless) {
            Ok(()) => {}
            Err(failure) => failures.push(failure),
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn run_case(case: &Case, bless: bool) -> Result<(), String> {
    let label = format!("{}/{}", case.group, case.name);
    let bytes = fs::read(case.dir.join("input.json")).map_err(|err| format!("{label}: {err}"))?;
    let request = decode_json(&bytes).map_err(|err| format!("{label}: input.json: {err}"))?;
    let prices =
        PriceTable::new(load_prices()).map_err(|err| format!("{label}: prices.json: {err}"))?;
    let normalized = normalize(&request, &context_of(case)?, &prices);
    let actual = document(&normalized);
    let Some(expected) = &case.expected else {
        if bless {
            return write_expected(case, &actual, &label);
        }
        #[allow(clippy::print_stdout)]
        {
            println!("{}", awaiting_bless(&label));
        }
        return Ok(());
    };
    let mut expected_spans = expected.spans.clone();
    sort_spans(&mut expected_spans);
    let expected = Value::Object(Map::from_iter([
        (
            "spans".to_owned(),
            serde_json::to_value(&expected_spans).map_err(|err| err.to_string())?,
        ),
        ("media".to_owned(), expected_media(&expected.media)),
        (
            "rejected_spans".to_owned(),
            Value::from(expected.rejected_spans),
        ),
    ]));
    let diff = first_diff(&actual, &expected, &label);
    // A matching case keeps its file, so float noise in costs never churns it.
    if bless {
        return match diff {
            Some(_) => write_expected(case, &actual, &label),
            None => Ok(()),
        };
    }
    let pending = PENDING.contains(&label.as_str());
    match (pending, diff) {
        (true, None) => Err(format!("{label}: remove it from PENDING")),
        (true, Some(_)) => Ok(()),
        (false, Some(path)) => Err(path),
        (false, None) => Ok(()),
    }
}

fn write_expected(case: &Case, actual: &Value, label: &str) -> Result<(), String> {
    let path = case.dir.join("expected.json");
    fs::write(&path, canonical_json(actual)).map_err(|err| format!("{label}: {err}"))
}

fn awaiting_bless(label: &str) -> String {
    format!("awaiting bless: {label}")
}

fn context_of(case: &Case) -> Result<IngestContext, String> {
    let label = format!("{}/{}", case.group, case.name);
    let settings = &case.meta.settings;
    Ok(IngestContext {
        project_id: case.meta.context.project_id,
        ingest_time: case.meta.context.ingest_time,
        retention_days: settings.retention_days,
        limits: IngestLimits {
            max_attribute_bytes: fit(&label, "max_attribute_bytes", settings.max_attribute_bytes)?,
            media_min_bytes: fit(&label, "media_min_bytes", settings.media_min_bytes)?,
            media_max_bytes: fit(&label, "media_max_bytes", settings.media_max_bytes)?,
        },
    })
}

fn fit(label: &str, name: &str, value: u64) -> Result<usize, String> {
    usize::try_from(value).map_err(|_| format!("{label}: {name} does not fit usize"))
}

fn document(normalized: &Normalized) -> Value {
    let mut spans = normalized.spans.clone();
    sort_spans(&mut spans);
    Value::Object(Map::from_iter([
        (
            "spans".to_owned(),
            serde_json::to_value(&spans).unwrap_or(Value::Null),
        ),
        ("media".to_owned(), media_json(&normalized.media)),
        (
            "rejected_spans".to_owned(),
            Value::from(normalized.rejections.total()),
        ),
    ]))
}

fn expected_media(media: &[ExpectedMedia]) -> Value {
    Value::Array(
        media
            .iter()
            .map(|item| {
                Value::Object(Map::from_iter([
                    ("sha256".to_owned(), Value::String(item.sha256.clone())),
                    (
                        "mime_type".to_owned(),
                        Value::String(item.mime_type.clone()),
                    ),
                    ("size_bytes".to_owned(), Value::from(item.size_bytes)),
                ]))
            })
            .collect(),
    )
}

fn media_json(media: &[MediaBlob]) -> Value {
    Value::Array(
        media
            .iter()
            .map(|blob| {
                Value::Object(Map::from_iter([
                    ("sha256".to_owned(), Value::String(blob.sha256.clone())),
                    (
                        "mime_type".to_owned(),
                        Value::String(blob.mime_type.clone()),
                    ),
                    (
                        "size_bytes".to_owned(),
                        Value::from(blob.bytes.len() as u64),
                    ),
                ]))
            })
            .collect(),
    )
}

/// First differing field path, or `None` when the documents match.
///
/// `cost_input`, `cost_output`, and `cost_total` match within [`COST_EPSILON`].
fn first_diff(actual: &Value, expected: &Value, path: &str) -> Option<String> {
    if costs_match(path, actual, expected) {
        return None;
    }
    match (actual, expected) {
        (Value::Object(actual), Value::Object(expected)) => {
            let mut keys: Vec<&String> = expected.keys().collect();
            for key in actual.keys() {
                if !expected.contains_key(key) {
                    keys.push(key);
                }
            }
            for key in keys {
                let child = format!("{path}.{key}");
                match (actual.get(key), expected.get(key)) {
                    (Some(actual), Some(expected)) => {
                        if let Some(found) = first_diff(actual, expected, &child) {
                            return Some(found);
                        }
                    }
                    _ => return Some(child),
                }
            }
            None
        }
        (Value::Array(actual), Value::Array(expected)) => {
            if actual.len() != expected.len() {
                return Some(format!("{path}.len"));
            }
            for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
                if let Some(found) = first_diff(actual, expected, &format!("{path}[{index}]")) {
                    return Some(found);
                }
            }
            None
        }
        (actual, expected) if actual == expected => None,
        _ => Some(path.to_owned()),
    }
}

#[test]
fn bless_writes_expected_json_when_a_captured_case_has_none() {
    let dir = std::env::temp_dir().join(format!("llmobs-bless-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap_or_else(|err| panic!("{err}"));
    fs::write(dir.join("input.json"), "{\"resourceSpans\":[]}\n")
        .unwrap_or_else(|err| panic!("{err}"));
    let case = unblessed_case(&dir);
    run_case(&case, true).unwrap_or_else(|err| panic!("{err}"));
    let text = fs::read_to_string(dir.join("expected.json")).unwrap_or_else(|err| panic!("{err}"));
    assert!(text.contains("\"spans\""));
    assert!(text.contains("\"rejected_spans\""));
    fs::remove_dir_all(&dir).unwrap_or_else(|err| panic!("{err}"));
}

#[test]
fn captured_case_without_expected_json_is_skipped() {
    let dir = std::env::temp_dir().join(format!("llmobs-await-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap_or_else(|err| panic!("{err}"));
    fs::write(dir.join("input.json"), "{\"resourceSpans\":[]}\n")
        .unwrap_or_else(|err| panic!("{err}"));
    let case = unblessed_case(&dir);
    run_case(&case, false).unwrap_or_else(|err| panic!("{err}"));
    assert_eq!(
        awaiting_bless("edge/awaiting"),
        "awaiting bless: edge/awaiting"
    );
    fs::remove_dir_all(&dir).unwrap_or_else(|err| panic!("{err}"));
}

fn unblessed_case(dir: &std::path::Path) -> Case {
    use support::{CaseMeta, Context, Origin, Settings};
    let project_id = support::DEFAULT_PROJECT_ID
        .parse()
        .unwrap_or_else(|err| panic!("{err}"));
    Case {
        group: "edge".to_owned(),
        name: "awaiting".to_owned(),
        dir: dir.to_owned(),
        meta: CaseMeta {
            description: "captured".to_owned(),
            origin: Origin::Captured,
            source: None,
            spec_refs: vec!["02".to_owned()],
            settings: Settings {
                max_attribute_bytes: 8,
                media_min_bytes: 1,
                media_max_bytes: 10,
                retention_days: None,
            },
            context: Context {
                project_id,
                ingest_time: 1_767_225_600_000_000_000,
            },
            notes: None,
        },
        input: Default::default(),
        expected: None,
    }
}

fn costs_match(path: &str, actual: &Value, expected: &Value) -> bool {
    let Some(field) = path.rsplit('.').next() else {
        return false;
    };
    if !COST_FIELDS.contains(&field) {
        return false;
    }
    match (actual.as_f64(), expected.as_f64()) {
        (Some(actual), Some(expected)) => (actual - expected).abs() <= COST_EPSILON,
        _ => false,
    }
}
