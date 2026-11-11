//! Truncation of payloads and span strings.
//!
//! Spec 02 § Truncation. Runs after media extraction. A string whose byte
//! length equals the limit is kept. A longer string is cut on a UTF-8 boundary
//! and the marker is appended, so the stored value is longer than the limit.

use std::collections::BTreeMap;

use llmobs_core::message::{Message, Part, Role, messages_to_json};
use llmobs_core::names::marker::TRUNCATED_MESSAGES_PREFIX;
use llmobs_core::names::marker::TRUNCATED_STRING_SUFFIX_PREFIX;
use llmobs_core::span::Span;
use serde_json::{Map, Value as Json};

use crate::payload::{Io, Payload};

/// Step 2 does not cut a string leaf below this many bytes.
const LEAF_FLOOR: usize = 1024;

/// Truncate `input` and `output`. A messages list that still does not fit
/// becomes text and records `truncate:<field>`.
pub(crate) fn truncate_payloads(io: &mut Io, limit: usize, names: &mut Vec<String>) {
    truncate_column(
        &mut io.input,
        limit,
        "input",
        "truncate:input",
        &mut io.errors,
        names,
    );
    truncate_column(
        &mut io.output,
        limit,
        "output",
        "truncate:output",
        &mut io.errors,
        names,
    );
}

fn truncate_column(
    slot: &mut Option<Payload>,
    limit: usize,
    name: &str,
    error: &'static str,
    errors: &mut Vec<&'static str>,
    names: &mut Vec<String>,
) {
    let Some(payload) = slot.take() else {
        return;
    };
    let (next, change) = apply_payload(payload, limit);
    *slot = Some(next);
    match change {
        Change::Same => {}
        Change::Cut => names.push(name.to_owned()),
        Change::Text => {
            names.push(name.to_owned());
            errors.push(error);
        }
    }
}

enum Change {
    Same,
    Cut,
    Text,
}

fn apply_payload(payload: Payload, limit: usize) -> (Payload, Change) {
    match payload {
        Payload::Messages(messages) => match fit_messages(messages, limit) {
            Fitted::Same(messages) => (Payload::Messages(messages), Change::Same),
            Fitted::Messages(messages) => (Payload::Messages(messages), Change::Cut),
            Fitted::Text(text) => (Payload::Text(text), Change::Text),
        },
        Payload::Json(value) => {
            let text = serde_json::to_string(&value).unwrap_or_else(|_| "null".to_owned());
            if text.len() <= limit {
                (Payload::Json(value), Change::Same)
            } else {
                (Payload::Text(cut_string(&text, limit)), Change::Cut)
            }
        }
        Payload::Text(text) => {
            if text.len() <= limit {
                (Payload::Text(text), Change::Same)
            } else {
                (Payload::Text(cut_string(&text, limit)), Change::Cut)
            }
        }
    }
}

enum Fitted {
    Same(Vec<Message>),
    Messages(Vec<Message>),
    Text(String),
}

fn fit_messages(messages: Vec<Message>, limit: usize) -> Fitted {
    if messages_to_json(&messages).len() <= limit {
        return Fitted::Same(messages);
    }
    let mut kept = keep_prefix_and_suffix(&messages, limit);
    if messages_to_json(&kept).len() <= limit {
        return Fitted::Messages(kept);
    }
    shrink_leaves(&mut kept, limit);
    if messages_to_json(&kept).len() <= limit {
        return Fitted::Messages(kept);
    }
    Fitted::Text(cut_string(&messages_to_json(&kept), limit))
}

/// Protected messages plus the longest suffix whose serialized form fits.
fn keep_prefix_and_suffix(messages: &[Message], limit: usize) -> Vec<Message> {
    let protected = protected_indices(messages);
    let fallback = assemble(messages, &protected, messages.len());
    for keep_from in 1..=messages.len() {
        let candidate = assemble(messages, &protected, keep_from);
        if messages_to_json(&candidate).len() <= limit {
            return candidate;
        }
    }
    fallback
}

fn protected_indices(messages: &[Message]) -> Vec<bool> {
    let mut flags = vec![false; messages.len()];
    let mut saw_user = false;
    for (index, message) in messages.iter().enumerate() {
        if message.role == Role::System {
            flags[index] = true;
        }
        if !saw_user && message.role == Role::User {
            flags[index] = true;
            saw_user = true;
        }
    }
    flags
}

/// `keep_from` is the first index of the kept suffix. Protected messages stay
/// wherever they are. Each removed run becomes one placeholder.
fn assemble(messages: &[Message], protected: &[bool], keep_from: usize) -> Vec<Message> {
    let mut out = Vec::new();
    let mut removed = 0usize;
    for (index, message) in messages.iter().enumerate() {
        let keep = protected[index] || index >= keep_from;
        if keep {
            if removed > 0 {
                out.push(placeholder(removed));
                removed = 0;
            }
            out.push(message.clone());
        } else {
            removed += 1;
        }
    }
    if removed > 0 {
        out.push(placeholder(removed));
    }
    out
}

fn placeholder(removed: usize) -> Message {
    Message {
        role: Role::System,
        name: None,
        parts: vec![Part::Text {
            content: format!("{TRUNCATED_MESSAGES_PREFIX}{removed} messages truncated]"),
            extra: Map::new(),
        }],
        finish_reason: None,
        extra: Map::new(),
    }
}

fn shrink_leaves(messages: &mut [Message], limit: usize) {
    loop {
        if messages_to_json(messages).len() <= limit {
            return;
        }
        let Some(pos) = longest_leaf(messages) else {
            return;
        };
        let original = leaf_text(messages, pos).to_owned();
        let marker = string_marker(original.len());
        let mut best = None;
        let mut low = LEAF_FLOOR;
        let mut high = original.len();
        while low <= high {
            let mid = low + (high - low) / 2;
            set_leaf(messages, pos, marked(&original, mid, &marker));
            if messages_to_json(messages).len() <= limit {
                best = Some(mid);
                if mid == original.len() {
                    break;
                }
                low = mid + 1;
            } else if mid == 0 {
                break;
            } else {
                high = mid - 1;
            }
        }
        let cut_at = best.unwrap_or(LEAF_FLOOR);
        set_leaf(messages, pos, marked(&original, cut_at, &marker));
        if best.is_some() {
            return;
        }
    }
}

#[derive(Clone, Copy)]
enum LeafPos {
    Text { message: usize, part: usize },
    Reasoning { message: usize, part: usize },
    Arguments { message: usize, part: usize },
    Response { message: usize, part: usize },
}

fn longest_leaf(messages: &[Message]) -> Option<LeafPos> {
    let mut best: Option<(usize, LeafPos)> = None;
    for (message_index, message) in messages.iter().enumerate() {
        for (part_index, part) in message.parts.iter().enumerate() {
            let found = match part {
                Part::Text { content, .. } if leaf_open(content) => Some(LeafPos::Text {
                    message: message_index,
                    part: part_index,
                }),
                Part::Reasoning { content, .. } if leaf_open(content) => Some(LeafPos::Reasoning {
                    message: message_index,
                    part: part_index,
                }),
                Part::ToolCall {
                    arguments: Some(Json::String(text)),
                    ..
                } if leaf_open(text) => Some(LeafPos::Arguments {
                    message: message_index,
                    part: part_index,
                }),
                Part::ToolCallResponse {
                    response: Json::String(text),
                    ..
                } if leaf_open(text) => Some(LeafPos::Response {
                    message: message_index,
                    part: part_index,
                }),
                _ => None,
            };
            if let Some(pos) = found {
                let len = leaf_text(messages, pos).len();
                if best.is_none_or(|(best_len, _)| len > best_len) {
                    best = Some((len, pos));
                }
            }
        }
    }
    best.map(|(_, pos)| pos)
}

fn leaf_open(text: &str) -> bool {
    text.len() > LEAF_FLOOR && !text.contains(TRUNCATED_STRING_SUFFIX_PREFIX)
}

fn leaf_text(messages: &[Message], pos: LeafPos) -> &str {
    let missing = "";
    let Some(message) = messages.get(message_index(pos)) else {
        return missing;
    };
    let Some(part) = message.parts.get(part_index(pos)) else {
        return missing;
    };
    match (pos, part) {
        (LeafPos::Text { .. }, Part::Text { content, .. })
        | (LeafPos::Reasoning { .. }, Part::Reasoning { content, .. }) => content,
        (
            LeafPos::Arguments { .. },
            Part::ToolCall {
                arguments: Some(Json::String(text)),
                ..
            },
        )
        | (
            LeafPos::Response { .. },
            Part::ToolCallResponse {
                response: Json::String(text),
                ..
            },
        ) => text,
        _ => missing,
    }
}

fn set_leaf(messages: &mut [Message], pos: LeafPos, value: String) {
    let Some(message) = messages.get_mut(message_index(pos)) else {
        return;
    };
    let Some(part) = message.parts.get_mut(part_index(pos)) else {
        return;
    };
    match (pos, part) {
        (LeafPos::Text { .. }, Part::Text { content, .. })
        | (LeafPos::Reasoning { .. }, Part::Reasoning { content, .. }) => *content = value,
        (
            LeafPos::Arguments { .. },
            Part::ToolCall {
                arguments: Some(Json::String(text)),
                ..
            },
        )
        | (
            LeafPos::Response { .. },
            Part::ToolCallResponse {
                response: Json::String(text),
                ..
            },
        ) => *text = value,
        _ => {}
    }
}

fn message_index(pos: LeafPos) -> usize {
    match pos {
        LeafPos::Text { message, .. }
        | LeafPos::Reasoning { message, .. }
        | LeafPos::Arguments { message, .. }
        | LeafPos::Response { message, .. } => message,
    }
}

fn part_index(pos: LeafPos) -> usize {
    match pos {
        LeafPos::Text { part, .. }
        | LeafPos::Reasoning { part, .. }
        | LeafPos::Arguments { part, .. }
        | LeafPos::Response { part, .. } => part,
    }
}

fn marked(original: &str, cut_at: usize, marker: &str) -> String {
    let end = original.floor_char_boundary(cut_at.min(original.len()));
    let mut out = String::with_capacity(end + marker.len());
    out.push_str(&original[..end]);
    out.push_str(marker);
    out
}

/// Cut `name`, `status_message`, attribute maps, event attribute JSON, and the
/// JSON payload columns. Sort `truncated` and drop duplicates.
pub(crate) fn truncate_columns(span: &mut Span, limit: usize) {
    if let Some(cut) = cut_if_over(&span.name, limit) {
        span.name = cut;
        span.truncated.push("name".to_owned());
    }
    if let Some(message) = span.status_message.as_mut()
        && let Some(cut) = cut_if_over(message, limit)
    {
        *message = cut;
        span.truncated.push("status_message".to_owned());
    }
    truncate_map(&mut span.attributes, &mut span.truncated, limit);
    truncate_map(&mut span.resource_attributes, &mut span.truncated, limit);
    for (index, event) in span.events.iter_mut().enumerate() {
        if let Some(attributes) = event.attributes.as_mut()
            && let Some(cut) = cut_if_over(attributes, limit)
        {
            *attributes = cut;
            span.truncated.push(format!("events[{index}].attributes"));
        }
    }
    if let Some(text) = span.system_instructions.as_mut()
        && let Some(cut) = cut_if_over(text, limit)
    {
        *text = cut;
        span.truncated.push("system_instructions".to_owned());
    }
    if let Some(text) = span.tool_definitions.as_mut()
        && let Some(cut) = cut_if_over(text, limit)
    {
        *text = cut;
        span.truncated.push("tool_definitions".to_owned());
    }
    span.truncated.sort();
    span.truncated.dedup();
}

fn truncate_map(map: &mut BTreeMap<String, String>, names: &mut Vec<String>, limit: usize) {
    for (key, value) in map.iter_mut() {
        if let Some(cut) = cut_if_over(value, limit) {
            *value = cut;
            names.push(key.clone());
        }
    }
}

fn cut_if_over(text: &str, limit: usize) -> Option<String> {
    if text.len() <= limit {
        None
    } else {
        Some(cut_string(text, limit))
    }
}

fn cut_string(text: &str, limit: usize) -> String {
    let end = text.floor_char_boundary(limit.min(text.len()));
    let marker = string_marker(text.len());
    let mut out = String::with_capacity(end + marker.len());
    out.push_str(&text[..end]);
    out.push_str(&marker);
    out
}

fn string_marker(original_bytes: usize) -> String {
    format!("{TRUNCATED_STRING_SUFFIX_PREFIX}{original_bytes} bytes]")
}

#[cfg(test)]
mod tests {
    use super::*;
    use llmobs_core::message::Role;

    fn user(content: &str) -> Message {
        Message {
            role: Role::User,
            name: None,
            parts: vec![Part::Text {
                content: content.to_owned(),
                extra: Map::new(),
            }],
            finish_reason: None,
            extra: Map::new(),
        }
    }

    fn system(content: &str) -> Message {
        Message {
            role: Role::System,
            name: None,
            parts: vec![Part::Text {
                content: content.to_owned(),
                extra: Map::new(),
            }],
            finish_reason: None,
            extra: Map::new(),
        }
    }

    fn assistant(content: &str) -> Message {
        Message {
            role: Role::Assistant,
            name: None,
            parts: vec![Part::Text {
                content: content.to_owned(),
                extra: Map::new(),
            }],
            finish_reason: None,
            extra: Map::new(),
        }
    }

    #[test]
    fn a_four_byte_character_is_not_split() {
        let text = "𝄞𝄞";
        assert_eq!(text.len(), 8);
        let cut = cut_string(text, 6);
        assert!(cut.starts_with('𝄞'));
        assert_eq!(cut.chars().filter(|c| *c == '𝄞').count(), 1);
        assert!(cut.contains(TRUNCATED_STRING_SUFFIX_PREFIX));
        assert!(cut.ends_with("8 bytes]"));
    }

    #[test]
    fn the_limit_is_inclusive() {
        assert_eq!(cut_if_over("abcd", 4), None);
        let cut = cut_if_over("abcde", 4).expect("over");
        assert!(cut.starts_with("abcd"));
        assert!(cut.ends_with("5 bytes]"));
    }

    #[test]
    fn the_middle_of_a_message_list_is_replaced() {
        let messages = vec![
            system("rules"),
            user("hello"),
            assistant("one"),
            assistant("two"),
            assistant("three"),
            assistant("end"),
        ];
        let full = messages_to_json(&messages).len();
        let protected = protected_indices(&messages);
        let suffix = assemble(&messages, &protected, 5);
        let limit = messages_to_json(&suffix).len();
        assert!(limit < full);
        let fitted = fit_messages(messages, limit);
        let Fitted::Messages(kept) = fitted else {
            panic!("expected a message list");
        };
        let json = messages_to_json(&kept);
        assert!(json.len() <= limit, "{json}");
        assert!(json.contains("[llmobs: 3 messages truncated]"), "{json}");
        assert!(json.contains("\"content\":\"rules\""), "{json}");
        assert!(json.contains("\"content\":\"hello\""), "{json}");
        assert!(json.contains("\"content\":\"end\""), "{json}");
        assert!(!json.contains("\"content\":\"one\""), "{json}");
        let fixture = vec![
            system("rules"),
            user("hello"),
            assistant("one"),
            assistant("two"),
            assistant("end"),
        ];
        let kept = assemble(&fixture, &protected_indices(&fixture), 4);
        assert_eq!(
            messages_to_json(&kept).len(),
            273,
            "{}",
            messages_to_json(&kept)
        );
    }

    #[test]
    fn a_system_message_between_gaps_makes_two_placeholders() {
        let messages = vec![
            system("rules"),
            user("hello"),
            assistant(&"one".repeat(40)),
            system("mid"),
            assistant(&"two".repeat(40)),
            assistant("end"),
        ];
        let protected = protected_indices(&messages);
        let kept = assemble(&messages, &protected, 5);
        let json = messages_to_json(&kept);
        assert_eq!(
            json.matches("[llmobs: 1 messages truncated]").count(),
            2,
            "{json}"
        );
        let limit = json.len();
        let Fitted::Messages(fitted) = fit_messages(messages, limit) else {
            panic!("step 1 keeps messages");
        };
        assert_eq!(messages_to_json(&fitted), json);
    }

    #[test]
    fn json_over_the_limit_becomes_text_without_an_error() {
        let mut names = Vec::new();
        let mut io = Io {
            input: Some(Payload::Json(
                serde_json::json!({"query": "abcdefghijklmnopqrstuvwxyz0123456789"}),
            )),
            ..Io::default()
        };
        truncate_payloads(&mut io, 32, &mut names);
        assert_eq!(names, ["input"]);
        assert!(io.errors.is_empty());
        let Some(Payload::Text(text)) = io.input else {
            panic!("text");
        };
        assert_eq!(
            text,
            "{\"query\":\"abcdefghijklmnopqrstuv…[llmobs: truncated, original 48 bytes]"
        );
    }

    #[test]
    fn a_huge_message_is_cut_at_its_leaf() {
        let content = "x".repeat(4_000);
        let messages = vec![user(&content)];
        let fitted = fit_messages(messages, 1_500);
        let Fitted::Messages(kept) = fitted else {
            panic!("step 2 keeps messages");
        };
        let json = messages_to_json(&kept);
        assert!(json.len() <= 1_500, "{}", json.len());
        assert!(json.contains(TRUNCATED_STRING_SUFFIX_PREFIX), "{json}");
        assert!(json.contains("4000 bytes]"), "{json}");
    }

    #[test]
    fn a_list_that_cannot_fit_becomes_text() {
        let mut names = Vec::new();
        let mut io = Io {
            input: Some(Payload::Messages(vec![user(&"y".repeat(4_000))])),
            ..Io::default()
        };
        truncate_payloads(&mut io, 64, &mut names);
        assert_eq!(names, ["input"]);
        assert_eq!(io.errors, ["truncate:input"]);
        let Some(Payload::Text(text)) = io.input else {
            panic!("text fallback");
        };
        assert!(text.len() > 64, "{}", text.len());
        assert!(text.contains(TRUNCATED_STRING_SUFFIX_PREFIX), "{text}");
    }
}
