//! What an MCP tool returns: compact JSON text, fitted to a character budget (D-H-11).

use rmcp::model::{CallToolResult, ContentBlock};
use serde_json::{Map, Value, json};

use crate::http::error::ApiError;

/// The default `max_chars` of a tool result (D-H-10).
pub(crate) const DEFAULT_MAX_CHARS: usize = 20_000;

/// Lowest and highest `max_chars` a tool accepts (D-H-10).
pub(crate) const MIN_MAX_CHARS: usize = 1_000;
pub(crate) const MAX_MAX_CHARS: usize = 100_000;

/// Compact JSON, as sent to the agent. `Value`'s `Display` cannot fail.
pub(crate) fn render(value: &Value) -> String {
    value.to_string()
}

/// Longest error message in a tool result: serde errors can echo up to 1 MiB of the input.
const MAX_MESSAGE: usize = 500;

/// Length in characters (Unicode scalar values), the unit of every budget.
pub(crate) fn char_len(text: &str) -> usize {
    text.chars().count()
}

/// `max_chars` as given, or [`DEFAULT_MAX_CHARS`].
///
/// # Errors
///
/// `400 bad_request` `max_chars must be between 1000 and 100000` outside that range
/// (a negative number too: the argument is an `i64`).
pub(crate) fn max_chars(given: Option<i64>) -> Result<usize, ApiError> {
    let value = given.unwrap_or(DEFAULT_MAX_CHARS as i64);
    match usize::try_from(value) {
        Ok(value) if (MIN_MAX_CHARS..=MAX_MAX_CHARS).contains(&value) => Ok(value),
        _ => Err(ApiError::BadRequest {
            detail: Some("max_chars must be between 1000 and 100000".to_owned()),
            errors: Vec::new(),
        }),
    }
}

/// `text` cut to at most `max` characters in total: its first `max - 1` characters followed
/// by `…` (for `max == 0`, the empty string), or `None` when it has at most `max` characters.
/// A cut string has exactly `max` characters, so cutting it again is a no-op. Characters are
/// Unicode scalar values, so a multibyte character is never split.
pub(crate) fn cut(text: &str, max: usize) -> Option<String> {
    if text.chars().count() <= max {
        return None;
    }
    let mut out: String = text.chars().take(max.saturating_sub(1)).collect();
    if max > 0 {
        out.push('…');
    }
    Some(out)
}

/// A successful tool result: one text item holding `body` as compact JSON.
pub(crate) fn success(body: &Value) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(render(body))])
}

/// A failed tool result: `{"error": {"code", "message", "position"?}}` with the spec 09
/// problem code. The message is the first field error's message when there is one (it carries
/// the position), else the problem's detail, else its title, cut to [`MAX_MESSAGE`]
/// characters. Logged like an HTTP error.
pub(crate) fn failure(error: &ApiError) -> CallToolResult {
    error.log();
    let problem = error.problem();
    // `errors` is borrowed by `first()`; `detail` and `title` are other fields, so moving them is allowed.
    let (message, position) = match problem.errors.first() {
        Some(field) => (field.message.clone(), field.position),
        None => (problem.detail.unwrap_or(problem.title), None),
    };
    let message = cut(&message, MAX_MESSAGE).unwrap_or(message);
    let mut inner = Map::new();
    inner.insert("code".to_owned(), json!(problem.code));
    inner.insert("message".to_owned(), Value::String(message));
    if let Some(position) = position {
        inner.insert(
            "position".to_owned(),
            json!({"line": position.line, "column": position.column}),
        );
    }
    CallToolResult::error(vec![ContentBlock::text(render(&json!({"error": inner})))])
}

/// Fits `body` in `max_chars` by keeping the longest prefix of the array `body[list]`.
///
/// Returns `false` and leaves `body` unchanged when it already fits, or when `body[list]` is
/// not an array. Otherwise sets `body["truncated"]` to `true`, appends `note` to the array
/// `body["notes"]` (creating either key when missing), replaces `body[list]` in place (key order
/// is kept) by its longest prefix that makes the whole body fit — possibly empty — and returns
/// `true`. The prefix length is found by binary search.
pub(crate) fn fit_list(
    body: &mut Map<String, Value>,
    list: &str,
    max_chars: usize,
    note: &str,
) -> bool {
    if char_len(&render_map(body)) <= max_chars {
        return false;
    }
    let items = match body.get(list) {
        Some(Value::Array(items)) => items.clone(),
        _ => return false,
    };
    body.insert("truncated".to_owned(), Value::Bool(true));
    match body.get_mut("notes") {
        Some(Value::Array(notes)) => notes.push(Value::String(note.to_owned())),
        _ => {
            body.insert(
                "notes".to_owned(),
                Value::Array(vec![Value::String(note.to_owned())]),
            );
        }
    }
    let (mut low, mut high) = (0, items.len());
    while low < high {
        let mid = (low + high).div_ceil(2);
        set_prefix(body, list, &items, mid);
        if char_len(&render_map(body)) <= max_chars {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    set_prefix(body, list, &items, low);
    true
}

fn render_map(body: &Map<String, Value>) -> String {
    Value::Object(body.clone()).to_string()
}

fn set_prefix(body: &mut Map<String, Value>, list: &str, items: &[Value], len: usize) {
    if let Some(slot) = body.get_mut(list) {
        *slot = Value::Array(items[..len].to_vec());
    }
}

#[cfg(test)]
mod tests {
    use llmobs_storage::{InvalidQuery, Position, RunError};
    use serde_json::{Map, Value, json};

    use super::{char_len, cut, failure, fit_list, max_chars, render};
    use crate::http::error::ApiError;

    fn text_of(result: &rmcp::model::CallToolResult) -> &str {
        result.content[0]
            .as_text()
            .map(|text| text.text.as_str())
            .expect("text content")
    }

    #[test]
    fn fit_list_leaves_a_fitting_body_alone() {
        let mut body: Map<String, Value> = serde_json::from_value(json!({
            "items": [1, 2, 3], "truncated": false, "notes": []
        }))
        .unwrap();
        let before = body.clone();
        assert!(!fit_list(&mut body, "items", 1000, "cut"));
        assert_eq!(body, before);
    }

    #[test]
    fn fit_list_keeps_the_longest_prefix() {
        let items: Vec<Value> = (0..100).map(|_| json!("aaaaaaaaaa")).collect();
        let mut body: Map<String, Value> = serde_json::from_value(json!({
            "items": items, "truncated": false, "notes": []
        }))
        .unwrap();
        assert!(fit_list(&mut body, "items", 300, "cut"));
        assert_eq!(body["truncated"], json!(true));
        assert_eq!(body["notes"], json!(["cut"]));
        let kept = body["items"].as_array().unwrap().len();
        assert!(kept > 0 && kept < 100);
        assert!(char_len(&render(&Value::Object(body.clone()))) <= 300);
        let mut one_more = body.clone();
        one_more["items"] = Value::Array(vec![json!("aaaaaaaaaa"); kept + 1]);
        assert!(char_len(&render(&Value::Object(one_more))) > 300);
        let keys: Vec<&str> = body.keys().map(String::as_str).collect();
        assert_eq!(keys, ["items", "truncated", "notes"]);
    }

    #[test]
    fn fit_list_can_empty_the_list() {
        let mut body: Map<String, Value> = serde_json::from_value(json!({
            "items": ["a".repeat(1000)], "truncated": false, "notes": []
        }))
        .unwrap();
        assert!(fit_list(&mut body, "items", 50, "cut"));
        assert_eq!(body["items"], json!([]));
        assert_eq!(body["truncated"], json!(true));
    }

    #[test]
    fn cut_counts_characters() {
        assert_eq!(cut("héllo", 5), None);
        assert_eq!(cut("héllo", 2), Some("h…".to_owned()));
        assert_eq!(cut("", 0), None);
        assert_eq!(cut(&"é".repeat(2000), 2000), None);
        let long = cut(&"é".repeat(2001), 2000).expect("cut");
        assert_eq!(long.chars().count(), 2000);
        assert_eq!(long, format!("{}…", "é".repeat(1999)));
        // Cutting a cut value is a no-op.
        assert_eq!(cut(&long, 2000), None);
        assert_eq!(cut("x", 0), Some(String::new()));
    }

    #[test]
    fn max_chars_bounds() {
        assert_eq!(max_chars(None).unwrap(), 20_000);
        assert_eq!(max_chars(Some(1_000)).unwrap(), 1_000);
        assert_eq!(max_chars(Some(100_000)).unwrap(), 100_000);
        for value in [999, 100_001, -1] {
            assert!(
                matches!(max_chars(Some(value)), Err(ApiError::BadRequest { .. })),
                "{value}"
            );
        }
    }

    #[test]
    fn failure_carries_code_message_and_position() {
        let query = RunError::Invalid(InvalidQuery {
            message: "unknown column x".to_owned(),
            position: Some(Position { line: 1, column: 8 }),
        });
        let expected = r#"{"error":{"code":"invalid_filter","message":"unknown column x","position":{"line":1,"column":8}}}"#;
        let result = failure(&ApiError::Query(query));
        assert_eq!(result.is_error, Some(true));
        assert_eq!(text_of(&result), expected);
        let query = RunError::Invalid(InvalidQuery {
            message: "unknown column x".to_owned(),
            position: Some(Position { line: 1, column: 8 }),
        });
        assert_eq!(text_of(&failure(&ApiError::Sql(query))), expected);
        assert_eq!(
            text_of(&failure(&ApiError::NotFound)),
            r#"{"error":{"code":"not_found","message":"Not Found"}}"#
        );
    }

    #[test]
    fn failure_clamps_the_message() {
        let error = ApiError::BadRequest {
            detail: Some(format!("invalid arguments: {}", "x".repeat(10_000))),
            errors: Vec::new(),
        };
        let result = failure(&error);
        let body: Value = serde_json::from_str(text_of(&result)).unwrap();
        let message = body["error"]["message"].as_str().unwrap();
        assert_eq!(message.chars().count(), 500);
        assert!(message.ends_with('…'));
        assert!(message.starts_with("invalid arguments: xxx"));
    }
}
