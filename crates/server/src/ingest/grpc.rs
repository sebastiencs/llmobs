//! OTLP/gRPC `TraceService/Export` (spec 02 § Endpoints, D-G-29).
//!
//! The credential is the `authorization` metadata. Only a project key with `ingest` is
//! admitted, on its own project, as on the HTTP route (D-G-15). Refusals use the static
//! messages of design § gRPC status mapping; nothing from the request is logged.

use std::fmt;
use std::time::Instant;

use llmobs_storage::error::{AppendError, MediaError};
use opentelemetry_proto::tonic::collector::trace::v1::trace_service_server::{
    TraceService, TraceServiceServer,
};
use opentelemetry_proto::tonic::collector::trace::v1::{
    ExportTracePartialSuccess, ExportTraceServiceRequest, ExportTraceServiceResponse,
};
use prost::Message;
use tonic::codec::CompressionEncoding;

use super::commit_in_task;
use super::otlp::partial_success;
use super::{Accepted, CommitTarget, IngestError};
use crate::auth::principal::{Principal, bearer_principal};
use crate::auth::roles::KeyScope;
use crate::http::AppState;
use crate::http::access::key_only_project;
use crate::http::error::ApiError;

/// The tonic service. Cloning shares the state.
#[derive(Clone)]
pub(crate) struct GrpcTraces {
    pub(crate) state: AppState,
}

impl fmt::Debug for GrpcTraces {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("GrpcTraces").finish_non_exhaustive()
    }
}

#[tonic::async_trait]
impl TraceService for GrpcTraces {
    async fn export(
        &self,
        request: tonic::Request<ExportTraceServiceRequest>,
    ) -> Result<tonic::Response<ExportTraceServiceResponse>, tonic::Status> {
        let started = Instant::now();
        match export_request(&self.state, request, started).await {
            Ok(response) => Ok(tonic::Response::new(response)),
            Err(status) => {
                tracing::warn!(
                    target: "ingest",
                    transport = "grpc",
                    code = ?status.code(),
                    duration_ms = elapsed_ms(started),
                    reason = status.message(),
                    "request refused"
                );
                Err(status)
            }
        }
    }
}

/// The service with its limits and compressions. The decoded message cap is
/// `ingest.max_grpc_message_bytes` (validated by `config`).
pub(crate) fn service(state: AppState) -> TraceServiceServer<GrpcTraces> {
    let limit =
        usize::try_from(state.config.ingest.max_grpc_message_bytes.as_u64()).unwrap_or(usize::MAX);
    TraceServiceServer::new(GrpcTraces { state })
        .max_decoding_message_size(limit)
        .accept_compressed(CompressionEncoding::Gzip)
}

/// Steps of the `export` contract: authenticate, check the project, reserve, accept,
/// commit, answer. The first failing step decides the status.
async fn export_request(
    state: &AppState,
    request: tonic::Request<ExportTraceServiceRequest>,
    started: Instant,
) -> Result<ExportTraceServiceResponse, tonic::Status> {
    let now = state.clock.now_ns();
    let headers = request.metadata().clone().into_headers();
    let principal = bearer_principal(state, &headers)
        .await
        .map_err(|error| api_status(&error))?;
    let Principal::ApiKey {
        project_id, scopes, ..
    } = principal
    else {
        return Err(denied());
    };
    if !scopes.contains(KeyScope::Ingest) {
        return Err(denied());
    }
    let project = key_only_project(state, project_id)
        .await
        .map_err(|error| api_status(&error))?;
    let message = request.into_inner();
    let bytes = u64::try_from(message.encoded_len()).unwrap_or(u64::MAX);
    let reservation = state.ingest.reserve_exact(bytes)?;
    let Accepted {
        normalized,
        rewritten,
        reservation,
        ..
    } = state
        .ingest
        .accept_request(message, reservation, bytes, bytes, &project, now)
        .await?;
    let rejected = normalized.rejections.total();
    let partial = partial_success(&normalized.rejections).map(|(rejected_spans, error_message)| {
        ExportTracePartialSuccess {
            rejected_spans,
            error_message,
        }
    });
    let target = CommitTarget {
        org_id: project.org_id,
        project_id: project.id,
        request_bytes: bytes,
        now_ns: now,
    };
    let receipt = commit_in_task(state, target, normalized, reservation).await?;
    tracing::info!(
        target: "ingest",
        transport = "grpc",
        project_id = %project.id,
        request_bytes = bytes,
        spans = receipt.accepted,
        rejected,
        media = receipt.media,
        rewritten,
        duration_ms = elapsed_ms(started),
        "request accepted"
    );
    Ok(ExportTraceServiceResponse {
        partial_success: partial,
    })
}

fn denied() -> tonic::Status {
    tonic::Status::permission_denied("credentials do not allow ingest")
}

/// The status of an `ApiError` from the credential or the project check.
fn api_status(error: &ApiError) -> tonic::Status {
    match error {
        ApiError::Unauthorized => tonic::Status::unauthenticated("missing or invalid credentials"),
        ApiError::Forbidden { .. } => denied(),
        ApiError::NotFound => tonic::Status::not_found("project not found"),
        _ => tonic::Status::internal("internal error"),
    }
}

/// The status of a failed ingest step (design § gRPC status mapping).
impl From<IngestError> for tonic::Status {
    fn from(error: IngestError) -> Self {
        match error {
            IngestError::TooManySpans => {
                tonic::Status::invalid_argument("too many spans in one request")
            }
            IngestError::Media(MediaError::Invalid { .. }) => {
                tonic::Status::invalid_argument("invalid media in the request")
            }
            IngestError::Busy
            | IngestError::Append(
                AppendError::Backpressure
                | AppendError::WalFailed
                | AppendError::ShuttingDown
                | AppendError::DiskFull,
            )
            | IngestError::Media(
                MediaError::DiskFull
                | MediaError::Object(_)
                | MediaError::Catalog(_)
                | MediaError::Query(_),
            ) => tonic::Status::unavailable("server is busy, retry later"),
            IngestError::Append(AppendError::ProjectDropped)
            | IngestError::Media(MediaError::ProjectDropped) => {
                tonic::Status::not_found("project not found")
            }
            IngestError::Body(_)
            | IngestError::Decode(_)
            | IngestError::Media(
                MediaError::HashMismatch
                | MediaError::NotFound
                | MediaError::InvalidRange
                | MediaError::Fault,
            )
            | IngestError::Append(AppendError::InvalidBatch { .. })
            | IngestError::Batch(_)
            | IngestError::MediaHash
            | IngestError::Join(_) => tonic::Status::internal("internal error"),
        }
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use datafusion::prelude::SessionContext;
    use llmobs_otel::decode_json;
    use llmobs_storage::QueryOptions;
    use opentelemetry_proto::tonic::collector::trace::v1::trace_service_server::TraceService;
    use tonic::Code;

    use super::*;
    use crate::test_support::{Seeded, TestApp};

    const TRACES: &str =
        include_str!("../../../../fixtures/otlp/genai/finish-reasons-fill/input.json");

    fn traces() -> ExportTraceServiceRequest {
        decode_json(TRACES.as_bytes()).expect("fixture decodes")
    }

    fn span_count(request: &ExportTraceServiceRequest) -> usize {
        request
            .resource_spans
            .iter()
            .flat_map(|resource| resource.scope_spans.iter())
            .map(|scope| scope.spans.len())
            .sum()
    }

    fn with_key(
        message: ExportTraceServiceRequest,
        key: &str,
    ) -> tonic::Request<ExportTraceServiceRequest> {
        let mut request = tonic::Request::new(message);
        request.metadata_mut().insert(
            "authorization",
            format!("Bearer {key}").parse().expect("ASCII token"),
        );
        request
    }

    /// Spans in the project, read through the storage query provider.
    async fn stored_spans(app: &TestApp, project: llmobs_core::ids::ProjectId) -> usize {
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

    fn assert_status(status: &tonic::Status, code: Code, message: &str) {
        assert_eq!(status.code(), code, "{status:?}");
        assert_eq!(status.message(), message);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_ingest_key_exports_and_is_counted() {
        let app = TestApp::new().await;
        let seeded: Seeded = app.seed().await;
        let message = traces();
        let spans = span_count(&message);
        let bytes = u64::try_from(message.encoded_len()).unwrap_or(u64::MAX);
        let service = GrpcTraces {
            state: app.state.clone(),
        };
        let response = service
            .export(with_key(message, &seeded.ingest_key))
            .await
            .expect("exported");
        assert_eq!(response.into_inner().partial_success, None);
        assert_eq!(stored_spans(&app, seeded.project.id).await, spans);

        app.state
            .usage
            .flush(&app.state.meta)
            .await
            .expect("usage flushes");
        let counted: (i64, i64) =
            sqlx::query_as("SELECT spans, bytes FROM usage_daily WHERE project_id = ?")
                .bind(seeded.project.id.to_string())
                .fetch_one(app.state.meta.reader())
                .await
                .expect("one usage row for the day");
        assert_eq!(
            counted,
            (
                i64::try_from(spans).unwrap_or(i64::MAX),
                i64::try_from(bytes).unwrap_or(i64::MAX)
            )
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refused_credentials_get_their_static_status_and_store_nothing() {
        let app = TestApp::new().await;
        let seeded: Seeded = app.seed().await;
        let service = GrpcTraces {
            state: app.state.clone(),
        };

        let anonymous = service
            .export(tonic::Request::new(traces()))
            .await
            .expect_err("no credentials");
        assert_status(
            &anonymous,
            Code::Unauthenticated,
            "missing or invalid credentials",
        );

        let read = service
            .export(with_key(traces(), &seeded.read_key))
            .await
            .expect_err("a read key cannot ingest");
        assert_status(
            &read,
            Code::PermissionDenied,
            "credentials do not allow ingest",
        );

        let mut cookied = tonic::Request::new(traces());
        cookied
            .metadata_mut()
            .insert("cookie", seeded.cookie.parse().expect("ASCII cookie"));
        let session = service
            .export(cookied)
            .await
            .expect_err("a session cookie is not a credential here");
        assert_status(
            &session,
            Code::Unauthenticated,
            "missing or invalid credentials",
        );

        assert_eq!(stored_spans(&app, seeded.project.id).await, 0);
    }
}
