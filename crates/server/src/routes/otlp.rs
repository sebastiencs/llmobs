//! `POST /v1/traces`: OTLP/HTTP trace export (spec 02 § Endpoints, D-G-2 to D-G-9).
//!
//! The route is key-only (D-G-15) and undocumented (D-G-6). Its body is read under its own
//! cap, so the router's JSON limit does not apply.

use std::time::Instant;

use axum::body::Body;
use axum::extract::{Extension, State};
use axum::http::{HeaderMap, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use llmobs_core::ids::ProjectId;

use crate::auth::roles::KeyScope;
use crate::http::AppState;
use crate::http::access::{Access, ProjectAccess, RouteDef};
use crate::http::error::ApiError;
use crate::ingest::body::{body_encoding, body_format, read_capped};
use crate::ingest::otlp::export_response;
use crate::ingest::{Accepted, CommitTarget, IngestError};
use crate::meta::projects::ProjectRow;

/// The OTLP/HTTP route. Undocumented, so it stays out of `openapi.json` (D-G-6).
pub(crate) fn routes() -> Vec<RouteDef> {
    vec![RouteDef {
        method: Method::POST,
        path: "/v1/traces",
        access: Access::Key {
            scope: KeyScope::Ingest,
        },
        documented: false,
        handler: post(export),
    }]
}

async fn export(
    State(state): State<AppState>,
    Extension(access): Extension<ProjectAccess>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    let started = Instant::now();
    match export_body(&state, &access.project, &headers, body, started).await {
        Ok(response) => Ok(response),
        Err(error) => {
            log_refusal(access.project.id, &error, started);
            Err(error.into())
        }
    }
}

/// Steps 2 to 8 of G1.7. Every refusal is returned, and logged by [`export`].
async fn export_body(
    state: &AppState,
    project: &ProjectRow,
    headers: &HeaderMap,
    body: Body,
    started: Instant,
) -> Result<Response, IngestError> {
    let format = body_format(headers).map_err(IngestError::Body)?;
    let encoding = body_encoding(headers).map_err(IngestError::Body)?;
    let reservation = state.ingest.reserve(content_length(headers))?;
    let raw = read_capped(body, headers, state.ingest.max_request_bytes())
        .await
        .map_err(IngestError::Body)?;
    let now_ns = state.clock.now_ns();
    let Accepted {
        normalized,
        request_bytes,
        decompressed_bytes,
        rewritten,
        reservation,
    } = state
        .ingest
        .accept(raw, reservation, format, encoding, project, now_ns)
        .await?;
    let rejections = normalized.rejections.clone();
    let target = CommitTarget {
        org_id: project.org_id,
        project_id: project.id,
        request_bytes: decompressed_bytes,
        now_ns,
    };
    let receipt = state
        .ingest
        .commit(
            &state.storage,
            &state.usage,
            &state.live,
            target,
            normalized,
        )
        .await?;
    // The budget covers the spans until they are appended (D-G-5).
    drop(reservation);
    tracing::info!(
        target: "ingest",
        project_id = %project.id,
        request_bytes,
        decompressed_bytes,
        spans = receipt.accepted,
        rejected = rejections.total(),
        media = receipt.media,
        rewritten,
        duration_ms = elapsed_ms(started),
        "request accepted"
    );
    let (content_type, payload) = export_response(format, &rejections);
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, content_type)],
        payload,
    )
        .into_response())
}

/// `Content-Length` as a number. Absent or unparsable is `None`.
fn content_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(header::CONTENT_LENGTH)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// One `warn` per refused request. A `500` is logged once by `ApiError::into_response`
/// instead, so the internal variants return early. The reason is a static string: no
/// header, key, body byte or attribute.
fn log_refusal(project: ProjectId, error: &IngestError, started: Instant) {
    let reason = match error {
        IngestError::Body(body) => match body {
            crate::ingest::body::BodyError::UnsupportedType => "unsupported_type",
            crate::ingest::body::BodyError::UnsupportedEncoding => "unsupported_encoding",
            crate::ingest::body::BodyError::TooLarge => "too_large",
            crate::ingest::body::BodyError::Corrupt => "corrupt",
            crate::ingest::body::BodyError::Read => "read",
        },
        IngestError::TooManySpans => "too_many_spans",
        IngestError::Decode(_) => "decode",
        IngestError::Busy => "budget",
        IngestError::Append(append) => match append {
            llmobs_storage::error::AppendError::Backpressure => "backpressure",
            llmobs_storage::error::AppendError::WalFailed => "wal_failed",
            llmobs_storage::error::AppendError::ShuttingDown => "shutting_down",
            llmobs_storage::error::AppendError::DiskFull => "disk_full",
            llmobs_storage::error::AppendError::ProjectDropped => "project_dropped",
            llmobs_storage::error::AppendError::InvalidBatch { .. } => return,
        },
        IngestError::Media(media) => match media {
            llmobs_storage::error::MediaError::Invalid { .. } => "invalid_media",
            llmobs_storage::error::MediaError::ProjectDropped => "project_dropped",
            llmobs_storage::error::MediaError::DiskFull => "disk_full",
            llmobs_storage::error::MediaError::Object(_)
            | llmobs_storage::error::MediaError::Catalog(_)
            | llmobs_storage::error::MediaError::Query(_) => "storage_unavailable",
            llmobs_storage::error::MediaError::HashMismatch
            | llmobs_storage::error::MediaError::NotFound
            | llmobs_storage::error::MediaError::InvalidRange
            | llmobs_storage::error::MediaError::Fault => return,
        },
        IngestError::Batch(_) | IngestError::MediaHash | IngestError::Join(_) => return,
    };
    tracing::warn!(
        target: "ingest",
        project_id = %project,
        duration_ms = elapsed_ms(started),
        reason,
        "request refused"
    );
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::time::Duration;

    use axum::http::{Method, StatusCode};
    use bytes::Bytes;
    use bytesize::ByteSize;
    use datafusion::prelude::SessionContext;
    use llmobs_otel::{IngestContext, PriceTable, builtin_price_rows, decode_json, normalize};
    use llmobs_storage::QueryOptions;
    use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceResponse;
    use prost::Message;
    use serde_json::Value;

    use super::*;
    use crate::api::common::ProblemCode;
    use crate::ingest::body::{BodyEncoding, BodyFormat};
    use crate::test_support::{Seeded, TestApp, assert_logs_clean, capture_logs};

    const MARKER: &str = "SECRET-BODY-MARKER";
    const ATTRIBUTE_VALUE: &str = "attr-secret-value";
    const RESOURCE_TIMES: &str =
        "\"startTimeUnixNano\":\"1767225590000000000\",\"endTimeUnixNano\":\"1767225591000000000\"";

    /// One OTLP/JSON span with valid ids and times. `extra` is appended inside the span
    /// object, so it starts with a comma when it is not empty.
    fn span_json(index: usize, extra: &str) -> String {
        let id = index + 1;
        format!(
            "{{\"traceId\":\"{id:032x}\",\"spanId\":\"{id:016x}\",\"name\":\"span-{index}\",{RESOURCE_TIMES}{extra}}}"
        )
    }

    /// An OTLP/JSON export of `spans` under `resource` (empty, or a `"resource":{..},` fragment).
    fn export_json(resource: &str, spans: &[String]) -> Vec<u8> {
        let mut body = String::from("{\"resourceSpans\":[{");
        body.push_str(resource);
        body.push_str("\"scopeSpans\":[{\"spans\":[");
        body.push_str(&spans.join(","));
        body.push_str("]}]}]}");
        body.into_bytes()
    }

    fn plain_export(count: usize) -> Vec<u8> {
        let spans: Vec<String> = (0..count).map(|index| span_json(index, "")).collect();
        export_json("", &spans)
    }

    /// The same request as protobuf, re-encoded from its JSON form.
    fn protobuf(json: &[u8]) -> Vec<u8> {
        decode_json(json)
            .expect("the test body is valid OTLP/JSON")
            .encode_to_vec()
    }

    fn compress(body: &[u8], encoding: BodyEncoding) -> Vec<u8> {
        match encoding {
            BodyEncoding::Identity => body.to_vec(),
            BodyEncoding::Gzip => {
                let mut encoder =
                    flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
                encoder.write_all(body).expect("gzip write");
                encoder.finish().expect("gzip finish")
            }
            BodyEncoding::Deflate => {
                let mut encoder =
                    flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
                encoder.write_all(body).expect("zlib write");
                encoder.finish().expect("zlib finish")
            }
            BodyEncoding::Zstd => zstd::stream::encode_all(body, 0).expect("zstd encode"),
        }
    }

    /// One span whose string attribute is `bytes` long.
    fn large_attribute_export(bytes: usize) -> Vec<u8> {
        let extra = format!(
            ",\"attributes\":[{{\"key\":\"payload\",\"value\":{{\"stringValue\":\"{}\"}}}}]",
            "a".repeat(bytes)
        );
        export_json("", &[span_json(0, &extra)])
    }

    fn post_traces(
        app: &TestApp,
        key: &str,
        content_type: &str,
        body: impl Into<Bytes>,
    ) -> crate::test_support::TestRequest {
        app.request(Method::POST, "/v1/traces")
            .header("authorization", &format!("Bearer {key}"))
            .header("content-type", content_type)
            .body(body)
    }

    /// Spans in the project, read through the storage query provider.
    async fn stored_spans(app: &TestApp, project: ProjectId) -> usize {
        let query = app
            .state
            .storage
            .query(project, QueryOptions::default())
            .await
            .expect("query opens");
        let ctx = SessionContext::new();
        ctx.register_table("raw_spans", query.spans)
            .expect("spans register");
        ctx.table("raw_spans")
            .await
            .expect("spans table")
            .count()
            .await
            .expect("count")
    }

    /// All stored span rows, formatted. Used to look at columns the tests need.
    async fn stored_rows(app: &TestApp, project: ProjectId) -> String {
        let query = app
            .state
            .storage
            .query(project, QueryOptions::default())
            .await
            .expect("query opens");
        let ctx = SessionContext::new();
        ctx.register_table("raw_spans", query.spans)
            .expect("spans register");
        let batches = ctx
            .sql("SELECT * FROM raw_spans")
            .await
            .expect("sql")
            .collect()
            .await
            .expect("collect");
        format!("{batches:?}")
    }

    fn json_body(response: &crate::test_support::TestResponse) -> Value {
        serde_json::from_slice(&response.body).expect("JSON body")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn protobuf_export_is_stored_and_answers_an_empty_body() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let body = protobuf(&plain_export(2));
        let response = post_traces(&app, &seeded.ingest_key, "application/x-protobuf", body)
            .send()
            .await;
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(
            response.header("content-type"),
            Some("application/x-protobuf")
        );
        assert!(response.body.is_empty());
        assert_eq!(stored_spans(&app, seeded.project.id).await, 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn json_export_answers_an_empty_object() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let response = post_traces(
            &app,
            &seeded.ingest_key,
            "application/json",
            plain_export(2),
        )
        .send()
        .await;
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.header("content-type"), Some("application/json"));
        assert_eq!(response.body.as_ref(), b"{}");
        assert_eq!(stored_spans(&app, seeded.project.id).await, 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn every_content_encoding_is_accepted() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let body = plain_export(2);
        for (name, encoding) in [
            ("gzip", BodyEncoding::Gzip),
            ("deflate", BodyEncoding::Deflate),
            ("zstd", BodyEncoding::Zstd),
        ] {
            let (project, key) = app.add_project(&seeded, name, None).await;
            let response = post_traces(&app, &key, "application/json", compress(&body, encoding))
                .header("content-encoding", name)
                .send()
                .await;
            assert_eq!(response.status, StatusCode::OK, "{name}");
            assert_eq!(stored_spans(&app, project.id).await, 2, "{name}");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn partial_success_stores_the_valid_spans_and_reports_the_rest() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let body = fixture_bytes("edge/rejected-ids/input.json");
        let response = post_traces(&app, &seeded.ingest_key, "application/json", body.clone())
            .send()
            .await;
        assert_eq!(response.status, StatusCode::OK);
        let partial = json_body(&response)["partialSuccess"].clone();
        assert_eq!(partial["rejectedSpans"], "4");
        // The message is the one `normalize` gives for the same request.
        let ctx = IngestContext {
            project_id: seeded.project.id,
            ingest_time: app.state.clock.now_ns(),
            retention_days: seeded.project.retention_days,
            limits: app.state.config.ingest.limits(&app.state.config.media),
        };
        let prices = PriceTable::new(builtin_price_rows()).expect("built-in prices");
        let request = decode_json(&body).expect("fixture decodes");
        let expected = normalize(&request, &ctx, &prices).rejections.message();
        assert_eq!(partial["errorMessage"].as_str(), expected.as_deref());
        assert_eq!(stored_spans(&app, seeded.project.id).await, 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn protobuf_partial_success_carries_the_count() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let body = protobuf(&fixture_bytes("edge/rejected-ids/input.json"));
        let response = post_traces(&app, &seeded.ingest_key, "application/x-protobuf", body)
            .send()
            .await;
        assert_eq!(response.status, StatusCode::OK);
        let decoded =
            ExportTraceServiceResponse::decode(response.body.as_ref()).expect("protobuf response");
        let partial = decoded.partial_success.expect("partial success");
        assert_eq!(partial.rejected_spans, 4);
        assert!(!partial.error_message.is_empty());
        assert_eq!(stored_spans(&app, seeded.project.id).await, 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_request_whose_only_span_is_rejected_stores_nothing() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let zero = format!(
            "{{\"traceId\":\"00000000000000000000000000000000\",\"spanId\":\"0000000000000001\",\"name\":\"zero\",{RESOURCE_TIMES}}}"
        );
        let response = post_traces(
            &app,
            &seeded.ingest_key,
            "application/json",
            export_json("", &[zero]),
        )
        .send()
        .await;
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(json_body(&response)["partialSuccess"]["rejectedSpans"], "1");
        assert_eq!(stored_spans(&app, seeded.project.id).await, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_empty_request_answers_ok() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let response = post_traces(&app, &seeded.ingest_key, "application/json", "{}")
            .send()
            .await;
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(response.body.as_ref(), b"{}");
        assert_eq!(stored_spans(&app, seeded.project.id).await, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_body_above_the_old_one_mib_router_limit_is_read() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let body = protobuf(&large_attribute_export(2 * 1024 * 1024));
        assert!(
            body.len() > 2 * 1024 * 1024,
            "the body is {} bytes",
            body.len()
        );
        let response = post_traces(&app, &seeded.ingest_key, "application/x-protobuf", body)
            .send()
            .await;
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(stored_spans(&app, seeded.project.id).await, 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_body_above_the_limit_is_refused_with_413() {
        let app = TestApp::with_config(|config| {
            config.ingest.max_request_bytes = ByteSize::kib(64);
        })
        .await;
        let seeded = app.seed().await;
        let body = vec![b' '; 64 * 1024 + 1];
        let response = post_traces(&app, &seeded.ingest_key, "application/json", body)
            .send()
            .await;
        assert_eq!(response.status, StatusCode::PAYLOAD_TOO_LARGE);
        let problem = response.problem();
        assert_eq!(problem.code, ProblemCode::PayloadTooLarge);
        assert_eq!(problem.detail.as_deref(), Some("request body is too large"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_gzip_body_that_decompresses_past_the_limit_is_refused_with_413() {
        let app = TestApp::with_config(|config| {
            config.ingest.max_request_bytes = ByteSize::kib(64);
        })
        .await;
        let seeded = app.seed().await;
        let bomb = compress(&vec![b'a'; 128 * 1024], BodyEncoding::Gzip);
        assert!(bomb.len() < 64 * 1024);
        let response = post_traces(&app, &seeded.ingest_key, "application/json", bomb)
            .header("content-encoding", "gzip")
            .send()
            .await;
        assert_eq!(response.status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(
            response.problem().detail.as_deref(),
            Some("request body is too large")
        );
        assert_eq!(stored_spans(&app, seeded.project.id).await, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn too_many_spans_are_refused_with_413() {
        let app = TestApp::with_config(|config| {
            config.ingest.max_spans_per_request = 2;
        })
        .await;
        let seeded = app.seed().await;
        let response = post_traces(
            &app,
            &seeded.ingest_key,
            "application/json",
            plain_export(3),
        )
        .send()
        .await;
        assert_eq!(response.status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(
            response.problem().detail.as_deref(),
            Some("too many spans in one request")
        );
        assert_eq!(stored_spans(&app, seeded.project.id).await, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unsupported_types_and_encodings_are_refused_with_415() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let text = post_traces(&app, &seeded.ingest_key, "text/plain", plain_export(1))
            .send()
            .await;
        assert_eq!(text.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert_eq!(
            text.problem().detail.as_deref(),
            Some("expected Content-Type application/x-protobuf or application/json")
        );
        let brotli = post_traces(
            &app,
            &seeded.ingest_key,
            "application/json",
            plain_export(1),
        )
        .header("content-encoding", "br")
        .send()
        .await;
        assert_eq!(brotli.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert_eq!(
            brotli.problem().detail.as_deref(),
            Some("unsupported Content-Encoding")
        );
        assert_eq!(stored_spans(&app, seeded.project.id).await, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn malformed_bodies_are_400_and_their_bytes_stay_out() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let protobuf_garbage = [&[0xff_u8; 11][..], MARKER.as_bytes()].concat();
        let bodies = [
            ("application/x-protobuf", None, protobuf_garbage),
            (
                "application/json",
                None,
                format!("{{\"resourceSpans\": {MARKER}").into_bytes(),
            ),
            (
                "application/json",
                Some("gzip"),
                format!("{MARKER} not gzip").into_bytes(),
            ),
        ];
        for (content_type, encoding, body) in bodies {
            let mut request = post_traces(&app, &seeded.ingest_key, content_type, body);
            if let Some(encoding) = encoding {
                request = request.header("content-encoding", encoding);
            }
            let response =
                assert_logs_clean(&[MARKER, seeded.ingest_key.as_str()], request.send()).await;
            assert_eq!(response.status, StatusCode::BAD_REQUEST, "{content_type}");
            assert_eq!(response.problem().code, ProblemCode::BadRequest);
            let text = String::from_utf8_lossy(&response.body);
            assert!(!text.contains(MARKER), "{text}");
        }
        assert_eq!(stored_spans(&app, seeded.project.id).await, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn callers_without_an_ingest_key_are_refused_and_store_nothing() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let anonymous = app
            .request(Method::POST, "/v1/traces")
            .header("content-type", "application/json")
            .body(plain_export(1))
            .send()
            .await;
        assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);

        let unknown = post_traces(
            &app,
            "sk-llmobs-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "application/json",
            plain_export(1),
        )
        .send()
        .await;
        assert_eq!(unknown.status, StatusCode::UNAUTHORIZED);

        for key in [&seeded.read_key, &seeded.public_key] {
            let response = post_traces(&app, key, "application/json", plain_export(1))
                .send()
                .await;
            assert_eq!(response.status, StatusCode::FORBIDDEN);
        }

        let session = app
            .request(Method::POST, "/v1/traces")
            .header("cookie", &seeded.cookie)
            .header("content-type", "application/json")
            .body(plain_export(1))
            .send()
            .await;
        assert_eq!(session.status, StatusCode::FORBIDDEN);
        assert_eq!(stored_spans(&app, seeded.project.id).await, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_used_key_of_a_deleting_project_is_not_found() {
        let app = TestApp::new().await;
        let seeded: Seeded = app.seed().await;
        let first = post_traces(&app, &seeded.ingest_key, "application/json", "{}")
            .send()
            .await;
        assert_eq!(first.status, StatusCode::OK);
        app.state
            .meta
            .write(|conn| {
                Box::pin(async move {
                    sqlx::query("UPDATE projects SET state = 'deleting'")
                        .execute(&mut *conn)
                        .await?;
                    Ok(())
                })
            })
            .await
            .unwrap();
        let second = post_traces(&app, &seeded.ingest_key, "application/json", "{}")
            .send()
            .await;
        assert_eq!(second.status, StatusCode::NOT_FOUND);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_full_budget_answers_503_until_the_held_request_is_done() {
        let app = TestApp::with_config(|config| {
            config.ingest.max_request_bytes = ByteSize::kib(64);
            config.ingest.max_inflight_bytes = ByteSize::kib(64);
        })
        .await;
        let seeded = app.seed().await;
        let held_body = large_attribute_export(40 * 1024);
        let reservation = app
            .state
            .ingest
            .reserve(Some(held_body.len() as u64))
            .expect("the budget is free");
        let held = app
            .state
            .ingest
            .accept(
                Bytes::from(held_body),
                reservation,
                BodyFormat::Json,
                BodyEncoding::Identity,
                &seeded.project,
                app.state.clock.now_ns(),
            )
            .await
            .expect("the held request is accepted");

        let body = large_attribute_export(40 * 1024);
        let busy = post_traces(&app, &seeded.ingest_key, "application/json", body.clone())
            .send()
            .await;
        assert_eq!(busy.status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(busy.header("retry-after"), Some("1"));
        assert_eq!(busy.problem().code, ProblemCode::Busy);
        assert_eq!(stored_spans(&app, seeded.project.id).await, 0);

        drop(held);
        let retried = post_traces(&app, &seeded.ingest_key, "application/json", body)
            .send()
            .await;
        assert_eq!(retried.status, StatusCode::OK);
        assert_eq!(stored_spans(&app, seeded.project.id).await, 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_reserved_environment_is_rewritten_and_warned() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let resource = "\"resource\":{\"attributes\":[{\"key\":\"deployment.environment.name\",\"value\":{\"stringValue\":\"llmobs-eval\"}}]},";
        let body = export_json(resource, &[span_json(0, "")]);
        let response = post_traces(&app, &seeded.ingest_key, "application/json", body)
            .send()
            .await;
        assert_eq!(response.status, StatusCode::OK);
        let rows = stored_rows(&app, seeded.project.id).await;
        assert!(rows.contains("llmobs-eval-external"), "{rows}");
        assert!(rows.contains("reserved_environment"), "{rows}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_accepted_log_names_the_route_and_leaves_out_secrets() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let extra = format!(
            ",\"attributes\":[{{\"key\":\"secret\",\"value\":{{\"stringValue\":\"{ATTRIBUTE_VALUE}\"}}}}]"
        );
        let body = export_json("", &[span_json(0, &extra), span_json(1, "")]);
        let (response, logs) =
            capture_logs(post_traces(&app, &seeded.ingest_key, "application/json", body).send())
                .await;
        assert_eq!(response.status, StatusCode::OK);
        assert!(logs.contains("/v1/traces"), "{logs}");
        assert!(logs.contains(&seeded.project.id.to_string()), "{logs}");
        assert!(logs.contains("spans=2"), "{logs}");
        assert!(!logs.contains(&seeded.ingest_key), "{logs}");
        assert!(!logs.contains(ATTRIBUTE_VALUE), "{logs}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refusals_log_a_reason_and_leave_out_the_body() {
        let app = TestApp::with_config(|config| {
            config.ingest.max_spans_per_request = 1;
        })
        .await;
        let seeded = app.seed().await;
        let (response, logs) = capture_logs(
            post_traces(
                &app,
                &seeded.ingest_key,
                "application/json",
                export_json(
                    "",
                    &[
                        span_json(0, ""),
                        span_json(1, &format!(",\"attributes\":[{{\"key\":\"k\",\"value\":{{\"stringValue\":\"{MARKER}\"}}}}]")),
                    ],
                ),
            )
            .send(),
        )
        .await;
        assert_eq!(response.status, StatusCode::PAYLOAD_TOO_LARGE);
        assert!(logs.contains("too_many_spans"), "{logs}");
        assert!(!logs.contains(MARKER), "{logs}");
        assert!(!logs.contains(&seeded.ingest_key), "{logs}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_accepted_export_is_published_on_the_live_bus() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let mut events = app.state.live.subscribe();
        let response = post_traces(
            &app,
            &seeded.ingest_key,
            "application/json",
            plain_export(2),
        )
        .send()
        .await;
        assert_eq!(response.status, StatusCode::OK);
        let event = tokio::time::timeout(Duration::from_secs(5), events.recv())
            .await
            .expect("an event arrives")
            .expect("the bus is open");
        assert_eq!(event.project_id, seeded.project.id);
        assert_eq!(
            event.spans.len(),
            stored_spans(&app, seeded.project.id).await
        );
        assert_eq!(event.spans.len(), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn accepted_spans_and_request_bytes_are_counted_for_the_day() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let body = export_json("", &[span_json(0, ""), span_json(1, "")]);
        let len = i64::try_from(body.len()).unwrap();
        let response = post_traces(&app, &seeded.ingest_key, "application/json", body)
            .send()
            .await;
        assert_eq!(response.status, StatusCode::OK);
        app.state
            .usage
            .flush(&app.state.meta)
            .await
            .expect("usage flushes");
        let counted: (i64, i64, i64) = sqlx::query_as(
            "SELECT spans, bytes, scores FROM usage_daily WHERE project_id = ? AND day = '2026-01-01'",
        )
        .bind(seeded.project.id.to_string())
        .fetch_one(app.state.meta.reader())
        .await
        .expect("one usage row for the day");
        assert_eq!(counted, (2, len, 0));
    }

    fn fixture_bytes(relative: &str) -> Vec<u8> {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/otlp")
            .join(relative);
        std::fs::read(&path).expect("fixture is readable")
    }
}
