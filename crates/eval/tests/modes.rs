//! Tool and prompt structured-output modes against the in-process mock.
//!
//! This file is an integration test, so clippy's test allow does not apply.
#![allow(clippy::expect_used)]

mod support {
    pub(crate) mod mock;
}

use std::time::Duration;

use llmobs_core::score::ScoreValue;
use llmobs_eval::{
    Adapter, Answer, EgressClient, EgressPolicy, JudgeCall, JudgeConnection, JudgeError,
    JudgeOutput, JudgeReply, RawAnswer, Secret, StructuredOutput, call, parse_answer,
};
use serde_json::json;
use support::mock::{Mock, MockReply};

const KEY: &str = "mode-SECRET-PAYLOAD";
const PROMPT: &str = "Grade the answer.";
const HINT: &str = "The model may not support this structured_output mode; try \"tool\" or \"prompt\" on the connection.";

struct Provider {
    adapter: Adapter,
    model: &'static str,
    path: &'static str,
    base: fn(&Mock) -> reqwest::Url,
}

const PROVIDERS: [Provider; 3] = [
    Provider {
        adapter: Adapter::OpenAi,
        model: "gpt-4o-mini",
        path: "/v1/chat/completions",
        base: Mock::url,
    },
    Provider {
        adapter: Adapter::Anthropic,
        model: "claude-sonnet-4-5",
        path: "/v1/messages",
        base: Mock::origin,
    },
    Provider {
        adapter: Adapter::Gemini,
        model: "gemini-2.5-flash",
        path: "/v1beta/models/gemini-2.5-flash:generateContent",
        base: Mock::origin,
    },
];

fn client() -> EgressClient {
    EgressClient::new(EgressPolicy {
        allow_private_network: true,
    })
    .expect("client")
}

fn connection(provider: &Provider, base: reqwest::Url, mode: StructuredOutput) -> JudgeConnection {
    JudgeConnection {
        adapter: provider.adapter,
        base_url: Some(base),
        api_key: Secret::new(KEY.to_owned()),
        extra_headers: Vec::new(),
        structured_output: mode,
    }
}

fn judge<'a>(provider: &'a Provider, output: &'a JudgeOutput) -> JudgeCall<'a> {
    JudgeCall {
        model: provider.model,
        prompt: PROMPT,
        output,
        include_reason: true,
        temperature: None,
        max_output_tokens: 1024,
        timeout: Duration::from_secs(2),
    }
}

fn numeric() -> JudgeOutput {
    JudgeOutput::Numeric { min: 0.0, max: 1.0 }
}

fn text_reply(provider: &Provider, text: &str) -> MockReply {
    let body = match provider.adapter {
        Adapter::OpenAi => json!({
            "choices": [{"finish_reason": "stop", "message": {"role": "assistant", "content": text}}]
        }),
        Adapter::Anthropic => json!({
            "stop_reason": "end_turn",
            "content": [{"type": "text", "text": text}]
        }),
        Adapter::Gemini => json!({
            "candidates": [{
                "finishReason": "STOP",
                "content": {"parts": [{"text": text}]}
            }]
        }),
    };
    MockReply {
        status: 200,
        body,
        delay: Duration::ZERO,
    }
}

fn tool_reply(provider: &Provider) -> MockReply {
    let body = match provider.adapter {
        Adapter::OpenAi => json!({
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "content": "SECRET-PAYLOAD",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "submit_judgement",
                            "arguments": "{\"value\":0.5}"
                        }
                    }]
                }
            }]
        }),
        Adapter::Anthropic => json!({
            "stop_reason": "tool_use",
            "content": [
                {"type": "text", "text": "SECRET-PAYLOAD"},
                {"type": "tool_use", "name": "submit_judgement", "input": {"value": 0.5}}
            ]
        }),
        Adapter::Gemini => json!({
            "candidates": [{
                "finishReason": "STOP",
                "content": {"parts": [
                    {"text": "SECRET-PAYLOAD"},
                    {"functionCall": {"name": "submit_judgement", "args": {"value": 0.5}}}
                ]}
            }]
        }),
    };
    MockReply {
        status: 200,
        body,
        delay: Duration::ZERO,
    }
}

fn parsed(reply: JudgeReply, output: &JudgeOutput) -> Answer {
    parse_answer(reply.answer, output, true).expect("parsed answer")
}

#[tokio::test]
async fn request_bodies_for_tool_and_prompt() {
    let mock = Mock::start().await;
    let http = client();
    let output = numeric();
    for provider in &PROVIDERS {
        for mode in [StructuredOutput::Tool, StructuredOutput::Prompt] {
            mock.enqueue(text_reply(provider, r#"{"value":0.5}"#));
            call(
                &http,
                &connection(provider, (provider.base)(&mock), mode),
                &judge(provider, &output),
            )
            .await
            .expect("request");
        }
    }
    let seen = mock.requests();
    assert_eq!(seen.len(), 6);
    assert!(seen.iter().all(|request| request.query.is_none()));
    assert_eq!(seen[0].path, PROVIDERS[0].path);
    assert_eq!(seen[2].path, PROVIDERS[1].path);
    assert_eq!(seen[4].path, PROVIDERS[2].path);
    insta::with_settings!({
        snapshot_path => "snapshots",
        prepend_module_to_snapshot => false,
    }, {
        insta::assert_json_snapshot!("openai_chat_tool", seen[0].body);
        insta::assert_json_snapshot!("openai_chat_prompt", seen[1].body);
        insta::assert_json_snapshot!("anthropic_messages_tool", seen[2].body);
        insta::assert_json_snapshot!("anthropic_messages_prompt", seen[3].body);
        insta::assert_json_snapshot!("gemini_generate_tool", seen[4].body);
        insta::assert_json_snapshot!("gemini_generate_prompt", seen[5].body);
    });
    for request in &seen {
        assert!(!format!("{:?}", request.body).contains(KEY));
    }
}

#[tokio::test]
async fn tool_replies_parse_and_text_only_still_parses() {
    let mock = Mock::start().await;
    let http = client();
    let output = numeric();
    for provider in &PROVIDERS {
        mock.enqueue(tool_reply(provider));
        let reply = call(
            &http,
            &connection(provider, (provider.base)(&mock), StructuredOutput::Tool),
            &judge(provider, &output),
        )
        .await
        .expect("tool");
        assert_eq!(reply.output_text, r#"{"value":0.5}"#);
        assert!(!format!("{reply:?}").contains("SECRET-PAYLOAD"));
        let RawAnswer::Object(_) = &reply.answer else {
            panic!("object answer");
        };
        let answer = parsed(reply, &output);
        assert_eq!(answer.value, ScoreValue::Numeric(0.5));
        assert_eq!(answer.explanation, None);

        mock.enqueue(text_reply(provider, r#"{"value":0.5}"#));
        let reply = call(
            &http,
            &connection(provider, (provider.base)(&mock), StructuredOutput::Tool),
            &judge(provider, &output),
        )
        .await
        .expect("text");
        assert!(matches!(reply.answer, RawAnswer::Text(_)));
        let answer = parsed(reply, &output);
        assert_eq!(answer.value, ScoreValue::Numeric(0.5));
    }
}

#[tokio::test]
async fn prompt_mode_parses_fenced_json() {
    let mock = Mock::start().await;
    let http = client();
    let output = numeric();
    let fenced = "```json\n{\"value\":0.5}\n```";
    for provider in &PROVIDERS {
        mock.enqueue(text_reply(provider, fenced));
        let reply = call(
            &http,
            &connection(provider, (provider.base)(&mock), StructuredOutput::Prompt),
            &judge(provider, &output),
        )
        .await
        .expect("fence");
        assert_eq!(reply.output_text, fenced);
        let answer = parsed(reply, &output);
        assert_eq!(answer.value, ScoreValue::Numeric(0.5));
    }
}

#[tokio::test]
async fn unparsable_tool_arguments_are_not_json() {
    let mock = Mock::start().await;
    let http = client();
    let output = numeric();
    let provider = &PROVIDERS[0];
    mock.enqueue(MockReply {
        status: 200,
        body: json!({
            "choices": [{
                "finish_reason": "tool_calls",
                "message": {
                    "content": "SECRET-PAYLOAD",
                    "tool_calls": [{
                        "function": {"name": "submit_judgement", "arguments": "SECRET-PAYLOAD"}
                    }]
                }
            }]
        }),
        delay: Duration::ZERO,
    });
    let err = call(
        &http,
        &connection(provider, mock.url(), StructuredOutput::Tool),
        &judge(provider, &output),
    )
    .await
    .expect_err("arguments");
    assert_eq!(err, JudgeError::NotJson);
    let rendered = format!("{err} {err:?}");
    assert!(!rendered.contains("SECRET-PAYLOAD"), "{rendered}");
    assert!(!rendered.contains(PROMPT), "{rendered}");
}

#[tokio::test]
async fn hint_on_a_400_in_native_and_tool_mode_only() {
    let mock = Mock::start().await;
    let http = client();
    let output = numeric();
    for provider in &PROVIDERS {
        for mode in [
            StructuredOutput::Native,
            StructuredOutput::Tool,
            StructuredOutput::Prompt,
        ] {
            let body = match provider.adapter {
                Adapter::OpenAi => {
                    json!({"error": {"message": "overloaded", "type": "invalid_request_error", "code": "bad_request"}})
                }
                Adapter::Anthropic => {
                    json!({"type": "error", "error": {"type": "invalid_request_error", "message": "overloaded"}})
                }
                Adapter::Gemini => {
                    json!({"error": {"code": 400, "status": "INVALID_ARGUMENT", "message": "overloaded"}})
                }
            };
            mock.enqueue(MockReply {
                status: 400,
                body,
                delay: Duration::ZERO,
            });
            let err = call(
                &http,
                &connection(provider, (provider.base)(&mock), mode),
                &judge(provider, &output),
            )
            .await
            .expect_err("status");
            let JudgeError::Provider { hint, .. } = &err else {
                panic!("provider error");
            };
            let expected = match mode {
                StructuredOutput::Prompt => None,
                StructuredOutput::Native | StructuredOutput::Tool => Some(HINT),
            };
            assert_eq!(hint, &expected, "{:?} {mode:?}", provider.adapter);
            assert_eq!(err.to_string(), "provider status 400");
            let rendered = format!("{err} {err:?}");
            assert!(!rendered.contains(KEY), "{rendered}");
            assert!(!rendered.contains(PROMPT), "{rendered}");
        }
    }
}
