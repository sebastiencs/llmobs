//! The JSON Schema a judge must fill, and the parser that reads its answer (D158).
//!
//! The schema lists `explanation` first when a reason is requested, then `value`
//! or `label`. The parser accepts a tool-call object, one JSON value, the first
//! markdown fence, or the first `{...}` object in the text, in that order.

use std::collections::BTreeMap;

use llmobs_core::score::ScoreValue;
use serde_json::{Map, Value};

use crate::config::JudgeOutput;
use crate::error::JudgeError;

const EXPLANATION_CHARS: usize = 8_000;

/// What an adapter received: tool-call arguments, or message text.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RawAnswer {
    /// A tool call's arguments, already a JSON object.
    Object(Map<String, Value>),
    /// Message text, which may wrap the object in prose or a fence.
    Text(String),
}

/// A score value and the optional explanation that becomes the score's reason.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Answer {
    /// The score, using the label spelling stored on the evaluator.
    pub(crate) value: ScoreValue,
    /// At most 8 000 characters. Absent when the judge does not ask for one.
    pub(crate) explanation: Option<String>,
}

/// The schema sent to the model. Bounds are checked after parsing, not here.
#[must_use]
pub(crate) fn answer_schema(output: &JudgeOutput, include_reason: bool) -> Value {
    let mut properties = Map::new();
    if include_reason {
        properties.insert("explanation".to_owned(), json_type("string"));
    }
    let field = match output {
        JudgeOutput::Numeric { .. } => {
            properties.insert("value".to_owned(), json_type("number"));
            "value"
        }
        JudgeOutput::Boolean => {
            properties.insert("value".to_owned(), json_type("boolean"));
            "value"
        }
        JudgeOutput::Categorical { labels, .. } => {
            properties.insert(
                "label".to_owned(),
                serde_json::json!({"type": "string", "enum": labels}),
            );
            "label"
        }
    };
    let required: Vec<&str> = if include_reason {
        vec!["explanation", field]
    } else {
        vec![field]
    };

    let mut schema = Map::new();
    schema.insert("type".to_owned(), Value::String("object".to_owned()));
    schema.insert("properties".to_owned(), Value::Object(properties));
    schema.insert("required".to_owned(), serde_json::json!(required));
    schema.insert("additionalProperties".to_owned(), Value::Bool(false));
    Value::Object(schema)
}

fn json_type(kind: &str) -> Value {
    serde_json::json!({"type": kind})
}

/// Reads a score from an adapter reply.
///
/// # Errors
///
/// Returns [`JudgeError`] when the reply has no object, or the score field is
/// missing, the wrong type, out of range, or not one of the configured labels.
/// The message names the field and the configured bounds, never the answer.
pub(crate) fn parse_answer(
    raw: RawAnswer,
    output: &JudgeOutput,
    include_reason: bool,
) -> Result<Answer, JudgeError> {
    let object = find_object(raw)?;
    let explanation = if include_reason {
        explanation_of(&object)
    } else {
        None
    };
    Ok(Answer {
        value: score_value(&object, output)?,
        explanation,
    })
}

fn find_object(raw: RawAnswer) -> Result<Map<String, Value>, JudgeError> {
    match raw {
        RawAnswer::Object(object) => Ok(object),
        RawAnswer::Text(text) => object_from_text(&text),
    }
}

fn object_from_text(text: &str) -> Result<Map<String, Value>, JudgeError> {
    let text = text.trim();
    if text.is_empty() {
        return Err(JudgeError::EmptyAnswer);
    }
    match one_json(text) {
        Found::Object(object) => return Ok(object),
        Found::Other => return Err(JudgeError::NotAnObject),
        Found::None => {}
    }
    if let Some(fenced) = first_fence(text) {
        match one_json(fenced.trim()) {
            Found::Object(object) => return Ok(object),
            Found::Other => return Err(JudgeError::NotAnObject),
            Found::None => {}
        }
    }
    scan_object(text).ok_or(JudgeError::NotJson)
}

enum Found {
    Object(Map<String, Value>),
    Other,
    None,
}

fn one_json(text: &str) -> Found {
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(object)) => Found::Object(object),
        Ok(_) => Found::Other,
        Err(_) => Found::None,
    }
}

/// The body of the first ``` or ```json fence. An unclosed fence does not count.
fn first_fence(text: &str) -> Option<&str> {
    let mut offset = 0;
    while offset < text.len() {
        let (line, next) = line_at(text, offset);
        if is_fence_opener(line.trim()) {
            return fence_body(text, next);
        }
        offset = next;
    }
    None
}

fn fence_body(text: &str, mut offset: usize) -> Option<&str> {
    let start = offset;
    while offset < text.len() {
        let (line, next) = line_at(text, offset);
        if line.trim() == "```" {
            return Some(&text[start..offset]);
        }
        offset = next;
    }
    None
}

fn line_at(text: &str, offset: usize) -> (&str, usize) {
    let rest = &text[offset..];
    match rest.find('\n') {
        Some(index) => (&rest[..index], offset + index + 1),
        None => (rest, text.len()),
    }
}

fn is_fence_opener(line: &str) -> bool {
    line == "```" || line.eq_ignore_ascii_case("```json")
}

fn scan_object(text: &str) -> Option<Map<String, Value>> {
    let mut rest = text;
    while let Some(index) = rest.find('{') {
        let candidate = &rest[index..];
        let mut values = serde_json::Deserializer::from_str(candidate).into_iter::<Value>();
        if let Some(Ok(Value::Object(object))) = values.next() {
            return Some(object);
        }
        rest = &candidate[1..];
    }
    None
}

fn explanation_of(object: &Map<String, Value>) -> Option<String> {
    let value = object.get("explanation").or_else(|| object.get("reason"))?;
    let text = match value {
        Value::String(text) => text.clone(),
        Value::Array(items) => {
            let mut lines = Vec::with_capacity(items.len());
            for item in items {
                let Value::String(line) = item else {
                    return None;
                };
                lines.push(line.as_str());
            }
            lines.join("\n")
        }
        _ => return None,
    };
    Some(cut_chars(&text, EXPLANATION_CHARS))
}

pub(crate) fn cut_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        text.chars().take(max).collect()
    }
}

fn score_value(
    object: &Map<String, Value>,
    output: &JudgeOutput,
) -> Result<ScoreValue, JudgeError> {
    match output {
        JudgeOutput::Numeric { min, max } => {
            let value = required(object, "value", "score")?;
            let number = as_number(value).ok_or(JudgeError::WrongType { field: "value" })?;
            if number.is_finite() && (*min..=*max).contains(&number) {
                Ok(ScoreValue::Numeric(number))
            } else {
                Err(JudgeError::OutOfRange {
                    min: *min,
                    max: *max,
                })
            }
        }
        JudgeOutput::Boolean => {
            let value = required(object, "value", "score")?;
            as_bool(value)
                .map(ScoreValue::Boolean)
                .ok_or(JudgeError::WrongType { field: "value" })
        }
        JudgeOutput::Categorical {
            labels,
            label_values,
        } => categorical(object, labels, label_values),
    }
}

fn categorical(
    object: &Map<String, Value>,
    labels: &[String],
    label_values: &BTreeMap<String, f64>,
) -> Result<ScoreValue, JudgeError> {
    let value = required(object, "label", "value")?;
    let Value::String(text) = value else {
        return Err(JudgeError::WrongType { field: "label" });
    };
    let folded = normalize_label(text);
    let label = labels
        .iter()
        .find(|label| normalize_label(label) == folded)
        .ok_or(JudgeError::UnknownLabel)?;
    Ok(ScoreValue::Categorical {
        label: label.clone(),
        value: label_values.get(label).copied(),
    })
}

/// `primary`, or `fallback` when that key is absent. A present null is not absent.
fn required<'a>(
    object: &'a Map<String, Value>,
    primary: &'static str,
    fallback: &'static str,
) -> Result<&'a Value, JudgeError> {
    object
        .get(primary)
        .or_else(|| object.get(fallback))
        .ok_or(JudgeError::MissingField { field: primary })
}

fn as_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

fn as_bool(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(value) => Some(*value),
        Value::String(text) => match text.trim().to_ascii_lowercase().as_str() {
            "true" => Some(true),
            "false" => Some(false),
            _ => None,
        },
        Value::Number(number) => match number.as_u64() {
            Some(1) => Some(true),
            Some(0) => Some(false),
            _ => None,
        },
        _ => None,
    }
}

pub(crate) fn normalize_label(label: &str) -> String {
    let folded = label.trim().to_lowercase();
    folded.strip_suffix('.').unwrap_or(&folded).to_owned()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use llmobs_core::score::ScoreValue;
    use serde_json::{Value, json};

    use super::{Answer, EXPLANATION_CHARS, RawAnswer, answer_schema, parse_answer};
    use crate::config::JudgeOutput;
    use crate::error::{JudgeError, assert_no_payload};

    const PLANTED: &str = "SECRET-PAYLOAD";

    fn numeric() -> JudgeOutput {
        JudgeOutput::Numeric { min: 0.0, max: 1.0 }
    }

    fn labels(values: &[(&str, f64)]) -> JudgeOutput {
        JudgeOutput::Categorical {
            labels: values
                .iter()
                .map(|(label, _)| (*label).to_owned())
                .collect(),
            label_values: values
                .iter()
                .map(|(label, value)| ((*label).to_owned(), *value))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    fn text(value: &str) -> RawAnswer {
        RawAnswer::Text(value.to_owned())
    }

    fn object(value: Value) -> RawAnswer {
        match value {
            Value::Object(object) => RawAnswer::Object(object),
            other => panic!("expected an object, got {other}"),
        }
    }

    fn parse(raw: RawAnswer, output: &JudgeOutput, include_reason: bool) -> Answer {
        parse_answer(raw, output, include_reason).unwrap()
    }

    fn assert_number(answer: &Answer, value: f64, explanation: Option<&str>) {
        assert_eq!(answer.value, ScoreValue::Numeric(value));
        assert_eq!(answer.explanation.as_deref(), explanation);
    }

    #[test]
    fn answer_schema_snapshots() {
        let categorical = JudgeOutput::Categorical {
            labels: vec!["correct".to_owned(), "incorrect".to_owned()],
            label_values: BTreeMap::new(),
        };
        let boolean = JudgeOutput::Boolean;
        let schemas = json!({
            "numeric": answer_schema(&numeric(), false),
            "numeric_with_reason": answer_schema(&numeric(), true),
            "boolean": answer_schema(&boolean, false),
            "boolean_with_reason": answer_schema(&boolean, true),
            "categorical": answer_schema(&categorical, false),
            "categorical_with_reason": answer_schema(&categorical, true),
        });
        insta::with_settings!({
            snapshot_path => "../../tests/snapshots",
            prepend_module_to_snapshot => false,
        }, {
            insta::assert_json_snapshot!("answer_schemas", schemas);
        });
    }

    #[test]
    fn parses_objects_fences_and_prose() {
        let output = numeric();
        assert_number(
            &parse(
                text(r#"{"value": 0.8, "explanation": "because"}"#),
                &output,
                true,
            ),
            0.8,
            Some("because"),
        );
        assert_number(
            &parse(
                object(json!({"value": 0.8, "explanation": "because"})),
                &output,
                true,
            ),
            0.8,
            Some("because"),
        );
        assert_number(
            &parse(
                text(r#"{"score": 0.8, "reason": "because"}"#),
                &output,
                true,
            ),
            0.8,
            Some("because"),
        );
        assert_number(
            &parse(
                text(r#"{"value": 0.8, "explanation": "kept", "reason": "dropped"}"#),
                &output,
                true,
            ),
            0.8,
            Some("kept"),
        );
        assert_number(
            &parse(text("```json\n{\"value\": 0.8}\n```"), &output, true),
            0.8,
            None,
        );
        assert_number(
            &parse(text("```\n{\"value\": 0.8}\n```"), &output, true),
            0.8,
            None,
        );
        assert_number(
            &parse(text("The score is {\"value\": 0.8} indeed."), &output, true),
            0.8,
            None,
        );
        assert_number(
            &parse(text(r#"{"value":0.2}{"value":0.9}"#), &output, true),
            0.2,
            None,
        );
        assert_number(
            &parse(
                text("{\"value\": 0.1}\n```json\n{\"value\": 0.8}\n```"),
                &output,
                true,
            ),
            0.8,
            None,
        );
        assert_number(
            &parse(text(r#"{"value": "0.8"}"#), &output, true),
            0.8,
            None,
        );
        assert_number(&parse(text(r#"{"value": 0}"#), &output, true), 0.0, None);
        assert_number(&parse(text(r#"{"value": 1}"#), &output, true), 1.0, None);
        assert_number(
            &parse(
                text(r#"{"value": 0.8, "explanation": ["line", "two"]}"#),
                &output,
                true,
            ),
            0.8,
            Some("line\ntwo"),
        );
        assert_number(
            &parse(
                text(&format!(r#"{{"value": 0.8, "explanation": "{PLANTED}"}}"#)),
                &output,
                false,
            ),
            0.8,
            None,
        );
    }

    #[test]
    fn parses_booleans_and_labels() {
        let output = JudgeOutput::Boolean;
        assert_eq!(
            parse(text(r#"{"value": true}"#), &output, false).value,
            ScoreValue::Boolean(true)
        );
        assert_eq!(
            parse(text(r#"{"value": "TRUE"}"#), &output, false).value,
            ScoreValue::Boolean(true)
        );
        assert_eq!(
            parse(text(r#"{"score": " false "}"#), &output, false).value,
            ScoreValue::Boolean(false)
        );
        assert_eq!(
            parse(text(r#"{"value": 1}"#), &output, false).value,
            ScoreValue::Boolean(true)
        );
        assert_eq!(
            parse(text(r#"{"value": 0}"#), &output, false).value,
            ScoreValue::Boolean(false)
        );

        let output = labels(&[("correct", 1.0), ("incorrect", 0.0)]);
        let answer = parse(text(r#"{"label": " Correct. "}"#), &output, false);
        assert_eq!(
            answer.value,
            ScoreValue::Categorical {
                label: "correct".to_owned(),
                value: Some(1.0),
            }
        );

        let output = labels(&[("Correct", 1.0), ("incorrect", 0.0)]);
        let answer = parse(text(r#"{"value": "correct"}"#), &output, false);
        assert_eq!(
            answer.value,
            ScoreValue::Categorical {
                label: "Correct".to_owned(),
                value: Some(1.0),
            }
        );
    }

    #[test]
    fn cuts_the_explanation_to_unicode_scalars() {
        let output = numeric();
        let kept = "é".repeat(EXPLANATION_CHARS);
        let answer = parse(
            object(json!({"value": 0.5, "explanation": kept})),
            &output,
            true,
        );
        assert_eq!(answer.explanation.as_deref(), Some(kept.as_str()));

        let long = format!("é{}", "é".repeat(EXPLANATION_CHARS));
        let answer = parse(
            object(json!({"value": 0.5, "explanation": long})),
            &output,
            true,
        );
        let explanation = answer.explanation.unwrap();
        assert_eq!(explanation.chars().count(), EXPLANATION_CHARS);
        assert!(explanation.chars().all(|char| char == 'é'));
    }

    #[test]
    fn reports_each_parse_error_without_the_answer() {
        let cases = [
            (text("  "), numeric(), JudgeError::EmptyAnswer),
            (text(PLANTED), numeric(), JudgeError::NotJson),
            (text("42"), numeric(), JudgeError::NotAnObject),
            (text("[1, 2]"), numeric(), JudgeError::NotAnObject),
            (
                text("```json\n[1]\n```"),
                numeric(),
                JudgeError::NotAnObject,
            ),
            (
                text("{}"),
                numeric(),
                JudgeError::MissingField { field: "value" },
            ),
            (
                text("{}"),
                labels(&[("correct", 1.0), ("incorrect", 0.0)]),
                JudgeError::MissingField { field: "label" },
            ),
            (
                text(r#"{"value": "nope", "score": 0.5}"#),
                numeric(),
                JudgeError::WrongType { field: "value" },
            ),
            (
                text(r#"{"value": "1"}"#),
                JudgeOutput::Boolean,
                JudgeError::WrongType { field: "value" },
            ),
            (
                text(r#"{"label": 1, "value": "correct"}"#),
                labels(&[("correct", 1.0), ("incorrect", 0.0)]),
                JudgeError::WrongType { field: "label" },
            ),
            (
                text(r#"{"value": 9}"#),
                numeric(),
                JudgeError::OutOfRange { min: 0.0, max: 1.0 },
            ),
            (
                text(r#"{"value": "inf"}"#),
                numeric(),
                JudgeError::OutOfRange { min: 0.0, max: 1.0 },
            ),
            (
                text(&format!(r#"{{"label": "{PLANTED}"}}"#)),
                labels(&[("correct", 1.0), ("incorrect", 0.0)]),
                JudgeError::UnknownLabel,
            ),
        ];
        for (raw, output, expected) in cases {
            let err = parse_answer(raw, &output, true).unwrap_err();
            assert_eq!(err, expected);
            let message = err.to_string();
            assert_no_payload(&message, PLANTED);
            assert_no_payload(&message, "nope");
            assert_no_payload(&message, "inf");
            assert_no_payload(&message, "9");
        }
    }
}
