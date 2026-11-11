//! Provider wire bodies rendered from a scripted [`Turn`](crate::scenario::Turn).
//!
//! Rendering is pure. The mock server adds the scripted delays.

pub mod anthropic;
pub mod gemini;
pub mod openai;

use serde_json::Value;

/// A rendered provider response.
#[derive(Debug, Clone, PartialEq)]
pub enum Rendered {
    /// One JSON body.
    Json(Value),
    /// Server-sent events, in order.
    Sse(Vec<SseFrame>),
}

/// One server-sent event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SseFrame {
    /// `event:` value. Absent for OpenAI chat chunks and Gemini frames.
    pub event: Option<String>,
    /// `data:` value. Not JSON for the chat stream's `[DONE]` frame.
    pub data: String,
}

impl SseFrame {
    /// Encodes one event. `line_end` is `"\n"` except for Gemini, which uses `"\r\n"`.
    pub fn encode(&self, line_end: &str) -> String {
        let mut out = String::new();
        if let Some(event) = &self.event {
            out.push_str("event: ");
            out.push_str(event);
            out.push_str(line_end);
        }
        out.push_str("data: ");
        out.push_str(&self.data);
        out.push_str(line_end);
        out.push_str(line_end);
        out
    }
}

/// Deterministic ids for one scripted turn.
///
/// `turn` is the 0-based index. Display numbers are `turn + 1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ids {
    /// 0-based turn index.
    pub turn: usize,
}

impl Ids {
    /// `created` and `created_at` on every rendered response (2026-01-01T00:00:00Z).
    pub const CREATED: i64 = 1_767_225_600;

    /// Chat Completions id, `chatcmpl-capture-{n}`.
    pub fn chat_completion(self) -> String {
        format!("chatcmpl-capture-{}", self.turn + 1)
    }

    /// Responses id, `resp_capture_{n}`.
    pub fn response(self) -> String {
        format!("resp_capture_{}", self.turn + 1)
    }

    /// Output message id, `msg_capture_{n}`.
    pub fn message(self) -> String {
        format!("msg_capture_{}", self.turn + 1)
    }

    /// Reasoning item id, `rs_capture_{n}`.
    pub fn reasoning(self) -> String {
        format!("rs_capture_{}", self.turn + 1)
    }

    /// Function-call item id, `fc_capture_{n}_{i}`. `index` is 0-based.
    pub fn function_call(self, index: usize) -> String {
        format!("fc_capture_{}_{}", self.turn + 1, index + 1)
    }
}

/// Splits `text` into at most `chunks` non-empty pieces, on char boundaries.
///
/// Pieces differ in length by at most one character. Earlier pieces take the extra
/// characters. An empty string, or `chunks == 0`, yields no pieces.
pub fn split_text(text: &str, chunks: u32) -> Vec<&str> {
    if text.is_empty() || chunks == 0 {
        return Vec::new();
    }
    let starts: Vec<usize> = text.char_indices().map(|(index, _)| index).collect();
    let count = starts.len();
    let pieces = (chunks as usize).min(count);
    let base = count / pieces;
    let extra = count % pieces;
    let mut out = Vec::with_capacity(pieces);
    let mut cursor = 0;
    for index in 0..pieces {
        let len = base + usize::from(index < extra);
        let start = starts[cursor];
        cursor += len;
        let end = if cursor == count {
            text.len()
        } else {
            starts[cursor]
        };
        out.push(&text[start..end]);
    }
    out
}

pub(crate) fn json_string(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "{}".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_frame_encoding() {
        let named = SseFrame {
            event: Some("response.created".to_owned()),
            data: "{}".to_owned(),
        };
        assert_eq!(named.encode("\n"), "event: response.created\ndata: {}\n\n");
        assert_eq!(
            named.encode("\r\n"),
            "event: response.created\r\ndata: {}\r\n\r\n"
        );
        let bare = SseFrame {
            event: None,
            data: "[DONE]".to_owned(),
        };
        assert_eq!(bare.encode("\n"), "data: [DONE]\n\n");
    }

    #[test]
    fn ids_are_one_based() {
        let ids = Ids { turn: 0 };
        assert_eq!(ids.chat_completion(), "chatcmpl-capture-1");
        assert_eq!(ids.response(), "resp_capture_1");
        assert_eq!(ids.message(), "msg_capture_1");
        assert_eq!(ids.reasoning(), "rs_capture_1");
        assert_eq!(ids.function_call(0), "fc_capture_1_1");
        assert_eq!(Ids { turn: 2 }.function_call(1), "fc_capture_3_2");
        assert_eq!(Ids::CREATED, 1_767_225_600);
    }

    #[test]
    fn split_text_keeps_char_boundaries_and_balances_pieces() {
        let pieces = split_text("héllo", 2);
        assert_eq!(pieces, vec!["hél", "lo"]);
        assert_eq!(pieces.concat(), "héllo");

        assert_eq!(split_text("abcdef", 4), vec!["ab", "cd", "e", "f"]);
        assert_eq!(split_text("éé", 5), vec!["é", "é"]);
        assert_eq!(split_text("héllo", 1), vec!["héllo"]);
        assert!(split_text("", 3).is_empty());
        assert!(split_text("ab", 0).is_empty());
    }
}
