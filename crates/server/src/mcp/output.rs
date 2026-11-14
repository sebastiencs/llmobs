//! What an MCP tool returns: compact JSON text, fitted to a character budget (D-H-11).

use rmcp::model::{CallToolResult, ContentBlock};
use serde_json::{Map, Value, json};

use crate::http::error::ApiError;

/// The default `max_chars` of a tool result (D-H-10).
pub(crate) const DEFAULT_MAX_CHARS: usize = 20_000;

/// Compact JSON, as sent to the agent. `Value`'s `Display` cannot fail.
pub(crate) fn render(value: &Value) -> String {
    value.to_string()
}

/// Length in characters (Unicode scalar values), the unit of every budget.
pub(crate) fn char_len(text: &str) -> usize {
    text.chars().count()
}

/// A successful tool result: one text item holding `body` as compact JSON.
pub(crate) fn success(body: &Value) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(render(body))])
}

/// A failed tool result: `{"error": {"code", "message", "position"?}}` with the spec 09
/// problem code. The message is the first field error's message when there is one (it carries
/// the position), else the problem's detail, else its title. Logged like an HTTP error.
pub(crate) fn failure(error: &ApiError) -> CallToolResult {
    error.log();
    let problem = error.problem();
    // `errors` is borrowed by `first()`; `detail` and `title` are other fields, so moving them is allowed.
    let (message, position) = match problem.errors.first() {
        Some(field) => (field.message.clone(), field.position),
        None => (problem.detail.unwrap_or(problem.title), None),
    };
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

    use super::{char_len, failure, fit_list, render};
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
}
