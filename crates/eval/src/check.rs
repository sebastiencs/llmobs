//! Text checks: exact match, contains, and regex.
//!
//! Spec 06 § `check` (D164). JSON and similarity checks arrive with the dispatcher.
//! Until that dispatcher calls these functions, only the unit tests do.

#![cfg_attr(not(test), allow(dead_code))]

use llmobs_core::score::ScoreValue;
use serde_json::Value;

use crate::config::ContainsMode;
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

/// A boolean score. Text checks do not set a reason.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct CheckResult {
    /// `true` when the check passes.
    pub value: ScoreValue,
    /// Always [`None`] for the text checks.
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

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::{CheckInput, CheckResult, contains, exact_match, regex};
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
}
