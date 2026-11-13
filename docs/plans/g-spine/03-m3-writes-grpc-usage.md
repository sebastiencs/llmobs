# G · M3 — Score writes, feedback, gRPC, usage counters

Implementation plan for milestone M3 of track G. It has 9 sub-phases, G3.1 to G3.9. Each one is done by one agent in one go, in order, and ends with `just check` green and one commit.

**Prerequisites** (checked when this plan was written, 2026-11-13):
- M1 (PR #43) and M2 (PR #52) are merged.
- Node `26.13.0` is installed at `$HOME/.local/share/nvm/v26.13.0/bin`. It is not on the non-interactive `PATH`.

**Read first** (every sub-phase):
- `AGENTS.md`, all of it.
- This file's sections up to § G3.1, then your own sub-phase.
- `docs/plans/g-spine/00-design.md`: § Decisions, § Architecture (State, Ingest pipeline, Errors and status codes, Commit API, Usage counters (M3), Score writes and feedback (M3)).
- `docs/mvp-decisions.md`: D-F-1, D-F-2, D-F-16, D140, D-G-3, D-G-11, D-G-15, D-G-16, D-G-21, D-G-26 to D-G-30.
- `docs/spec/06-evaluation.md` § Scores; `docs/spec/09-api.md` § Scores & annotations; `docs/spec/01-data-model.md` § Score; `docs/spec/03-storage.md` §5 (day close: every version of a score shares its `created_at`); `docs/spec/04-metadata-db.md` § Metering; `docs/spec/02-ingestion.md` § Endpoints.
- Code the sub-phase points to, before editing it.

## What M3 is, and what it is not

When M3 is done:
- the SDKs' score queue (`POST /api/v1/scores`, `sk` key with `ingest`) and the browser `feedback()` helper (`POST /api/public/feedback`, `pk` key) work against the server;
- the UI and API clients can write and delete scores on `POST /api/v1/projects/{project}/scores` and `DELETE /api/v1/projects/{project}/scores/{score_id}`;
- OTel exporters can send traces over gRPC to port 4317;
- `usage_daily` counts accepted spans, request bytes and scores per org, project and UTC day.

**Out of scope:** score definitions and their value validation, evaluators, online rules, judge traces (track I); the usage API, quotas and per-org rate limits (track N); the live bus (M4); the UI (M5). M3 adds no route beyond the five above.

## Decisions this plan relies on

| # | Summary |
|---|---|
| D-G-11 | `usage_daily` counters in memory, upserted every 10 s and at shutdown. |
| D-G-15 | `Access::Key { scope }` for `POST /api/v1/scores` (`ingest`) and `/api/public/feedback` (`feedback`). |
| D-G-16 | Project score writes need role `member` (a key needs `write`); feedback is CORS `*`, ≤ 4 KiB, 10 requests/s per (key, client IP). |
| D-G-21 | gRPC `TraceService/Export` on `grpc_addr` (4317; empty disables it). |
| D-G-26 | **`created_at` of a score write.** A UUIDv7 `score_id` gives `created_at` from the id's time; any other id looks up the live version with `scores_by_id`; a miss is a new score with `created_at = now`. |
| D-G-27 | **Score sources by caller.** Session → `human` + `author_user_id`; PAT or `sk` key on the project route → `api`, or `sdk` when the body says so; key route → `sdk`; feedback → `end_user`. Other sources are 400. |
| D-G-28 | **Score write contract.** `{"scores": [ScoreCreate]}`, 1–1000 items, answers `200 {"score_ids": […]}` in request order; DELETE answers `204`; feedback answers `200 {"score_id"}`. Derived ids are UUIDv8 over a fixed byte layout. |
| D-G-29 | **gRPC.** A tonic `transport::Server` on `grpc_addr`, gzip accepted, `ingest.max_grpc_message_bytes` decoded-size cap, the `authorization` metadata as credential. |
| D-G-30 | **Usage counters.** `crate::usage::Usage` in `AppInner`; `commit` takes a `CommitTarget` and counts; score writes count rows; migration `0002_usage.sql`. |

## Versions

Add to the root `[workspace.dependencies]`, with a one-line comment saying why, then `dep.workspace = true` in `crates/server/Cargo.toml` (G3.7). The version was checked on crates.io when this plan was written: released 2026-10-29, more than 7 days ago.

| Crate | Version | Features | Why |
|---|---|---|---|
| `tonic` | `0.14.9` | `default-features = false, features = ["server", "router", "codegen", "gzip"]` | gRPC server for `TraceService/Export` (D-G-29). `router` is needed for `Server::builder().add_service`. No `zstd`: tonic's pulls `zstd` 0.14 next to the workspace's 0.13, and spec 02 asks for gzip only |

Change in the root `opentelemetry-proto` entry: add the feature `"gen-tonic"` to its list (it generates `trace_service_server` and `trace_service_client`; it pulls `tonic-prost` 0.14 and `tonic/channel`, which the gRPC tests use as a client). Update the entry's comment: "OTLP messages and the tonic `TraceService` (server and test client)." Do not change its version.

`chrono` (workspace entry exists) becomes a dependency of `llmobs-server` in G3.6 (UTC day of the usage counters).

After G3.7, `cargo deny check` must still pass. `deny.toml` sets `multiple-versions = "warn"` and denies duplicates only in the arrow/datafusion/parquet/object_store family: duplicate *warnings* for other crates are fine; an *error* is a STOP.

## Conventions for every sub-phase

- **Node.** Before any `just` recipe that runs web or TypeScript checks (`just check`, `just check-web`, `just openapi`), run `export PATH="$HOME/.local/share/nvm/v26.13.0/bin:$PATH"` in the same shell command.
- **Checks.** While iterating, `just check-rust`. Before the commit, `just check` once, in the foreground, with a timeout of at least 20 minutes. Never start background or duplicate runs, and leave no polling loop behind. Never commit with a failing check.
- **Temporary files** go in the session's scratchpad directory, not `/tmp`.
- **Errors and panics.** No `unwrap()`/`expect()` outside tests (AGENTS.md). Handlers return `ApiError`.
- **Privacy.** Never log a score value, reason, metadata value, name, feedback comment, key or token. Log ids, counts, sizes and error kinds.
- **Tests.** HTTP tests build a `TestApp`, call `seed()`, and use `#[tokio::test(flavor = "multi_thread")]`. They read stored scores back with `GET /api/v1/projects/{id}/scores` (M2) or `llmobs_storage::score_list`, and assert exact values.
- **Names.** Storage types share names with API types. Refer to storage items as `llmobs_storage::…`.
- **STOP rules.** Stop and report, without committing, when:
  - the plan is wrong about the code in a way you cannot resolve without guessing;
  - a test of an earlier milestone fails for a reason outside your change;
  - G3.7's dependency change makes `cargo deny check` fail (an error, not a warning).
- **Commit.** One commit per sub-phase, with the subject given, and no trailer lines.
- **Report.** What you did; every deviation from this plan, and why; new dependency versions or "no new dependency"; anything left unverified.

## Final layout after M3

```
crates/storage/src/reads/scores.rs        scores_by_id (G3.1)
crates/server/src/
  api/scores.rs        ScoreCreate, ScoreBatch, ScoreWriteResult, FeedbackRequest, FeedbackValue, FeedbackResult (G3.2)
  api/openapi.rs       create_scores, delete_score, create_key_scores, post_feedback (G3.2)
  auth/roles.rs        ProjectPerm::Annotate (G3.3)
  auth/limits.rs       Limits.feedback (G3.5)
  auth/principal.rs    bearer_principal becomes pub(crate) (G3.8)
  scores.rs            score write core: validation, ids, created_at, append (G3.3)
  usage.rs             Usage counters and flush (G3.6)
  ingest/mod.rs        CommitTarget, accept_request (G3.6, G3.7)
  ingest/grpc.rs       TraceService for tonic (G3.8)
  routes/scores.rs     list (M2) + create, delete, key route (G3.4)
  routes/feedback.rs   POST + OPTIONS /api/public/feedback (G3.5)
  http/access.rs       key_only_project shared by HTTP and gRPC (G3.8)
  serve.rs             gRPC listener, usage flush task (G3.6, G3.8)
  config.rs            grpc_addr, ingest.max_grpc_message_bytes (G3.7)
crates/server/migrations/meta/0002_usage.sql   (G3.6)
fixtures/api/requests/{score-batch.json,feedback.json}   (G3.2)
```

## Shared reference

### Score write rules (D-G-26, D-G-27, D-G-28)

Every write path (project route, key route, feedback) ends in one function, `crate::scores::write`, that turns request items into `llmobs_core::score::Score` rows and appends them.

**Caller → source and author.**

| Caller (`Principal`) | Route | Body `source` allowed | Stored `source` | `author_user_id` | Author for derived ids |
|---|---|---|---|---|---|
| `Session { user }` | project route | absent or `human` | `human` | `Some(user.id)` | `user.id` |
| `Pat { token_id, .. }` | project route | absent, `api`, `sdk` | the body's, default `api` | `None` | `token_id` |
| `ApiKey { key_id, .. }` | project route | absent, `api`, `sdk` | the body's, default `api` | `None` | `key_id` |
| `ApiKey { key_id, .. }` | key route `POST /api/v1/scores` | absent or `sdk` | `sdk` | `None` | `key_id` |
| `ApiKey { key_id, .. }` (`pk`) | feedback | — (not in the body) | `end_user` | `None` | `key_id` |

A body `source` outside the allowed column is a field error `scores[i].source`: `"source <value> is not allowed here"`, where `<value>` is the source's snake_case name (an enum name, not user content).

**Validation** (all-or-nothing: any error answers 400 with every field error, `errors[]` in item order, and nothing is appended):

| Check | Field | Message |
|---|---|---|
| 1 ≤ items ≤ 1000 | `scores` | `"between 1 and 1000 scores per request"` |
| `name` is 1 to 255 bytes | `scores[i].name` | `"name must be 1 to 255 bytes"` |
| session target: `session_id` is 1 to 255 bytes | `scores[i].target.session_id` | `"session_id must be 1 to 255 bytes"` |
| categorical `label` is 1 to 255 bytes | `scores[i].value` | `"label must be 1 to 255 bytes"` |
| a UUIDv7 `score_id` whose time is more than 1 hour after `now`, or overflows | `scores[i].score_id` | `"score_id time is in the future"` |
| a UUIDv7 `score_id` older than the project's retention | `scores[i].score_id` | `"score_id time is older than the project's retention"` |
| source (table above) | `scores[i].source` | as above |

Ids, enums, `value` shapes and finite numbers are checked by serde when the body is parsed (`ScoreValue`'s `Deserialize`, `TraceId`, `SpanId`, `Uuid`); a parse failure is the existing `error::Json` 400 (`invalid JSON body at <path>`).

**Duplicates in one request.** Items with the same final `score_id` (given or derived) keep only the last one; `score_ids` in the answer still has one entry per input item, in input order.

**Derived `score_id`** (spec 06), when the item has none: `Uuid` from `uuid::Builder::from_custom_bytes(first 16 bytes of SHA-256(input)).into_uuid()` (version 8, RFC variant). `input` is the ASCII tag `llmobs.score.v1` followed by these fields in order, each written as its byte length (`u32`, big-endian) then its UTF-8 bytes:
1. the project id, as `ProjectId::to_string()` (lowercase hyphenated);
2. the target kind (`ScoreTargetKind::as_str()`: `span`, `trace`, `session`);
3. the trace id as 32 lowercase hex, or the empty string for a session target;
4. the span id as 16 lowercase hex for a span target, the session id for a session target, else the empty string;
5. the score name;
6. the stored source (`ScoreSource::as_str()`);
7. the author (table above), as `Uuid::to_string()`.

**Times** (`now` = `state.clock.now_ns()`, read once per request):
- `score_id` is UUIDv7 (`uuid.get_version() == Some(uuid::Version::SortRand)`): `created_at` = the id's Unix time (`get_timestamp()` → `to_unix()` → `(secs, nanos)`, computed as `i64::try_from(secs)` then `checked_mul(1_000_000_000)` then `checked_add(nanos)`; any overflow is the field error `score_id time is in the future`; v7 has millisecond precision). `updated_at = max(now, created_at)`. No lookup.
- A v7 time older than the project's retention (`project.retention_days` is `Some(d)` and `created_at < now - d·86 400·10⁹`, computed with saturating arithmetic) is the field error `scores[i].score_id`: `"score_id time is older than the project's retention"` (storage would drop such a row at flush, spec 03 §8).
- any other id: it goes into one `llmobs_storage::scores_by_id` call per request (G3.1). Found: `created_at` = the live version's `created_at`, `updated_at = max(now, live.updated_at + 1)`. Not found: `created_at = updated_at = now`.
- `ingest_time = now`, `deleted = false`. `evaluator_id`, `rule_id`, `experiment_id`, `dataset_item_id` are `None` (track I sets them).

**Delete** (`DELETE …/scores/{score_id}`): `scores_by_id` for that one id (a lookup error is 503 as in `write` step 5); none → 404. Otherwise append a copy of the live `Score` with `deleted = true`, `updated_at = max(now, live.updated_at + 1)`, `ingest_time = now`; every other field unchanged, so `created_at` stays (spec 03 §5).

**Append.** `llmobs_core::batch::scores_to_record_batch(&rows)` (a `CoreError` is `ApiError::Internal`) then `state.storage.append_scores(project_id, batch)`. `AppendError` maps through the M1 table: `ApiError::from(IngestError::Append(error))` (503 for backpressure, WAL failure, disk full and shutdown; 404 for a dropped project; 500 otherwise).

### Request and response types (G3.2)

```rust
/// One score to write. Spec 06 § Scores, D-F-1, D-G-28.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ScoreCreate {
    /// Client id; a UUIDv7 gives `created_at` from its time (D-G-26). Derived when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score_id: Option<Uuid>,
    pub target: ScoreTarget,                 // llmobs_core::score::ScoreTarget
    /// Session of a span or trace target. Ignored for a session target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    pub name: String,
    pub value: ScoreValue,                   // llmobs_core::score::ScoreValue
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<ScoreSource>,         // llmobs_core::enums::ScoreSource
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, String>,
}

/// `POST …/scores` and `POST /api/v1/scores` body.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ScoreBatch { pub scores: Vec<ScoreCreate> }

/// Answer of a score write: one id per input item, in input order.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
pub struct ScoreWriteResult { pub score_ids: Vec<Uuid> }

/// `POST /api/public/feedback` body (spec 06 § End-user feedback).
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct FeedbackRequest {
    pub trace_id: TraceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span_id: Option<SpanId>,
    /// Default `user_feedback`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub value: FeedbackValue,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

/// A number, a boolean or a string.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, ToSchema)]
#[serde(untagged)]
pub enum FeedbackValue { Boolean(bool), Number(f64), Text(String) }

/// Answer of `POST /api/public/feedback`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
pub struct FeedbackResult { pub score_id: Uuid }
```

Every field gets a `///` doc line (the OpenAPI lint checks descriptions); the comments above are the text to use.

**Feedback → `ScoreCreate`:** target `Span { trace_id, span_id }` when `span_id` is set, else `Trace { trace_id }`; `name` = the body's or `"user_feedback"`; value `Boolean(b)` → `ScoreValue::Boolean(b)`, `Number(n)` → `ScoreValue::Numeric(n)` (serde_json never yields a non-finite number, so no check), `Text(s)` → `ScoreValue::Categorical { label: s, value: None }`; `reason` = `comment`; no `score_id` (derived, author = the key id); `source` `end_user`.

### Usage counters (D-G-11, D-G-30)

| Counted by | `spans` | `bytes` | `scores` |
|---|---|---|---|
| `Ingest::commit` (HTTP and gRPC) | spans appended | `CommitTarget.request_bytes` | 0 |
| score writes (all three routes) | 0 | 0 | rows appended |
| score delete | 0 | 0 | 0 |

`request_bytes` is the decompressed body length for OTLP/HTTP (`Accepted.decompressed_bytes`) and the decoded message's `encoded_len()` for gRPC. The day is the UTC date of the request's `now`, as `YYYY-MM-DD`.

### gRPC status mapping (D-G-29)

| Cause | `tonic::Code` | Message (static) |
|---|---|---|
| no or invalid `authorization` | `Unauthenticated` | `missing or invalid credentials` |
| a PAT, a key without `ingest` | `PermissionDenied` | `credentials do not allow ingest` |
| project or org not active | `NotFound` | `project not found` |
| budget full; `Backpressure`, `WalFailed`, `ShuttingDown`, `DiskFull`; `Media(Object \| Catalog \| Query \| DiskFull)` | `Unavailable` | `server is busy, retry later` |
| more than `max_spans_per_request` spans; `Media(Invalid)` | `InvalidArgument` | `too many spans in one request` / `invalid media in the request` |
| `ProjectDropped` (append or media) | `NotFound` | `project not found` |
| anything else | `Internal` | `internal error` |

A message larger than `ingest.max_grpc_message_bytes` is refused by tonic itself: `OutOfRange` when its wire length is over the limit, `ResourceExhausted` when a compressed message decompresses past it (tonic 0.14.9 `codec/decode.rs`).

## G3.1 — Storage: `scores_by_id`

**Files:** `crates/storage/src/reads/scores.rs`, `crates/storage/src/reads/mod.rs`, `crates/storage/src/lib.rs` (the `pub use` list). This edits A2's directory; the user approved it for M3.

### Steps

1. In `reads/scores.rs`, add:
   ```rust
   /// The live version of each score in `ids` (scores-view columns), in no particular order.
   ///
   /// Ids are UUID strings, either case. An empty list returns no batches and runs no query.
   ///
   /// # Errors
   ///
   /// [`RunError::InvalidRequest`] for more than [`MAX_PAGE`] ids or an id that is not a UUID.
   /// [`RunError::Timeout`] when the query exceeds `timeout`.
   pub async fn scores_by_id(
       ctx: &QueryContext,
       ids: &[&str],
       timeout: Duration,
   ) -> Result<Vec<RecordBatch>, RunError>
   ```
   - More than `MAX_PAGE` ids → `InvalidRequest("at most 1000 ids")`. Each id is parsed with `uuid::Uuid::parse_str` (`uuid` is a dependency of `llmobs-storage`; check `crates/storage/Cargo.toml`, and if it is not, use the `llmobs_core` re-export or STOP); a failure → `InvalidRequest("invalid score_id")`. The literal compared is `uuid.to_string()` (lowercase hyphenated, as `scores_to_record_batch` stores it).
   - Query: `ctx.session().table("scores")` → `.filter(col(scores::SCORE_ID).in_list(<literals>, false))` → `.collect()`, under `tokio::time::timeout(timeout, …)` like `latest_scores`; DataFusion errors go through `classify` / `ctx.fail` exactly as `latest` does. The `scores` view already keeps one live row per `score_id` (last writer wins, deleted rows dropped).
2. Re-export `scores_by_id` from `reads/mod.rs` and in `lib.rs`'s `pub use reads::{…}` list (keep it sorted).

### Tests (in `reads/scores.rs`'s test module, on the `test_support` dataset A2's other score tests use)

- The ids of two dataset scores (one of them `…0302`) → two rows; their `score_id`, `name` and `created_at` equal the dataset's.
- Uppercase ids give the same rows.
- An unknown UUID → no rows; an empty list → `Ok(vec![])`.
- `"nope"` → `InvalidRequest`; 1001 ids → `InvalidRequest`.

### Done when

`just check` passes. Commit: `storage: read live scores by id`.

## G3.2 — Score write contract

**Files:** `crates/server/src/api/scores.rs`, `crates/server/src/api/openapi.rs`, `crates/server/tests/{openapi.rs,api_examples.rs}`, `crates/server/openapi.json` and `web/src/api/schema.gen.ts` (regenerated), `fixtures/api/requests/` (new), `fixtures/api/README.md`.

### Steps

1. **Types.** Add the types of § Request and response types to `api/scores.rs` (it holds `ScoreListParams` today), with the doc comments given.
2. **Operations.** In `api/openapi.rs`, in `score_paths` (and register the new schema names wherever `CreateSavedView` / `SavedView` are registered — follow that pattern exactly, including `collect_schemas`):

   | Operation id | Method, path | Request body | Success | Errors | Security |
   |---|---|---|---|---|---|
   | `create_scores` | `POST {PROJECT}/scores` | `ScoreBatch` | `200` `ScoreWriteResult` | `400, 401, 403, 404, 413, 429, 503` | default (session or bearer) |
   | `delete_score` | `DELETE {PROJECT}/scores/{score_id}` | — | `204` | `400, 401, 403, 404, 429, 503` | default |
   | `create_key_scores` | `POST /api/v1/scores` | `ScoreBatch` | `200` `ScoreWriteResult` | `400, 401, 403, 404, 413, 429, 503` | bearer only |
   | `post_feedback` | `POST /api/public/feedback` | `FeedbackRequest` | `200` `FeedbackResult` | `400, 401, 403, 404, 413, 429` | bearer only |

   - All four use the existing tag `"scores"`; add no tag.
   - `{score_id}` is a path parameter declared like `path_uuid("score_id", "Score id.")`.
   - The `/scores` path item already has `GET list_scores`; add `post` to the same `PathItem` (as `trace_paths` does with several methods on one item, or as the saved views do).
   - Summaries: "Write scores", "Delete a score", "Write scores with an ingest key", "Send end-user feedback". Descriptions: one sentence each, stating the batch size (1–1000) and that the answer has one id per item in order; for `create_key_scores` "The project comes from the `sk` key, which needs the `ingest` scope."; for `post_feedback` "Needs a `pk` key with the `feedback` scope. CORS allows any origin. At most 4 KiB, 10 requests per second per key and client IP."
   - "Bearer only" security: an operation-level security list with the one `bearer` requirement. Find how session-only operations set theirs and mirror it with `bearer`.
   - Add `413 => "The request body is larger than the limit."` to `problem()`'s descriptions if it is not there.
   - The `bearer` security scheme's description lists the accepted prefixes; add `pk-llmobs-` (public keys, feedback only).
3. **Lint.** In `crates/server/tests/openapi.rs`:
   - add the four `(method, path, id)` rows to `OPERATIONS`;
   - in `document_lint`, add a bearer-only security branch for `create_key_scores` and `post_feedback` (`security` is a one-element list whose element has `bearer` and not `session`), and an expected 4xx set per new operation, each in its own `else if` before the generic branches: `create_scores` and `create_key_scores` {400, 401, 403, 404, 413, 429}; `delete_score` {400, 401, 403, 404, 429}; `post_feedback` {400, 401, 403, 404, 413, 429}.
   - In `crates/server/src/http/access.rs`'s `route_table_matches_parameters_and_openapi_security`, nothing changes yet (no route is added in G3.2).
4. **Request examples.** Create `fixtures/api/requests/score-batch.json` with exactly this body (the SDK example of `docs/plans/f-sdk/README.md` § Score request, with fixed ids):
   ```json
   {
     "scores": [
       {
         "score_id": "01900000-0000-7000-8000-000000000401",
         "target": {"type": "span", "trace_id": "a1000000000000000000000000000001", "span_id": "b100000000000012"},
         "name": "accuracy",
         "value": {"data_type": "numeric", "value": 0.8},
         "reason": "Matches the reference answer.",
         "source": "sdk",
         "user_id": "user-42",
         "metadata": {"k": "v"}
       }
     ]
   }
   ```
   and `fixtures/api/requests/feedback.json`:
   ```json
   {"trace_id": "a1000000000000000000000000000001", "value": true, "comment": "Helpful."}
   ```
   These show the content only. The committed files are the canonical text: parse each JSON above into its Rust type (`ScoreBatch`, `FeedbackRequest`) once and write `canonical_json(&value)` (`serde_json::to_string_pretty` plus a trailing newline, keys in struct-field order, as `tests/support/mod.rs` defines it). The test then compares text.
5. **Example checks.** In `crates/server/tests/api_examples.rs`, add `#[test] fn request_examples_match_their_types_and_the_openapi_document()`: each file round-trips through its type (`roundtrip::<ScoreBatch>`, `roundtrip::<FeedbackRequest>`), and validates against the operation's `requestBody.content["application/json"].schema` (`create_key_scores` and `post_feedback`), using the same `jsonschema` registry code as `examples_match_the_openapi_document`. Also extend `every_fixture_file_is_used` so `requests/` files count as used when this test names them (simplest: it ignores `requests/`; say so in a comment).
6. **README.** `fixtures/api/README.md` § Layout gains a row: `requests/*.json` | hand | example request bodies, checked against their types and `openapi.json`.
7. **Regenerate.** `export PATH="$HOME/.local/share/nvm/v26.13.0/bin:$PATH" && just openapi`. Commit the regenerated files; never hand-edit them.

### Tests

The two contract tests above; `cargo test -p llmobs-server --test openapi` passes.

### Done when

`just check` passes. Commit: `server: declare the score write and feedback contract`.

## G3.3 — Score write core

**Files:** `crates/server/src/{lib.rs,scores.rs}` (new `scores.rs`), `crates/server/src/auth/roles.rs`, `crates/server/src/http/access_suite.rs` (only if the new permission needs it).

### Steps

1. **`ProjectPerm::Annotate`** in `auth/roles.rs`: doc `/// Write and delete scores (spec 08: member and above).`; `min_role` → `Role::Member`; `key_scope` → `Some(KeyScope::Write)`. Update every exhaustive `match` the compiler reports (only `roles.rs` has them; the access suite uses `min_role()` and `key_scope()`), and add `Annotate` to the `permissions_follow_the_spec_table` test in `roles.rs`: viewer no, member, admin and owner yes.
2. **`crate::scores`** (`pub(crate) mod scores;` in `lib.rs`), with:
   ```rust
   /// Who is writing, which decides `source` and the author (D-G-27).
   #[derive(Debug, Clone, Copy)]
   pub(crate) enum Writer {
       /// A web session on the project route.
       Human { user_id: Uuid },
       /// A PAT (`token_id`) or `sk` key (`key_id`) on the project route.
       Api { author: Uuid },
       /// An `sk` key on `POST /api/v1/scores`.
       Sdk { key_id: Uuid },
       /// A `pk` key on `POST /api/public/feedback`.
       EndUser { key_id: Uuid },
   }

   /// Validates `items`, resolves ids and times, appends the rows, and returns one id per item.
   pub(crate) async fn write(
       state: &AppState,
       project: &ProjectRow,
       writer: Writer,
       items: Vec<ScoreCreate>,
   ) -> Result<Vec<Uuid>, ApiError>

   /// Appends a deleted copy of the live score `score_id` (spec 03 §5). `NotFound` when none.
   pub(crate) async fn delete(state: &AppState, project: &ProjectRow, score_id: Uuid) -> Result<(), ApiError>
   ```
   `write` follows § Score write rules, in this order:
   1. `now = state.clock.now_ns()`; count check (`scores` field error).
   2. Per item: source check, length checks, v7 future check → collect every `FieldError` (`crate::api::common::FieldError { field, message, position: None }`); any error → `ApiError::BadRequest { detail: Some("invalid scores".to_owned()), errors }`.
   3. Per item: final `score_id` (given, or `derive_id(project.id, &item, source, author)`).
   4. Dedupe: keep the last item per final id; remember the per-input id list for the answer.
   5. Non-v7 ids → one `llmobs_storage::scores_by_id` call (skip when empty) on `crate::routes::read::open_query(state, project.id, None)`, timeout `READ_TIMEOUT`; convert with `crate::api::convert::scores(&batches, project.id)`; build `HashMap<Uuid, Score>`. A `RunError` from the lookup (timeout, memory, internal) becomes `ApiError::Unavailable { detail: "score lookup is unavailable, retry later" }` (503, so the SDKs retry; no 422 on write routes), logged with `RunError::kind()`; `open_query`'s own errors keep their mapping (503 busy).
   6. Build each `Score` (fields of `ScoreCreate` plus the resolved times, `source`, `author_user_id`, `project_id = project.id`). For a session target, `session_id` is `None` (the target holds it).
   7. Append (one batch); count usage — **not in G3.3**: G3.6 adds the counting call here.
   8. Return the per-input ids.

   `derive_id` and the v7 time helper are private functions with their own unit tests.
3. No route yet. Mark the module `#![cfg_attr(not(test), allow(dead_code))]` until G3.4 calls it (remove that line in G3.4).

### Tests (in `scores.rs`, on `TestApp` + `seed()`)

- **Derived ids.** `derive_id` for a fixed input equals a constant written in the test (compute it once, then pin it); it is version 8 (`get_version_num() == 8`); changing any one of the seven fields changes it.
- **v7 time.** For `01900000-0000-7000-8000-000000000401`, `created_at` = the id's ms time × 10⁶; a v7 id more than 1 h after `now` is a field error; at exactly `now + 1 h` it is accepted.
- **New score.** Write one v4-id score (`Uuid::from_u128(…)` with version 4 bits, or `Uuid::new_v4()` if the `v4` feature is enabled — check; otherwise build the bytes with `uuid::Builder::from_random_bytes`) → read back with `llmobs_storage::score_list`: one row, `created_at == updated_at == now`, `source` per writer.
- **Update keeps `created_at`.** Write it again with another value after `state.clock.advance(Duration::from_secs(60))` → one live row, new value, `created_at` unchanged, `updated_at` = the later `now`.
- **Delete.** `delete` it → `score_list` returns no row; deleting again → `NotFound`.
- **Sources.** `Writer::Human` with body source `api` → 400 with field `scores[0].source`; `Writer::Sdk` with `human` → 400; `Writer::Api` with `sdk` → stored `sdk`; `Writer::Human` stores `author_user_id`.
- **Validation.** Empty name, 256-byte name, empty session id, 0 and 1001 items: each the field and message of the table.
- **Duplicates.** Two items with the same `score_id` → one stored row (the second item's value); the answer has the id twice.
- **Derived id overwrite.** Two writes without `score_id`, same target/name/writer → one live row (the second), and the same id returned both times.

### Done when

`just check` passes. Commit: `server: add the score write core`.

## G3.4 — Score routes

**Files:** `crates/server/src/routes/scores.rs`, `crates/server/src/scores.rs`, `crates/server/src/http/{access.rs,access_suite.rs}`.

### Steps

1. Routes, in `routes/scores.rs`'s `routes()` next to `list_scores`:

   | Method | Path | Access | Handler |
   |---|---|---|---|
   | POST | `/api/v1/projects/{project}/scores` | `Access::Project { perm: ProjectPerm::Annotate, session_only: false }` | `create_scores` |
   | DELETE | `/api/v1/projects/{project}/scores/{score_id}` | same | `delete_score` |
   | POST | `/api/v1/scores` | `Access::Key { scope: KeyScope::Ingest }` | `create_key_scores` |

   All `documented: true`. The two POST handlers get a larger body limit: `post(handler).layer(DefaultBodyLimit::max(SCORE_BODY_LIMIT))` with `const SCORE_BODY_LIMIT: usize = 10 * 1024 * 1024;` (`axum::extract::DefaultBodyLimit`), doc `/// Score batches: up to 1000 items with reasons and metadata (D-G-28).`
2. Handlers:
   - `create_scores(State, Extension<Principal>, Extension<ProjectAccess>, error::Json<ScoreBatch>)`: `Writer` from the principal (`Session { user }` → `Human { user_id: user.id }`; `Pat { token_id }` → `Api { author: token_id }`; `ApiKey { key_id }` → `Api { author: key_id }`); `scores::write(…)` → `200` `ScoreWriteResult`.
   - `delete_score(…, error::Path<ScorePath { score_id: Uuid }>)` → `scores::delete` → `StatusCode::NO_CONTENT`.
   - `create_key_scores(State, Extension<Principal>, Extension<ProjectAccess>, error::Json<ScoreBatch>)`: the principal is an `ApiKey` (the access layer guarantees it; any other principal → `ApiError::Forbidden { code: ProblemCode::Forbidden }`) → `Writer::Sdk { key_id }`.
   - Log one `tracing::info!` per write with `project_id`, `count` and the writer kind (`human`, `api`, `sdk`) only.
3. Keep `crate::scores`'s `dead_code` allowance: `Writer::EndUser` has no caller until G3.5, which removes the allowance.
4. `http/access.rs` test `route_table_matches_parameters_and_openapi_security`: the `Access::Key { .. }` arm now expects `Some(vec![SecurityRequirement::new("bearer", [] as [&str; 0])])` for documented key routes; replace the M1 comment with `// Key routes are bearer-only (D-G-15).` The undocumented `/v1/traces` is skipped by the `documented` check.
5. `http/access_suite.rs` `concrete_path`: add `.replace("{score_id}", "01900000-0000-7000-8000-000000009999")` before `{id}`.

### Tests (in `routes/scores.rs`)

- **SDK body.** `POST /api/v1/scores` with the seeded `ingest_key` and the bytes of `fixtures/api/requests/score-batch.json` → 200 `{"score_ids": ["01900000-0000-7000-8000-000000000401"]}`; `GET …/scores` returns it with `source: "sdk"`, the value, reason, user id and metadata.
- **Key route refusals.** The `read_key` → 403; a session cookie → 403; no credential → 401.
- **Project route.** Session cookie (`Origin` set by `TestRequest` for POST) → `source: "human"`, `author_user_id` = the seeded user's id (`Seeded` has no user id: read it with `crate::meta::users::user_by_email(app.state.meta.reader(), "ada@example.com")`, as `add_project` does); the `read_key` → 403. Viewers and `write` keys are covered by the access suite, and the `Api` writer by G3.3's unit tests.
- **Delete.** `DELETE …/scores/<id>` → 204, then `GET …/scores` no longer lists it; an unknown id → 404; `…/scores/nope` → 400.
- **Body limit.** A 2 MiB batch (1000 items with a 2 KiB `reason`) → 200 (the router's 1 MiB default does not apply); 11 MiB → 413.
- **Storage refusal.** Not tested here (covered by M1's mapping).

### Done when

`just check` passes; the access suite covers the three routes. Commit: `server: serve score writes and deletes`.

## G3.5 — Public feedback

**Files:** `crates/server/src/routes/{mod.rs,feedback.rs}` (new `feedback.rs`), `crates/server/src/auth/limits.rs`, `crates/server/src/http/access.rs`.

### Steps

1. **Limiter.** `auth/limits.rs` `Limits` gains `/// 10 requests per second per (key id, client IP) on public feedback (D-G-16).` `pub(crate) feedback: KeyedLimiter<(Uuid, IpAddr)>` with `Bucket { capacity: 10, per: Duration::from_secs(1) }` and `MAX_KEYS`; `Limits::prune` prunes it too.
2. **Routes** (`routes/feedback.rs`, registered in `access::routes()` after the score routes):

   | Method | Path | Access | Documented | Handler |
   |---|---|---|---|---|
   | POST | `/api/public/feedback` | `Access::Key { scope: KeyScope::Feedback }` | true | `post_feedback`, layered with `DefaultBodyLimit::max(4096)` |
   | OPTIONS | `/api/public/feedback` | `Access::Public` | false | `preflight` |
3. **`post_feedback(State, Extension<Principal>, Extension<ProjectAccess>, ClientIp(ip), error::Json<FeedbackRequest>)`:**
   1. Principal must be `ApiKey { key_id, .. }` (else 403 as in G3.4).
   2. `state.limits.feedback.check(&(key_id, ip), now)` → `Err(RetryAfter(d))` → `ApiError::RateLimited { retry_after_s: d.as_secs().max(1) }`.
   3. Build the `ScoreCreate` (§ Feedback → `ScoreCreate`) and call `scores::write` with `Writer::EndUser { key_id }`. In a `BadRequest` from `write`, rewrite each field `scores[0].<rest>` to `<rest>` (`scores[0].name` → `name`, `scores[0].value` → `value`), so the errors name the feedback body's fields.
   4. Answer `200` `FeedbackResult { score_id }`. Every answer of the handler, success or error (400, 429, 503), carries `Access-Control-Allow-Origin: *`: build the `Response` (`result.into_response()` for both arms) and insert the header.
5. Remove `crate::scores`'s `dead_code` allowance (every `Writer` variant now has a caller).
6. **`preflight`** → `204` with `Access-Control-Allow-Origin: *`, `Access-Control-Allow-Methods: POST, OPTIONS`, `Access-Control-Allow-Headers: Authorization, Content-Type`, `Access-Control-Max-Age: 600`.
7. Errors from the access layer (401, 403) and from the body limit carry no CORS header; the browser then sees a network failure and `feedback()` resolves `{ ok: false }`, which is its contract (D-F-16). Write that as the module doc's last sentence.
8. `route_table_matches_parameters_and_openapi_security` skips the OPTIONS row (undocumented), and its method `match` has no OPTIONS arm: confirm the `documented` check comes first (it does today) and change nothing else.

### Tests (in `routes/feedback.rs`)

- The bytes of `fixtures/api/requests/feedback.json` with `Bearer <public_key>` → 200, `access-control-allow-origin: *`; `GET …/scores` (cookie) has one row: target trace 1… (the seeded project has no dataset; the trace need not exist), `name: "user_feedback"`, `value: boolean true`, `reason: "Helpful."`, `source: "end_user"`.
- Same body twice → still one live row (derived id, same author) and the same `score_id`.
- `{"value": "great", "trace_id": …, "span_id": …, "name": "rating"}` → categorical `great` on a span target.
- `OPTIONS /api/public/feedback` without credentials → 204 and the four headers.
- An `sk` ingest key → 403; no credential → 401.
- A 5000-byte body → 413.
- A 400 (empty `name`) has `access-control-allow-origin: *` and its field error is `name`.
- Rate limit: 10 requests in a row → 200, the 11th → 429 with `Retry-After`; from another `peer` IP → 200.
- `assert_logs_clean(&["Helpful."], …)` around one request.

### Done when

`just check` passes. Commit: `server: serve public feedback`.

## G3.6 — Usage counters

**Files:** `crates/server/migrations/meta/0002_usage.sql` (new), `crates/server/src/{lib.rs,usage.rs,scores.rs,serve.rs,admin.rs}` (new `usage.rs`), `crates/server/src/meta/mod.rs` (tests), `crates/server/src/http/mod.rs`, `crates/server/src/ingest/{mod.rs,fixtures_tests.rs}`, `crates/server/src/routes/otlp.rs`, `crates/server/src/test_support/mod.rs`, `crates/server/tests/snapshots/meta_schema.snap` (re-accepted), `crates/server/Cargo.toml` (`chrono`).

### Steps

1. **Migration** `0002_usage.sql`: the `usage_daily` table of spec 04 § Metering, verbatim. Then fix the tests that know there is one migration:
   - `meta/mod.rs` `open_twice_is_idempotent`: the `_sqlx_migrations` count becomes 2;
   - `meta/mod.rs` `open_existing_checks_the_file_and_the_version`: insert fake version **3** (version 2 now exists) and expect found 3, expected 2;
   - `admin.rs` `schema_mismatch_names_the_versions`: insert version 3 and expect `this binary expects schema 2, found 3`.

   Re-accept the snapshot: `INSTA_UPDATE=always cargo test -p llmobs-server --lib meta::tests::schema_matches_the_snapshot` (or `cargo insta accept` when `cargo-insta` is installed); the diff of `crates/server/tests/snapshots/meta_schema.snap` adds only `usage_daily` and its `sqlite_autoindex_usage_daily_1`.
2. **`crate::usage`**, modelled on `auth/last_used.rs`:
   ```rust
   #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
   struct UsageKey { org_id: OrgId, project_id: ProjectId, day: NaiveDate }
   #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
   struct UsageCounts { spans: u64, bytes: u64, scores: u64 }

   /// In-memory usage counters, upserted into `usage_daily` (D-G-11).
   #[derive(Debug, Default)]
   pub(crate) struct Usage { counts: Mutex<HashMap<UsageKey, UsageCounts>> }

   impl Usage {
       pub(crate) fn add(&self, org_id: OrgId, project_id: ProjectId, now_ns: i64, spans: u64, bytes: u64, scores: u64);
       /// Upserts and clears the counts; a failed write adds them back.
       pub(crate) async fn flush(&self, meta: &MetaDb) -> Result<(), MetaError>;
   }
   ```
   - `add` with all three zero does nothing. Counts saturate (`saturating_add`).
   - Day: `chrono::DateTime::from_timestamp_nanos(now_ns).date_naive()`; stored as `day.format("%Y-%m-%d").to_string()`.
   - `flush`: take the map (as `LastUsed::flush`), then in one `meta.write` run per row `INSERT INTO usage_daily (org_id, project_id, day, spans, bytes, scores) VALUES (?, ?, ?, ?, ?, ?) ON CONFLICT (org_id, project_id, day) DO UPDATE SET spans = spans + excluded.spans, bytes = bytes + excluded.bytes, scores = scores + excluded.scores`. Bind counts as `i64::try_from(…).unwrap_or(i64::MAX)`. On error, add every taken row back with `add`-like merging, then return the error.
3. **State.** `AppInner.usage: crate::usage::Usage` (doc `/// Usage counters, flushed every 10 s (D-G-11).`), `Usage::default()` in `serve.rs` and `test_support`.
4. **`CommitTarget`** in `ingest/mod.rs` (design § Commit API):
   ```rust
   /// Where a commit goes and what it counts (D-G-8, D-G-30).
   #[derive(Debug, Clone, Copy)]
   pub(crate) struct CommitTarget {
       pub(crate) org_id: OrgId,
       pub(crate) project_id: ProjectId,
       /// Bytes counted in `usage_daily.bytes`. In-process commits pass 0.
       pub(crate) request_bytes: u64,
       /// The request's `now`; its UTC day is the usage day.
       pub(crate) now_ns: i64,
   }
   ```
   `commit(&self, storage: &Storage, usage: &Usage, target: CommitTarget, normalized: Normalized)`. In `routes/otlp.rs`, read `now = state.clock.now_ns()` once and pass the same value to `accept` and to `CommitTarget.now_ns`. After a successful append (and also when no span was appended, so bytes are still counted), call `usage.add(target.org_id, target.project_id, target.now_ns, accepted as u64, target.request_bytes, 0)`. Update every caller (`routes/otlp.rs` with `Accepted.decompressed_bytes`, `ingest/fixtures_tests.rs`, the ingest tests). Update the `// M3:` comments of the design's Commit API in code docs if any reference it.
5. **Score writes** (`scores::write`): after the append, `state.usage.add(project.org_id, project.id, now, 0, 0, rows as u64)`. Delete counts nothing.
6. **Flush task.** `serve.rs`: `const USAGE_EVERY: Duration = Duration::from_secs(10);` and `spawn_usage(state)` like `spawn_last_used` (error log: `"usage flush failed"`, with `error = %error`; `MetaError`'s display holds no user content); add it to `tasks`. In `Running::shutdown`, flush `state.usage` once more right after the two `last_used` flushes.

### Tests

- `usage.rs` unit tests (with `MetaDb::open` on a temp dir, as other meta tests do): two `add`s for the same key and one for another day → `flush` → two rows with summed counts; a second `flush` with new counts adds to the stored row (`spans` 3 + 2 = 5); `add(…, 0, 0, 0)` writes nothing.
- `routes/otlp.rs`: one export of a JSON fixture → after `state.usage.flush(&state.meta)`, `SELECT spans, bytes FROM usage_daily` has the accepted span count and the decompressed size, day `2026-01-01` (the manual clock).
- `routes/scores.rs`: a 2-item SDK batch → `scores = 2`; a delete adds nothing.
- `serve.rs`: extend `shutdown_flushes_last_used` (or add a sibling test) so a span ingested over TCP appears in `usage_daily` after `shutdown`.

### Done when

`just check` passes. Commit: `server: count usage per org, project and day`.

## G3.7 — gRPC dependencies, config, and the split `accept`

**Files:** root `Cargo.toml`, `Cargo.lock`, `crates/server/Cargo.toml`, `crates/server/src/{config.rs,ingest/mod.rs,ingest/fixtures_tests.rs,serve.rs}`, `crates/cli/src/login.rs` (its test TOML), `crates/server/src/test_support/mod.rs`, `docs/spec/12-operations.md` only if a key text differs from what is implemented.

### Steps

1. **Dependencies** per § Versions. `tonic.workspace = true` in `crates/server/Cargo.toml`. Run `cargo deny check` (STOP rule).
2. **Config** (mirror `otlp_http_addr` exactly):
   - `grpc_addr: Option<SocketAddr>`, default `Some(0.0.0.0:4317)`, `deserialize_optional_socket_addr`, env `LLMOBS_GRPC_ADDR` in the env table; doc `/// OTLP/gRPC listener (D-G-21, D-G-29). \`None\` when the value is an empty string.`
   - Validation: `grpc_addr` must differ from `http_addr` and from `otlp_http_addr` (`"must differ from http_addr"`, `"must differ from otlp_http_addr"`), checked next to the existing `otlp_http_addr` check.
   - `IngestConfig.max_grpc_message_bytes: ByteSize`, default `32MiB`, doc `/// Decoded gRPC message cap. Default 32 MiB.`; `validate_ingest`: in the platform-size list; between 1 KiB and 1 GiB (`"must be between 1KiB and 1GiB"`); at most `max_inflight_bytes` (`"must be at most max_inflight_bytes"`), checked **after** the existing `max_inflight_bytes` check so its tests still hit their own message first.
   - Existing literals and tests to update: the default-equality test near `config.rs` line 2150 gains the new field; `ingest_and_media_accept_the_edges` (inflight `1KiB`) adds `max_grpc_message_bytes = "1KiB"` to its TOML; the `ServerConfig` literals in `serve.rs`'s test `config()` and `ingest/fixtures_tests.rs` get `grpc_addr: None`; `serve.rs`'s `empty_otlp_address_in_toml_binds_one_listener` TOML adds `grpc_addr = ""` (otherwise it binds 4317).
   - `test_support::default_config` sets `grpc_addr: None` (comment `// Tests must not bind the gRPC port (4317).`); the CLI login test TOML gets `grpc_addr = ""`.
3. **Split `accept`.** Extract the work after decoding into:
   ```rust
   /// Counts, normalizes and rewrites a decoded request (D-G-2, D-G-7). Used by OTLP/HTTP after
   /// decoding and by gRPC, whose request tonic decodes.
   pub(crate) async fn accept_request(
       &self,
       request: ExportTraceServiceRequest,
       reservation: Reservation,
       request_bytes: u64,
       decompressed_bytes: u64,
       project: &ProjectRow,
       now_ns: i64,
   ) -> Result<Accepted, IngestError>
   ```
   It runs the span count, `normalize` and `rewrite_reserved_environment` in `spawn_blocking`, exactly as `accept` does today. `accept` keeps its signature: it still reconciles the reservation, decompresses and decodes in `spawn_blocking`, then calls `accept_request` (two blocking hops instead of one is accepted). Behaviour and the error mapping do not change; every existing ingest test must pass unchanged.
4. Add `pub(crate) fn reserve_exact(&self, bytes: u64) -> Result<Reservation, IngestError>` for gRPC: reserves `max(bytes, 1)` (a value over `u32::MAX` is `Busy`), never waiting.

### Tests

- Config: default `grpc_addr` is `0.0.0.0:4317`; empty string and `LLMOBS_GRPC_ADDR=""` disable it; equal to `http_addr` is an error; `max_grpc_message_bytes` bounds (512 B, 2 GiB, more than `max_inflight_bytes`) are errors with their messages.
- `accept_request` on a decoded fixture request gives the same `Normalized` as `accept` on its bytes.
- `reserve_exact(u64::MAX)` is `Busy`.

### Done when

`just check` passes. Commit: `server: add gRPC config and split ingest accept`.

## G3.8 — OTLP/gRPC

**Files:** `crates/server/src/ingest/{mod.rs,grpc.rs,otlp.rs}` (new `grpc.rs`), `crates/server/src/http/access.rs`, `crates/server/src/auth/principal.rs`, `crates/server/src/serve.rs`.

### Steps

1. **Shared auth pieces.** In `auth/principal.rs`, make `bearer_principal` `pub(crate)`. In `http/access.rs`, extract the body of `grant_key_only_project` into `pub(crate) async fn key_only_project(state: &AppState, project_id: ProjectId) -> Result<ProjectRow, ApiError>` and make `grant_key_only_project` call it.
2. **`ingest/grpc.rs`:**
   ```rust
   //! OTLP/gRPC `TraceService/Export` (spec 02 § Endpoints, D-G-29).

   /// The tonic service. Cloning shares the state.
   #[derive(Clone)]
   pub(crate) struct GrpcTraces { pub(crate) state: AppState }

   #[tonic::async_trait]
   impl TraceService for GrpcTraces {
       async fn export(&self, request: tonic::Request<ExportTraceServiceRequest>)
           -> Result<tonic::Response<ExportTraceServiceResponse>, tonic::Status>;
   }

   /// The service with its limits and compressions.
   pub(crate) fn service(state: AppState) -> TraceServiceServer<GrpcTraces>
   ```
   - `service`: `TraceServiceServer::new(GrpcTraces { state })` `.max_decoding_message_size(limit)` with `limit = usize::try_from(config.ingest.max_grpc_message_bytes.as_u64()).unwrap_or(usize::MAX)` (validated in G3.7) and `.accept_compressed(tonic::codec::CompressionEncoding::Gzip)`. `GrpcTraces` needs a manual `Debug` that prints nothing of the state (`missing_debug_implementations`).
   - `export`, steps (each failure maps per § gRPC status mapping; for `ApiError`s from `bearer_principal` / `key_only_project`: `Unauthorized` → `Unauthenticated`, `Forbidden` → `PermissionDenied`, `NotFound` → `NotFound`, anything else → `Internal`; read `now = state.clock.now_ns()` once at the start):
     1. `headers = request.metadata().clone().into_headers()`; `bearer_principal(&state, &headers)`; the principal must be `ApiKey` with `scopes.contains(KeyScope::Ingest)`.
     2. `project = key_only_project(&state, project_id)`.
     3. `message = request.into_inner()`; `bytes = message.encoded_len() as u64` (`prost::Message`).
     4. `reservation = ingest.reserve_exact(bytes)`.
     5. `accepted = ingest.accept_request(message, reservation, bytes, bytes, &project, now)`.
     6. `ingest.commit(&storage, &usage, CommitTarget { org_id: project.org_id, project_id: project.id, request_bytes: bytes, now_ns: now }, accepted.normalized)`.
     7. Answer `ExportTraceServiceResponse { partial_success }`, built from `accepted.normalized.rejections` with `ingest/otlp.rs`'s private `partial_success(rejections)` helper, made `pub(crate)` (it returns `Option<(rejected_spans, error_message)>`; map it to `ExportTracePartialSuccess` as `export_response` does for protobuf). Read the rejections before `commit` consumes `normalized`.
   - Log like `routes/otlp.rs` does (project id, sizes, counts, duration; refusals by kind), under `transport = "grpc"`.
3. **Listener** in `serve.rs`, mirroring the OTLP/HTTP listener:
   - `bind_listener(grpc_addr)` before either server starts, with the same release-on-error handling.
   - `tonic::transport::Server::builder().add_service(grpc::service(state.clone())).serve_with_incoming_shutdown(tonic::transport::server::TcpIncoming::from(listener), async move { let _ = rx.await; })` in a spawned task; inside the task map its `tonic::transport::Error` to `io::Error::other(error)` so the task is `JoinHandle<io::Result<()>>` and the existing `drain` is reused.
   - `Running.grpc_local_addr: Option<SocketAddr>` (doc `/// The OTLP/gRPC listener's address, when \`grpc_addr\` is set (D-G-21).`), `grpc_serve`, `grpc_shutdown_tx`; shutdown sends all three signals and drains the three tasks together (`tokio::join!`); `Running::drop` aborts `grpc_serve` too.
   - Log `tracing::info!(grpc_local_addr = %addr, "listening")`.

### Tests

- **`grpc.rs` unit tests** (`TestApp` + `seed()`, calling `GrpcTraces { state: app.state.clone() }.export(request)` directly; build the request with `tonic::Request::new(message)` and insert `authorization` with `request.metadata_mut().insert("authorization", format!("Bearer {key}").parse().unwrap())`; the message is `llmobs_otel::decode_json` of `fixtures/otlp/genai/finish-reasons-fill/input.json`):
  - the `ingest_key` → OK with `partial_success: None`; `llmobs_storage` / `GET …/traces` (cookie) then lists the trace; after `state.usage.flush(&state.meta)` `usage_daily.spans` equals the stored span count and `bytes` equals the message's `encoded_len()`;
  - no metadata → `Code::Unauthenticated`; the `read_key` → `Code::PermissionDenied`; a session cookie in `cookie` metadata only → `Code::Unauthenticated`;
  - each `tonic::Status` message is the static text of § gRPC status mapping.
- **`serve.rs` TCP tests** (reuse `with_init`, `OTLP_KEY` and `TRACES`; set `grpc_addr` to `Some(127.0.0.1:0)`), using `opentelemetry_proto::tonic::collector::trace::v1::trace_service_client::TraceServiceClient::connect(format!("http://{addr}"))`:
  - export `decode_json(TRACES)` with `authorization: Bearer OTLP_KEY` → OK;
  - the same with `client.send_compressed(CompressionEncoding::Gzip)` → OK;
  - with `ingest.max_grpc_message_bytes = 1 KiB` and a padded message → `Code::OutOfRange`. Pad it: `decode_json(TRACES)`, then push a 4 KiB `KeyValue { key: "pad".into(), value: Some(AnyValue { value: Some(any_value::Value::StringValue("x".repeat(4096))) }) }` onto the first span's `attributes` (`opentelemetry_proto::tonic::common::v1`);
  - `grpc_addr: None` binds no gRPC listener (`grpc_local_addr` is `None`);
  - extend `shutdown_with_both_listeners_releases_the_lock` so all three listeners are bound and `LOCK` is released after `shutdown`.

### Done when

`just check` passes. Commit: `server: accept OTLP over gRPC`.

## G3.9 — Docs and roadmap

**Files:** `docs/plans/g-spine/00-design.md`, `docs/roadmap.md`.

### Steps

1. `00-design.md` Status line: add "M3 implemented (branch `claude/g-m3-impl`)" after M2's entry.
2. `docs/roadmap.md` G row status: "**In progress.** M1, M2 and M3 implemented." Leave other rows alone.

### Done when

`just check` passes. Commit: `docs: mark G M3 implemented`.

## After M3

- **M4:** the live bus; `commit` gains the `live` argument (design § Commit API).
- **Track I** calls `Ingest::commit` with `CommitTarget { request_bytes: 0, … }` for judge traces and writes judge scores through `crate::scores` (it adds `Writer` variants for `llm_judge`, `check`, `webhook`, which set `rule_id` / `evaluator_id`) and validates values against score definitions.
- **Track N** reads `usage_daily` for quotas and the usage API.
