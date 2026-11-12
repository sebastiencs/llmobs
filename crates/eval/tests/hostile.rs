//! Hostile-response property tests (E20).
//!
//! The adapter decoders, the provider error reader, the webhook score reader,
//! and the answer parser get arbitrary bytes, arbitrary JSON, truncated JSON,
//! deeply nested JSON, and JSON shaped like a real reply with arbitrary leaves.
//! The decoders stay private: their properties live in each module's tests and
//! use the strategies here. Each property runs 256 cases unless
//! `LLMOBS_PROPTEST_CASES` sets another count. Inputs stay far below the 4 MiB
//! judge and 64 KiB webhook caps, which the egress client enforces.
#![allow(clippy::expect_used)]

use std::collections::BTreeMap;

use llmobs_core::score::ScoreValue;
use proptest::prelude::*;
use proptest::test_runner::FileFailurePersistence;
use serde_json::{Map, Value};

use crate::config::JudgeOutput;
use crate::error::JudgeError;
use crate::judge::{JudgeReply, RawAnswer, parse_answer};

/// Field names the decoders read, so random objects hit real paths.
const KEYS: &[&str] = &[
    "choices",
    "message",
    "content",
    "refusal",
    "finish_reason",
    "tool_calls",
    "function",
    "arguments",
    "usage",
    "model",
    "stop_reason",
    "stop_details",
    "type",
    "text",
    "input",
    "candidates",
    "finishReason",
    "parts",
    "thought",
    "functionCall",
    "args",
    "promptFeedback",
    "blockReason",
    "usageMetadata",
    "modelVersion",
    "error",
    "code",
    "status",
    "value",
    "score",
    "label",
    "explanation",
    "reason",
];

/// Strings the decoders branch on, plus answer-shaped text.
const WORDS: &[&str] = &[
    "",
    " ",
    "stop",
    "length",
    "content_filter",
    "max_tokens",
    "refusal",
    "end_turn",
    "tool_use",
    "text",
    "STOP",
    "MAX_TOKENS",
    "SAFETY",
    "RECITATION",
    "insufficient_quota",
    "content_policy_violation",
    "RESOURCE_EXHAUSTED",
    "INVALID_ARGUMENT",
    "true",
    " FALSE ",
    "0.5",
    "inf",
    "NaN",
    "correct",
    " Correct. ",
    "incorrect",
    "{\"value\": 0.5}",
    "{\"label\": \"correct\", \"explanation\": \"x\"}",
    "```json\n{\"value\": true}\n```",
    "```\n[1]\n```",
    "Score: {\"score\": 1} done",
    "{\"value\":",
    "é\u{0}\u{feff}",
];

const LABELS: &[&str] = &["correct", "incorrect", "Partially correct"];

pub(crate) fn config() -> ProptestConfig {
    let cases = std::env::var("LLMOBS_PROPTEST_CASES")
        .ok()
        .and_then(|text| text.parse().ok())
        .unwrap_or(256);
    ProptestConfig {
        cases,
        failure_persistence: Some(Box::new(FileFailurePersistence::Off)),
        ..ProptestConfig::default()
    }
}

fn word() -> impl Strategy<Value = String> {
    prop_oneof![
        2 => prop::sample::select(WORDS).prop_map(str::to_owned),
        1 => any::<String>(),
    ]
}

fn key() -> impl Strategy<Value = String> {
    prop_oneof![
        3 => prop::sample::select(KEYS).prop_map(str::to_owned),
        1 => any::<String>(),
    ]
}

fn leaf() -> impl Strategy<Value = Value> {
    prop_oneof![
        Just(Value::Null),
        any::<bool>().prop_map(Value::Bool),
        any::<i64>().prop_map(Value::from),
        any::<u64>().prop_map(Value::from),
        any::<f64>().prop_map(Value::from),
        prop::sample::select(&[0.0, 0.5, 1.0, -1.0, 1e308, -0.0][..]).prop_map(Value::from),
        word().prop_map(Value::String),
        (7_990usize..8_010).prop_map(|count| Value::String("é".repeat(count))),
    ]
}

/// Any JSON value, with keys biased toward the names the decoders read.
pub(crate) fn json() -> BoxedStrategy<Value> {
    leaf()
        .prop_recursive(6, 64, 6, |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
                prop::collection::vec((key(), inner), 0..6)
                    .prop_map(|entries| Value::Object(entries.into_iter().collect())),
            ]
        })
        .boxed()
}

/// A JSON value, or its compact text as a string (tool-call arguments).
fn field() -> BoxedStrategy<Value> {
    prop_oneof![
        3 => json(),
        1 => json().prop_map(|value| Value::String(value.to_string())),
    ]
    .boxed()
}

/// `Some` entries become object members; `None` leaves the key absent.
fn object(entries: Vec<(&'static str, Option<Value>)>) -> Value {
    Value::Object(
        entries
            .into_iter()
            .filter_map(|(key, value)| value.map(|value| (key.to_owned(), value)))
            .collect(),
    )
}

fn maybe(strategy: BoxedStrategy<Value>) -> BoxedStrategy<Option<Value>> {
    prop::option::weighted(0.7, strategy).boxed()
}

fn word_value() -> BoxedStrategy<Value> {
    prop_oneof![3 => word().prop_map(Value::String), 1 => json()].boxed()
}

/// One of the values a decoder branches on, or anything else.
fn tag(known: &'static [&'static str]) -> BoxedStrategy<Option<Value>> {
    maybe(
        prop_oneof![
            3 => prop::sample::select(known).prop_map(|tag| Value::String(tag.to_owned())),
            1 => word_value(),
        ]
        .boxed(),
    )
}

/// Message text: usually answer-shaped, sometimes any value.
fn text_value() -> BoxedStrategy<Value> {
    prop_oneof![3 => answer_text().prop_map(Value::String), 1 => word_value()].boxed()
}

/// Tool-call arguments as a string: usually a score object's JSON text.
fn arguments_value() -> BoxedStrategy<Value> {
    prop_oneof![
        3 => score_object().prop_map(|value| Value::String(value.to_string())),
        1 => field(),
    ]
    .boxed()
}

/// Tool-call arguments as a value: usually a score object.
fn input_value() -> BoxedStrategy<Value> {
    prop_oneof![3 => score_object(), 1 => json()].boxed()
}

/// A Chat Completions reply with arbitrary leaves.
pub(crate) fn openai_reply() -> BoxedStrategy<Value> {
    (
        tag(&["stop", "length", "content_filter", "tool_calls"]),
        maybe(text_value()),
        prop::option::weighted(0.2, word_value()),
        prop::option::weighted(0.4, maybe(arguments_value())),
        maybe(json()),
        maybe(word_value()),
    )
        .prop_map(|(finish, content, refusal, call, usage, model)| {
            let tool_calls = call.map(|arguments| {
                Value::Array(vec![object(vec![(
                    "function",
                    Some(object(vec![("arguments", arguments)])),
                )])])
            });
            let message = object(vec![
                ("content", content),
                ("refusal", refusal),
                ("tool_calls", tool_calls),
            ]);
            let choice = object(vec![("finish_reason", finish), ("message", Some(message))]);
            object(vec![
                ("choices", Some(Value::Array(vec![choice]))),
                ("usage", usage),
                ("model", model),
            ])
        })
        .boxed()
}

/// A Messages reply with arbitrary leaves.
pub(crate) fn anthropic_reply() -> BoxedStrategy<Value> {
    (
        tag(&["end_turn", "max_tokens", "refusal", "tool_use"]),
        prop::option::weighted(0.2, tag(&["refusal", "other"])),
        prop::collection::vec(
            (
                tag(&["text", "tool_use", "thinking"]),
                maybe(text_value()),
                maybe(input_value()),
            ),
            0..4,
        ),
        maybe(json()),
    )
        .prop_map(|(stop_reason, stop_type, blocks, usage)| {
            let stop_details = stop_type.map(|kind| object(vec![("type", kind)]));
            let blocks = blocks
                .into_iter()
                .map(|(kind, text, input)| {
                    object(vec![("type", kind), ("text", text), ("input", input)])
                })
                .collect();
            object(vec![
                ("stop_reason", stop_reason),
                ("stop_details", stop_details),
                ("content", Some(Value::Array(blocks))),
                ("usage", usage),
            ])
        })
        .boxed()
}

/// A `generateContent` reply with arbitrary leaves.
pub(crate) fn gemini_reply() -> BoxedStrategy<Value> {
    (
        prop::option::weighted(0.2, tag(&["", "SAFETY", "OTHER"])),
        tag(&["STOP", "MAX_TOKENS", "SAFETY", "RECITATION", "OTHER"]),
        prop::collection::vec(
            (
                prop::option::weighted(0.3, word_value()),
                maybe(text_value()),
                prop::option::weighted(0.3, maybe(input_value())),
            ),
            0..4,
        ),
        maybe(json()),
    )
        .prop_map(|(block, finish, parts, usage)| {
            let parts = parts
                .into_iter()
                .map(|(thought, text, call)| {
                    object(vec![
                        ("thought", thought),
                        ("text", text),
                        (
                            "functionCall",
                            call.map(|args| object(vec![("args", args)])),
                        ),
                    ])
                })
                .collect();
            let candidate = object(vec![
                ("finishReason", finish),
                (
                    "content",
                    Some(object(vec![("parts", Some(Value::Array(parts)))])),
                ),
            ]);
            let feedback = block.map(|reason| object(vec![("blockReason", reason)]));
            object(vec![
                ("promptFeedback", feedback),
                ("candidates", Some(Value::Array(vec![candidate]))),
                ("usageMetadata", usage),
            ])
        })
        .boxed()
}

/// An `error` envelope in either provider style.
pub(crate) fn error_reply() -> BoxedStrategy<Value> {
    (
        maybe(word_value()),
        maybe(word_value()),
        maybe(word_value()),
        maybe(word_value()),
    )
        .prop_map(|(kind, code, status, message)| {
            object(vec![(
                "error",
                Some(object(vec![
                    ("type", kind),
                    ("code", code),
                    ("status", status),
                    ("message", message),
                ])),
            )])
        })
        .boxed()
}

/// A value that often fits one of the outputs under test.
fn score_value() -> BoxedStrategy<Value> {
    prop_oneof![
        2 => prop::sample::select(&[0.0, 0.5, 1.0, 3.0, -0.5, 7.0][..]).prop_map(Value::from),
        1 => any::<bool>().prop_map(Value::Bool),
        1 => prop::sample::select(&[0u64, 1, 2][..]).prop_map(Value::from),
        2 => prop::sample::select(
            &["0.5", " 1 ", "true", "False", "correct", " Correct. ", "incorrect", "partially correct.", "nope"][..],
        )
        .prop_map(|text| Value::String(text.to_owned())),
        2 => word_value(),
    ]
    .boxed()
}

/// A webhook score object, or the score fields of a judge answer.
pub(crate) fn score_object() -> BoxedStrategy<Value> {
    (
        maybe(score_value()),
        prop::option::weighted(0.3, score_value()),
        maybe(score_value()),
        maybe(word_value()),
        maybe(word_value()),
    )
        .prop_map(|(value, score, label, explanation, reason)| {
            object(vec![
                ("value", value),
                ("score", score),
                ("label", label),
                ("explanation", explanation),
                ("reason", reason),
            ])
        })
        .boxed()
}

/// Bytes a hostile server could send: noise, any JSON, a shaped reply, a
/// truncated reply, or nesting past serde's depth limit.
pub(crate) fn body(shaped: BoxedStrategy<Value>) -> BoxedStrategy<Vec<u8>> {
    prop_oneof![
        1 => prop::collection::vec(any::<u8>(), 0..512),
        2 => json().prop_map(|value| value.to_string().into_bytes()),
        4 => shaped.clone().prop_map(|value| value.to_string().into_bytes()),
        1 => (shaped, any::<prop::sample::Index>()).prop_map(|(value, cut)| {
            let mut bytes = value.to_string().into_bytes();
            bytes.truncate(cut.index(bytes.len() + 1));
            bytes
        }),
        1 => (1usize..20_000, prop::bool::ANY).prop_map(|(depth, array)| {
            if array { "[".repeat(depth) } else { "{\"a\":".repeat(depth) }.into_bytes()
        }),
    ]
    .boxed()
}

fn answer_text() -> BoxedStrategy<String> {
    prop_oneof![
        2 => word(),
        2 => json().prop_map(|value| value.to_string()),
        3 => score_object().prop_map(|value| value.to_string()),
        2 => (word(), score_object(), word())
            .prop_map(|(before, value, after)| format!("{before} {value} {after}")),
        2 => (word(), score_object(), prop::sample::select(&["```json", "```", "```JSON"][..]))
            .prop_map(|(before, value, fence)| format!("{before}\n{fence}\n{value}\n```\n")),
        1 => (score_object(), any::<prop::sample::Index>()).prop_map(|(value, cut)| {
            let text = value.to_string();
            let end = cut.index(text.len() + 1);
            let end = (0..=end).rev().find(|&index| text.is_char_boundary(index)).unwrap_or(0);
            text[..end].to_owned()
        }),
        1 => (1usize..20_000).prop_map(|depth| "{".repeat(depth)),
    ]
    .boxed()
}

fn raw_answer() -> BoxedStrategy<RawAnswer> {
    prop_oneof![
        3 => answer_text().prop_map(RawAnswer::Text),
        2 => score_object().prop_map(|value| match value {
            Value::Object(object) => RawAnswer::Object(object),
            _ => RawAnswer::Object(Map::new()),
        }),
        1 => json().prop_map(|value| match value {
            Value::Object(object) => RawAnswer::Object(object),
            other => RawAnswer::Text(other.to_string()),
        }),
    ]
    .boxed()
}

fn judge_output() -> BoxedStrategy<JudgeOutput> {
    prop_oneof![
        prop::sample::select(&[(0.0, 1.0), (1.0, 5.0), (-1.0, 1.0), (0.5, 0.5)][..])
            .prop_map(|(min, max)| JudgeOutput::Numeric { min, max }),
        Just(JudgeOutput::Boolean),
        Just(JudgeOutput::Categorical {
            labels: LABELS.iter().map(|label| (*label).to_owned()).collect(),
            label_values: BTreeMap::from([("correct".to_owned(), 1.0)]),
        }),
    ]
    .boxed()
}

/// The checks every decoded answer must pass: a bounded explanation and a
/// value inside the configured output, or one of the documented parse errors.
pub(crate) fn check_answer(raw: RawAnswer, output: &JudgeOutput, include_reason: bool) {
    match parse_answer(raw, output, include_reason) {
        Ok(answer) => {
            if let Some(explanation) = &answer.explanation {
                assert!(include_reason);
                assert!(explanation.chars().count() <= 8_000);
            }
            match (&answer.value, output) {
                (ScoreValue::Numeric(value), JudgeOutput::Numeric { min, max }) => {
                    assert!(value.is_finite() && (*min..=*max).contains(value));
                }
                (ScoreValue::Boolean(_), JudgeOutput::Boolean) => {}
                (
                    ScoreValue::Categorical { label, value },
                    JudgeOutput::Categorical {
                        labels,
                        label_values,
                    },
                ) => {
                    assert!(labels.contains(label));
                    assert_eq!(value, &label_values.get(label).copied());
                }
                (value, output) => panic!("{value:?} does not fit {output:?}"),
            }
        }
        Err(
            JudgeError::EmptyAnswer
            | JudgeError::NotJson
            | JudgeError::NotAnObject
            | JudgeError::UnknownLabel
            | JudgeError::MissingField {
                field: "value" | "label",
            }
            | JudgeError::WrongType {
                field: "value" | "label",
            },
        ) => {}
        Err(JudgeError::OutOfRange { min, max }) => {
            assert_eq!(output, &JudgeOutput::Numeric { min, max });
        }
        Err(other) => panic!("undocumented answer error {other:?}"),
    }
}

/// A decoded reply's text matches its answer, and the answer parser accepts it
/// or fails with a documented error.
pub(crate) fn check_reply(reply: JudgeReply) {
    match &reply.answer {
        RawAnswer::Text(text) => assert_eq!(&reply.output_text, text),
        RawAnswer::Object(object) => {
            let echoed: Value = serde_json::from_str(&reply.output_text).expect("compact JSON");
            assert_eq!(echoed.as_object(), Some(object));
        }
    }
    check_answer(
        reply.answer,
        &JudgeOutput::Numeric { min: 0.0, max: 1.0 },
        true,
    );
}

proptest! {
    #![proptest_config(config())]

    #[test]
    fn the_answer_parser_accepts_only_fitting_scores(
        raw in raw_answer(),
        output in judge_output(),
        include_reason in any::<bool>(),
    ) {
        check_answer(raw, &output, include_reason);
    }
}
