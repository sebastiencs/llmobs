//! Gemini adapter against the in-process mock. Nothing here calls a real provider.
#![allow(clippy::expect_used)]

use std::collections::BTreeMap;
use std::time::Duration;

use crate::config::JudgeOutput;
use crate::egress::{EgressClient, EgressPolicy};
use crate::error::{EgressError, ErrorClass, JudgeError};
use crate::judge::{Adapter, JudgeCall, JudgeConnection, RawAnswer, StructuredOutput, call};
use crate::mock::{Mock, MockReply};
use crate::secret::Secret;
use serde_json::{Value, json};

const KEY: &str = "AIza-SECRET-PAYLOAD";
const PROMPT: &str = "Grade the answer.";
const MODEL: &str = "gemini-2.5-flash";
const PATH: &str = "/v1beta/models/gemini-2.5-flash:generateContent";

fn client() -> EgressClient {
    EgressClient::new(EgressPolicy {
        allow_private_network: true,
    })
    .expect("client")
}

fn connection(base: reqwest::Url, extra: Vec<(&str, &str)>) -> JudgeConnection {
    JudgeConnection {
        adapter: Adapter::Gemini,
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
    output: &'a JudgeOutput,
    temperature: Option<f64>,
    timeout: Duration,
) -> JudgeCall<'a> {
    JudgeCall {
        model: MODEL,
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
            "modelVersion": "gemini-2.5-flash",
            "candidates": [{
                "finishReason": "STOP",
                "content": {"role": "model", "parts": [{"text": text}]}
            }]
        }),
        delay: Duration::ZERO,
    }
}

fn schema(body: &Value) -> &Value {
    &body["generationConfig"]["responseJsonSchema"]
}

#[tokio::test]
async fn request_body_with_and_without_temperature() {
    let mock = Mock::start().await;
    let conn = connection(mock.origin(), Vec::new());
    let output = numeric();
    let http = client();
    mock.enqueue(ok(r#"{"value":1}"#));
    mock.enqueue(ok(r#"{"value":1}"#));
    call(&http, &conn, &judge(&output, None, Duration::from_secs(2)))
        .await
        .expect("no temperature");
    call(
        &http,
        &conn,
        &judge(&output, Some(0.2), Duration::from_secs(2)),
    )
    .await
    .expect("temperature");
    let seen = mock.requests();
    assert_eq!(seen.len(), 2);
    assert!(seen.iter().all(|request| request.path == PATH));
    assert!(seen.iter().all(|request| request.query.is_none()));
    insta::with_settings!({
        snapshot_path => "snapshots",
        prepend_module_to_snapshot => false,
    }, {
        insta::assert_json_snapshot!("gemini_generate_no_temperature", seen[0].body);
        insta::assert_json_snapshot!("gemini_generate_temperature", seen[1].body);
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
        let reply = call(&http, &conn, &judge(output, None, Duration::from_secs(2)))
            .await
            .expect(kind);
        assert_eq!(reply.output_text, content);
        assert_eq!(reply.answer, RawAnswer::Text(content.to_owned()));
        assert_eq!(reply.response_model.as_deref(), Some(MODEL));
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
    assert_eq!(
        seen[0].body["generationConfig"]["responseMimeType"],
        "application/json"
    );
}

#[tokio::test]
async fn skips_thought_parts_and_concatenates_text() {
    let mock = Mock::start().await;
    let conn = connection(mock.origin(), Vec::new());
    let http = client();
    let output = numeric();
    mock.enqueue(MockReply {
        status: 200,
        body: json!({
            "modelVersion": MODEL,
            "candidates": [{
                "finishReason": "STOP",
                "content": {"parts": [
                    {"text": "SECRET-PAYLOAD", "thought": true},
                    {"text": "{\"value\":"},
                    {"text": "0.8}"}
                ]}
            }]
        }),
        delay: Duration::ZERO,
    });
    let reply = call(&http, &conn, &judge(&output, None, Duration::from_secs(2)))
        .await
        .expect("text");
    assert_eq!(reply.output_text, r#"{"value":0.8}"#);
    assert!(!format!("{reply:?}").contains("SECRET-PAYLOAD"));

    mock.enqueue(MockReply {
        status: 200,
        body: json!({
            "candidates": [{
                "finishReason": "STOP",
                "content": {"parts": [{"text": "SECRET-PAYLOAD", "thought": true}]}
            }]
        }),
        delay: Duration::ZERO,
    });
    let err = call_err(&http, &conn, &judge(&output, None, Duration::from_secs(2))).await;
    assert_eq!(err, JudgeError::EmptyAnswer);
    assert_hidden(&err);
}

#[tokio::test]
async fn function_call_args_are_the_answer() {
    let mock = Mock::start().await;
    let conn = connection(mock.origin(), Vec::new());
    let http = client();
    let output = numeric();
    mock.enqueue(MockReply {
        status: 200,
        body: json!({
            "modelVersion": MODEL,
            "candidates": [{
                "finishReason": "STOP",
                "content": {"parts": [
                    {"text": "SECRET-PAYLOAD"},
                    {"functionCall": {"name": "submit_judgement", "args": {"value": 0.5}}}
                ]}
            }]
        }),
        delay: Duration::ZERO,
    });
    let reply = call(&http, &conn, &judge(&output, None, Duration::from_secs(2)))
        .await
        .expect("function call");
    let RawAnswer::Object(object) = &reply.answer else {
        panic!("object answer");
    };
    assert_eq!(object.get("value").and_then(Value::as_f64), Some(0.5));
    assert_eq!(reply.output_text, r#"{"value":0.5}"#);
    assert!(!format!("{reply:?}").contains("SECRET-PAYLOAD"));

    mock.enqueue(MockReply {
        status: 200,
        body: json!({
            "candidates": [{
                "content": {"parts": [
                    {"functionCall": {"name": "submit_judgement", "args": ["SECRET-PAYLOAD"]}}
                ]}
            }]
        }),
        delay: Duration::ZERO,
    });
    let err = call_err(&http, &conn, &judge(&output, None, Duration::from_secs(2))).await;
    assert_eq!(err, JudgeError::NotJson);
    assert_hidden(&err);
}

#[tokio::test]
async fn prompt_block_truncation_and_a_missing_candidate() {
    let mock = Mock::start().await;
    let conn = connection(mock.origin(), Vec::new());
    let http = client();
    let output = numeric();
    let request = judge(&output, None, Duration::from_secs(2));

    mock.enqueue(MockReply {
        status: 200,
        body: json!({
            "promptFeedback": {"blockReason": "SAFETY"},
            "candidates": [{
                "finishReason": "STOP",
                "content": {"parts": [{"text": "SECRET-PAYLOAD"}]}
            }]
        }),
        delay: Duration::ZERO,
    });
    let err = call_err(&http, &conn, &request).await;
    assert_eq!(err, JudgeError::ContentFiltered);
    assert_eq!(err.class(), ErrorClass::Permanent);
    assert_hidden(&err);

    mock.enqueue(MockReply {
        status: 200,
        body: json!({
            "promptFeedback": {"blockReason": "OTHER"},
            "candidates": [{
                "content": {"parts": [{"text": "SECRET-PAYLOAD"}]}
            }]
        }),
        delay: Duration::ZERO,
    });
    let err = call_err(&http, &conn, &request).await;
    assert_eq!(err, JudgeError::ContentFiltered);
    assert_hidden(&err);

    for reason in [
        "SAFETY",
        "PROHIBITED_CONTENT",
        "BLOCKLIST",
        "SPII",
        "RECITATION",
    ] {
        mock.enqueue(MockReply {
            status: 200,
            body: json!({
                "candidates": [{
                    "finishReason": reason,
                    "content": {"parts": [{"text": "SECRET-PAYLOAD"}]}
                }]
            }),
            delay: Duration::ZERO,
        });
        let err = call_err(&http, &conn, &request).await;
        assert_eq!(err, JudgeError::ContentFiltered, "{reason}");
        assert_hidden(&err);
    }

    mock.enqueue(MockReply {
        status: 200,
        body: json!({
            "candidates": [{
                "finishReason": "MAX_TOKENS",
                "content": {"parts": [{"text": "SECRET-PAYLOAD"}]}
            }]
        }),
        delay: Duration::ZERO,
    });
    let err = call_err(&http, &conn, &request).await;
    assert_eq!(err, JudgeError::Truncated);
    assert_hidden(&err);

    mock.enqueue(MockReply {
        status: 200,
        body: json!({
            "candidates": [{
                "finishReason": "LANGUAGE",
                "content": {"parts": [{"text": "kept"}]}
            }]
        }),
        delay: Duration::ZERO,
    });
    let reply = call(&http, &conn, &request).await.expect("language");
    assert_eq!(reply.output_text, "kept");

    mock.enqueue(MockReply {
        status: 200,
        body: json!({"candidates": []}),
        delay: Duration::ZERO,
    });
    assert_eq!(
        call_err(&http, &conn, &request).await,
        JudgeError::EmptyAnswer
    );
}

#[tokio::test]
async fn maps_usage_including_thoughts_and_cache() {
    let mock = Mock::start().await;
    let conn = connection(mock.origin(), Vec::new());
    let http = client();
    let output = numeric();
    mock.enqueue(MockReply {
        status: 200,
        body: json!({
            "modelVersion": MODEL,
            "candidates": [{
                "finishReason": "STOP",
                "content": {"parts": [{"text": "{\"value\":0.5}"}]}
            }],
            "usageMetadata": {
                "promptTokenCount": 10,
                "cachedContentTokenCount": 3,
                "candidatesTokenCount": 4,
                "thoughtsTokenCount": 2
            }
        }),
        delay: Duration::ZERO,
    });
    let reply = call(&http, &conn, &judge(&output, None, Duration::from_secs(2)))
        .await
        .expect("usage");
    assert_eq!(reply.usage.input, 10);
    assert_eq!(reply.usage.output, 6);
    assert_eq!(reply.usage.cache_read, 3);
    assert_eq!(reply.usage.cache_write, 0);
    assert_eq!(reply.usage.reasoning, 2);

    mock.enqueue(MockReply {
        status: 200,
        body: json!({
            "candidates": [{
                "finishReason": "STOP",
                "content": {"parts": [{"text": "{\"value\":0.5}"}]}
            }],
            "usageMetadata": {"promptTokenCount": 1, "candidatesTokenCount": 1}
        }),
        delay: Duration::ZERO,
    });
    let reply = call(&http, &conn, &judge(&output, None, Duration::from_secs(2)))
        .await
        .expect("bare usage");
    assert_eq!(reply.usage.input, 1);
    assert_eq!(reply.usage.cache_read, 0);
    assert_eq!(reply.usage.reasoning, 0);
    assert_eq!(reply.response_model, None);
}

#[tokio::test]
async fn classifies_provider_statuses() {
    let cases = [
        (429, "RESOURCE_EXHAUSTED", ErrorClass::Retryable),
        (401, "UNAUTHENTICATED", ErrorClass::Pause),
        (403, "PERMISSION_DENIED", ErrorClass::Pause),
        (404, "NOT_FOUND", ErrorClass::Pause),
        (400, "INVALID_ARGUMENT", ErrorClass::Permanent),
        (400, "FAILED_PRECONDITION", ErrorClass::Permanent),
        (503, "UNAVAILABLE", ErrorClass::Retryable),
        (500, "INTERNAL", ErrorClass::Retryable),
        (504, "DEADLINE_EXCEEDED", ErrorClass::Retryable),
    ];
    let mock = Mock::start().await;
    let conn = connection(mock.origin(), Vec::new());
    let http = client();
    let output = numeric();
    let request = judge(&output, None, Duration::from_secs(2));
    for (status, kind, class) in cases {
        mock.enqueue(MockReply {
            status,
            body: json!({"error": {"code": status, "status": kind, "message": "SECRET-PAYLOAD"}}),
            delay: Duration::ZERO,
        });
        let err = call_err(&http, &conn, &request).await;
        assert_eq!(err.class(), class, "{kind}");
        let JudgeError::Provider {
            status: got,
            provider_type,
            provider_code,
            provider_message,
            ..
        } = &err
        else {
            panic!("provider error for {kind}");
        };
        assert_eq!(*got, status);
        assert_eq!(provider_type, &None);
        assert_eq!(provider_code.as_deref(), Some(kind));
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
        body: json!({"candidates": []}),
        delay: Duration::from_secs(5),
    });
    let err = call(
        &http,
        &conn,
        &judge(&output, None, Duration::from_millis(200)),
    )
    .await
    .expect_err("timeout");
    assert_eq!(err.class(), ErrorClass::Retryable);
    assert_eq!(err, JudgeError::Transport(EgressError::Timeout));
    assert_hidden(&err);
    assert!(!err.to_string().contains("/v1beta"));
}

#[tokio::test]
async fn sends_the_key_in_the_header_and_not_the_query() {
    let mock = Mock::start().await;
    let conn = connection(mock.origin(), vec![("X-Trace", "trace-SECRET-PAYLOAD")]);
    let http = client();
    let output = numeric();
    mock.enqueue(MockReply {
        status: 500,
        body: json!({"error": {"code": 500, "status": "INTERNAL", "message": "overloaded"}}),
        delay: Duration::ZERO,
    });
    let err = call(&http, &conn, &judge(&output, None, Duration::from_secs(2)))
        .await
        .expect_err("status");
    let request = &mock.requests()[0];
    assert_eq!(request.path, PATH);
    assert_eq!(request.query, None);
    assert!(!request.path.contains(KEY));
    assert_eq!(request.header("x-goog-api-key"), Some(KEY));
    assert_eq!(request.header("authorization"), None);
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
        &JudgeCall {
            model: "gemini/flash",
            prompt: PROMPT,
            output: &output,
            include_reason: true,
            temperature: None,
            max_output_tokens: 1024,
            timeout: Duration::from_secs(2),
        },
    )
    .await
    .expect_err("model");
    assert_eq!(err, JudgeError::InvalidModel);
    assert_eq!(err.class(), ErrorClass::Permanent);
    assert!(mock.requests().is_empty());

    let bad = connection(mock.origin(), vec![("bad name", "SECRET-PAYLOAD")]);
    let err = call(&http, &bad, &judge(&output, None, Duration::from_secs(2)))
        .await
        .expect_err("header");
    assert_eq!(err, JudgeError::InvalidHeader);
    assert!(!err.to_string().contains("SECRET-PAYLOAD"));
    assert!(mock.requests().is_empty());

    let err = call(
        &http,
        &conn,
        &judge(&output, Some(f64::NAN), Duration::from_secs(2)),
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
    let err = call(&http, &conn, &judge(&output, None, Duration::from_secs(5)))
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
    crate::judge::call(http, conn, call)
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
