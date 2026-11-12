//! Built-in checks and the dispatcher that runs one.
//!
//! Spec 06 § `check` (D164).

use llmobs_core::score::ScoreValue;
use serde::de::IgnoredAny;
use serde_json::Value;

use crate::config::{Check, ContainsMode};
use crate::error::CheckError;
use crate::text::extract_text;

/// Compiled-program cap for a check regex. One mebibyte.
const REGEX_SIZE_LIMIT: usize = 1 << 20;

/// Values a check reads. Absent variables stay [`None`].
#[derive(Debug, Clone, Copy)]
pub struct CheckInput<'a> {
    /// The `output` variable.
    pub output: Option<&'a Value>,
    /// The `expected` variable.
    pub expected: Option<&'a Value>,
}

/// The score a check produced.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckResult {
    /// Boolean for every check except Levenshtein, which is a number in `0..=1`.
    pub value: ScoreValue,
    /// Set when JSON was invalid or a schema keyword failed. Otherwise [`None`].
    pub reason: Option<String>,
}

/// Compiles `pattern` with a 1 MiB program limit.
///
/// The regex error is dropped. Its [`Display`](std::fmt::Display) quotes the pattern.
pub(crate) fn compile_regex(pattern: &str) -> Result<regex::Regex, ()> {
    regex::RegexBuilder::new(pattern)
        .size_limit(REGEX_SIZE_LIMIT)
        .build()
        .map_err(|_| ())
}

/// Compares the output text with the expected text.
///
/// # Errors
///
/// Returns [`CheckError::MissingVariable`] when `output` or `expected` is absent.
pub(crate) fn exact_match(
    case_sensitive: bool,
    trim: bool,
    input: &CheckInput<'_>,
) -> Result<CheckResult, CheckError> {
    let left = prepare(required(input.output, "output")?, trim, case_sensitive);
    let right = prepare(required(input.expected, "expected")?, trim, case_sensitive);
    Ok(passed(left == right))
}

/// Reports whether the output text contains the needles.
///
/// `values` are the needles. When `values` is empty, the needle is the expected
/// text. `any` passes when one needle occurs. `all` passes when every needle occurs.
///
/// # Errors
///
/// Returns [`CheckError::MissingVariable`] when `output` is absent, or when
/// `values` is empty and `expected` is absent.
pub(crate) fn contains(
    values: &[String],
    mode: ContainsMode,
    case_sensitive: bool,
    input: &CheckInput<'_>,
) -> Result<CheckResult, CheckError> {
    let haystack = fold(
        extract_text(required(input.output, "output")?),
        case_sensitive,
    );
    let owned;
    let needles: &[String] = if values.is_empty() {
        owned = vec![extract_text(required(input.expected, "expected")?)];
        &owned
    } else {
        values
    };
    let hit = |needle: &str| haystack.contains(&fold(needle.to_owned(), case_sensitive));
    let matched = match mode {
        ContainsMode::Any => needles.iter().any(|needle| hit(needle)),
        ContainsMode::All => needles.iter().all(|needle| hit(needle)),
    };
    Ok(passed(matched))
}

/// Reports whether the pattern matches anywhere in the output text.
///
/// # Errors
///
/// Returns [`CheckError::MissingVariable`] when `output` is absent, and
/// [`CheckError::InvalidPattern`] when the pattern does not compile within 1 MiB.
pub(crate) fn regex(pattern: &str, input: &CheckInput<'_>) -> Result<CheckResult, CheckError> {
    let output = extract_text(required(input.output, "output")?);
    let compiled = compile_regex(pattern).map_err(|()| CheckError::InvalidPattern)?;
    Ok(passed(compiled.is_match(&output)))
}

fn required<'a>(value: Option<&'a Value>, name: &'static str) -> Result<&'a Value, CheckError> {
    value.ok_or(CheckError::MissingVariable { name })
}

fn prepare(value: &Value, trim: bool, case_sensitive: bool) -> String {
    let text = extract_text(value);
    let text = if trim { text.trim().to_owned() } else { text };
    fold(text, case_sensitive)
}

fn fold(text: String, case_sensitive: bool) -> String {
    if case_sensitive {
        text
    } else {
        text.to_lowercase()
    }
}

fn passed(value: bool) -> CheckResult {
    CheckResult {
        value: ScoreValue::Boolean(value),
        reason: None,
    }
}

fn failed(reason: String) -> CheckResult {
    CheckResult {
        value: ScoreValue::Boolean(false),
        reason: Some(reason),
    }
}

/// Reports whether the output is JSON.
///
/// An object or array is valid as it stands. Any other value is parsed from its
/// extracted text. A parse failure is a `false` score, not an error.
///
/// # Errors
///
/// Returns [`CheckError::MissingVariable`] when `output` is absent.
pub(crate) fn json_valid(input: &CheckInput<'_>) -> Result<CheckResult, CheckError> {
    let output = required(input.output, "output")?;
    if output.is_object() || output.is_array() {
        return Ok(passed(true));
    }
    let owned;
    let text = if let Value::String(text) = output {
        text.as_str()
    } else {
        owned = extract_text(output);
        owned.as_str()
    };
    match parse_json(text) {
        Ok(()) => Ok(passed(true)),
        Err(reason) => Ok(failed(reason)),
    }
}

/// Validates the output against `schema`.
///
/// A string output is parsed as JSON first. Any other output is the instance
/// itself. The schema is compiled once per call and never fetched over the network.
///
/// # Errors
///
/// Returns [`CheckError::MissingVariable`] when `output` is absent, and
/// [`CheckError::InvalidSchema`] when `schema` does not compile.
pub(crate) fn json_schema(
    schema: &Value,
    input: &CheckInput<'_>,
) -> Result<CheckResult, CheckError> {
    let output = required(input.output, "output")?;
    let parsed;
    let instance = if let Value::String(text) = output {
        match serde_json::from_str::<Value>(text) {
            Ok(value) => {
                parsed = value;
                &parsed
            }
            Err(error) => return Ok(failed(json_reason(&error))),
        }
    } else {
        output
    };
    let validator = compile_schema(schema).map_err(|()| CheckError::InvalidSchema)?;
    let Some(error) = validator.iter_errors(instance).next() else {
        return Ok(passed(true));
    };
    Ok(failed(format!(
        "{}: {}",
        error.instance_path(),
        error.kind().keyword()
    )))
}

/// Compiles `schema` as Draft 2020-12 and refuses any reference that is not local.
///
/// The compiler error is dropped. Its [`Display`](std::fmt::Display) can quote the schema or a URL.
pub(crate) fn compile_schema(schema: &Value) -> Result<jsonschema::Validator, ()> {
    jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .offline()
        .build(schema)
        .map_err(|_| ())
}

/// Similarity of the output and expected texts, in `0..=1`.
///
/// With `normalize`, both sides are trimmed, case-folded, and have each
/// whitespace run collapsed to one space before the distance. Two empty strings
/// score 1.
///
/// # Errors
///
/// Returns [`CheckError::MissingVariable`] when `output` or `expected` is absent.
pub(crate) fn levenshtein(
    normalize: bool,
    input: &CheckInput<'_>,
) -> Result<CheckResult, CheckError> {
    let mut left = extract_text(required(input.output, "output")?);
    let mut right = extract_text(required(input.expected, "expected")?);
    if normalize {
        left = normalize_similarity(&left);
        right = normalize_similarity(&right);
    }
    let score = if left.is_empty() && right.is_empty() {
        1.0
    } else {
        strsim::normalized_levenshtein(&left, &right)
    };
    Ok(CheckResult {
        value: ScoreValue::Numeric(score),
        reason: None,
    })
}

/// Runs one check.
///
/// # Errors
///
/// Returns [`CheckError`] when a required variable is absent, the pattern does
/// not compile, or the schema does not compile.
pub fn run_check(check: &Check, input: &CheckInput<'_>) -> Result<CheckResult, CheckError> {
    match check {
        Check::ExactMatch {
            case_sensitive,
            trim,
        } => exact_match(*case_sensitive, *trim, input),
        Check::Contains {
            values,
            mode,
            case_sensitive,
            ..
        } => contains(values, *mode, *case_sensitive, input),
        Check::Regex { pattern } => regex(pattern, input),
        Check::JsonValid => json_valid(input),
        Check::JsonSchema { schema } => json_schema(schema, input),
        Check::Levenshtein { normalize } => levenshtein(*normalize, input),
    }
}

fn parse_json(text: &str) -> Result<(), String> {
    match serde_json::from_str::<IgnoredAny>(text) {
        Ok(_) => Ok(()),
        Err(error) => Err(json_reason(&error)),
    }
}

fn json_reason(error: &serde_json::Error) -> String {
    format!(
        "invalid JSON at line {} column {}",
        error.line(),
        error.column()
    )
}

fn normalize_similarity(text: &str) -> String {
    let folded = text.trim().to_lowercase();
    let mut normalized = String::with_capacity(folded.len());
    let mut in_space = false;
    for character in folded.chars() {
        if character.is_whitespace() {
            if !in_space {
                normalized.push(' ');
                in_space = true;
            }
        } else {
            in_space = false;
            normalized.push(character);
        }
    }
    normalized
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{
        CheckInput, CheckResult, contains, exact_match, json_schema, json_valid, levenshtein,
        regex, run_check,
    };
    use crate::config::{Check, ContainsMode, EvaluatorConfig};
    use crate::error::{CheckError, assert_no_payload};
    use llmobs_core::score::ScoreValue;

    fn input<'a>(output: Option<&'a Value>, expected: Option<&'a Value>) -> CheckInput<'a> {
        CheckInput { output, expected }
    }

    fn assert_pass(result: &CheckResult, expected: bool) {
        assert_eq!(result.value, ScoreValue::Boolean(expected));
        assert_eq!(result.reason, None);
    }

    fn stored_contains(value: &Value) -> (Vec<String>, ContainsMode, bool) {
        let EvaluatorConfig::Check(Check::Contains {
            values,
            mode,
            case_sensitive,
            value: folded,
        }) = EvaluatorConfig::from_parts("check", value).expect("contains config")
        else {
            panic!("contains");
        };
        assert_eq!(folded, None);
        (values, mode, case_sensitive)
    }

    #[test]
    fn exact_match_defaults_and_flipped_flags() {
        let output = json!(" ÉCOLE ");
        let expected = json!("école");
        assert_pass(
            &exact_match(true, true, &input(Some(&output), Some(&expected))).expect("match"),
            false,
        );
        assert_pass(
            &exact_match(false, true, &input(Some(&output), Some(&expected))).expect("match"),
            true,
        );
        let spaced = json!(" a ");
        let bare = json!("a");
        assert_pass(
            &exact_match(true, true, &input(Some(&spaced), Some(&bare))).expect("trim"),
            true,
        );
        assert_pass(
            &exact_match(true, false, &input(Some(&spaced), Some(&bare))).expect("trim"),
            false,
        );

        let messages = json!([
            {"role": "assistant", "parts": [{"type": "text", "content": "ÉCOLE"}]}
        ]);
        assert_pass(
            &exact_match(false, true, &input(Some(&messages), Some(&expected))).expect("messages"),
            true,
        );
    }

    #[test]
    fn contains_needles_modes_and_expected() {
        let hello = json!({"check": "contains", "value": "ÉCOLE"});
        let (values, mode, case_sensitive) = stored_contains(&hello);
        assert_eq!(values, ["ÉCOLE"]);
        assert_eq!(mode, ContainsMode::Any);
        assert!(!case_sensitive);
        let output = json!("pré école");
        assert_pass(
            &contains(&values, mode, false, &input(Some(&output), None)).expect("value"),
            true,
        );
        assert_pass(
            &contains(&values, mode, true, &input(Some(&output), None)).expect("case"),
            false,
        );

        let any = json!({"check": "contains", "values": ["red", "blue"], "mode": "any"});
        let (values, mode, case_sensitive) = stored_contains(&any);
        let blue = json!("a blue boat");
        let green = json!("a green boat");
        assert_pass(
            &contains(&values, mode, case_sensitive, &input(Some(&blue), None)).expect("any"),
            true,
        );
        assert_pass(
            &contains(&values, mode, case_sensitive, &input(Some(&green), None)).expect("any"),
            false,
        );

        let all = json!({
            "check": "contains",
            "values": ["red", "blue"],
            "mode": "all",
            "case_sensitive": true
        });
        let (values, mode, case_sensitive) = stored_contains(&all);
        let both = json!("red then BLUE");
        let lower = json!("red then blue");
        assert_pass(
            &contains(&values, mode, case_sensitive, &input(Some(&both), None)).expect("all"),
            false,
        );
        assert_pass(
            &contains(&values, mode, false, &input(Some(&both), None)).expect("all folded"),
            true,
        );
        assert_pass(
            &contains(&values, mode, case_sensitive, &input(Some(&lower), None)).expect("all"),
            true,
        );

        let output = json!("the cat sat");
        let expected = json!("cat");
        assert_pass(
            &contains(
                &[],
                ContainsMode::Any,
                false,
                &input(Some(&output), Some(&expected)),
            )
            .expect("expected"),
            true,
        );
        let missing = json!("dog");
        assert_pass(
            &contains(
                &[],
                ContainsMode::Any,
                false,
                &input(Some(&output), Some(&missing)),
            )
            .expect("expected"),
            false,
        );
    }

    #[test]
    fn regex_searches_anywhere_and_rejects_a_bad_pattern() {
        let output = json!("hello world");
        assert_pass(
            &regex("world", &input(Some(&output), None)).expect("match"),
            true,
        );
        assert_pass(
            &regex("^world", &input(Some(&output), None)).expect("anchor"),
            false,
        );

        let err = regex("(SECRET-PAYLOAD", &input(Some(&output), None)).expect_err("pattern");
        assert_eq!(err, CheckError::InvalidPattern);
        assert_no_payload(&err.to_string(), "SECRET-PAYLOAD");

        let err = EvaluatorConfig::from_parts("check", &json!({"check": "regex", "pattern": "("}))
            .expect_err("config");
        assert_eq!(
            err,
            crate::error::ConfigError::Invalid {
                field: "pattern",
                reason: "invalid regular expression",
            }
        );
        assert_no_payload(&err.to_string(), "(");
    }

    #[test]
    fn regex_over_one_mebibyte_is_rejected() {
        let pattern = format!("{}a", "a?".repeat(100_000));
        let output = json!("a");
        let err = regex(&pattern, &input(Some(&output), None)).expect_err("size");
        assert_eq!(err, CheckError::InvalidPattern);
        let err =
            EvaluatorConfig::from_parts("check", &json!({"check": "regex", "pattern": pattern}))
                .expect_err("config size");
        assert_eq!(
            err,
            crate::error::ConfigError::Invalid {
                field: "pattern",
                reason: "invalid regular expression",
            }
        );
    }

    #[test]
    fn missing_variables_name_the_absent_one() {
        let present = json!("a");
        let missing_output =
            exact_match(true, true, &input(None, Some(&present))).expect_err("output");
        assert_eq!(
            missing_output,
            CheckError::MissingVariable { name: "output" }
        );
        let missing_expected =
            exact_match(true, true, &input(Some(&present), None)).expect_err("expected");
        assert_eq!(
            missing_expected,
            CheckError::MissingVariable { name: "expected" }
        );
        let missing_contains = contains(
            &["a".to_owned()],
            ContainsMode::Any,
            false,
            &input(None, None),
        )
        .expect_err("contains");
        assert_eq!(
            missing_contains,
            CheckError::MissingVariable { name: "output" }
        );
        let missing_needle = contains(&[], ContainsMode::All, true, &input(Some(&present), None))
            .expect_err("expected needle");
        assert_eq!(
            missing_needle,
            CheckError::MissingVariable { name: "expected" }
        );
        let missing_regex = regex("a", &input(None, None)).expect_err("regex");
        assert_eq!(
            missing_regex,
            CheckError::MissingVariable { name: "output" }
        );
    }

    fn boolean(result: &CheckResult) -> bool {
        match result.value {
            ScoreValue::Boolean(value) => value,
            ScoreValue::Numeric(_) | ScoreValue::Categorical { .. } | ScoreValue::Text(_) => {
                panic!("expected a boolean score");
            }
        }
    }

    fn similarity(result: &CheckResult) -> f64 {
        match result.value {
            ScoreValue::Numeric(value) => value,
            ScoreValue::Boolean(_) | ScoreValue::Categorical { .. } | ScoreValue::Text(_) => {
                panic!("expected a numeric score");
            }
        }
    }

    #[test]
    fn json_valid_accepts_json_and_reports_only_the_position() {
        let object = json!({"a": 1});
        let array = json!([1, 2]);
        let number = json!(12);
        let valid_string = json!(r#"{"a":1}"#);
        for output in [&object, &array, &number, &valid_string] {
            let result = json_valid(&input(Some(output), None)).expect("valid");
            assert!(boolean(&result));
            assert_eq!(result.reason, None);
        }

        let invalid = json!("SECRET-PAYLOAD");
        let result = json_valid(&input(Some(&invalid), None)).expect("invalid");
        assert!(!boolean(&result));
        let reason = result.reason.expect("reason");
        assert_eq!(reason, "invalid JSON at line 1 column 1");
        assert_no_payload(&reason, "SECRET-PAYLOAD");

        let missing = json_valid(&input(None, None)).expect_err("output");
        assert_eq!(missing, CheckError::MissingVariable { name: "output" });
    }

    #[test]
    fn json_schema_reports_the_path_and_keyword_only() {
        let schema = json!({
            "type": "object",
            "properties": {"answer": {"type": "number"}},
            "required": ["answer"]
        });
        let ok = json!({"answer": 1});
        let result = json_schema(&schema, &input(Some(&ok), None)).expect("pass");
        assert!(boolean(&result));
        assert_eq!(result.reason, None);

        let wrong_type = json!({"answer": "SECRET-PAYLOAD"});
        let result = json_schema(&schema, &input(Some(&wrong_type), None)).expect("type");
        assert!(!boolean(&result));
        let reason = result.reason.expect("reason");
        assert_eq!(reason, "/answer: type");
        assert_no_payload(&reason, "SECRET-PAYLOAD");

        let missing = json!({});
        let result = json_schema(&schema, &input(Some(&missing), None)).expect("required");
        assert_eq!(result.reason.as_deref(), Some(": required"));
        assert_no_payload(result.reason.as_deref().unwrap_or(""), "answer");

        let text = json!(r#"{"answer":"SECRET-PAYLOAD"}"#);
        let result = json_schema(&schema, &input(Some(&text), None)).expect("parsed string");
        assert_eq!(result.reason.as_deref(), Some("/answer: type"));
        assert_no_payload(result.reason.as_deref().unwrap_or(""), "SECRET-PAYLOAD");

        let not_json = json!("SECRET-PAYLOAD");
        let result = json_schema(&schema, &input(Some(&not_json), None)).expect("not json");
        assert_eq!(
            result.reason.as_deref(),
            Some("invalid JSON at line 1 column 1")
        );
        assert_no_payload(result.reason.as_deref().unwrap_or(""), "SECRET-PAYLOAD");

        let remote = json!({"$ref": "https://example.com/SECRET-PAYLOAD.json"});
        let err = EvaluatorConfig::from_parts(
            "check",
            &json!({"check": "json_schema", "schema": remote}),
        )
        .expect_err("remote ref");
        assert_eq!(
            err,
            crate::error::ConfigError::Invalid {
                field: "schema",
                reason: "invalid JSON schema",
            }
        );
        assert_no_payload(&err.to_string(), "SECRET-PAYLOAD");
        assert_no_payload(&err.to_string(), "https://");
    }

    #[test]
    fn levenshtein_scores_characters_and_normalizes() {
        let kitten = json!("kitten");
        let sitting = json!("sitting");
        let result = levenshtein(true, &input(Some(&kitten), Some(&sitting))).expect("pair");
        assert!((similarity(&result) - (1.0 - 3.0 / 7.0)).abs() < 1e-12);
        assert_eq!(result.reason, None);

        let spaced = json!("  École   des   lois ");
        let folded = json!("école des lois");
        let normalized = levenshtein(true, &input(Some(&spaced), Some(&folded))).expect("norm");
        assert!((similarity(&normalized) - 1.0).abs() < 1e-12);
        let raw = levenshtein(false, &input(Some(&spaced), Some(&folded))).expect("raw");
        assert!(similarity(&raw) < 1.0);

        let empty = json!("");
        let blanks = json!("   ");
        let both_empty = levenshtein(false, &input(Some(&empty), Some(&empty))).expect("empty");
        assert!((similarity(&both_empty) - 1.0).abs() < 1e-12);
        let blank_normalized =
            levenshtein(true, &input(Some(&blanks), Some(&empty))).expect("blanks");
        assert!((similarity(&blank_normalized) - 1.0).abs() < 1e-12);

        let cafe = json!("café");
        let cafe_ascii = json!("cafe");
        let multibyte = levenshtein(false, &input(Some(&cafe), Some(&cafe_ascii))).expect("cafe");
        assert!((similarity(&multibyte) - 0.75).abs() < 1e-12);
    }

    #[test]
    fn run_check_dispatches_each_check_from_its_json_config() {
        let output = json!("kitten");
        let expected = json!("sitting");
        let cases = [
            (
                json!({"check": "exact_match"}),
                Some(&output),
                Some(&expected),
                false,
            ),
            (
                json!({"check": "contains", "value": "ten"}),
                Some(&output),
                None,
                true,
            ),
            (
                json!({"check": "regex", "pattern": "kit"}),
                Some(&output),
                None,
                true,
            ),
            (
                json!({"check": "json_valid"}),
                Some(&json!({"ok": true})),
                None,
                true,
            ),
            (
                json!({"check": "json_schema", "schema": {"type": "object"}}),
                Some(&json!({"ok": true})),
                None,
                true,
            ),
        ];
        for (config, output, expected, pass) in cases {
            let EvaluatorConfig::Check(check) =
                EvaluatorConfig::from_parts("check", &config).expect("config")
            else {
                panic!("check");
            };
            let result = run_check(&check, &input(output, expected)).expect("run");
            assert_eq!(boolean(&result), pass);
        }

        let levenshtein = json!({"check": "levenshtein", "normalize": false});
        let EvaluatorConfig::Check(check) =
            EvaluatorConfig::from_parts("check", &levenshtein).expect("levenshtein")
        else {
            panic!("levenshtein");
        };
        let result = run_check(&check, &input(Some(&output), Some(&expected))).expect("run");
        assert!((similarity(&result) - (1.0 - 3.0 / 7.0)).abs() < 1e-12);
    }
}
