//! OpenAI adapter against the in-process mock. Nothing here calls a real provider.
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

const KEY: &str = "sk-test-SECRET-PAYLOAD";
const PROMPT: &str = "Grade the answer.";

fn client() -> EgressClient {
    EgressClient::new(EgressPolicy {
        allow_private_network: true,
    })
    .expect("client")
}

fn connection(base: reqwest::Url, extra: Vec<(&str, &str)>) -> JudgeConnection {
    JudgeConnection {
        adapter: Adapter::OpenAi,
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

fn ok(content: &str) -> MockReply {
    MockReply {
        status: 200,
        body: json!({
            "model": "gpt-test",
            "choices": [{
                "finish_reason": "stop",
                "message": {"role": "assistant", "content": content}
            }]
        }),
        delay: Duration::ZERO,
    }
}

fn schema(body: &Value) -> &Value {
    &body["response_format"]["json_schema"]["schema"]
}

#[tokio::test]
async fn request_body_with_and_without_temperature() {
    let mock = Mock::start().await;
    let conn = connection(mock.url(), Vec::new());
    let output = numeric();
    let http = client();
    mock.enqueue(ok(r#"{"value":1}"#));
    mock.enqueue(ok(r#"{"value":1}"#));
    call(
        &http,
        &conn,
        &judge("gpt-4o-mini", &output, None, Duration::from_secs(2)),
    )
    .await
    .expect("no temperature");
    call(
        &http,
        &conn,
        &judge("gpt-4o-mini", &output, Some(0.2), Duration::from_secs(2)),
    )
    .await
    .expect("temperature");
    let seen = mock.requests();
    assert_eq!(seen.len(), 2);
    assert!(
        seen.iter()
            .all(|request| request.path == "/v1/chat/completions")
    );
    insta::with_settings!({
        snapshot_path => "snapshots",
        prepend_module_to_snapshot => false,
    }, {
        insta::assert_json_snapshot!("openai_chat_no_temperature", seen[0].body);
        insta::assert_json_snapshot!("openai_chat_temperature", seen[1].body);
    });
}

#[tokio::test]
async fn returns_text_for_each_output_type() {
    let mock = Mock::start().await;
    let conn = connection(mock.url(), Vec::new());
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
            &judge("gpt-4o-mini", output, None, Duration::from_secs(2)),
        )
        .await
        .expect(kind);
        assert_eq!(reply.output_text, content);
        assert_eq!(reply.answer, RawAnswer::Text(content.to_owned()));
        assert_eq!(reply.response_model.as_deref(), Some("gpt-test"));
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
async fn refusal_length_filter_and_a_missing_choice() {
    let mock = Mock::start().await;
    let conn = connection(mock.url(), Vec::new());
    let http = client();
    let output = numeric();
    let request = judge("gpt-4o-mini", &output, None, Duration::from_secs(2));

    mock.enqueue(MockReply {
        status: 200,
        body: json!({"choices":[{"finish_reason":"stop","message":{"refusal":"SECRET-PAYLOAD","content":null}}]}),
        delay: Duration::ZERO,
    });
    let err = call_err(&http, &conn, &request).await;
    assert_eq!(err, JudgeError::Refused);
    assert_eq!(err.class(), ErrorClass::Permanent);
    assert_hidden(&err);

    mock.enqueue(MockReply {
        status: 200,
        body: json!({"choices":[{"finish_reason":"length","message":{"content":"SECRET-PAYLOAD"}}]}),
        delay: Duration::ZERO,
    });
    let err = call_err(&http, &conn, &request).await;
    assert_eq!(err, JudgeError::Truncated);
    assert_hidden(&err);

    mock.enqueue(MockReply {
        status: 200,
        body: json!({"choices":[{"finish_reason":"content_filter","message":{"content":"SECRET-PAYLOAD"}}]}),
        delay: Duration::ZERO,
    });
    let err = call_err(&http, &conn, &request).await;
    assert_eq!(err, JudgeError::ContentFiltered);
    assert_hidden(&err);

    mock.enqueue(MockReply {
        status: 200,
        body: json!({"choices":[{"finish_reason":"stop","message":{"refusal":"","content":"kept"}}]}),
        delay: Duration::ZERO,
    });
    let reply = call(&http, &conn, &request).await.expect("empty refusal");
    assert_eq!(reply.output_text, "kept");

    mock.enqueue(MockReply {
        status: 200,
        body: json!({"choices":[]}),
        delay: Duration::ZERO,
    });
    assert_eq!(
        call_err(&http, &conn, &request).await,
        JudgeError::EmptyAnswer
    );
}

#[tokio::test]
async fn maps_usage_including_details() {
    let mock = Mock::start().await;
    let conn = connection(mock.url(), Vec::new());
    let http = client();
    let output = numeric();
    mock.enqueue(MockReply {
        status: 200,
        body: json!({
            "model": "gpt-test",
            "choices": [{"finish_reason": "stop", "message": {"content": "{\"value\":0.5}"}}],
            "usage": {
                "prompt_tokens": 10,
                "completion_tokens": 4,
                "prompt_tokens_details": {"cached_tokens": 3, "cache_write_tokens": 1},
                "completion_tokens_details": {"reasoning_tokens": 2}
            }
        }),
        delay: Duration::ZERO,
    });
    let reply = call(
        &http,
        &conn,
        &judge("gpt-4o-mini", &output, None, Duration::from_secs(2)),
    )
    .await
    .expect("usage");
    assert_eq!(reply.usage.input, 10);
    assert_eq!(reply.usage.output, 4);
    assert_eq!(reply.usage.cache_read, 3);
    assert_eq!(reply.usage.cache_write, 1);
    assert_eq!(reply.usage.reasoning, 2);

    mock.enqueue(MockReply {
        status: 200,
        body: json!({
            "choices": [{"finish_reason": "stop", "message": {"content": "{\"value\":0.5}"}}],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1}
        }),
        delay: Duration::ZERO,
    });
    let reply = call(
        &http,
        &conn,
        &judge("gpt-4o-mini", &output, None, Duration::from_secs(2)),
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
        (429, "tokens", "rate_limit_exceeded", ErrorClass::Retryable),
        (
            429,
            "insufficient_quota",
            "insufficient_quota",
            ErrorClass::Pause,
        ),
        (
            401,
            "authentication_error",
            "invalid_api_key",
            ErrorClass::Pause,
        ),
        (403, "permission_error", "forbidden", ErrorClass::Pause),
        (
            404,
            "invalid_request_error",
            "model_not_found",
            ErrorClass::Pause,
        ),
        (
            400,
            "invalid_request_error",
            "bad_request",
            ErrorClass::Permanent,
        ),
        (500, "server_error", "internal_error", ErrorClass::Retryable),
        (503, "server_error", "unavailable", ErrorClass::Retryable),
    ];
    let mock = Mock::start().await;
    let conn = connection(mock.url(), Vec::new());
    let http = client();
    let output = numeric();
    let request = judge("gpt-4o-mini", &output, None, Duration::from_secs(2));
    for (status, kind, code, class) in cases {
        mock.enqueue(MockReply {
            status,
            body: json!({"error": {"message": "SECRET-PAYLOAD", "type": kind, "code": code}}),
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
        assert_eq!(provider_code.as_deref(), Some(code));
        assert_eq!(provider_message.as_deref(), Some("SECRET-PAYLOAD"));
        assert_hidden(&err);
    }
}

#[tokio::test]
async fn timeout_is_retryable() {
    let mock = Mock::start().await;
    let conn = connection(mock.url(), Vec::new());
    let http = client();
    let output = numeric();
    mock.enqueue(MockReply {
        status: 200,
        body: json!({"choices":[]}),
        delay: Duration::from_secs(5),
    });
    let err = call(
        &http,
        &conn,
        &judge("gpt-4o-mini", &output, None, Duration::from_millis(200)),
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
    let conn = connection(mock.url(), vec![("X-Trace", "trace-SECRET-PAYLOAD")]);
    let http = client();
    let output = numeric();
    mock.enqueue(MockReply {
        status: 500,
        body: json!({"error": {"message": "overloaded", "type": "server_error", "code": "internal_error"}}),
        delay: Duration::ZERO,
    });
    let err = call(
        &http,
        &conn,
        &judge("gpt-4o-mini", &output, None, Duration::from_secs(2)),
    )
    .await
    .expect_err("status");
    let request = &mock.requests()[0];
    let authorization = format!("Bearer {KEY}");
    assert_eq!(
        request.header("authorization"),
        Some(authorization.as_str())
    );
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
    let conn = connection(mock.url(), Vec::new());
    mock.enqueue(ok("{}"));
    let err = call(
        &http,
        &conn,
        &judge("gpt/4", &output, None, Duration::from_secs(2)),
    )
    .await
    .expect_err("model");
    assert_eq!(err, JudgeError::InvalidModel);
    assert_eq!(err.class(), ErrorClass::Permanent);
    assert!(mock.requests().is_empty());

    let bad = connection(mock.url(), vec![("bad name", "SECRET-PAYLOAD")]);
    let err = call(
        &http,
        &bad,
        &judge("gpt-4o-mini", &output, None, Duration::from_secs(2)),
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
            "gpt-4o-mini",
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
    let conn = connection(mock.url(), Vec::new());
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
        &judge("gpt-4o-mini", &output, None, Duration::from_secs(5)),
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
