//! Webhook evaluator against the in-process mock.
//!
//! This file is an integration test, so clippy's test allow does not apply.
#![allow(clippy::expect_used)]

mod support {
    pub(crate) mod mock;
}

use std::collections::BTreeMap;

use llmobs_core::enums::ScoreDataType;
use llmobs_core::ids::{ProjectId, SpanId, TraceId};
use llmobs_core::score::ScoreValue;
use llmobs_eval::webhook::{WebhookCall, call};
use llmobs_eval::{
    EgressClient, EgressError, EgressPolicy, ErrorClass, OutputScore, Secret, Variables,
    WebhookConfig, WebhookError,
};
use serde_json::{Value, json};
use support::mock::{Mock, MockReply};
use uuid::Uuid;

const SECRET: &str = "whsec_SECRET-PAYLOAD";
const VECTOR_SECRET: &str = "whsec_test";
const EVALUATOR_ID: &str = "00000000-0000-4000-8000-000000000001";
const PROJECT_ID: &str = "00000000-0000-4000-8000-000000000002";
const TRACE_ID: &str = "4bf92f3577b34da6a3ce929d0e0e4736";
const SPAN_ID: &str = "00f067aa0ba902b7";
const BODY: &str = r#"{"evaluator":{"id":"00000000-0000-4000-8000-000000000001","name":"grader"},"project_id":"00000000-0000-4000-8000-000000000002","trace_id":"4bf92f3577b34da6a3ce929d0e0e4736","span_id":"00f067aa0ba902b7","variables":{"output":"hello"}}"#;
// printf '1700000000.%s' "$body" | openssl dgst -sha256 -hmac whsec_test
const SIGNATURE: &str =
    "t=1700000000,v1=0afa4defe534c3c5fd9d85591250486d7b4ac07115241b92771a02ca3985e4b4";

fn webhook_url(mock: &Mock) -> String {
    let mut url = mock.origin();
    url.set_path("/webhook");
    url.to_string()
}

fn client(allow_private_network: bool) -> EgressClient {
    EgressClient::new(EgressPolicy {
        allow_private_network,
    })
    .expect("client")
}

fn variables() -> Variables {
    Variables::from_map(BTreeMap::from([(
        "output".to_owned(),
        Value::String("hello".to_owned()),
    )]))
}

fn output(data_type: ScoreDataType, categories: Option<Vec<&str>>) -> OutputScore {
    OutputScore {
        name: "grade".to_owned(),
        data_type,
        categories: categories.map(|labels| labels.into_iter().map(str::to_owned).collect()),
    }
}

fn config(url: String, headers: Vec<(&str, &str)>) -> WebhookConfig {
    WebhookConfig {
        url,
        timeout_ms: 10_000,
        headers: headers
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value.to_owned()))
            .collect(),
        secret_enc: None,
    }
}

fn invocation<'a>(
    config: &'a WebhookConfig,
    secret: &'a Secret,
    variables: &'a Variables,
    output: &'a OutputScore,
    trace_id: Option<TraceId>,
    span_id: Option<SpanId>,
) -> WebhookCall<'a> {
    WebhookCall {
        config,
        secret,
        evaluator_id: Uuid::parse_str(EVALUATOR_ID).expect("evaluator"),
        evaluator_name: "grader",
        project_id: PROJECT_ID.parse::<ProjectId>().expect("project"),
        trace_id,
        span_id,
        variables,
        output,
        now_unix_s: 1_700_000_000,
    }
}

fn ids() -> (TraceId, SpanId) {
    (
        TRACE_ID.parse::<TraceId>().expect("trace"),
        SPAN_ID.parse::<SpanId>().expect("span"),
    )
}

fn assert_clean(rendered: &str) {
    assert!(!rendered.contains(SECRET), "{rendered}");
    assert!(!rendered.contains(VECTOR_SECRET), "{rendered}");
    assert!(!rendered.contains("SECRET-PAYLOAD"), "{rendered}");
}

#[tokio::test]
async fn signs_the_exact_body_and_sends_the_headers() {
    let mock = Mock::start().await;
    let http = client(true);
    let config = config(
        webhook_url(&mock),
        vec![("X-Extra", "keep"), ("x-llmobs-signature", "forged")],
    );
    let secret = Secret::new(VECTOR_SECRET.to_owned());
    let variables = variables();
    let output = output(ScoreDataType::Numeric, None);
    let (trace_id, span_id) = ids();
    let request = invocation(
        &config,
        &secret,
        &variables,
        &output,
        Some(trace_id),
        Some(span_id),
    );
    assert_clean(&format!("{request:?}"));
    mock.enqueue(MockReply {
        status: 200,
        body: json!({"value": 0.5, "reason": "fits"}),
        delay: std::time::Duration::ZERO,
    });
    let reply = call(&http, &request).await.expect("webhook");
    assert_eq!(reply.status, 200);
    assert_eq!(reply.request_body, BODY.as_bytes());
    assert_eq!(reply.value, ScoreValue::Numeric(0.5));
    assert_eq!(reply.reason.as_deref(), Some("fits"));
    assert_eq!(
        reply.response_body,
        Some(json!({"value": 0.5, "reason": "fits"}))
    );
    assert_clean(&format!("{reply:?}"));

    let seen = mock.requests();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].path, "/webhook");
    assert_eq!(seen[0].query, None);
    assert_eq!(
        seen[0].body,
        serde_json::from_str::<Value>(BODY).expect("body")
    );
    assert_eq!(seen[0].header("content-type"), Some("application/json"));
    assert_eq!(seen[0].header("x-extra"), Some("keep"));
    assert_eq!(seen[0].header("x-llmobs-signature"), Some(SIGNATURE));
    assert_clean(&format!("{:?}", seen[0].headers));
}

#[tokio::test]
async fn absent_ids_are_null() {
    let mock = Mock::start().await;
    let http = client(true);
    let config = config(webhook_url(&mock), Vec::new());
    let secret = Secret::new(SECRET.to_owned());
    let variables = variables();
    let output = output(ScoreDataType::Boolean, None);
    let request = invocation(&config, &secret, &variables, &output, None, None);
    mock.enqueue(MockReply {
        status: 200,
        body: json!({"value": false}),
        delay: std::time::Duration::ZERO,
    });
    let reply = call(&http, &request).await.expect("webhook");
    assert_eq!(reply.value, ScoreValue::Boolean(false));
    assert_eq!(reply.reason, None);
    let body = mock.requests().remove(0).body;
    assert_eq!(body["trace_id"], Value::Null);
    assert_eq!(body["span_id"], Value::Null);
    assert_eq!(body["evaluator"]["name"], "grader");
    assert_clean(&format!("{reply:?}"));
}

#[tokio::test]
async fn each_data_type_and_a_normalized_label() {
    let mock = Mock::start().await;
    let http = client(true);
    let config = config(webhook_url(&mock), Vec::new());
    let secret = Secret::new(SECRET.to_owned());
    let variables = variables();
    let numeric = output(ScoreDataType::Numeric, None);
    let boolean = output(ScoreDataType::Boolean, None);
    let categorical = output(ScoreDataType::Categorical, Some(vec!["Correct"]));
    let cases = [
        (
            &numeric,
            json!({"value": 0.5, "reason": "because"}),
            ScoreValue::Numeric(0.5),
            Some("because"),
        ),
        (
            &boolean,
            json!({"value": true}),
            ScoreValue::Boolean(true),
            None,
        ),
        (
            &categorical,
            json!({"label": " correct. ", "reason": null}),
            ScoreValue::Categorical {
                label: "Correct".to_owned(),
                value: None,
            },
            None,
        ),
    ];
    for (output, body, value, reason) in cases {
        mock.enqueue(MockReply {
            status: 200,
            body,
            delay: std::time::Duration::ZERO,
        });
        let (trace_id, span_id) = ids();
        let request = invocation(
            &config,
            &secret,
            &variables,
            output,
            Some(trace_id),
            Some(span_id),
        );
        let reply = call(&http, &request).await.expect("score");
        assert_eq!(reply.value, value);
        assert_eq!(reply.reason.as_deref(), reason);
        assert_clean(&format!("{reply:?} {request:?}"));
    }
}

#[tokio::test]
async fn cuts_the_reason_to_unicode_scalars() {
    let mock = Mock::start().await;
    let http = client(true);
    let config = config(webhook_url(&mock), Vec::new());
    let secret = Secret::new(SECRET.to_owned());
    let variables = variables();
    let output = output(ScoreDataType::Numeric, None);
    let (trace_id, span_id) = ids();
    let request = invocation(
        &config,
        &secret,
        &variables,
        &output,
        Some(trace_id),
        Some(span_id),
    );
    mock.enqueue(MockReply {
        status: 200,
        body: json!({"value": 1, "reason": "é".repeat(8_001)}),
        delay: std::time::Duration::ZERO,
    });
    let reply = call(&http, &request).await.expect("reason");
    let reason = reply.reason.expect("reason");
    assert_eq!(reason.chars().count(), 8_000);
    assert!(reason.chars().all(|character| character == 'é'));
}

#[tokio::test]
async fn classifies_each_status_and_does_not_follow_a_redirect() {
    let mock = Mock::start().await;
    let http = client(true);
    let config = config(webhook_url(&mock), Vec::new());
    let secret = Secret::new(SECRET.to_owned());
    let variables = variables();
    let output = output(ScoreDataType::Numeric, None);
    let cases = [
        (408, ErrorClass::Retryable),
        (425, ErrorClass::Retryable),
        (429, ErrorClass::Retryable),
        (500, ErrorClass::Retryable),
        (599, ErrorClass::Retryable),
        (401, ErrorClass::Pause),
        (403, ErrorClass::Pause),
        (404, ErrorClass::Pause),
        (410, ErrorClass::Pause),
        (301, ErrorClass::Permanent),
        (400, ErrorClass::Permanent),
        (402, ErrorClass::Permanent),
        (422, ErrorClass::Permanent),
    ];
    for (status, class) in cases {
        mock.enqueue(MockReply {
            status,
            body: json!({"error": SECRET}),
            delay: std::time::Duration::ZERO,
        });
        let (trace_id, span_id) = ids();
        let request = invocation(
            &config,
            &secret,
            &variables,
            &output,
            Some(trace_id),
            Some(span_id),
        );
        let err = call(&http, &request).await.expect_err("status");
        assert_eq!(err, WebhookError::Status { status, class });
        assert_eq!(err.class(), class);
        assert_eq!(err.to_string(), format!("webhook status {status}"));
        assert_clean(&format!("{err} {err:?}"));
    }

    mock.enqueue_raw(302, b"moved".to_vec(), Some("/landed"));
    let (trace_id, span_id) = ids();
    let request = invocation(
        &config,
        &secret,
        &variables,
        &output,
        Some(trace_id),
        Some(span_id),
    );
    let err = call(&http, &request).await.expect_err("redirect");
    assert_eq!(
        err,
        WebhookError::Status {
            status: 302,
            class: ErrorClass::Permanent,
        }
    );
    let seen = mock.requests();
    assert_eq!(seen.len(), cases.len() + 1);
    assert!(seen.iter().all(|request| request.path == "/webhook"));
    assert!(!seen.iter().any(|request| request.path == "/landed"));
}

#[tokio::test]
async fn rejects_bad_bodies() {
    let mock = Mock::start().await;
    let http = client(true);
    let config = config(webhook_url(&mock), Vec::new());
    let secret = Secret::new(SECRET.to_owned());
    let variables = variables();
    let numeric = output(ScoreDataType::Numeric, None);
    let boolean = output(ScoreDataType::Boolean, None);
    let categorical = output(ScoreDataType::Categorical, Some(vec!["Correct"]));
    let unlabeled = output(ScoreDataType::Categorical, None);
    let text = output(ScoreDataType::Text, None);
    let json_cases = [
        (&numeric, json!([]), "not a JSON object"),
        (&numeric, json!("SECRET-PAYLOAD"), "not a JSON object"),
        (&numeric, json!({}), "missing value"),
        (&numeric, json!({"value": null}), "missing value"),
        (
            &numeric,
            json!({"value": "0.5"}),
            "value has the wrong type",
        ),
        (&numeric, json!({"value": true}), "value has the wrong type"),
        (&boolean, json!({}), "missing value"),
        (&boolean, json!({"value": 1}), "value has the wrong type"),
        (&categorical, json!({}), "missing label"),
        (
            &categorical,
            json!({"label": 1}),
            "label has the wrong type",
        ),
        (&categorical, json!({"label": "nope"}), "unknown label"),
        (&unlabeled, json!({"label": "Correct"}), "unknown label"),
        (
            &numeric,
            json!({"value": 1, "reason": 2}),
            "reason has the wrong type",
        ),
        (&text, json!({"value": "nope"}), "unsupported data type"),
    ];
    for (output, body, reason) in json_cases {
        mock.enqueue(MockReply {
            status: 200,
            body,
            delay: std::time::Duration::ZERO,
        });
        let (trace_id, span_id) = ids();
        let request = invocation(
            &config,
            &secret,
            &variables,
            output,
            Some(trace_id),
            Some(span_id),
        );
        let err = call(&http, &request).await.expect_err("body");
        assert_eq!(err, WebhookError::BadResponse { reason });
        assert_eq!(err.class(), ErrorClass::Permanent);
        assert_clean(&format!("{err} {err:?}"));
    }

    for raw in [b"not-json".as_slice(), b"", b"{", b"SECRET-PAYLOAD"] {
        mock.enqueue_raw(200, raw.to_vec(), None);
        let (trace_id, span_id) = ids();
        let request = invocation(
            &config,
            &secret,
            &variables,
            &numeric,
            Some(trace_id),
            Some(span_id),
        );
        let err = call(&http, &request).await.expect_err("raw");
        assert_eq!(
            err,
            WebhookError::BadResponse {
                reason: "not a JSON object"
            }
        );
        assert_clean(&format!("{err} {err:?}"));
    }
}

#[tokio::test]
async fn rejects_an_oversized_body_without_quoting_it() {
    let mock = Mock::start().await;
    let http = client(true);
    let config = config(webhook_url(&mock), Vec::new());
    let secret = Secret::new(SECRET.to_owned());
    let variables = variables();
    let output = output(ScoreDataType::Numeric, None);
    let (trace_id, span_id) = ids();
    let request = invocation(
        &config,
        &secret,
        &variables,
        &output,
        Some(trace_id),
        Some(span_id),
    );
    mock.enqueue(MockReply {
        status: 200,
        body: Value::String(format!("{SECRET}{}", "x".repeat(70_000))),
        delay: std::time::Duration::ZERO,
    });
    let err = call(&http, &request).await.expect_err("cap");
    assert_eq!(
        err,
        WebhookError::Transport(EgressError::TooLarge { limit: 64 * 1024 })
    );
    assert_eq!(err.class(), ErrorClass::Permanent);
    assert_eq!(err.to_string(), "response exceeds 65536 bytes");
    assert_clean(&format!("{err} {err:?}"));
}

#[tokio::test]
async fn keeps_the_secret_out_of_request_failures() {
    let http = client(true);
    let secret = Secret::new(SECRET.to_owned());
    // A newline is not a valid header value, so this fails before connecting.
    let bad_header = config_with_header(
        "http://127.0.0.1:9/webhook?token=whsec_SECRET-PAYLOAD",
        "X-Token",
        "not-a-header\n",
    );
    let variables = variables();
    let output = output(ScoreDataType::Numeric, None);
    let (trace_id, span_id) = ids();
    let request = invocation(
        &bad_header,
        &secret,
        &variables,
        &output,
        Some(trace_id),
        Some(span_id),
    );
    let err = call(&http, &request).await.expect_err("header");
    assert_eq!(err, WebhookError::InvalidHeader);
    assert_eq!(err.to_string(), "invalid header");
    let rendered = format!("{err} {err:?}");
    assert_clean(&rendered);
    assert!(!rendered.contains("not-a-header"), "{rendered}");
    assert_eq!(format!("{:?}", request.secret), "Secret([redacted])");

    let blocked = client(false);
    let denied = config(
        "http://127.0.0.1:9/webhook?token=whsec_SECRET-PAYLOAD".to_owned(),
        Vec::new(),
    );
    let plain = Secret::new(SECRET.to_owned());
    let request = invocation(
        &denied,
        &plain,
        &variables,
        &output,
        Some(trace_id),
        Some(span_id),
    );
    let err = call(&blocked, &request).await.expect_err("denied");
    assert_eq!(
        err,
        WebhookError::Transport(EgressError::Denied {
            host: "127.0.0.1".to_owned(),
        })
    );
    let rendered = format!("{err} {err:?}");
    assert_clean(&rendered);
    assert!(!rendered.contains("/webhook"), "{rendered}");
    assert!(!rendered.contains("token"), "{rendered}");

    let broken = config("http://".to_owned(), Vec::new());
    let request = invocation(
        &broken,
        &plain,
        &variables,
        &output,
        Some(trace_id),
        Some(span_id),
    );
    let err = call(&http, &request).await.expect_err("url");
    assert_eq!(err, WebhookError::InvalidRequest);
    assert_eq!(err.to_string(), "invalid request");
    assert_clean(&format!("{err} {err:?}"));
}

fn config_with_header(url: &str, name: &str, value: &str) -> WebhookConfig {
    config(url.to_owned(), vec![(name, value)])
}
