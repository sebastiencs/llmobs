//! The public egress API, used the way the server's alert delivery uses it.
#![allow(clippy::expect_used)] // clippy's test allowance does not reach files included with #[path]

use std::time::Duration;

use reqwest::header::{HeaderName, HeaderValue};

use crate::mock::{Mock, MockReply};
use crate::{EgressClient, EgressError, EgressPolicy, EgressRequest};

#[tokio::test]
async fn posts_through_the_public_client() {
    let mock = Mock::start().await;
    mock.enqueue(MockReply {
        status: 200,
        body: serde_json::json!({"ok": true}),
        delay: Duration::ZERO,
    });
    let client = EgressClient::new(EgressPolicy {
        allow_private_network: true,
    })
    .expect("client");
    let mut url = mock.origin();
    url.set_path("/webhook"); // the mock routes only /webhook and the provider paths
    let response = client
        .post(EgressRequest {
            url: &url,
            headers: vec![(
                HeaderName::from_static("content-type"),
                HeaderValue::from_static("application/json"),
            )],
            body: br#"{"a":1}"#.to_vec(),
            timeout: Duration::from_secs(5),
            max_response_bytes: 4096,
        })
        .await
        .expect("post");
    assert_eq!(response.status.as_u16(), 200);
    let seen = mock.requests();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].path, "/webhook");
    assert_eq!(seen[0].body, serde_json::json!({"a": 1}));
}

#[tokio::test]
async fn the_public_client_keeps_the_guard() {
    let client = EgressClient::new(EgressPolicy {
        allow_private_network: false,
    })
    .expect("client");
    let url = reqwest::Url::parse("http://127.0.0.1:9/webhook").expect("url");
    let result = client
        .post(EgressRequest {
            url: &url,
            headers: Vec::new(),
            body: Vec::new(),
            timeout: Duration::from_secs(5),
            max_response_bytes: 4096,
        })
        .await;
    assert!(
        matches!(result, Err(EgressError::Denied { .. })),
        "{result:?}"
    );
}
