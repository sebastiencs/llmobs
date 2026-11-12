//! Anthropic adapter against the in-process mock. Nothing here calls a real provider.
//!
//! This file is an integration test, so clippy's test allow does not apply.
#![allow(clippy::expect_used)]

mod support {
    pub(crate) mod mock;
}

use std::collections::BTreeMap;
use std::time::Duration;

use llmobs_eval::{
    Adapter, EgressClient, EgressError, EgressPolicy, ErrorClass, JudgeCall, JudgeConnection,
    JudgeError, JudgeOutput, RawAnswer, Secret, StructuredOutput, call,
};
use serde_json::{Value, json};
use support::mock::{Mock, MockReply};

const KEY: &str = "sk-ant-SECRET-PAYLOAD";
const PROMPT: &str = "Grade the answer.";

fn client() -> EgressClient {
    EgressClient::new(EgressPolicy {
        allow_private_network: true,
    })
    .expect("client")
}

fn connection(base: reqwest::Url, extra: Vec<(&str, &str)>) -> JudgeConnection {
    JudgeConnection {
        adapter: Adapter::Anthropic,
        base_url: Some(base),
        api_key: Secret::new(KEY.to_owned()),
        extra_headers: extra
            .into_iter()
            .map(|(name, value)| (name.to_owned(), Secret::new(value.to_owned())))
            .collect(),
        structured_output: StructuredOutput::Native,
    }
}

fn judge<'a>(
    model: &'a str,
    output: &'a JudgeOutput,
    temperature: Option<f64>,
    timeout: Duration,
) -> JudgeCall<'a> {
    JudgeCall {
        model,
        prompt: PROMPT,
        output,
        include_reason: true,
        temperature,
        max_output_tokens: 1024,
        timeout,
    }
}

fn numeric() -> JudgeOutput {
    JudgeOutput::Numeric { min: 0.0, max: 1.0 }
}

fn ok(text: &str) -> MockReply {
    MockReply {
        status: 200,
        body: json!({
            "model": "claude-test",
            "stop_reason": "end_turn",
            "content": [{"type": "text", "text": text}]
        }),
        delay: Duration::ZERO,
    }
}

fn schema(body: &Value) -> &Value {
    &body["output_config"]["format"]["schema"]
}

#[tokio::test]
async fn request_body_with_and_without_temperature() {
    let mock = Mock::start().await;
    let conn = connection(mock.origin(), Vec::new());
    let output = numeric();
    let http = client();
    mock.enqueue(ok(r#"{"value":1}"#));
    mock.enqueue(ok(r#"{"value":1}"#));
    call(
        &http,
        &conn,
        &judge("claude-sonnet-4-5", &output, None, Duration::from_secs(2)),
    )
    .await
    .expect("no temperature");
    call(
        &http,
        &conn,
        &judge(
            "claude-sonnet-4-5",
            &output,
            Some(0.2),
            Duration::from_secs(2),
        ),
    )
    .await
    .expect("temperature");
    let seen = mock.requests();
    assert_eq!(seen.len(), 2);
    assert!(seen.iter().all(|request| request.path == "/v1/messages"));
    insta::with_settings!({
        snapshot_path => "snapshots",
        prepend_module_to_snapshot => false,
    }, {
        insta::assert_json_snapshot!("anthropic_messages_no_temperature", seen[0].body);
        insta::assert_json_snapshot!("anthropic_messages_temperature", seen[1].body);
    });
}

#[tokio::test]
async fn returns_text_for_each_output_type() {
    let mock = Mock::start().await;
    let conn = connection(mock.origin(), Vec::new());
    let http = client();
    let numeric = numeric();
    let boolean = JudgeOutput::Boolean;
    let categorical = JudgeOutput::Categorical {
        labels: vec!["correct".to_owned(), "incorrect".to_owned()],
        label_values: BTreeMap::new(),
    };
    let cases = [
        (&numeric, r#"{"value":0.8}"#, "number"),
        (&boolean, r#"{"value":true}"#, "boolean"),
        (&categorical, r#"{"label":"correct"}"#, "string"),
    ];
    for (output, content, kind) in cases {
        mock.enqueue(ok(content));
        let reply = call(
            &http,
            &conn,
            &judge("claude-sonnet-4-5", output, None, Duration::from_secs(2)),
        )
        .await
        .expect(kind);
        assert_eq!(reply.output_text, content);
        assert_eq!(reply.answer, RawAnswer::Text(content.to_owned()));
        assert_eq!(reply.response_model.as_deref(), Some("claude-test"));
    }
    let seen = mock.requests();
    assert_eq!(
        schema(&seen[0].body)["properties"]["value"]["type"],
        "number"
    );
    assert_eq!(
        schema(&seen[1].body)["properties"]["value"]["type"],
        "boolean"
    );
    assert_eq!(
        schema(&seen[2].body)["properties"]["label"]["enum"],
        json!(["correct", "incorrect"])
    );
    assert_eq!(
        schema(&seen[0].body)["properties"]["explanation"]["type"],
        "string"
    );
}

#[tokio::test]
async fn skips_thinking_blocks_and_concatenates_text() {
    let mock = Mock::start().await;
    let conn = connection(mock.origin(), Vec::new());
    let http = client();
    let output = numeric();
    mock.enqueue(MockReply {
        status: 200,
        body: json!({
            "model": "claude-test",
            "stop_reason": "end_turn",
            "content": [
                {"type": "thinking", "thinking": "SECRET-PAYLOAD"},
                {"type": "text", "text": "{\"value\":"},
                {"type": "redacted_thinking", "data": "SECRET-PAYLOAD"},
                {"type": "text", "text": "0.8}"}
            ]
        }),
        delay: Duration::ZERO,
    });
    let reply = call(
        &http,
        &conn,
        &judge("claude-sonnet-4-5", &output, None, Duration::from_secs(2)),
    )
    .await
    .expect("text");
    assert_eq!(reply.output_text, r#"{"value":0.8}"#);
    assert!(!format!("{reply:?}").contains("SECRET-PAYLOAD"));

    mock.enqueue(MockReply {
        status: 200,
        body: json!({
            "stop_reason": "end_turn",
            "content": [{"type": "thinking", "thinking": "SECRET-PAYLOAD"}]
        }),
        delay: Duration::ZERO,
    });
    let err = call_err(
        &http,
        &conn,
        &judge("claude-sonnet-4-5", &output, None, Duration::from_secs(2)),
    )
    .await;
    assert_eq!(err, JudgeError::EmptyAnswer);
    assert_hidden(&err);
}

#[tokio::test]
async fn tool_use_input_is_the_answer() {
    let mock = Mock::start().await;
    let conn = connection(mock.origin(), Vec::new());
    let http = client();
    let output = numeric();
    mock.enqueue(MockReply {
        status: 200,
        body: json!({
            "model": "claude-test",
            "stop_reason": "tool_use",
            "content": [
                {"type": "text", "text": "SECRET-PAYLOAD"},
                {"type": "tool_use", "id": "toolu_1", "name": "submit_judgement", "input": {"value": 0.5}}
            ]
        }),
        delay: Duration::ZERO,
    });
    let reply = call(
        &http,
        &conn,
        &judge("claude-sonnet-4-5", &output, None, Duration::from_secs(2)),
    )
    .await
    .expect("tool");
    let RawAnswer::Object(object) = &reply.answer else {
        panic!("object answer");
    };
    assert_eq!(object.get("value").and_then(Value::as_f64), Some(0.5));
    assert_eq!(reply.output_text, r#"{"value":0.5}"#);
    assert!(!format!("{reply:?}").contains("SECRET-PAYLOAD"));

    mock.enqueue(MockReply {
        status: 200,
        body: json!({
            "stop_reason": "tool_use",
            "content": [{"type": "tool_use", "name": "submit_judgement", "input": ["SECRET-PAYLOAD"]}]
        }),
        delay: Duration::ZERO,
    });
    let err = call_err(
        &http,
        &conn,
        &judge("claude-sonnet-4-5", &output, None, Duration::from_secs(2)),
    )
    .await;
    assert_eq!(err, JudgeError::NotJson);
    assert_hidden(&err);
}

#[tokio::test]
async fn refusal_truncation_and_a_missing_message() {
    let mock = Mock::start().await;
    let conn = connection(mock.origin(), Vec::new());
    let http = client();
    let output = numeric();
    let request = judge("claude-sonnet-4-5", &output, None, Duration::from_secs(2));

    mock.enqueue(MockReply {
        status: 200,
        body: json!({
            "stop_reason": "refusal",
            "stop_details": {"type": "refusal", "explanation": "SECRET-PAYLOAD"},
            "content": [{"type": "text", "text": "SECRET-PAYLOAD"}]
        }),
        delay: Duration::ZERO,
    });
    let err = call_err(&http, &conn, &request).await;
    assert_eq!(err, JudgeError::Refused);
    assert_eq!(err.class(), ErrorClass::Permanent);
    assert_hidden(&err);

    mock.enqueue(MockReply {
        status: 200,
        body: json!({
            "stop_reason": "end_turn",
            "stop_details": {"type": "refusal", "explanation": "SECRET-PAYLOAD"},
            "content": [{"type": "text", "text": "SECRET-PAYLOAD"}]
        }),
        delay: Duration::ZERO,
    });
    let err = call_err(&http, &conn, &request).await;
    assert_eq!(err, JudgeError::Refused);
    assert_hidden(&err);

    mock.enqueue(MockReply {
        status: 200,
        body: json!({
            "stop_reason": "max_tokens",
            "content": [{"type": "text", "text": "SECRET-PAYLOAD"}]
        }),
        delay: Duration::ZERO,
    });
    let err = call_err(&http, &conn, &request).await;
    assert_eq!(err, JudgeError::Truncated);
    assert_hidden(&err);

    mock.enqueue(MockReply {
        status: 200,
        body: json!({"stop_reason": "end_turn"}),
        delay: Duration::ZERO,
    });
    assert_eq!(
        call_err(&http, &conn, &request).await,
        JudgeError::EmptyAnswer
    );
}

#[tokio::test]
async fn maps_usage_including_cache_and_thinking() {
    let mock = Mock::start().await;
    let conn = connection(mock.origin(), Vec::new());
    let http = client();
    let output = numeric();
    mock.enqueue(MockReply {
        status: 200,
        body: json!({
            "model": "claude-test",
            "stop_reason": "end_turn",
            "content": [{"type": "text", "text": "{\"value\":0.5}"}],
            "usage": {
                "input_tokens": 10,
                "cache_read_input_tokens": 3,
                "cache_creation_input_tokens": 1,
                "output_tokens": 4,
                "output_tokens_details": {"thinking_tokens": 2}
            }
        }),
        delay: Duration::ZERO,
    });
    let reply = call(
        &http,
        &conn,
        &judge("claude-sonnet-4-5", &output, None, Duration::from_secs(2)),
    )
    .await
    .expect("usage");
    assert_eq!(reply.usage.input, 14);
    assert_eq!(reply.usage.output, 4);
    assert_eq!(reply.usage.cache_read, 3);
    assert_eq!(reply.usage.cache_write, 1);
    assert_eq!(reply.usage.reasoning, 2);

    mock.enqueue(MockReply {
        status: 200,
        body: json!({
            "stop_reason": "end_turn",
            "content": [{"type": "text", "text": "{\"value\":0.5}"}],
            "usage": {"input_tokens": 1, "output_tokens": 1}
        }),
        delay: Duration::ZERO,
    });
    let reply = call(
        &http,
        &conn,
        &judge("claude-sonnet-4-5", &output, None, Duration::from_secs(2)),
    )
    .await
    .expect("bare usage");
    assert_eq!(reply.usage.input, 1);
    assert_eq!(reply.usage.cache_read, 0);
    assert_eq!(reply.usage.cache_write, 0);
    assert_eq!(reply.usage.reasoning, 0);
    assert_eq!(reply.response_model, None);
}

#[tokio::test]
async fn classifies_provider_statuses() {
    let cases = [
        (429, "rate_limit_error", ErrorClass::Retryable),
        (429, "insufficient_quota", ErrorClass::Pause),
        (401, "authentication_error", ErrorClass::Pause),
        (403, "permission_error", ErrorClass::Pause),
        (404, "not_found_error", ErrorClass::Pause),
        (400, "invalid_request_error", ErrorClass::Permanent),
        (500, "api_error", ErrorClass::Retryable),
        (503, "api_error", ErrorClass::Retryable),
        (529, "overloaded_error", ErrorClass::Retryable),
    ];
    let mock = Mock::start().await;
    let conn = connection(mock.origin(), Vec::new());
    let http = client();
    let output = numeric();
    let request = judge("claude-sonnet-4-5", &output, None, Duration::from_secs(2));
    for (status, kind, class) in cases {
        mock.enqueue(MockReply {
            status,
            body: json!({"type": "error", "error": {"type": kind, "message": "SECRET-PAYLOAD"}}),
            delay: Duration::ZERO,
        });
        let err = call_err(&http, &conn, &request).await;
        assert_eq!(err.class(), class, "{status}");
        let JudgeError::Provider {
            status: got,
            provider_type,
            provider_code,
            provider_message,
            ..
        } = &err
        else {
            panic!("provider error for {status}");
        };
        assert_eq!(*got, status);
        assert_eq!(provider_type.as_deref(), Some(kind));
        assert_eq!(provider_code, &None);
        assert_eq!(provider_message.as_deref(), Some("SECRET-PAYLOAD"));
        assert_hidden(&err);
    }
}

#[tokio::test]
async fn timeout_is_retryable() {
    let mock = Mock::start().await;
    let conn = connection(mock.origin(), Vec::new());
    let http = client();
    let output = numeric();
    mock.enqueue(MockReply {
        status: 200,
        body: json!({"content": []}),
        delay: Duration::from_secs(5),
    });
    let err = call(
        &http,
        &conn,
        &judge(
            "claude-sonnet-4-5",
            &output,
            None,
            Duration::from_millis(200),
        ),
    )
    .await
    .expect_err("timeout");
    assert_eq!(err.class(), ErrorClass::Retryable);
    assert_eq!(err, JudgeError::Transport(EgressError::Timeout));
    assert_hidden(&err);
    assert!(!err.to_string().contains("/v1"));
}

#[tokio::test]
async fn sends_the_key_and_extra_headers_without_leaking_them() {
    let mock = Mock::start().await;
    let conn = connection(mock.origin(), vec![("X-Trace", "trace-SECRET-PAYLOAD")]);
    let http = client();
    let output = numeric();
    mock.enqueue(MockReply {
        status: 500,
        body: json!({"type": "error", "error": {"type": "api_error", "message": "overloaded"}}),
        delay: Duration::ZERO,
    });
    let err = call(
        &http,
        &conn,
        &judge("claude-sonnet-4-5", &output, None, Duration::from_secs(2)),
    )
    .await
    .expect_err("status");
    let request = &mock.requests()[0];
    assert_eq!(request.header("x-api-key"), Some(KEY));
    assert_eq!(request.header("authorization"), None);
    assert_eq!(request.header("anthropic-version"), Some("2026-10-21"));
    assert_eq!(request.header("anthropic-beta"), None);
    assert_eq!(request.header("x-trace"), Some("trace-SECRET-PAYLOAD"));
    assert_eq!(request.header("content-type"), Some("application/json"));
    let rendered = format!("{conn:?} {err:?} {err}");
    assert!(!rendered.contains(KEY), "{rendered}");
    assert!(!rendered.contains("trace-SECRET-PAYLOAD"), "{rendered}");
    assert!(!rendered.contains(PROMPT), "{rendered}");
}

#[tokio::test]
async fn rejects_a_bad_model_header_or_temperature_before_sending() {
    let mock = Mock::start().await;
    let http = client();
    let output = numeric();
    let conn = connection(mock.origin(), Vec::new());
    mock.enqueue(ok("{}"));
    let err = call(
        &http,
        &conn,
        &judge("claude/sonnet", &output, None, Duration::from_secs(2)),
    )
    .await
    .expect_err("model");
    assert_eq!(err, JudgeError::InvalidModel);
    assert_eq!(err.class(), ErrorClass::Permanent);
    assert!(mock.requests().is_empty());

    let bad = connection(mock.origin(), vec![("bad name", "SECRET-PAYLOAD")]);
    let err = call(
        &http,
        &bad,
        &judge("claude-sonnet-4-5", &output, None, Duration::from_secs(2)),
    )
    .await
    .expect_err("header");
    assert_eq!(err, JudgeError::InvalidHeader);
    assert!(!err.to_string().contains("SECRET-PAYLOAD"));
    assert!(mock.requests().is_empty());

    let err = call(
        &http,
        &conn,
        &judge(
            "claude-sonnet-4-5",
            &output,
            Some(f64::NAN),
            Duration::from_secs(2),
        ),
    )
    .await
    .expect_err("temperature");
    assert_eq!(err, JudgeError::InvalidRequest);
    assert!(mock.requests().is_empty());
}

#[tokio::test]
async fn response_over_4_mib_is_permanent() {
    let mock = Mock::start().await;
    let conn = connection(mock.origin(), Vec::new());
    let http = client();
    let output = numeric();
    mock.enqueue(MockReply {
        status: 200,
        body: Value::String("x".repeat(4 * 1024 * 1024 + 1)),
        delay: Duration::ZERO,
    });
    let err = call(
        &http,
        &conn,
        &judge("claude-sonnet-4-5", &output, None, Duration::from_secs(5)),
    )
    .await
    .expect_err("too large");
    assert_eq!(
        err,
        JudgeError::Transport(EgressError::TooLarge {
            limit: 4 * 1024 * 1024
        })
    );
    assert_eq!(err.class(), ErrorClass::Permanent);
    assert_eq!(err.to_string(), "response exceeds 4194304 bytes");
}

async fn call_err(http: &EgressClient, conn: &JudgeConnection, call: &JudgeCall<'_>) -> JudgeError {
    llmobs_eval::call(http, conn, call)
        .await
        .expect_err("provider error")
}

fn assert_hidden(err: &JudgeError) {
    let rendered = err.to_string();
    assert!(!rendered.contains("SECRET-PAYLOAD"), "{rendered}");
    assert!(!rendered.contains(PROMPT), "{rendered}");
    let debug = format!("{err:?}");
    assert!(!debug.contains(PROMPT), "{debug}");
    if !matches!(err, JudgeError::Provider { .. }) {
        assert!(!debug.contains("SECRET-PAYLOAD"), "{debug}");
    }
}
