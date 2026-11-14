# G · Integration spine — design

Track G connects the Wave 1 areas into a running product: OTLP ingest into storage, the read API over A2's queries, score writes, live updates, and the embedded UI on the real API. It runs first and alone in Wave 2 because it settles the wiring every later track extends: the server's state and route table, `openapi.json`, the `fixtures/api` manifest and the web router (`docs/roadmap.md` § Wave 2).

This document holds the decisions, the architecture, the interfaces later tracks build on, and the milestones. Each milestone gets its own plan of small sub-phases: [`01-m1-otlp-http-ingest.md`](01-m1-otlp-http-ingest.md), [`02-m2-read-api.md`](02-m2-read-api.md) and [`03-m3-writes-grpc-usage.md`](03-m3-writes-grpc-usage.md) so far.

**Status.** Design, M1, M2 and M3 plans written. M1 implemented (PR #43), M2 implemented (PR #52), M3 implemented (branch `claude/g-m3-impl`). The A2 fixes M2 needs are merged (PR #48). The C review findings that touch G are in § Review findings, with the milestone each fix must precede.

**Read first:** `AGENTS.md`; `docs/mvp-decisions.md` (D39–D51, D60, D64, D69–D76, D83–D101, D103, D105, D108, D111, D135–D151, D152–D154, D161, D-F-1, D-F-2, D-F-16, D-A1-6, D-A1-7 and the Track G section); specs 01, 02, 03 §1–2 and §9–10, 05, 08, 09, 11, 12; the research note [`docs/research/spine/competitors-ingest-ui.md`](../../research/spine/competitors-ingest-ui.md).

## Decisions

All are in `docs/mvp-decisions.md` § Track G. The user chose the option marked here unless stated.

| # | Decision |
|---|---|
| D-G-1 | **One app state.** D's `AppState(Arc<AppInner>)` stays the only state. G adds plain fields (`ingest`, later `live` and `usage`); no `FromRef` sub-states, no traits. |
| D-G-2 | **Ingest runs per request and acknowledges after the WAL.** Read the body under the cap → decompress (capped) → decode, count, normalize in `spawn_blocking` → environment rewrite → `put_media` → `append_spans` (group commit) → publish to the live bus → count usage → `200`. No queue sits between the request and storage. |
| D-G-3 | **Overload is 503, limits are 429.** Server overload (memtable at its hard limit, in-flight budget full, WAL failed, disk full, shutting down) answers `503` with `Retry-After: 1` (gRPC `UNAVAILABLE`). `429` (gRPC `RESOURCE_EXHAUSTED`) is kept for per-org rate limits and quotas (track N). Changes spec 03 §2. |
| D-G-4 | **Request limits sit before decoding.** The raw body and the decompressed body are both capped at `ingest.max_request_bytes` (10 MiB); decompression stops at the cap. More than `ingest.max_spans_per_request` (50 000) spans after decoding is `413` too. An unknown `Content-Type` or `Content-Encoding` is `415`. |
| D-G-5 | **In-flight budget.** `ingest.max_inflight_bytes` (default `256MiB`) bounds the request bytes held at once: each request reserves its `Content-Length` (or the maximum when chunked) before reading, and each 64 KiB of decompressed output before producing it. A reservation that does not fit is refused at once with `503` (no waiting). zstd frames are limited to a 16 MiB window. |
| D-G-6 | **OTLP errors are problem+json.** `/v1/traces` answers success with `ExportTraceServiceResponse` in the request's encoding (with `partial_success` when spans were rejected) and every error as the API's problem+json, like every other route. The OTLP spec asks for a `google.rpc.Status` body; exporters act only on the status code and `Retry-After`. `/v1/traces` is not in the OpenAPI document. |
| D-G-7 | **Reserved environment.** After normalization, the OTLP handler rewrites `environment = "llmobs-eval"` to `llmobs-eval-external` and adds the warning code `reserved_environment` to `llmobs.normalize_warning` (spec 02 rule 18 gains the code). In-process commits are not rewritten. |
| D-G-8 | **In-process commit API.** Ingest is two steps: `accept` (OTLP only: decode, normalize, environment rewrite) and `commit(project, Normalized)` (media, append, live bus, usage). Track I commits judge traces with `commit` directly (D161). |
| D-G-9 | **Media before the append.** `commit` stores every blob with `put_media` (at most 4 at a time) before `append_spans`. Any media failure fails the request with nothing appended; a retry is safe because blobs are content-addressed. |
| D-G-10 | **Built-in prices only.** The price table is built from the embedded snapshot at startup and held as `RwLock<Arc<PriceTable>>`, so it can be swapped. The `model_prices` table, the `/models` endpoints and custom prices belong to a later track, still to be assigned. |
| D-G-11 | **G writes the usage counters.** A `usage_daily` migration, and counts of accepted spans, decompressed request bytes and scores per (org, project, UTC day of the ingest time), added in memory after each acknowledgement and upserted every 10 s and at shutdown. A crash loses at most 10 s of counts. Limits, quotas and `GET /orgs/{org}/usage` are track N. |
| D-G-12 | **Live bus, no replay.** `commit` publishes each appended batch's light rows on a `tokio::sync::broadcast` channel after `append_spans` returns, for every source. SSE events carry a monotonic `id:`. A reconnect with `Last-Event-ID`, or a lagging subscriber, gets `resync` and the UI refetches. `ping` every 15 s. |
| D-G-13 | **Trace list stream re-reads summaries.** `/traces/stream` collects the trace ids its project's batches touch, and once a second re-reads them with A2's `trace_list` (the stream's filter, plus `trace_id IN (…)`). It sends merged `TraceSummary` rows, never partial aggregates computed from one batch. |
| D-G-14 | **Read handlers are thin.** A handler parses the query, builds `QueryContext::new(storage.query(project, options), None)`, calls the A2 read, maps the Arrow result to API types in `api::convert`, and answers. Every read has a 30 s timeout. A2's `RunError` maps to problem codes as D94 says. |
| D-G-15 | **Key-only routes.** A new access kind, `Access::Key { scope }`, for routes whose only credential is a project API key: `POST /v1/traces` (`ingest`), `POST /api/v1/scores` (`ingest`, D-F-1), `POST /api/public/feedback` (`feedback`). No credential: `401`. A session, a PAT or a key without the scope: `403`. |
| D-G-16 | **Score writes.** `POST /projects/{p}/scores` and `DELETE /projects/{p}/scores/{id}` need project role `member` (a key needs `write`); `POST /api/v1/scores` takes an `ingest` key; feedback takes a `pk` key with `feedback`, CORS `*`, 4 KiB, 10 requests/s per (key, client IP). Server-derived score ids follow spec 06. |
| D-G-17 | **API examples come from the real handlers.** `just api-examples` loads `fixtures/api/dataset` into a real storage and `meta.db`, calls the real routes in process and writes `fixtures/api/responses/` and `manifest.json`. The hand model in `crates/server/tests/api_examples.rs` is deleted. MSW page tests stay (D74, D132). |
| D-G-18 | **The UI is its own crate.** `crates/ui` (`llmobs-ui`) embeds `web/dist` at compile time with `memory-serve` (precompressed, ETags, SPA fallback). Only `llmobs-cli` depends on it. Without `web/dist`, the binary serves a plain "UI not built" page. `just build-release` builds the web first; release CI checks that the UI is embedded. `just check` is unchanged. |
| D-G-19 | **G builds a minimal login page.** `/login` (email and password) and the API client's `401` handling, so the UI runs on the real API before track L; L later restyles it and adds setup, signup and reset. (User's choice; the recommended option was to wait for L.) |
| D-G-20 | **End-to-end test.** In `just check`: a Rust test starts the real server on ephemeral ports, sends the SDK conformance fixtures over OTLP/HTTP (protobuf and JSON) and gRPC, signs in and reads them back through the UI API. `just e2e` runs the real Python and TypeScript SDKs against `llmobs serve`, as its own CI job. |
| D-G-21 | **Ports.** `/v1/traces` is served on `http_addr` (8080) and on `otlp_http_addr` (4318, only that route; empty disables it). gRPC `TraceService/Export` is served by `tonic` on `grpc_addr` (4317; empty disables it). |
| D-G-22 | **Stream requests are validated before streaming.** `/traces/stream` applies `filter` and `search` (already in its parameters, D-C-5) on each re-read; a trace that matched `search` once keeps being sent on every update while the stream is open (D-C-5). The filter and search are checked when the stream opens: an invalid filter answers `400 invalid_filter` as a problem, not a stream, so `EventSource` closes without retrying. A re-read that fails later (timeout, memory cap) sends `resync` and logs the error kind. (From the C review, U27.) |
| D-G-23 | **Read error contract.** The read operations declare `422` and `503`; `get_media` declares `503`; a new problem code `range_not_satisfiable` (416). (User's choice.) |
| D-G-24 | **Media ranges from a whole read.** `GET /media` reads the whole blob (≤ `media.max_bytes`) and slices one `bytes=` range; anything else gets `200`. (User's choice; A1 is asked for a size-only lookup.) |
| D-G-25 | **Example generator.** `just api-examples` is a unit test in `llmobs-server` over `fixtures/api/dataset`; `GET /schema` is checked against `static/schema.json`; `GET /metrics` uses a fixed window and an empty manifest query. |
| D-G-26 | **Score `created_at`.** UUIDv7 ids give it from their time; other ids look up the live version with storage's `scores_by_id`. (User's choice.) |
| D-G-27 | **Score sources by caller.** Session → `human`; PAT or key → `api` or `sdk`; key route → `sdk`; feedback → `end_user`. (User's choice.) |
| D-G-28 | **Score write contract.** `{scores: […]}` 1–1000, `200 {score_ids}`; UUIDv8 derived ids; DELETE `204`; feedback `200 {score_id}`. (User's choice of the answer.) |
| D-G-29 | **OTLP/gRPC** with a tonic `transport::Server`, gzip, `authorization` metadata. |
| D-G-30 | **Usage counters** through `CommitTarget` and score writes, flushed to `usage_daily`. |

## Scope

**In G:**
- OTLP/HTTP ingest on 8080 and 4318, OTLP/gRPC on 4317 (D-G-21);
- the ingest pipeline, its limits, the in-flight budget, media, the environment rewrite (D-G-2 to D-G-9);
- the in-process commit API for track I (D-G-8);
- built-in prices (D-G-10), usage counters (D-G-11);
- the read API: traces, trace detail, spans, span detail, sessions, users, scores list, metrics, schema, media (spec 09);
- score writes, score delete, the key-scoped score route and public feedback (D-G-16);
- SSE on `/traces/stream` and `/traces/{id}/stream` (D-G-12, D-G-13);
- `fixtures/api` generated by the real handlers (D-G-17);
- the embedded UI, the minimal login page, the web on the real API (D-G-18, D-G-19);
- the end-to-end test and `just e2e` (D-G-20).

**Not in G (owner in brackets):**
- saved views on the server, the SQL console, MCP, CLI query commands [H]. The UI keeps its in-memory saved-view mocks until H; on the real API the saved-views menu gets `404` and shows its empty state (M5 checks this);
- score definitions, the annotation form, evaluators, online rules, judge execution [I]; I calls G's `commit`;
- datasets, experiments [J]; alerts [K]; admin pages, setup, signup, reset, settings [L];
- deletion endpoints and retention settings [M];
- per-org rate limits and quotas, `GET /orgs/{org}/usage`, Docker, Helm, release pipeline [N]; G adds only the release build recipe and the UI check it needs (D-G-18);
- `model_prices` and `/models` [unassigned, D-G-10].

**Edits outside G's own directories.** G owns `crates/server`, `crates/cli`, the new `crates/ui`, `web/`, `fixtures/api/`, the `justfile` recipes and CI jobs it adds, and the docs it changes. It does not change `llmobs-core`, `llmobs-storage`, `llmobs-otel` or `llmobs-eval` (requests below).

## Architecture

### State

`crates/server/src/http/mod.rs` `AppInner` gains (D-G-1):

| Field | Type | Milestone | What |
|---|---|---|---|
| `ingest` | `crate::ingest::Ingest` | M1 | price table, in-flight budget, ingest limits |
| `usage` | `crate::usage::Usage` | M3 | in-memory counters, flushed to `usage_daily` |
| `live` | `crate::live::LiveBus` | M4 | broadcast sender and the event id counter |
| `metrics_cache` | `crate::routes::metrics::MetricsCache` | M2 | `GET /metrics` answers, 10 s |

The feedback limiter (10/s per key and IP) is a field of D's `auth::Limits` (D-G-30).

`Ingest` is a plain struct; it holds no `AppState` and takes what it needs as arguments, so `commit` can be called from a background task in track I.

### Modules (end state)

```
crates/server/src/
  ingest/
    mod.rs        Ingest: accept, commit, IngestError, the budget
    body.rs       content type, content encoding, capped read and decompression
    otlp.rs       ExportTraceServiceResponse encoding (protobuf / JSON)
    environment.rs  the llmobs-eval rewrite (D-G-7)
    grpc.rs       tonic TraceService (M3)
  usage.rs        Usage counters and flush (M3)
  live.rs         LiveBus, LiveEvent (M4)
  routes/
    otlp.rs       POST /v1/traces (M1)
    traces.rs     traces, trace detail, spans, span detail (M2), streams (M4)
    sessions.rs, users.rs, metrics.rs, schema.rs, media.rs (M2)
    scores.rs     list (M2), create, delete, key route (M3)
    feedback.rs   public feedback (M3)
  api/convert.rs  Arrow → API types (M2)
crates/ui/        llmobs-ui: embedded web/dist (M5)
```

The existing `api/` types (`traces.rs`, `sessions.rs`, …) are the response types; G implements handlers for the operations that are already declared in `api/openapi.rs`. The OpenAPI declarations stay in the builder style D uses; the committed `openapi.json` must not change when a handler lands for an already-declared operation (spec 09 § The contract file).

### Ingest pipeline

```
POST /v1/traces (8080 or 4318)            TraceService/Export (4317, M3)
  │ access: Access::Key{Ingest} → Principal::ApiKey{project_id}
  ▼
check Content-Type / Content-Encoding ──415
reserve budget: Content-Length, or max_request_bytes when chunked ──503 Retry-After: 1
read body ≤ max_request_bytes ──413 ; give back the unused reservation
  ▼  spawn_blocking
decompress ≤ max_request_bytes, each 64 KiB chunk reserved first ──413 / 503 ; decode ──400 ; count spans ──413
normalize(request, IngestContext{project, now, retention, limits}, prices)
rewrite reserved environment (D-G-7)
  ▼  Ingest::commit(project_id, Normalized)
put_media × n (≤ 4 concurrent) ──503 / 404
spans_to_record_batch → append_spans ──503 (Backpressure, WalFailed, ShuttingDown) / 404 (ProjectDropped)
live.publish(project, spans)          (M4)
usage.add(org, project, spans, bytes) (M3)
  ▼
200 ExportTraceServiceResponse { partial_success }
```

- **Acknowledgement.** `append_spans` returns when the record is durable per `storage.wal.sync` and visible to queries (spec 03 §1). The `200` is sent after it, so "200 means every accepted span is in the fsynced WAL" holds (spec 02).
- **CPU work** runs in `tokio::task::spawn_blocking`: a 10 MiB request takes tens of milliseconds to decompress, decode and normalize, which must not stall the async workers.
- **Empty results.** A request whose spans are all rejected, or that has none, appends nothing and answers `200` with `partial_success` as needed.
- **Duplicates.** A retried request after a lost acknowledgement appends the same spans again. Storage tolerates it: trace detail keeps the first row of a span id (spec 09) and compaction stops duplicates inflating trace totals (D88). Not deduplicated at ingest.

### Errors and status codes

| Cause | HTTP | gRPC (M3) | Problem code |
|---|---|---|---|
| no or invalid credential | 401 | `UNAUTHENTICATED` | `unauthorized` |
| session, PAT, or key without the scope | 403 | `PERMISSION_DENIED` | `forbidden` |
| `Content-Type` not `application/x-protobuf` or `application/json` (parameters ignored) | 415 | — | `unsupported_media_type` |
| `Content-Encoding` not `gzip`, `deflate`, `zstd` or identity | 415 | — (tonic handles gzip) | `unsupported_media_type` |
| raw or decompressed body above `max_request_bytes`; more than `max_spans_per_request` spans | 413 | `RESOURCE_EXHAUSTED` (tonic's own message limit), `INVALID_ARGUMENT` for the span count | `payload_too_large` |
| corrupt compressed stream, decode error; a media blob `put_media` refuses as invalid (an over-long MIME type) | 400 | `INVALID_ARGUMENT` | `bad_request` |
| in-flight budget full; `AppendError::Backpressure`, `WalFailed`, `ShuttingDown`, `DiskFull`; `MediaError::Object`, `Catalog`, `Query`, `DiskFull` | 503 + `Retry-After: 1` | `UNAVAILABLE` | `busy` |
| `AppendError::ProjectDropped`, `MediaError::ProjectDropped` | 404 | `NOT_FOUND` | `not_found` |
| `AppendError::InvalidBatch`, `MediaError::HashMismatch` / `NotFound` / `InvalidRange` / `Fault`, batch conversion failure | 500 | `INTERNAL` | `internal` |

The `detail` never contains request content. Decode errors carry `DecodeError`'s display, which never contains payload bytes (`crates/otel/src/error.rs`). Logs carry the project id, the request size, the span and rejected counts, and the duration (AGENTS.md § Privacy).

The disk-full condition (spec 03 §10, `storage.min_free_bytes`, A1 M6) arrives as `AppendError::DiskFull` / `MediaError::DiskFull` and is in the 503 row, with the detail "storage is out of disk space".

### Commit API (for track I)

```rust
impl Ingest {
    /// Stores `normalized`'s media, then appends its spans (M1); publishes them (M4)
    /// and counts them (M3). Nothing is appended when a blob fails.
    pub(crate) async fn commit(
        &self,
        storage: &Storage,
        project_id: ProjectId,          // M3: target: CommitTarget
        normalized: Normalized,
    ) -> Result<CommitReceipt, IngestError>;
}

/// M3. Org and raw size for the usage counters.
pub(crate) struct CommitTarget { pub(crate) org_id: OrgId, pub(crate) project_id: ProjectId, pub(crate) request_bytes: u64, pub(crate) now_ns: i64 }
pub(crate) struct CommitReceipt { pub(crate) accepted: usize, pub(crate) media: usize }
```

M1's `commit` takes the project id; M3 replaces it with `CommitTarget` and adds the `usage` argument, and M4 adds `live` (each milestone updates every caller, AGENTS.md § No compatibility layers). Rejections stay in `Normalized`; the OTLP handler reads them before calling `commit`. Track I builds `Normalized` with `llmobs-eval` and calls `commit` with `request_bytes: 0`. Its spans keep `environment = "llmobs-eval"` because the rewrite lives in `accept`, not in `commit`.

### Limits and configuration

New config keys (spec 12 table updated in the M1 change):

| Key | Default | Milestone |
|---|---|---|
| `otlp_http_addr` (`LLMOBS_OTLP_HTTP_ADDR`) | `0.0.0.0:4318`; empty string disables | M1 |
| `ingest.max_request_bytes` | `"10MiB"` | M1 |
| `ingest.max_spans_per_request` | `50000` | M1 |
| `ingest.max_attribute_bytes` | `"8MiB"` | M1 |
| `ingest.max_inflight_bytes` | `"256MiB"` | M1 |
| `media.min_bytes` / `media.max_bytes` | `"4KiB"` / `"32MiB"` | M1 |
| `grpc_addr` (`LLMOBS_GRPC_ADDR`) | `0.0.0.0:4317`; empty disables | M3 |
| `ingest.max_grpc_message_bytes` | `"32MiB"` | M3 |

Sizes are strings with a binary unit, as `[storage]` (D90).

### Media

`Normalized.media` holds `llmobs_core::media::MediaBlob { sha256: String (hex), mime_type, bytes: Vec<u8> }`; storage takes `llmobs_storage::MediaBlob { sha256: [u8; 32], mime_type, bytes: Bytes }`. `commit` converts (hex decode; a bad hex is the `internal` row above) and calls `put_media`, at most 4 at a time (`futures::stream::iter(..).buffer_unordered(4)`), before the append. `GET /media/{sha256}` (M2) reads with `get_media`, supports one `Range: bytes=a-b` range (`206`, `Content-Range`; an unsatisfiable range is `416`), and always sends the stored `Content-Type`, `X-Content-Type-Options: nosniff`, `Content-Disposition: inline`, `Cache-Control: private, max-age=31536000, immutable` (the name is the hash) and `Content-Security-Policy: sandbox`.

### Usage counters (M3)

`usage_daily` (spec 04 § Metering) is migration `0002_usage.sql`. `Usage` keeps `Mutex<HashMap<(OrgId, ProjectId, NaiveDate), Counts>>`; `commit` adds accepted spans and `request_bytes`, score writes add scores. A task swaps the map every 10 s and upserts the totals with `INSERT … ON CONFLICT DO UPDATE SET spans = spans + excluded.spans, …` through `MetaDb::write`; shutdown flushes once more after the HTTP drain. A failed flush logs an error and adds the counts back.

### Live bus and SSE (M4)

- `LiveBus` wraps `broadcast::Sender<Arc<LiveEvent>>` with capacity 1024 and an `AtomicU64` event id. `LiveEvent { id, project_id, spans: Vec<SpanSummary>, trace_ids: Vec<TraceId> }` is built once per committed batch from the light columns (spec 09 § Response shapes), after `append_spans` returns.
- `GET /traces/{id}/stream`: filters events on project and trace id and sends one `span` event per matching span, with the event's `id`. It closes after 10 minutes without a matching event.
- `GET /traces/stream` (D-G-13, D-G-22): at open, compiles `filter` and `search` with A2 (an error answers `400 invalid_filter` / `bad_request` before any event); then collects touched trace ids of its project and every second, if any, runs `trace_list` with the request's `filter`, `include_eval`, `include_experiments` and `search`, plus `trace_id IN (…)` (at most 500 ids per tick; more is a `resync`), and sends one `trace` event per row. Trace ids that matched `search` are remembered for the stream's life and re-read without `search` afterwards (D-C-5's rule; at most 10 000 remembered ids, then `resync` and the set is cleared). A failed re-read sends `resync`. `TraceStreamParams.search` already exists (C's fix).
- Server-side bounds: one broadcast channel of 1024 events; per stream at most 500 trace ids per tick and one `trace_list` page (`limit` 500); nothing is buffered per client beyond the socket. Client-side bounds on rows and the paused buffer are C's fix (see § Review findings).
- Both: `ping` every 15 s; a request with `Last-Event-ID`, or a `RecvError::Lagged`, sends `resync` first (data `{}`); at most 100 open streams per user (`429` problem `rate_limited` beyond, counted per user id, keys per key id); access `ProjectPerm::View`.
- The UI already speaks this protocol (D134, `web/src/mocks/sse.ts`).

### Read API (M2)

Handlers for every declared read operation: `list_traces`, `get_trace`, `list_spans`, `get_span`, `list_sessions`, `get_session`, `list_users`, `get_user`, `list_scores`, `get_metrics`, `get_schema`, `get_media`. Each:
1. takes `ProjectAccess` (D's middleware already resolved the project and role);
2. parses its params with D's `Query` extractor (the existing `*Params` types);
3. builds the A2 request (`TraceListRequest`, …) and `QueryContext::new(storage.query(project, QueryOptions { around }), None)`;
4. calls the read with a 30 s timeout (D-G-14);
5. maps Arrow batches to the API types in `api::convert` (one function per type, columns by name as A2 documents them);
6. answers JSON.

Metrics results are cached 10 s per `(project, params)` (spec 05) in a small `Mutex<HashMap>` with expiry (at most 256 entries), held in `AppInner.metrics_cache`. `RunError` → problem: `Invalid` → 400 `invalid_filter` with `errors[0] = {field: "filter", message, position}`, `InvalidRequest` → 400 `bad_request`, `Failed` → 400 `query_failed`, `Timeout` → 422 `query_timeout`, `TooLarge` → 422 `query_too_large`, `Internal` → 500. `Storage::query`'s `QueryError::Busy` (admission) → 503 `busy`. A `from` or `to` alone leaves the other side of the window open. The CLI runtime gives workers 8 MiB stacks, as A2 asks. Media follows D-G-24. The plan is [`02-m2-read-api.md`](02-m2-read-api.md).

### Score writes and feedback (M3)

- `POST /api/v1/projects/{p}/scores` (batch 1–1000, `ProjectPerm::Annotate`: role `member`, key scope `write`), `DELETE /api/v1/projects/{p}/scores/{score_id}` (same permission). A delete reads the score's latest version and appends a copy with `deleted = true` and a later `updated_at`, keeping its `created_at` so a day close sees every version (spec 01 § scores, spec 03 §5); an unknown id is `404`, `POST /api/v1/scores` (`Access::Key { Ingest }`, D-F-1), `POST /api/public/feedback` (`Access::Key { Feedback }`).
- Every write validates and builds `llmobs_core::Score` rows, derives missing ids (spec 06 § deterministic ids), and calls `append_scores`. Errors map as the ingest table.
- Feedback: body ≤ 4 KiB, CORS `Access-Control-Allow-Origin: *` with a preflight answer for `OPTIONS` (`Authorization, Content-Type`), 10 requests/s per (key id, client IP), `source = end_user`.
- The API contract for these operations is added to `openapi.json` in M3 (C's request R1 asked for it).

### Embedded UI (M5)

- `crates/ui/build.rs` checks for `web/dist/index.html`; `memory-serve` embeds the directory when present. `llmobs_ui::router() -> axum::Router` serves the assets with their content types, precompressed variants and ETags; `index.html` for any other `GET`/`HEAD` path (SPA fallback); `Cache-Control: public, max-age=31536000, immutable` under `/assets/`, `no-cache` for `index.html`. When `web/dist` was missing at build time, every UI path answers `503` text "web UI not built: run `just build-release`".
- `llmobs_server::serve::bind(config, ui: Option<axum::Router>)` merges the UI router as the fallback of the main listener only. `/api/*`, `/v1/*`, `/mcp`, `/healthz`, `/readyz` never fall back to `index.html`: unknown paths there stay `404` problems. UI responses carry spec 12's CSP (`default-src 'self'; img-src 'self' blob: data:; media-src 'self'; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'`), `X-Content-Type-Options: nosniff`, `Referrer-Policy: same-origin`.
- The minimal `/login` page posts `/api/auth/login`, then navigates to the `redirect` search param (a relative path, D's rule) or `/`. The API client sends any `401` from `/api/v1/*` to `/login?redirect=<current path>`. The root route picks the first project from `/api/auth/me` (as today on mocks); a user with no project sees an empty state.

### End-to-end test (M5)

`crates/cli/tests/e2e.rs`: bind the real server (`serve::bind` with ports 0, a temp `data_dir`, `init.*` creating the admin, org, project and an `sk` key), send every `fixtures/otlp/llmobs/sdk-*` case over OTLP/HTTP protobuf, OTLP/HTTP JSON (a second project) and gRPC (a third), sign in with `POST /api/auth/login`, and read each trace through `GET /traces`, `GET /traces/{id}` and `GET /traces/{id}/spans/{span_id}`, comparing with `insta` snapshots whose ids and times are redacted. `just e2e` starts `llmobs serve` on free ports and runs `sdks/python` and `sdks/typescript` integration tests against it (spec 10 § Testing); a CI job `e2e` runs it.

## Review findings from A2 and C

The A2 and C reviews (`docs/reviews/a2-query.md`, `docs/reviews/c-web-ui.md` on their review branches) found issues that touch G. Their fixes belong to their own tracks and are expected to land before the G milestone that needs them; G plans no workaround for them. M1 needs none of them.

| Finding | Effect on G | Needed before |
|---|---|---|
| A2 B-1 (uncounted operator chains abort the process), B-2 (size-generating scalars bypass the memory pool) | None in G: no G handler runs user SQL. G's handlers pass `filter` (the DSL, 4 KiB, which the review found safe) and `search` to A2's reads only. `/sql` and MCP `query_sql` are track H, which depends on both fixes. | H |
| A2 M-4 (float sort keys such as `cost_total` unstable across cursor pages) | G's list handlers pass A2's opaque cursors through; their paging contract (no repeated or skipped row) holds only once the fix lands in A1/A2. | M2 |
| A2 M-3 (session and user detail collect every row to test existence) | Reached by `GET /sessions/{id}` and `GET /users/{id}`. | M2 |
| A2 M-2 (`bucket=auto` has no bucket cap) | Reached by `GET /metrics`; the cap is recorded in spec 05 by the fix. | M2 |
| A2 M-1 (Parquet export buffers the whole file) | None: G serves no export (H). | H |
| C U27 (live mode ignores `search`; unbounded rows and paused buffer; retries forever on an invalid filter) | G's side is D-G-22: the stream applies `search`, validates at open with a `400`, and bounds its own work. The client must pass `search`, not open a stream while the filter is invalid, and bound its rows and buffer. | M4 (server), M5 (client) |
| C U11/U12 (the filter bar's chips can change a filter's meaning: OR precedence, `BETWEEN`, bare `AND` appends) | The server's `invalid_filter` and spec 05's semantics stay the source of truth; G changes nothing. | M5 |
| C U18 (the external-media gate can be bypassed: upper-case schemes, `//host`) | G serves media only same-origin, at `GET /api/v1/projects/{p}/media/{sha256}` with `Content-Security-Policy: sandbox` and `nosniff`, and the UI's CSP allows media from `self` only (§ Embedded UI), which blocks external loads even if the gate is bypassed. The gate fix is still needed. | M5 |
| C: `j`/`k` after a deep link, the table header not scrolling horizontally, the default saved view re-applied on every empty search | UI-only. | M5 |

M2's plan checked that the A2 fixes it needs are merged (PR #48); M5's plan starts by checking the C fixes. If one is not merged, that milestone waits or the coordinator decides to ship without it.

## Testing

- HTTP tests drive the router with `oneshot` (D151); listener tests bind ephemeral ports.
- **Ingest golden test (M1).** For every `fixtures/otlp` case that has an `expected.json`, ingesting `input.json` over `/v1/traces` stores exactly what `normalize` returns for the same context and price table (`fixtures/otlp/prices.json`, injected into `Ingest`). Protobuf, JSON and each content encoding are exercised.
- **Limits and errors (M1):** every row of the error table, without echoing request content (`assert_logs_clean`).
- **Access (M1, M3):** `Access::Key` routes join D's generated authorization suite.
- **Read API (M2):** the generated examples are the test: `just api-examples` output must equal the committed files, and every file is checked against `openapi.json` (the existing checks in `api_examples.rs`).
- **Concurrency (M1):** concurrent ingests into one project; the budget refuses with 503 and recovers; a shutdown during ingest loses no acknowledged span.
- **SSE (M4):** event order, filters and search, ping (manual clock or paused tokio time), resync on lag, on `Last-Event-ID` and on a failed re-read, `400 invalid_filter` at open, stream cap.
- **Web (M5):** page tests stay on MSW. The login page and the 401 redirect get tests. `pnpm dev:server` against `just run serve` is the manual check.

## Milestones

| # | Milestone | Done when |
|---|---|---|
| **M1** | **OTLP/HTTP ingest end to end.** Config (`[ingest]`, `[media]`, `otlp_http_addr`); `Access::Key`; body handling; OTLP responses; `Ingest` with built-in prices, budget, `accept` and `commit` (media, append); `POST /v1/traces` on 8080 and 4318; golden and limit tests. | An OTel exporter pointed at `llmobs serve` with an `sk` key gets `200` and its spans are queryable through storage. |
| **M2** | **Read API.** `api::convert`; the 12 read handlers; the metrics cache; `fixtures/api` regenerated by the real handlers (D-G-17, D-G-25); the read error contract (D-G-23). | Every declared read operation has a handler; the committed examples equal the real responses; `openapi.json` declares the read errors. |
| **M3** | **Writes, gRPC, usage.** Score create/delete, the key score route, public feedback (contract added to `openapi.json`); `tonic` gRPC on 4317; `usage_daily` counters. | The SDKs' score route and the browser feedback helper work against the server; gRPC exporters work; usage rows grow. |
| **M4** | **Live updates.** `LiveBus`, `commit` publishes, both SSE routes, `search` on the trace stream (D-C-5, D-G-22). | The UI's live mode works against the real server. |
| **M5** | **UI on the real API and the end-to-end test.** `llmobs-ui`, SPA fallback and headers, minimal login, 401 handling, the e2e test, `just e2e` and its CI job, `just build-release` builds the web. | `llmobs serve` serves the UI; a user signs in, sees ingested traces live; `just check` runs the e2e test. |

M2 and M3 are independent of each other after M1; they stay in order because G runs alone.

## Spec changes (made with this design)

- **02 § Endpoints:** status codes per D-G-3/D-G-4/D-G-6; `Content-Type` parameters ignored; problem+json errors; spans-per-request limit checked after decoding. **§ Limits:** add `ingest.max_inflight_bytes`. **§ Pipeline:** the two steps `accept` / `commit` (D-G-8) and where the live bus and usage sit. **Rule 18:** add `reserved_environment`. **§ Context fields `environment`:** name the warning code.
- **03 §2 Backpressure:** `503 Retry-After: 1` / `UNAVAILABLE` instead of `429` / `RESOURCE_EXHAUSTED` (D-G-3).
- **09:** OTLP errors are problem+json (D-G-6); the `/v1/traces` row notes it is not in the OpenAPI document; SSE resync on `Last-Event-ID` (D-G-12); `/traces/stream` sends merged summaries (D-G-13); media response headers.
- **12:** the new config keys; `otlp_http_addr` and `grpc_addr` accept an empty string.

## Requests to other tracks

- **B1 (`llmobs-otel`):** cap the MIME type of an extracted blob at 255 bytes (storage's `put_media` limit; `is_mime` checks characters only), falling back to `application/octet-stream`. Until then G answers such a request `400`.
- **A1:** the doc comment of `AppendError` (`crates/storage/src/error.rs`) still says `Backpressure` is answered `429`; D-G-3 makes it `503`.
- **A1:** a size-only media lookup (`media_info(project, sha256) -> Option<(mime_type, size)>`) so `GET /media` can read only the requested range (D-G-24).
- **A2 / A1:** the review fixes listed in § Review findings: M-2, M-3, M-4 before M2 (merged, PR #48); B-1, B-2, M-1 before H (merged, PR #48).
- **C:** the review fixes listed in § Review findings, before M5. C's R2 (batch span payloads) and R3 (autocomplete values) are not in G; they need A2 reads first.
- **Coordinator:** assign `model_prices` and `/models` (D-G-10) to a track.
- **L:** restyle G's `/login` and add setup, signup, reset and settings pages (D-G-19).
- **I:** call `Ingest::commit` for judge traces (D-G-8); hold `usage` and `live` the same way.

## Open items

- Each `Access::Key` request reads the project and its org from `meta.db` (two reads on the pool of 4). Fine for M1; if ingest benchmarks show it, cache the project row next to the key cache with the same 60 s expiry and eviction.
- `deflate` is decoded as zlib (RFC 9110). Raw-deflate senders get `400`; add a fallback only if a real exporter needs it.

- Feedback rate limit state lives in memory, like D's login limits; multi-node is out of scope (D15).
- `memory-serve`'s exact API (version 2.4.0 at the time of writing) is checked when M5 is planned.
- `GET /media` reads the whole blob for a range request (D-G-24) until A1 offers a size-only lookup.
- The examples of D's endpoints (`static/`) stay hand-written; they can move to the generator when their owners next touch them.
