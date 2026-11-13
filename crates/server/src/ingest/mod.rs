//! OTLP/HTTP ingest (spec 02 § Endpoints, D-G-2 to D-G-9).
#![cfg_attr(not(test), allow(dead_code))] // no non-test caller until the route lands (G1.7)

pub(crate) mod body;
pub(crate) mod environment;
#[cfg(test)]
mod fixtures_tests;
pub(crate) mod otlp;

use std::sync::{Arc, PoisonError, RwLock};

use bytes::Bytes;
use futures::{StreamExt, stream};
use llmobs_core::batch::spans_to_record_batch;
use llmobs_core::error::CoreError;
use llmobs_core::ids::ProjectId;
use llmobs_core::media::MediaBlob as CoreBlob;
use llmobs_otel::{
    DecodeError, IngestContext, IngestLimits, Normalized, PriceTable, decode_json, decode_protobuf,
    normalize,
};
use llmobs_storage::error::{AppendError, MediaError};
use llmobs_storage::{MediaBlob, Storage};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::config::{IngestConfig, MediaConfig};
use crate::http::error::ApiError;
use crate::meta::projects::ProjectRow;
use body::{BodyEncoding, BodyError, BodyFormat, DecompressError, decompress};
use environment::rewrite_reserved_environment;

/// Media puts in flight per request (D-G-9).
const MEDIA_CONCURRENCY: usize = 4;

/// Ingest state shared by every request (D-G-1). Holds no `AppState`.
pub(crate) struct Ingest {
    prices: RwLock<Arc<PriceTable>>,
    budget: Arc<Semaphore>,
    max_request_bytes: usize,
    max_spans_per_request: usize,
    limits: IngestLimits,
}

/// Budget held by one request from before its body is read until it is answered.
#[derive(Debug)]
pub(crate) struct Reservation(OwnedSemaphorePermit);

/// What `accept` produced. Holds the request's reservation until dropped.
#[derive(Debug)]
pub(crate) struct Accepted {
    pub(crate) normalized: Normalized,
    /// Raw body length, for logs.
    pub(crate) request_bytes: u64,
    pub(crate) decompressed_bytes: u64,
    /// Spans whose environment was rewritten (D-G-7).
    pub(crate) rewritten: usize,
    pub(crate) reservation: Reservation,
}

/// What `commit` stored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CommitReceipt {
    /// Spans appended.
    pub(crate) accepted: usize,
    /// Blobs stored (including ones that already existed).
    pub(crate) media: usize,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum IngestError {
    #[error("{0:?}")]
    Body(BodyError),
    #[error("too many spans in one request")]
    TooManySpans,
    #[error("invalid OTLP payload")]
    Decode(#[source] DecodeError),
    #[error("ingest budget is full")]
    Busy,
    #[error("storage refused the append")]
    Append(#[source] AppendError),
    #[error("storage refused a media blob")]
    Media(#[source] MediaError),
    #[error("could not build the span batch")]
    Batch(#[source] CoreError),
    #[error("normalized media has an invalid hash")]
    MediaHash,
    #[error("ingest task failed")]
    Join(#[source] tokio::task::JoinError),
}

impl Ingest {
    pub(crate) fn new(config: &IngestConfig, media: &MediaConfig, prices: PriceTable) -> Self {
        // Sizes were checked by `config::validate_ingest`; a value that does not fit is clipped.
        let max_request_bytes =
            usize::try_from(config.max_request_bytes.as_u64()).unwrap_or(usize::MAX);
        let max_inflight =
            usize::try_from(config.max_inflight_bytes.as_u64()).unwrap_or(usize::MAX);
        Self {
            prices: RwLock::new(Arc::new(prices)),
            budget: Arc::new(Semaphore::new(max_inflight.min(Semaphore::MAX_PERMITS))),
            max_request_bytes,
            max_spans_per_request: usize::try_from(config.max_spans_per_request)
                .unwrap_or(usize::MAX),
            limits: config.limits(media),
        }
    }

    /// Swaps the price table (D-G-10). `#[cfg(test)]` in M1: only tests call it.
    /// The track that adds custom prices removes the `cfg`.
    #[cfg(test)]
    pub(crate) fn replace_prices(&self, prices: PriceTable) {
        *self.prices.write().unwrap_or_else(PoisonError::into_inner) = Arc::new(prices);
    }

    pub(crate) fn max_request_bytes(&self) -> usize {
        self.max_request_bytes
    }

    /// Reserves budget before the body is read (D-G-5). Never waits.
    ///
    /// The amount is `Content-Length` when it is at most `max_request_bytes`, otherwise
    /// `max_request_bytes` (a chunked body, or one the read will refuse anyway).
    pub(crate) fn reserve(&self, content_length: Option<u64>) -> Result<Reservation, IngestError> {
        let amount = content_length
            .and_then(|length| usize::try_from(length).ok())
            .filter(|length| *length <= self.max_request_bytes)
            .unwrap_or(self.max_request_bytes)
            .max(1);
        let amount = u32::try_from(amount).map_err(|_| IngestError::Busy)?;
        self.budget
            .clone()
            .try_acquire_many_owned(amount)
            .map(Reservation)
            .map_err(|_| IngestError::Busy)
    }

    /// Decompresses, decodes, counts, normalizes and rewrites one OTLP request (D-G-2, D-G-7).
    ///
    /// `reservation` covers the raw body; decompressed chunks are reserved as they are
    /// produced. The result keeps the reservation until it is dropped.
    pub(crate) async fn accept(
        &self,
        raw: Bytes,
        mut reservation: Reservation,
        format: BodyFormat,
        encoding: BodyEncoding,
        project: &ProjectRow,
        now_ns: i64,
    ) -> Result<Accepted, IngestError> {
        let request_bytes = raw.len();
        // Give back the part of a chunked reservation that the body does not use, and cover
        // any part of the body the reservation does not.
        let held = reservation.0.num_permits();
        if held > request_bytes {
            drop(reservation.0.split(held - request_bytes));
        } else if held < request_bytes {
            let missing = u32::try_from(request_bytes - held).map_err(|_| IngestError::Busy)?;
            let extra = self
                .budget
                .clone()
                .try_acquire_many_owned(missing)
                .map_err(|_| IngestError::Busy)?;
            reservation.0.merge(extra);
        }
        let prices: Arc<PriceTable> = self
            .prices
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let ctx = IngestContext {
            project_id: project.id,
            ingest_time: now_ns,
            retention_days: project.retention_days,
            limits: self.limits,
        };
        let max_bytes = self.max_request_bytes;
        let max_spans = self.max_spans_per_request;
        let semaphore = Arc::clone(&self.budget);
        let work = tokio::task::spawn_blocking(move || -> Result<_, IngestError> {
            let decompressed = {
                let mut reserve = |bytes: usize| -> bool {
                    let Ok(count) = u32::try_from(bytes) else {
                        return false;
                    };
                    match Arc::clone(&semaphore).try_acquire_many_owned(count) {
                        Ok(extra) => {
                            reservation.0.merge(extra);
                            true
                        }
                        Err(_) => false,
                    }
                };
                decompress(raw, encoding, max_bytes, &mut reserve).map_err(|error| match error {
                    DecompressError::Body(body) => IngestError::Body(body),
                    DecompressError::Budget => IngestError::Busy,
                })?
            };
            let decompressed_bytes = decompressed.len();
            let request = match format {
                BodyFormat::Protobuf => decode_protobuf(&decompressed),
                BodyFormat::Json => decode_json(&decompressed),
            }
            .map_err(IngestError::Decode)?;
            let spans = request
                .resource_spans
                .iter()
                .flat_map(|resource| resource.scope_spans.iter())
                .fold(0_usize, |total, scope| {
                    total.saturating_add(scope.spans.len())
                });
            if spans > max_spans {
                return Err(IngestError::TooManySpans);
            }
            let mut normalized = normalize(&request, &ctx, &prices);
            let rewritten = rewrite_reserved_environment(&mut normalized.spans);
            Ok((normalized, decompressed_bytes, rewritten, reservation))
        });
        let (normalized, decompressed_bytes, rewritten, reservation) = match work.await {
            Ok(result) => result?,
            Err(error) => return Err(IngestError::Join(error)),
        };
        Ok(Accepted {
            normalized,
            request_bytes: u64::try_from(request_bytes).unwrap_or(u64::MAX),
            decompressed_bytes: u64::try_from(decompressed_bytes).unwrap_or(u64::MAX),
            rewritten,
            reservation,
        })
    }

    /// Stores the media, then appends the spans (D-G-8, D-G-9). Nothing is appended when a
    /// blob fails. The caller's reservation covers the spans until this returns.
    pub(crate) async fn commit(
        &self,
        storage: &Storage,
        project_id: ProjectId,
        normalized: Normalized,
    ) -> Result<CommitReceipt, IngestError> {
        let Normalized { spans, media, .. } = normalized;
        let blobs = media
            .into_iter()
            .map(store_blob)
            .collect::<Result<Vec<_>, _>>()?;
        let mut stored = 0_usize;
        let mut puts = stream::iter(blobs)
            .map(|blob| storage.put_media(project_id, blob))
            .buffer_unordered(MEDIA_CONCURRENCY);
        while let Some(result) = puts.next().await {
            result.map_err(IngestError::Media)?;
            stored += 1;
        }
        if spans.is_empty() {
            return Ok(CommitReceipt {
                accepted: 0,
                media: stored,
            });
        }
        let accepted = spans.len();
        let batch = tokio::task::spawn_blocking(move || spans_to_record_batch(&spans))
            .await
            .map_err(IngestError::Join)?
            .map_err(IngestError::Batch)?;
        storage
            .append_spans(project_id, batch)
            .await
            .map_err(IngestError::Append)?;
        Ok(CommitReceipt {
            accepted,
            media: stored,
        })
    }
}

/// Converts a normalized blob to the storage type, checking that its hash is 32 bytes.
fn store_blob(blob: CoreBlob) -> Result<MediaBlob, IngestError> {
    let digest = hex::decode(&blob.sha256).map_err(|_| IngestError::MediaHash)?;
    let sha256 = <[u8; 32]>::try_from(digest.as_slice()).map_err(|_| IngestError::MediaHash)?;
    Ok(MediaBlob {
        sha256,
        mime_type: blob.mime_type,
        bytes: Bytes::from(blob.bytes),
    })
}

fn bad_request(detail: &str) -> ApiError {
    ApiError::BadRequest {
        detail: Some(detail.to_owned()),
        errors: Vec::new(),
    }
}

/// The mapping of spec 02 § Endpoints and the design's error table.
impl From<IngestError> for ApiError {
    fn from(error: IngestError) -> Self {
        match error {
            IngestError::Body(BodyError::UnsupportedType) => ApiError::UnsupportedMediaType {
                detail: "expected Content-Type application/x-protobuf or application/json",
            },
            IngestError::Body(BodyError::UnsupportedEncoding) => ApiError::UnsupportedMediaType {
                detail: "unsupported Content-Encoding",
            },
            IngestError::Body(BodyError::TooLarge) => ApiError::PayloadTooLarge {
                detail: "request body is too large",
            },
            IngestError::TooManySpans => ApiError::PayloadTooLarge {
                detail: "too many spans in one request",
            },
            IngestError::Body(BodyError::Corrupt) => bad_request("invalid compressed body"),
            IngestError::Body(BodyError::Read) => bad_request("could not read the request body"),
            IngestError::Decode(error) => bad_request(&error.to_string()),
            IngestError::Media(MediaError::Invalid { .. }) => {
                bad_request("invalid media in the request")
            }
            IngestError::Busy | IngestError::Append(AppendError::Backpressure) => {
                ApiError::Unavailable {
                    detail: "server is busy, retry later",
                }
            }
            IngestError::Append(AppendError::WalFailed)
            | IngestError::Media(
                MediaError::Object(_) | MediaError::Catalog(_) | MediaError::Query(_),
            ) => ApiError::Unavailable {
                detail: "storage is unavailable",
            },
            IngestError::Append(AppendError::DiskFull)
            | IngestError::Media(MediaError::DiskFull) => ApiError::Unavailable {
                detail: "storage is out of disk space",
            },
            IngestError::Append(AppendError::ShuttingDown) => ApiError::Unavailable {
                detail: "server is shutting down",
            },
            IngestError::Append(AppendError::ProjectDropped)
            | IngestError::Media(MediaError::ProjectDropped) => ApiError::NotFound,
            error @ (IngestError::Media(
                MediaError::HashMismatch
                | MediaError::NotFound
                | MediaError::InvalidRange
                | MediaError::Fault,
            )
            | IngestError::MediaHash
            | IngestError::Append(AppendError::InvalidBatch { .. })
            | IngestError::Batch(_)
            | IngestError::Join(_)) => ApiError::Internal(Box::new(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::path::Path;

    use axum::http::{StatusCode, header};
    use axum::response::IntoResponse;
    use bytesize::ByteSize;
    use llmobs_core::ids::ProjectId;
    use llmobs_core::media::MediaBlob as CoreBlob;
    use llmobs_otel::{ExportTraceServiceRequest, Rejections, builtin_price_rows, decode_json};
    use llmobs_storage::QueryOptions;
    use llmobs_storage::error::ObjectError;
    use prost::Message;
    use sha2::{Digest, Sha256};

    use super::*;
    use crate::meta::projects::ProjectRow;
    use crate::test_support::{Seeded, TestApp};

    fn fixture(relative: &str) -> Vec<u8> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/otlp")
            .join(relative);
        std::fs::read(&path).expect("fixture is readable")
    }

    fn builtin_prices() -> PriceTable {
        PriceTable::new(builtin_price_rows()).expect("built-in price table")
    }

    fn span_count(request: &ExportTraceServiceRequest) -> usize {
        request
            .resource_spans
            .iter()
            .flat_map(|resource| resource.scope_spans.iter())
            .map(|scope| scope.spans.len())
            .sum()
    }

    fn encode(body: &[u8], encoding: BodyEncoding) -> Vec<u8> {
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

    /// `normalize` with the context `accept` builds for `project` at `now`.
    fn expected(
        app: &TestApp,
        project: &ProjectRow,
        now: i64,
        request: &ExportTraceServiceRequest,
        prices: &PriceTable,
    ) -> Normalized {
        let ctx = IngestContext {
            project_id: project.id,
            ingest_time: now,
            retention_days: project.retention_days,
            limits: app.state.config.ingest.limits(&app.state.config.media),
        };
        normalize(request, &ctx, prices)
    }

    /// Reserves for `body` and runs `accept` on it.
    async fn accept_body(
        app: &TestApp,
        project: &ProjectRow,
        body: Vec<u8>,
        format: BodyFormat,
        encoding: BodyEncoding,
    ) -> Result<Accepted, IngestError> {
        let reservation = app.state.ingest.reserve(Some(body.len() as u64))?;
        app.state
            .ingest
            .accept(
                Bytes::from(body),
                reservation,
                format,
                encoding,
                project,
                app.state.clock.now_ns(),
            )
            .await
    }

    /// `accept` on the fixture's OTLP/JSON body, then the normalized result.
    async fn accepted_fixture(app: &TestApp, project: &ProjectRow, relative: &str) -> Normalized {
        let body = fixture(relative);
        accept_body(app, project, body, BodyFormat::Json, BodyEncoding::Identity)
            .await
            .expect("fixture is accepted")
            .normalized
    }

    /// Spans in the project, read through the storage query provider.
    async fn stored_spans(app: &TestApp, project: ProjectId) -> usize {
        let query = app
            .state
            .storage
            .query(project, QueryOptions::default())
            .await
            .expect("query opens");
        let ctx = datafusion::prelude::SessionContext::new();
        ctx.register_table("raw_spans", query.spans)
            .expect("spans register");
        ctx.table("raw_spans")
            .await
            .expect("spans table")
            .count()
            .await
            .expect("count")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn accept_returns_what_normalize_returns_for_json_and_protobuf() {
        let app = TestApp::new().await;
        let seeded: Seeded = app.seed().await;
        let now = app.state.clock.now_ns();
        let json = fixture("genai/openai-v2-chat-tool/input.json");
        let request = decode_json(&json).expect("fixture decodes");
        let want = expected(&app, &seeded.project, now, &request, &builtin_prices());
        let cases = [
            (BodyFormat::Json, json.clone()),
            (BodyFormat::Protobuf, request.encode_to_vec()),
        ];
        for (format, body) in cases {
            let body_len = body.len();
            let accepted = accept_body(&app, &seeded.project, body, format, BodyEncoding::Identity)
                .await
                .expect("accepted");
            assert_eq!(accepted.normalized, want, "{format:?}");
            assert_eq!(accepted.rewritten, 0);
            assert_eq!(accepted.request_bytes, u64::try_from(body_len).unwrap_or(0));
            assert_eq!(accepted.decompressed_bytes, accepted.request_bytes);
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn every_content_encoding_decodes_to_the_same_result() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let now = app.state.clock.now_ns();
        let json = fixture("genai/openai-v2-chat-tool/input.json");
        let request = decode_json(&json).expect("fixture decodes");
        let want = expected(&app, &seeded.project, now, &request, &builtin_prices());
        let protobuf = request.encode_to_vec();
        let encodings = [
            BodyEncoding::Identity,
            BodyEncoding::Gzip,
            BodyEncoding::Deflate,
            BodyEncoding::Zstd,
        ];
        for (format, plain) in [(BodyFormat::Json, &json), (BodyFormat::Protobuf, &protobuf)] {
            for encoding in encodings {
                let body = encode(plain, encoding);
                let compressed_len = body.len();
                let accepted = accept_body(&app, &seeded.project, body, format, encoding)
                    .await
                    .expect("accepted");
                assert_eq!(accepted.normalized, want, "{format:?} {encoding:?}");
                assert_eq!(
                    accepted.request_bytes,
                    u64::try_from(compressed_len).unwrap_or(0)
                );
                assert_eq!(
                    accepted.decompressed_bytes,
                    u64::try_from(plain.len()).unwrap_or(0)
                );
            }
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn too_many_spans_are_refused_before_normalizing() {
        let body = fixture("llmobs/input-output-formats/input.json");
        let spans = span_count(&decode_json(&body).expect("fixture decodes"));
        assert!(spans >= 3, "the fixture has {spans} spans");
        let app = TestApp::with_config(|config| config.ingest.max_spans_per_request = 2).await;
        let seeded = app.seed().await;
        let error = accept_body(
            &app,
            &seeded.project,
            body,
            BodyFormat::Json,
            BodyEncoding::Identity,
        )
        .await
        .expect_err("over the span limit");
        assert!(matches!(error, IngestError::TooManySpans), "{error:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reserve_refuses_while_the_budget_is_held() {
        let app = TestApp::with_config(|config| {
            config.ingest.max_request_bytes = ByteSize::kib(1);
            config.ingest.max_inflight_bytes = ByteSize::kib(1);
        })
        .await;
        let ingest = &app.state.ingest;
        let held = ingest.reserve(Some(600)).expect("the first request fits");
        assert!(matches!(ingest.reserve(Some(600)), Err(IngestError::Busy)));
        drop(held);
        ingest
            .reserve(Some(600))
            .expect("fits again after the release");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn accept_gives_back_the_unused_part_of_a_chunked_reservation() {
        let app = TestApp::with_config(|config| {
            config.ingest.max_request_bytes = ByteSize::kib(1);
            config.ingest.max_inflight_bytes = ByteSize::kib(1);
        })
        .await;
        let seeded = app.seed().await;
        let ingest = &app.state.ingest;
        assert_eq!(ingest.max_request_bytes(), 1024);
        // No Content-Length: the whole budget is reserved.
        let reservation = ingest.reserve(None).expect("the whole budget");
        assert_eq!(ingest.budget.available_permits(), 0);
        let body = format!("{:<20}", "{}");
        let accepted = ingest
            .accept(
                Bytes::from(body),
                reservation,
                BodyFormat::Json,
                BodyEncoding::Identity,
                &seeded.project,
                app.state.clock.now_ns(),
            )
            .await
            .expect("an empty request is accepted");
        assert_eq!(ingest.budget.available_permits(), 1024 - 20);
        assert!(matches!(ingest.reserve(Some(1005)), Err(IngestError::Busy)));
        drop(accepted.reservation);
        assert_eq!(ingest.budget.available_permits(), 1024);
        ingest
            .reserve(Some(1000))
            .expect("the whole budget is free again");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn decompression_reserves_its_output_against_the_budget() {
        let app = TestApp::with_config(|config| {
            config.ingest.max_request_bytes = ByteSize::kib(8);
            config.ingest.max_inflight_bytes = ByteSize::kib(2);
        })
        .await;
        let seeded = app.seed().await;
        // Small on the wire, 4 KiB once decompressed: above the 2 KiB budget.
        let compressed = encode(&vec![b' '; 4096], BodyEncoding::Gzip);
        assert!(compressed.len() < 100, "{} bytes", compressed.len());
        let error = accept_body(
            &app,
            &seeded.project,
            compressed,
            BodyFormat::Json,
            BodyEncoding::Gzip,
        )
        .await
        .expect_err("the output does not fit the budget");
        assert!(matches!(error, IngestError::Busy), "{error:?}");
        assert_eq!(app.state.ingest.budget.available_permits(), 2048);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn replace_prices_changes_what_accept_uses() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let now = app.state.clock.now_ns();
        let body = fixture("edge/cost-embedding/input.json");
        let request = decode_json(&body).expect("fixture decodes");
        let priced = expected(&app, &seeded.project, now, &request, &builtin_prices());
        let empty = PriceTable::new(Vec::new()).expect("an empty table compiles");
        let unpriced = expected(&app, &seeded.project, now, &request, &empty);
        assert_ne!(priced, unpriced, "the fixture must have a built-in price");
        app.state
            .ingest
            .replace_prices(PriceTable::new(Vec::new()).expect("an empty table compiles"));
        let accepted = accept_body(
            &app,
            &seeded.project,
            body,
            BodyFormat::Json,
            BodyEncoding::Identity,
        )
        .await
        .expect("accepted");
        assert_eq!(accepted.normalized, unpriced);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn commit_stores_media_and_spans() {
        let app = TestApp::with_config(|config| config.media.min_bytes = ByteSize::b(1)).await;
        let seeded = app.seed().await;
        let normalized =
            accepted_fixture(&app, &seeded.project, "llmobs/sdk-media-bytes/input.json").await;
        let blob = normalized
            .media
            .first()
            .cloned()
            .expect("the fixture has a blob");
        let span_total = normalized.spans.len();
        let receipt = app
            .state
            .ingest
            .commit(&app.state.storage, seeded.project.id, normalized)
            .await
            .expect("committed");
        assert_eq!(
            receipt,
            CommitReceipt {
                accepted: span_total,
                media: 1
            }
        );
        let mut sha = [0_u8; 32];
        sha.copy_from_slice(&hex::decode(&blob.sha256).expect("hex"));
        let read = app
            .state
            .storage
            .get_media(seeded.project.id, &sha, None)
            .await
            .expect("the blob is stored");
        assert_eq!(read.bytes.as_ref(), blob.bytes.as_slice());
        assert_eq!(read.mime_type, blob.mime_type);
        assert_eq!(stored_spans(&app, seeded.project.id).await, span_total);
        // The span row carries the media reference.
        let query = app
            .state
            .storage
            .query(seeded.project.id, QueryOptions::default())
            .await
            .expect("query opens");
        let ctx = datafusion::prelude::SessionContext::new();
        ctx.register_table("raw_spans", query.spans)
            .expect("register");
        let batches = ctx
            .sql("SELECT media_refs FROM raw_spans")
            .await
            .expect("sql")
            .collect()
            .await
            .expect("collect");
        assert!(
            format!("{batches:?}").contains(&blob.sha256),
            "the stored span does not reference the blob"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn commit_refuses_a_blob_whose_hash_does_not_match_and_appends_nothing() {
        let app = TestApp::with_config(|config| config.media.min_bytes = ByteSize::b(1)).await;
        let seeded = app.seed().await;
        let mut normalized =
            accepted_fixture(&app, &seeded.project, "llmobs/sdk-media-bytes/input.json").await;
        normalized.media = vec![CoreBlob {
            sha256: "ab".repeat(32),
            mime_type: "image/png".to_owned(),
            bytes: b"bytes that do not hash to that".to_vec(),
        }];
        let error = app
            .state
            .ingest
            .commit(&app.state.storage, seeded.project.id, normalized)
            .await
            .expect_err("the hash does not match");
        assert!(
            matches!(error, IngestError::Media(MediaError::HashMismatch)),
            "{error:?}"
        );
        assert_eq!(stored_spans(&app, seeded.project.id).await, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn commit_refuses_an_invalid_mime_type_and_appends_nothing() {
        let app = TestApp::with_config(|config| config.media.min_bytes = ByteSize::b(1)).await;
        let seeded = app.seed().await;
        let mut normalized =
            accepted_fixture(&app, &seeded.project, "llmobs/sdk-media-bytes/input.json").await;
        let bytes = b"long mime".to_vec();
        normalized.media = vec![CoreBlob {
            sha256: hex::encode(Sha256::digest(&bytes)),
            mime_type: "x".repeat(300),
            bytes,
        }];
        let error = app
            .state
            .ingest
            .commit(&app.state.storage, seeded.project.id, normalized)
            .await
            .expect_err("the MIME type is too long");
        assert!(
            matches!(error, IngestError::Media(MediaError::Invalid { .. })),
            "{error:?}"
        );
        assert_eq!(stored_spans(&app, seeded.project.id).await, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn commit_with_nothing_to_store_appends_nothing() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let receipt = app
            .state
            .ingest
            .commit(
                &app.state.storage,
                seeded.project.id,
                Normalized {
                    spans: Vec::new(),
                    media: Vec::new(),
                    rejections: Rejections::default(),
                },
            )
            .await
            .expect("nothing to commit is not an error");
        assert_eq!(
            receipt,
            CommitReceipt {
                accepted: 0,
                media: 0
            }
        );
        assert_eq!(stored_spans(&app, seeded.project.id).await, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn commit_after_the_project_is_dropped_is_refused() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let normalized = accepted_fixture(
            &app,
            &seeded.project,
            "genai/openai-v2-chat-tool/input.json",
        )
        .await;
        app.state
            .storage
            .drop_project(seeded.project.id)
            .await
            .expect("the project drops");
        let error = app
            .state
            .ingest
            .commit(&app.state.storage, seeded.project.id, normalized)
            .await
            .expect_err("a dropped project takes no spans");
        assert!(
            matches!(error, IngestError::Append(AppendError::ProjectDropped)),
            "{error:?}"
        );
    }

    /// Status, `Retry-After`, problem `code` and `detail` of the response for `error`.
    async fn problem_of(error: IngestError) -> (StatusCode, Option<String>, serde_json::Value) {
        let response = ApiError::from(error).into_response();
        let status = response.status();
        let retry_after = response
            .headers()
            .get(header::RETRY_AFTER)
            .map(|value| value.to_str().expect("ascii").to_owned());
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let problem = serde_json::from_slice(&body).expect("problem JSON");
        (status, retry_after, problem)
    }

    #[tokio::test]
    async fn error_mapping_matches_the_design_table() {
        // (error, status, code, detail, Retry-After)
        type Row = (
            IngestError,
            u16,
            &'static str,
            Option<&'static str>,
            Option<&'static str>,
        );
        let cases: Vec<Row> = vec![
            (
                IngestError::Body(BodyError::UnsupportedType),
                415,
                "unsupported_media_type",
                Some("expected Content-Type application/x-protobuf or application/json"),
                None,
            ),
            (
                IngestError::Body(BodyError::UnsupportedEncoding),
                415,
                "unsupported_media_type",
                Some("unsupported Content-Encoding"),
                None,
            ),
            (
                IngestError::Body(BodyError::TooLarge),
                413,
                "payload_too_large",
                Some("request body is too large"),
                None,
            ),
            (
                IngestError::TooManySpans,
                413,
                "payload_too_large",
                Some("too many spans in one request"),
                None,
            ),
            (
                IngestError::Body(BodyError::Corrupt),
                400,
                "bad_request",
                Some("invalid compressed body"),
                None,
            ),
            (
                IngestError::Body(BodyError::Read),
                400,
                "bad_request",
                Some("could not read the request body"),
                None,
            ),
            (
                IngestError::Media(MediaError::Invalid { reason: "mime" }),
                400,
                "bad_request",
                Some("invalid media in the request"),
                None,
            ),
            (
                IngestError::Busy,
                503,
                "busy",
                Some("server is busy, retry later"),
                Some("1"),
            ),
            (
                IngestError::Append(AppendError::Backpressure),
                503,
                "busy",
                Some("server is busy, retry later"),
                Some("1"),
            ),
            (
                IngestError::Append(AppendError::WalFailed),
                503,
                "busy",
                Some("storage is unavailable"),
                Some("1"),
            ),
            (
                IngestError::Append(AppendError::ShuttingDown),
                503,
                "busy",
                Some("server is shutting down"),
                Some("1"),
            ),
            (
                IngestError::Media(MediaError::Object(ObjectError::AlreadyExists {
                    path: "media/x".to_owned(),
                })),
                503,
                "busy",
                Some("storage is unavailable"),
                Some("1"),
            ),
            (
                IngestError::Media(MediaError::Catalog(
                    llmobs_storage::error::CatalogError::Corrupt { what: "test" },
                )),
                503,
                "busy",
                Some("storage is unavailable"),
                Some("1"),
            ),
            (
                IngestError::Media(MediaError::Query(
                    datafusion::error::DataFusionError::Internal("test".to_owned()),
                )),
                503,
                "busy",
                Some("storage is unavailable"),
                Some("1"),
            ),
            (
                IngestError::Append(AppendError::ProjectDropped),
                404,
                "not_found",
                None,
                None,
            ),
            (
                IngestError::Media(MediaError::ProjectDropped),
                404,
                "not_found",
                None,
                None,
            ),
            (
                IngestError::Media(MediaError::HashMismatch),
                500,
                "internal",
                Some("internal error"),
                None,
            ),
            (
                IngestError::Media(MediaError::NotFound),
                500,
                "internal",
                Some("internal error"),
                None,
            ),
            (
                IngestError::Media(MediaError::InvalidRange),
                500,
                "internal",
                Some("internal error"),
                None,
            ),
            (
                IngestError::Media(MediaError::Fault),
                500,
                "internal",
                Some("internal error"),
                None,
            ),
            (
                IngestError::MediaHash,
                500,
                "internal",
                Some("internal error"),
                None,
            ),
            (
                IngestError::Append(AppendError::InvalidBatch { reason: "test" }),
                500,
                "internal",
                Some("internal error"),
                None,
            ),
            (
                IngestError::Batch(CoreError::UnknownVariant {
                    kind: "span_kind",
                    value: "test".to_owned(),
                }),
                500,
                "internal",
                Some("internal error"),
                None,
            ),
            (
                IngestError::Join(aborted_join_error().await),
                500,
                "internal",
                Some("internal error"),
                None,
            ),
        ];
        for (error, status, code, detail, retry_after) in cases {
            let label = format!("{error:?}");
            let (got_status, got_retry, problem) = problem_of(error).await;
            assert_eq!(got_status.as_u16(), status, "{label}");
            assert_eq!(problem["code"], code, "{label}");
            assert_eq!(problem["status"], status, "{label}");
            assert_eq!(
                problem.get("detail").and_then(|v| v.as_str()),
                detail,
                "{label}"
            );
            assert_eq!(got_retry.as_deref(), retry_after, "{label}");
        }
    }

    #[tokio::test]
    async fn a_decode_error_is_a_bad_request_with_its_own_message() {
        let error = decode_json(b"{").expect_err("not JSON");
        let (status, retry, problem) = problem_of(IngestError::Decode(error)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(retry, None);
        assert_eq!(problem["code"], "bad_request");
        let detail = problem["detail"].as_str().expect("a detail");
        assert!(detail.starts_with("invalid OTLP JSON"), "{detail}");
    }

    async fn aborted_join_error() -> tokio::task::JoinError {
        let handle = tokio::spawn(std::future::pending::<()>());
        handle.abort();
        handle.await.expect_err("an aborted task does not finish")
    }
}
