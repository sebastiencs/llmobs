//! Write `case.json` and `input.json` for one captured case.
//!
//! `input.json` is the proto JSON, pretty-printed, with one trailing newline.
//! `case.json` uses the field order of the fixture loader's `CaseFile`.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use serde::Serialize;
use serde_json::{Map, Value};

use crate::error::CaptureError;
use crate::scenario::Scenario;

const INPUT_LIMIT: usize = 256 * 1024;

/// Language of the scenario program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    /// `tools/capture/py`.
    Python,
    /// `tools/capture/ts`.
    Typescript,
}

/// Provenance stored on a captured `case.json`.
///
/// `captured_at` is RFC 3339 in UTC with whole seconds and a `Z` suffix. The
/// caller supplies it; this writer does not read the clock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Source {
    /// Instrumentation package name.
    pub instrumentation: String,
    /// Instrumentation package version.
    pub instrumentation_version: String,
    /// Language of the captured program.
    pub language: Language,
    /// Library name to version, written sorted by name.
    pub libraries: BTreeMap<String, String>,
    /// When the capture ran.
    pub captured_at: String,
}

/// What [`write_case`] did with the files already in the directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Written {
    /// `expected.json` is present and `input.json` changed, so it needs a re-bless.
    pub stale_expected: bool,
}

#[derive(Serialize)]
struct CaseFile<'a> {
    description: &'a str,
    origin: &'static str,
    source: &'a Source,
    spec_refs: &'a [String],
    #[serde(skip_serializing_if = "Option::is_none")]
    settings: Option<&'a Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<&'a Map<String, Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    notes: Option<&'a str>,
}

/// Creates `dir` if needed and overwrites `case.json` and `input.json`.
///
/// `expected.json` is left untouched. `stale_expected` is set when that file
/// exists and the new `input.json` differs from the previous one.
pub fn write_case(
    dir: &Path,
    scenario: &Scenario,
    request: &ExportTraceServiceRequest,
    source: &Source,
) -> Result<Written, CaptureError> {
    let input = encode("input.json", request)?;
    if input.len() > INPUT_LIMIT {
        return Err(CaptureError::TooLarge { bytes: input.len() });
    }
    let case = encode(
        "case.json",
        &CaseFile {
            description: &scenario.description,
            origin: "captured",
            source,
            spec_refs: &scenario.spec_refs,
            settings: scenario.settings.as_ref(),
            context: scenario.context.as_ref(),
            notes: scenario.notes.as_deref(),
        },
    )?;
    fs::create_dir_all(dir).map_err(|source| CaptureError::WriteIo {
        path: dir.to_path_buf(),
        source,
    })?;
    let input_path = dir.join("input.json");
    let previous = if input_path.is_file() {
        Some(
            fs::read(&input_path).map_err(|source| CaptureError::WriteIo {
                path: input_path.clone(),
                source,
            })?,
        )
    } else {
        None
    };
    let stale_expected =
        dir.join("expected.json").is_file() && previous.as_deref() != Some(input.as_bytes());
    fs::write(&input_path, &input).map_err(|source| CaptureError::WriteIo {
        path: input_path,
        source,
    })?;
    let case_path = dir.join("case.json");
    fs::write(&case_path, &case).map_err(|source| CaptureError::WriteIo {
        path: case_path,
        source,
    })?;
    Ok(Written { stale_expected })
}

fn encode<T: Serialize>(file: &'static str, value: &T) -> Result<String, CaptureError> {
    // prost's JSON encoder and serde_json print some f64 values differently. The
    // fixture check parses the file and pretty-prints it again, so the writer does
    // that same round trip before the file is written.
    let text = serde_json::to_string(value).map_err(|_| CaptureError::Encode { file })?;
    let parsed: Value = serde_json::from_str(&text).map_err(|_| CaptureError::Encode { file })?;
    let mut pretty =
        serde_json::to_string_pretty(&parsed).map_err(|_| CaptureError::Encode { file })?;
    pretty.push('\n');
    Ok(pretty)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;

    use serde_json::{Value, json};

    use super::*;
    use crate::error::CaptureError;
    use crate::scenario::{Api, CasePath, Finish, Message, Turn, Usage};
    use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
    use opentelemetry_proto::tonic::common::v1::any_value::Value as Any;
    use opentelemetry_proto::tonic::common::v1::{AnyValue, ArrayValue, KeyValue};
    use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span};

    fn source(language: Language) -> Source {
        let mut libraries = BTreeMap::new();
        libraries.insert("zeta".to_owned(), "2".to_owned());
        libraries.insert("alpha".to_owned(), "1".to_owned());
        Source {
            instrumentation: "openinference-instrumentation-openai".to_owned(),
            instrumentation_version: "0.1.0".to_owned(),
            language,
            libraries,
            captured_at: "2026-01-01T00:00:00Z".to_owned(),
        }
    }

    fn scenario(with_optional: bool) -> Scenario {
        let (notes, settings, context) = if with_optional {
            (
                Some("Copied note.".to_owned()),
                Some(
                    json!({"media_max_bytes": 128, "media_min_bytes": 64})
                        .as_object()
                        .unwrap()
                        .clone(),
                ),
                Some(
                    json!({
                        "project_id": "00000000-0000-7000-8000-000000000001",
                        "ingest_time": "2026-01-01T00:00:00.000000000Z"
                    })
                    .as_object()
                    .unwrap()
                    .clone(),
                ),
            )
        } else {
            (None, None, None)
        };
        Scenario {
            description: "A captured chat.".to_owned(),
            spec_refs: vec!["02 § Normalization rules".to_owned()],
            notes,
            settings,
            context,
            instrumentation: "openinference-instrumentation-openai".to_owned(),
            libraries: vec!["openinference-instrumentation-openai".to_owned()],
            env: BTreeMap::new(),
            timestamp_attributes: Vec::new(),
            timing_attributes: Vec::new(),
            turns: vec![Turn {
                api: Api::OpenaiChat,
                model: "gpt-test".to_owned(),
                message: Some(Message {
                    text: Some("Hi".to_owned()),
                    reasoning: None,
                    tool_calls: Vec::new(),
                }),
                finish: Some(Finish::Stop),
                usage: Usage::default(),
                dims: None,
                ttft_ms: 0,
                chunk_delay_ms: 0,
                chunks: 1,
            }],
            case: CasePath {
                group: "genai".to_owned(),
                case: "chat-tool".to_owned(),
            },
        }
    }

    fn request(name: &str) -> ExportTraceServiceRequest {
        ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                scope_spans: vec![ScopeSpans {
                    spans: vec![Span {
                        name: name.to_owned(),
                        attributes: vec![KeyValue {
                            key: "flag".to_owned(),
                            value: Some(AnyValue {
                                value: Some(Any::BoolValue(false)),
                            }),
                            ..KeyValue::default()
                        }],
                        ..Span::default()
                    }],
                    ..ScopeSpans::default()
                }],
                ..ResourceSpans::default()
            }],
        }
    }

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("llmobs-capture-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    fn keys(value: &Value) -> Vec<&str> {
        value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect()
    }

    fn canonical(text: &str) -> String {
        let value: Value = serde_json::from_str(text).unwrap();
        let mut pretty = serde_json::to_string_pretty(&value).unwrap();
        pretty.push('\n');
        pretty
    }

    #[test]
    fn written_files_are_canonical_and_case_json_uses_fixture_field_order() {
        let dir = temp_dir("order");
        let written = write_case(
            &dir,
            &scenario(true),
            &request("chat"),
            &source(Language::Python),
        )
        .unwrap();
        assert!(!written.stale_expected);
        let case_text = fs::read_to_string(dir.join("case.json")).unwrap();
        let input_text = fs::read_to_string(dir.join("input.json")).unwrap();
        assert_eq!(case_text, canonical(&case_text));
        assert_eq!(input_text, canonical(&input_text));
        let case: Value = serde_json::from_str(&case_text).unwrap();
        assert_eq!(
            keys(&case),
            [
                "description",
                "origin",
                "source",
                "spec_refs",
                "settings",
                "context",
                "notes",
            ]
        );
        assert_eq!(case["origin"], "captured");
        assert_eq!(
            keys(&case["source"]),
            [
                "instrumentation",
                "instrumentation_version",
                "language",
                "libraries",
                "captured_at",
            ]
        );
        assert_eq!(case["source"]["language"], "python");
        assert_eq!(case["source"]["captured_at"], "2026-01-01T00:00:00Z");
        assert_eq!(keys(&case["source"]["libraries"]), ["alpha", "zeta"]);
        assert_eq!(
            keys(&case["settings"]),
            ["media_max_bytes", "media_min_bytes"]
        );
        assert_eq!(keys(&case["context"]), ["project_id", "ingest_time"]);
        assert!(input_text.contains("\"boolValue\": false"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_widened_f32_uses_the_fixture_number_format() {
        // Embedding vectors are f32 values widened to f64. prost's JSON and the
        // fixture check's parse-and-pretty-print disagree on some of those values.
        let request = request_with_double(f64::from(0.1_f32));
        let dir = temp_dir("doubles");
        write_case(&dir, &scenario(false), &request, &source(Language::Python)).unwrap();
        let input_text = fs::read_to_string(dir.join("input.json")).unwrap();
        assert_eq!(input_text, canonical(&input_text));
        let _ = fs::remove_dir_all(&dir);
    }

    fn request_with_double(value: f64) -> ExportTraceServiceRequest {
        let mut request = request("embed");
        let span = &mut request.resource_spans[0].scope_spans[0].spans[0];
        span.attributes.push(KeyValue {
            key: "embedding.embeddings.0.embedding.vector".to_owned(),
            value: Some(AnyValue {
                value: Some(Any::ArrayValue(ArrayValue {
                    values: vec![AnyValue {
                        value: Some(Any::DoubleValue(value)),
                    }],
                })),
            }),
            ..KeyValue::default()
        });
        request
    }

    #[test]
    fn omitted_case_fields_stay_absent_and_typescript_is_snake_case() {
        let dir = temp_dir("omit");
        write_case(
            &dir,
            &scenario(false),
            &request("chat"),
            &source(Language::Typescript),
        )
        .unwrap();
        let case: Value =
            serde_json::from_str(&fs::read_to_string(dir.join("case.json")).unwrap()).unwrap();
        assert_eq!(
            keys(&case),
            ["description", "origin", "source", "spec_refs"]
        );
        assert_eq!(case["source"]["language"], "typescript");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_oversized_input_is_refused_before_the_directory_is_created() {
        let dir = temp_dir("large");
        let error = write_case(
            &dir,
            &scenario(false),
            &request(&"x".repeat(300_000)),
            &source(Language::Python),
        )
        .unwrap_err();
        let CaptureError::TooLarge { bytes } = error else {
            panic!("{error}");
        };
        assert!(bytes > INPUT_LIMIT, "{bytes}");
        assert!(!dir.exists());
    }

    #[test]
    fn expected_json_goes_stale_only_when_input_changes() {
        let dir = temp_dir("stale");
        let scenario = scenario(false);
        let source = source(Language::Python);
        let first = write_case(&dir, &scenario, &request("chat"), &source).unwrap();
        assert!(!first.stale_expected);
        fs::write(dir.join("expected.json"), "{}\n").unwrap();
        let same = write_case(&dir, &scenario, &request("chat"), &source).unwrap();
        assert!(!same.stale_expected);
        let changed = write_case(&dir, &scenario, &request("other"), &source).unwrap();
        assert!(changed.stale_expected);
        assert_eq!(
            fs::read_to_string(dir.join("expected.json")).unwrap(),
            "{}\n"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
