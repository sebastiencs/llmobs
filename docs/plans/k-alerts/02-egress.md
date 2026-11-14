# K2 — Public egress client and payload signature in `llmobs-eval`

**Commit subject:** `eval: expose the egress client and the payload signature`

Alert deliveries (K7) must send Slack and webhook POSTs through E's SSRF-guarded client and sign webhook bodies exactly like the webhook evaluator (D165, D166). Both are crate-private today. This sub-phase makes them public with no change in behaviour, and lets `EvalRunner` lend its client so the server keeps one egress client (D-I-29, D-K-11).

## Read first

- README § Design → Delivery, D-K-8, D-K-11.
- `crates/eval/src/egress.rs` (all of it), `crates/eval/src/webhook.rs` (`SIGNATURE_HEADER`, `exchange`, `signature`, `headers`), `crates/eval/src/runner.rs` (`EvalRunner`, `EvalRunner::new`), `crates/eval/src/lib.rs`, `crates/eval/src/secret.rs`.
- `crates/eval/tests/webhook.rs`: `VECTOR_SECRET`, `BODY`, `SIGNATURE`, `client()`; `crates/eval/tests/support/mock.rs`: `Mock::start`, `Mock::origin`, `Mock::enqueue`, `MockReply`, `Mock::requests`, `RecordedRequest::header`.
- Root `Cargo.toml` `[workspace.lints]` (`unreachable_pub = "warn"`, so every item made `pub` must be reachable from `lib.rs`).

## Files

| File | Change |
|---|---|
| `crates/eval/src/egress.rs` | `EgressClient`, `EgressRequest`, `EgressResponse` and their members become `pub` |
| `crates/eval/src/webhook.rs` | new `pub fn sign_payload`; `SIGNATURE_HEADER` becomes `pub`; `signature` calls `sign_payload` |
| `crates/eval/src/runner.rs` | new `pub fn egress(&self) -> &EgressClient` |
| `crates/eval/src/lib.rs` | re-exports |
| `crates/eval/tests/webhook.rs` | two tests |
| `crates/eval/tests/egress_public.rs` | new: two tests (wired like the other test files, see § 5) |

No other crate changes in this sub-phase.

## 1. `egress.rs`

Change only visibility and doc comments, not behaviour:

- `pub(crate) struct EgressClient` → `pub struct EgressClient`. Its doc comment becomes `/// One shared client with the SSRF guard (D166). Redirects are not followed; proxies are ignored.`
- `pub(crate) fn new(policy: EgressPolicy) -> Result<Self, EgressError>` → `pub fn new`.
- `pub(crate) async fn post(&self, req: EgressRequest<'_>) -> Result<EgressResponse, EgressError>` → `pub async fn post`.
- `pub(crate) struct EgressRequest<'a>` → `#[derive(Debug)] pub struct EgressRequest<'a>` (the workspace lint `missing_debug_implementations` fires once it is public; every field type is `Debug`), and each of its five fields from `pub(crate)` to `pub`. Add one doc line per field:
  - `url`: `/// Destination. Only http and https are allowed.`
  - `headers`: `/// Extra request headers, in order.`
  - `body`: `/// Request body, sent as is.`
  - `timeout`: `/// Whole-request timeout.`
  - `max_response_bytes`: `/// Larger bodies fail with [`EgressError::TooLarge`].`
- `pub(crate) struct EgressResponse` → `pub struct EgressResponse`. Its fields are already `pub`; add `/// HTTP status.` and `/// Body, at most the request's cap.`
- `is_denied` stays `pub(crate)`.

## 2. `webhook.rs`

- `pub(crate) const SIGNATURE_HEADER: &str = "x-llmobs-signature";` → `pub const`. Doc: `/// The signature header, lower case. D46 renames it with the product.`
- Add, above the private `signature` function:

```rust
/// `t=<timestamp>,v1=<hex HMAC-SHA256(secret, "<timestamp>.<body>")>`, the value of
/// [`SIGNATURE_HEADER`]. The webhook evaluator and alert webhooks sign the same way.
///
/// `None` only when the HMAC cannot be keyed, which HMAC-SHA256 never refuses.
#[must_use]
pub fn sign_payload(secret: &Secret, timestamp: i64, body: &[u8]) -> Option<String> {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.expose().as_bytes()).ok()?;
    let mut payload = timestamp.to_string().into_bytes();
    payload.push(b'.');
    payload.extend_from_slice(body);
    mac.update(&payload);
    Some(format!(
        "t={timestamp},v1={}",
        hex::encode(mac.finalize().into_bytes())
    ))
}
```

- Rewrite the private `signature` to use it, keeping its signature and errors:

```rust
fn signature(secret: &Secret, timestamp: i64, body: &[u8]) -> Result<HeaderValue, WebhookError> {
    let text = sign_payload(secret, timestamp, body).ok_or(WebhookError::InvalidRequest)?;
    HeaderValue::from_str(&text).map_err(|_| WebhookError::InvalidRequest)
}
```

## 3. `runner.rs`

Add to `impl EvalRunner`, after `new`:

```rust
    /// The shared egress client, for other outbound calls that need the same guard
    /// (alert deliveries, D-K-11).
    #[must_use]
    pub fn egress(&self) -> &EgressClient {
        &self.client
    }
```

## 4. `lib.rs`

- Replace `pub use egress::EgressPolicy;` with `pub use egress::{EgressClient, EgressPolicy, EgressRequest, EgressResponse};`.
- Add `pub use webhook::{SIGNATURE_HEADER, sign_payload};` after the `pub use trace::…` line.
- Add one line at the end of the module doc: `//! [`EgressClient`] and [`sign_payload`] are public so the server's alert deliveries share the guard and the signature (D-K-11).`

`reqwest` types appear in the public API now (`reqwest::Url`, `reqwest::header::{HeaderName, HeaderValue}`, `reqwest::StatusCode`). That is fine: the server already depends on the workspace `reqwest`. Do not re-export reqwest.

## 5. Tests

In `crates/eval/tests/webhook.rs` (it already has `VECTOR_SECRET`, `BODY`, `SIGNATURE`), add:

```rust
#[test]
fn sign_payload_matches_the_test_vector() {
    let secret = Secret::new(VECTOR_SECRET.to_owned());
    assert_eq!(
        crate::sign_payload(&secret, 1_700_000_000, BODY.as_bytes()).as_deref(),
        Some(SIGNATURE)
    );
}

#[test]
fn sign_payload_depends_on_the_timestamp_and_the_body() {
    let secret = Secret::new(VECTOR_SECRET.to_owned());
    let base = crate::sign_payload(&secret, 1_700_000_000, b"{}");
    assert_ne!(base, crate::sign_payload(&secret, 1_700_000_001, b"{}"));
    assert_ne!(base, crate::sign_payload(&secret, 1_700_000_000, b"{ }"));
}
```

These test files are compiled as modules of the library (`lib.rs` has `#[cfg(test)] #[path = "../tests/webhook.rs"] mod webhook_tests;`), so `crate::sign_payload` resolves. Do not add any import: the file already has `use crate::secret::Secret;`, and `crate::sign_payload` resolves through the `lib.rs` re-export.

Create `crates/eval/tests/egress_public.rs` and register it in `lib.rs` exactly like the others, after the `openai_tests` entry:

```rust
#[cfg(test)]
#[path = "../tests/egress_public.rs"]
mod egress_public_tests;
```

Its content:

```rust
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
    let client = EgressClient::new(EgressPolicy { allow_private_network: true }).expect("client");
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
    let client = EgressClient::new(EgressPolicy { allow_private_network: false }).expect("client");
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
    assert!(matches!(result, Err(EgressError::Denied { .. })), "{result:?}");
}
```

`MockReply { status, body, delay }`, `RecordedRequest.path` and `RecordedRequest.body` are the definitions in `tests/support/mock.rs`. Keep `reqwest::Url::parse` qualified (do not import `Url`). `EgressResponse` and `EgressError` derive `Debug`.

## Commands

```bash
cargo test -p llmobs-eval
```

```bash
just check
```

## Done when

- `llmobs_eval::{EgressClient, EgressRequest, EgressResponse, SIGNATURE_HEADER, sign_payload}` are public and `EvalRunner::egress` exists.
- The existing webhook tests still pass unchanged (the signature is byte-identical), and the four new tests pass.
- `just check` is green with no new warnings.
