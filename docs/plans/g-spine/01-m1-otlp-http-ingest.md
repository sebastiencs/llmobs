# G · M1 — OTLP/HTTP ingest end to end

Implementation plan for milestone M1 of track G, split into sub-phases. Each sub-phase is done by one agent in one go, in order, and ends with `just check` green and one commit.

**Prerequisites:** the G design (`docs/plans/g-spine/00-design.md`), its decisions (`docs/mvp-decisions.md` § Track G) and its spec edits are merged. M1 needs none of the A2 and C review fixes (design § Review findings): it touches neither A2's reads nor the web.

**Read first:**
- `AGENTS.md`, all of it.
- `docs/plans/g-spine/00-design.md`: § Decisions, § Architecture (State, Modules, Ingest pipeline, Errors and status codes, Commit API, Limits and configuration, Media), § Testing.
- `docs/spec/02-ingestion.md` § Endpoints, § Limits, § Pipeline, rules 15, 18 and 21, § Media extraction; `docs/spec/03-storage.md` §1, §2 (Backpressure), §9; `docs/spec/08-auth.md` § API keys; `docs/spec/09-api.md` § Conventions; `docs/spec/12-operations.md` § Configuration, § Process lifecycle.
- `docs/mvp-decisions.md`: D90, D103, D105, D108, D111, D135–D142, D150, D151, D161, D-A1-6, D-A1-7, D-G-1 to D-G-10, D-G-15, D-G-21.
- Code, in this order:
  - `crates/server/src/http/{mod.rs,access.rs,access_suite.rs,error.rs}`: `AppState`, the route table, `Access`, `enforce`/`prepare`, `ApiError`;
  - `crates/server/src/auth/{principal.rs,roles.rs}`: `Principal::ApiKey`, `KeyScope`, `KeyScopes`;
  - `crates/server/src/{config.rs,serve.rs,clock.rs}` and `crates/server/src/test_support/mod.rs`;
  - `crates/otel/src/{lib.rs,decode.rs,normalize.rs,reject.rs,error.rs,cost.rs}`: `decode_protobuf`, `decode_json`, `normalize`, `IngestContext`, `IngestLimits`, `Normalized`, `Rejections::message`, `PriceTable`, `builtin_price_rows`;
  - `crates/storage/src/lib.rs` (`append_spans`, `put_media`, `query`), `crates/storage/src/error.rs` (`AppendError`, `MediaError`), `crates/storage/src/media.rs` (`MediaBlob`);
  - `crates/core/src/{batch.rs,media.rs,names.rs,span.rs}`;
  - `fixtures/otlp/README.md` (case layout, `settings`, `context`, `prices.json`).

## What M1 is, and what it is not

When M1 is done, an OTel exporter pointed at `llmobs serve` with an `sk-llmobs-` key that has the `ingest` scope gets `200` from `POST /v1/traces` on port 8080 or 4318, and its spans and media are durable in storage and queryable through `Storage::query`.

**Out of scope:** read endpoints (M2); gRPC, score writes, feedback, usage counters (M3); the live bus and SSE (M4); the UI (M5); per-org rate limits (track N); custom prices (D-G-10). M1 adds none of their fields, types or routes.

## Decisions this plan relies on

| # | Summary |
|---|---|
| D-G-1 | One `AppState`; G adds the `ingest` field. |
| D-G-2 | Per request: capped read, capped decompression, decode + normalize in `spawn_blocking`, media, append, `200` after the WAL. |
| D-G-3 | Overload is `503` with `Retry-After: 1`. |
| D-G-4 | Both body sizes capped at `ingest.max_request_bytes`; span count → `413`; unknown type or encoding → `415`. |
| D-G-5 | `ingest.max_inflight_bytes` budget, refused at once with `503`. |
| D-G-6 | Success is `ExportTraceServiceResponse` in the request's encoding; errors are problem+json; `/v1/traces` is not in the OpenAPI document. |
| D-G-7 | `llmobs-eval` → `llmobs-eval-external` plus the `reserved_environment` warning, in `accept` only. |
| D-G-8 | `accept` then `commit`. |
| D-G-9 | `put_media` (≤ 4 at a time) before `append_spans`; a media failure appends nothing. |
| D-G-10 | Built-in prices in `RwLock<Arc<PriceTable>>`. |
| D-G-15 | `Access::Key { scope }`. |
| D-G-21 | `/v1/traces` on `http_addr` and `otlp_http_addr`. |

## Versions

Add each to the root `[workspace.dependencies]` (with a one-line comment saying why, as the existing entries do), then `dep.workspace = true` in `crates/server/Cargo.toml`. `flate2` and `zstd` are already in `Cargo.lock` through `parquet`: use exactly the locked versions so the tree keeps one copy of each. Check the others with `cargo info <crate>`, take the latest release at least 7 days old, and report the exact versions.

| Crate | Version | Section | Why |
|---|---|---|---|
| `flate2` | `1.1.11` (locked), `default-features = false, features = ["zlib-rs"]` | server | `gzip` and `deflate` request bodies; `zlib-rs` is the backend parquet already uses (`flate2-zlib-rs`), so no `miniz_oxide` is added. Confirm the feature name with `cargo info flate2@1.1.11` |
| `zstd` | `0.13.4` (locked), `default-features = false` | server | `zstd` request bodies |
| `http-body-util` | workspace entry exists (`0.1.5`) | server | `Limited` body reads. Update the root entry's comment, which says it is only a dev-dependency of `llmobs-capture` |
| `bytesize` | workspace entry exists | server (move from dev to normal) | `[ingest]`, `[media]` sizes (D90) |
| `bytes` | workspace entry exists | server | request bodies, media bytes |
| `prost` | workspace entry exists | server | `ExportTraceServiceResponse::encode_to_vec` |
| `opentelemetry-proto` | workspace entry exists | server | `ExportTraceServiceResponse`, `ExportTracePartialSuccess` |
| `hex` | workspace entry exists | server | media hash conversion |
| `futures` | workspace entry exists | server | `buffer_unordered` for media puts; `stream::iter` in G1.4's tests |
| `arrow` | workspace entry exists | server (dev) | golden test: comparing batches |
| `datafusion` | workspace entry exists | server (dev) | golden test: scanning the raw `spans` provider |

If `zstd`'s locked version needs a feature the workspace entry does not enable, add only that feature and say so in the report.

## Conventions for every sub-phase

- **Edit only the files a sub-phase lists**, plus `Cargo.lock`, insta snapshots it creates, and any `ServerConfig { … }` literal that must gain a new field (`rg -n 'ServerConfig \{' crates`).
- **Errors:** no new `thiserror` enum besides `IngestError` (G1.6). Messages and `detail`s carry static text, sizes, counts and ids, never request content, attribute values or keys (AGENTS.md § Privacy).
- **No panics on network input.** Every length and count from a request is checked; arithmetic on them uses checked or saturating operations.
- **`unwrap`/`expect`** only in tests or with an invariant comment.
- **Blocking work** (decompression, decoding, normalization, batch conversion) runs in `tokio::task::spawn_blocking`.
- **Unused code between sub-phases.** `ingest/mod.rs` starts with `#![cfg_attr(not(test), allow(dead_code))]`, like every other server module (`http/access.rs`, `routes/mod.rs`, …), because `body.rs`, `otlp.rs`, `environment.rs` and `Ingest` have no non-test caller until G1.7 and `just lint` denies warnings. The same attribute is already on `config.rs`'s neighbours; add it to any new module that needs it.
- **Log assertions** only see events emitted on the test's own thread (`capture_logs` installs a thread-default subscriber). The ingest code logs only from the handler, never from inside `spawn_blocking`.
- **Logging:** `tracing` with `target: "ingest"`; `info` once per accepted request, `warn` for a refused one (`413`, `415`, `503`), `error` for `500`. Fields: `project_id`, `request_bytes`, `decompressed_bytes`, `spans`, `rejected`, `media`, `duration_ms`, `reason` (a static string). Never a header value, a key, a body byte or an attribute.
- **Tests** live next to the code (`#[cfg(test)] mod tests`), use `crate::test_support::TestApp`, and `#[tokio::test(flavor = "multi_thread")]` whenever the code path calls `spawn_blocking` or storage.
- **`openapi.json` must not change** in M1 (`/v1/traces` is undocumented, D-G-6). `just openapi` leaves the tree clean.
- **Commit message:** the subject given at the end of each sub-phase, no body unless the sub-phase deviated from the plan.

## Final layout after M1

```
crates/server/src/
  config.rs           + IngestConfig, MediaConfig, otlp_http_addr
  clock.rs            + Clock::set (tests)
  http/
    mod.rs            + AppInner.ingest; otlp_router()
    access.rs         + Access::Key, its enforcement
    access_suite.rs   + Access::Key rows
    error.rs          ApiError: PayloadTooLarge{detail}, UnsupportedMediaType{detail}, + Unavailable
  ingest/
    mod.rs            Ingest, Accepted, CommitTarget, CommitReceipt, IngestError
    body.rs           BodyFormat, BodyEncoding, read_capped, decompress
    otlp.rs           success response encoding
    environment.rs    rewrite_reserved_environment
    fixtures_tests.rs golden test (G1.9)
  routes/
    otlp.rs           POST /v1/traces
  serve.rs            + the 4318 listener, Running.otlp_local_addr, ServeError::Prices
  test_support/mod.rs + Ingest in TestApp, seed_project()
```

`crates/server/src/ingest/fixtures_tests.rs` is the golden ingest test (a unit-test module, G1.9).

`ingest` is declared in `lib.rs` as `pub(crate) mod ingest;`.

---

## G1.1 — Ingest configuration

**Goal:** `ServerConfig` has the `[ingest]` and `[media]` sections and `otlp_http_addr`, validated and documented.

**Edit:** `crates/server/Cargo.toml` (move `bytesize` to `[dependencies]`), `crates/server/src/config.rs`, every `ServerConfig { … }` literal (`crates/server/src/serve.rs` tests, `crates/server/src/test_support/mod.rs`, and any other `rg` finds).

### Steps

1. In `config.rs`, add:

   ```rust
   /// `[ingest]` (spec 02 § Limits, spec 12).
   #[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
   #[serde(deny_unknown_fields, default)]
   pub struct IngestConfig {
       /// Raw and decompressed body cap. Default 10 MiB.
       pub max_request_bytes: ByteSize,
       /// Spans per request after decoding. Default 50 000.
       pub max_spans_per_request: u32,
       /// Single attribute value before truncation. Default 8 MiB.
       pub max_attribute_bytes: ByteSize,
       /// Decompressed bytes processed at once, all requests (D-G-5). Default 256 MiB.
       pub max_inflight_bytes: ByteSize,
   }

   /// `[media]` (spec 02 § Media extraction).
   #[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
   #[serde(deny_unknown_fields, default)]
   pub struct MediaConfig {
       /// Blobs below this stay inline. Default 4 KiB.
       pub min_bytes: ByteSize,
       /// Blobs above this are dropped. Default 32 MiB.
       pub max_bytes: ByteSize,
   }
   ```

   `ByteSize` is `bytesize::ByteSize`, deserialized from a string such as `"10MiB"` exactly as `[storage]` does (look at how `crates/storage/src/config.rs` deserializes `ByteSize` and do the same). Implement `Default` for both with the defaults above (`ByteSize::mib(10)`, `50_000`, `ByteSize::mib(8)`, `ByteSize::mib(256)`, `ByteSize::kib(4)`, `ByteSize::mib(32)`).

2. Add to `ServerConfig`, after `trusted_proxies`:

   ```rust
   /// OTLP/HTTP-only listener (D-G-21). `None` when the value is an empty string.
   #[serde(default = "default_otlp_http_addr", deserialize_with = "deserialize_optional_socket_addr")]
   pub otlp_http_addr: Option<SocketAddr>,
   ```

   and after `storage`:

   ```rust
   /// `[ingest]` limits.
   #[serde(default)]
   pub ingest: IngestConfig,
   /// `[media]` extraction bounds.
   #[serde(default)]
   pub media: MediaConfig,
   ```

   `default_otlp_http_addr()` returns `Some(0.0.0.0:4318)`. `deserialize_optional_socket_addr` reads a string: empty (after trimming) → `None`, otherwise parsed like `deserialize_socket_addr`, error text `"invalid socket address"`.

3. Add the alias `("LLMOBS_OTLP_HTTP_ADDR", "otlp_http_addr")` to `ALIASES`. Check that an empty env value reaches the deserializer as an empty string (figment's `parse_env_value` may turn `""` into something else); if it does not, special-case `otlp_http_addr` in `env_value` the way secret paths are kept as strings, and say so in the report.

4. Remove `ingest` from the doc comment of `ServerConfig` that lists keys "for later work", since it is now a field. Keep gRPC in that list.

5. In `validate`, after the storage check, add `validate_ingest(&config.ingest, &config.media)`:

   | Rule | `key` | `reason` |
   |---|---|---|
   | `max_request_bytes` in 1 KiB ..= 1 GiB | `ingest.max_request_bytes` | `must be between 1KiB and 1GiB` |
   | `max_spans_per_request` ≥ 1 | `ingest.max_spans_per_request` | `must be at least 1` |
   | `max_attribute_bytes` in 1 KiB ..= 64 MiB | `ingest.max_attribute_bytes` | `must be between 1KiB and 64MiB` |
   | `max_inflight_bytes` ≥ `max_request_bytes` and ≤ `u32::MAX` bytes | `ingest.max_inflight_bytes` | `must be at least max_request_bytes and below 4GiB` |
   | `media.min_bytes` ≥ 1 and ≤ `media.max_bytes` | `media.min_bytes` | `must be at least 1 byte and at most media.max_bytes` |
   | `media.max_bytes` ≤ 1 GiB | `media.max_bytes` | `must be at most 1GiB` |
   | each size fits in `usize` | the field's key | `too large for this platform` |
   | `otlp_http_addr` differs from `http_addr` | `otlp_http_addr` | `must differ from http_addr` |

   The `u32::MAX` bound exists because the in-flight budget is a `tokio::sync::Semaphore` whose `try_acquire_many_owned` takes a `u32` (G1.6).

6. Add `impl IngestConfig { pub(crate) fn limits(&self, media: &MediaConfig) -> llmobs_otel::IngestLimits }` mapping `max_attribute_bytes`, `media.min_bytes`, `media.max_bytes` to `IngestLimits` (`usize` conversions are safe after validation; use `usize::try_from(…).unwrap_or(usize::MAX)` so no `expect` is needed).

7. Update every `ServerConfig { … }` literal with `otlp_http_addr: None` (tests must not bind 4318), `ingest: IngestConfig::default()`, `media: MediaConfig::default()`.

### Tests (in `config.rs`)

- No `[ingest]`/`[media]` → every default above; `otlp_http_addr` is `Some(0.0.0.0:4318)`.
- TOML `[ingest] max_request_bytes = "2MiB"` and env `LLMOBS_INGEST__MAX_SPANS_PER_REQUEST=10` both apply.
- `otlp_http_addr = ""` and `LLMOBS_OTLP_HTTP_ADDR=` give `None`; `"127.0.0.1:9999"` parses; `"nope"` fails with `invalid socket address`.
- An unknown key under `[ingest]` and under `[media]` fails.
- Each row of the validation table fails with its `key: reason`, and the message does not contain the rejected value.

### Done when

`just check` is green.

**Commit:** `server: add ingest and media configuration`

---

## G1.2 — Problem errors for ingest, and `Clock::set`

**Goal:** `ApiError` can answer every ingest status of the design's error table.

**Edit:** `crates/server/src/http/error.rs`, `crates/server/src/clock.rs`, and the callers of the changed variants.

### Steps

0. Note for every `ApiError::BadRequest` written in this plan: the variant is `BadRequest { detail: Option<String>, errors: Vec<FieldError> }`; the plan writes `BadRequest { detail: Some(..) }` for short, always with `errors: Vec::new()`.
1. Change `ApiError::PayloadTooLarge` to `PayloadTooLarge { detail: &'static str }` and `ApiError::UnsupportedMediaType` to `UnsupportedMediaType { detail: &'static str }`. Their problem `detail` is that text. Update the two existing callers in `json_rejection` (around `error.rs:243`): `"expected Content-Type application/json"` and `"request body is too large"`, so the existing JSON responses do not change.
2. Add `ApiError::Unavailable { detail: &'static str }`: status 503, code `busy`, the `detail`, and the header `Retry-After: 1` (set in `into_response` like `RateLimited` sets its headers; no `RateLimit-*` headers).
3. In `clock.rs`, add next to `advance`:

   ```rust
   /// Sets a [`Self::Manual`] clock. Tests only.
   #[cfg(test)]
   pub(crate) fn set(&self, now_ns: i64)
   ```

   It panics on `Clock::System`, like `advance` (test-only code).

### Tests

- `error_statuses` (in `http/mod.rs` tests) gains: `PayloadTooLarge` → 413 with its detail, `UnsupportedMediaType` → 415 with its detail, `Unavailable` → 503, code `busy`, header `retry-after: 1`, no `ratelimit-limit` header.
- The existing `json_rejections_hide_the_input` test still passes unchanged.
- `Clock::set` then `now_ns` returns the value.

### Done when

`just check` is green; `crates/server/openapi.json` is unchanged.

**Commit:** `server: add problem errors for ingest`

---

## G1.3 — Key-only routes (`Access::Key`)

**Goal:** a route can require a project API key with one scope (D-G-15), and the authorization suite covers it.

**Edit:** `crates/server/src/http/access.rs`, `crates/server/src/http/access_suite.rs`, `crates/server/src/test_support/mod.rs`.

### Steps

1. Add to `Access`:

   ```rust
   /// A project API key with `scope`. Sessions and PATs are refused.
   Key {
       /// The scope the key must hold.
       scope: KeyScope,
   },
   ```

2. In `prepare`, after the read-PAT check, add the arm:
   - principal is not `Principal::ApiKey` → `403 forbidden`;
   - `!scopes.contains(scope)` → `403 forbidden`;
   - load the key's project with `project_by_id(state.meta.reader(), project_id)`; missing, or `state != "active"` → `404`;
   - load its org with `org_by_id`; missing or not `active` → `404`;
   - insert `ProjectAccess { project, role: None }` into the request extensions (handlers read the project, its `org_id` and `retention_days` from it).

   An anonymous request is already `401` before this arm, and a read PAT on `POST` is already `403`.

3. `access_suite.rs`:
   - `documented_routes()` becomes `checked_routes()`: routes where `route.documented || matches!(route.access, Access::Key { .. })`. Rename its callers. `assert_lists_omit_secrets` keeps using documented routes only (add a separate filter there if it relied on the old name).
   - `expected()`: `Access::Key { scope }` → `Allowed` when `is_key(who) && holds_scope(who, scope)`, else `Denied403`. (Anonymous and the read PAT on unsafe methods are decided above the `match`, as today.)
   - `assert_every_route_declares_access`: add `Access::Key { .. }` to the match.
   - `access.rs` test `route_table_matches_parameters_and_openapi_security` matches `route.access` exhaustively (its second `match`, the one that computes the expected security): add `Access::Key { .. } => None` (the document's default security). That `match` runs only for documented routes, and M1's only key route is undocumented; M3, which documents `POST /api/v1/scores`, revisits the arm.
   - The suite has no `Access::Key` route until G1.7. Add a unit test in `access.rs` with `TestApp::with_route_table` and one route `POST /test/key` (`Access::Key { scope: KeyScope::Ingest }`, handler returns `204`), checked against: no credential (401), a session (403), a read PAT (403), a write PAT (403), an `ingest` key (204), a `read` key (403), a `pk` key (403), an `ingest` key whose project's `state` was set to `deleting` in `meta.db`. Two cases: a key never used before answers `401`, because `bearer_principal` refuses a key whose project is not active when the key is not cached (`auth/principal.rs`, `loaded.active`); a key used successfully once (now in `key_cache` for 60 s) answers `404` from the `Access::Key` arm. Test both, and expect exactly those statuses.

4. `test_support/mod.rs`: add a shared seed helper, since `routes/api_keys.rs`, `routes/invites.rs` and others each define their own:

   ```rust
   /// A user (`ada@example.com`, instance admin), an org `acme` they own, a
   /// project `support-bot`, a session cookie, and one `sk` key per scope set.
   pub(crate) struct Seeded {
       pub(crate) org_id: OrgId,
       pub(crate) project: ProjectRow,
       pub(crate) cookie: String,
       /// `sk-llmobs-…` with `["ingest"]`.
       pub(crate) ingest_key: String,
       /// `sk-llmobs-…` with `["read"]`.
       pub(crate) read_key: String,
       /// `pk-llmobs-…` with `["feedback"]`.
       pub(crate) public_key: String,
   }

   impl TestApp {
       pub(crate) async fn seed(&self) -> Seeded;
       /// Another project in `seeded`'s org, with optional retention.
       pub(crate) async fn add_project(&self, seeded: &Seeded, slug: &str, retention_days: Option<u32>) -> (ProjectRow, String /* ingest key */);
   }
   ```

   Build it from the same calls `routes/api_keys.rs::owner` uses (`insert_user`, `insert_org`, `add_org_member`, `insert_project`, `start_session`), and create keys with `auth::secret::new_key` plus `meta::api_keys::insert_api_key` directly (no HTTP). Do not change the existing per-module helpers in this sub-phase.

### Tests

The unit test of step 3, and the suite (`access_suite`) still passing.

### Done when

`just check` is green.

**Commit:** `server: add key-only route access`

---

## G1.4 — Request bodies: type, encoding, capped read, decompression

**Goal:** pure, tested functions that turn an HTTP request body into decompressed OTLP bytes under the limits (D-G-4).

**Edit:** `crates/server/Cargo.toml` (`flate2`, `zstd`, `http-body-util`, `bytes`, `futures`), root `Cargo.toml`, `crates/server/src/lib.rs` (`pub(crate) mod ingest;`), new `crates/server/src/ingest/mod.rs` (the `dead_code` attribute and module declarations only in this sub-phase), new `crates/server/src/ingest/body.rs`.

### `body.rs`

```rust
/// OTLP body encoding from `Content-Type` (spec 02 § Endpoints).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BodyFormat { Protobuf, Json }

/// `Content-Encoding`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BodyEncoding { Identity, Gzip, Deflate, Zstd }

/// Why a body was refused. Mapped to `ApiError` by the route (G1.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BodyError {
    /// 415 `unsupported_media_type`, detail "expected Content-Type application/x-protobuf or application/json".
    UnsupportedType,
    /// 415, detail "unsupported Content-Encoding".
    UnsupportedEncoding,
    /// 413, detail "request body is too large".
    TooLarge,
    /// 400 `bad_request`, detail "invalid compressed body".
    Corrupt,
    /// 400, detail "could not read the request body" (client disconnect, transport error).
    Read,
}

pub(crate) fn body_format(headers: &HeaderMap) -> Result<BodyFormat, BodyError>;
pub(crate) fn body_encoding(headers: &HeaderMap) -> Result<BodyEncoding, BodyError>;
pub(crate) async fn read_capped(body: axum::body::Body, headers: &HeaderMap, max: usize) -> Result<Bytes, BodyError>;
/// Grows the caller's budget reservation; `false` means the budget is full.
pub(crate) type Reserve<'a> = &'a mut dyn FnMut(usize) -> bool;
pub(crate) fn decompress(raw: Bytes, encoding: BodyEncoding, max: usize, reserve: Reserve<'_>) -> Result<Bytes, DecompressError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DecompressError {
    /// A body error (`TooLarge` or `Corrupt`).
    Body(BodyError),
    /// `reserve` returned `false`: the in-flight budget is full (503).
    Budget,
}
```

Rules:
- `body_format`: the media type before any `;`, trimmed, ASCII case-insensitive: `application/x-protobuf` → `Protobuf`, `application/json` → `Json`. Missing header, any other type, or a non-ASCII header → `UnsupportedType`. Parameters (`charset=utf-8`) are ignored.
- `body_encoding`: missing or `identity` → `Identity`; `gzip`, `x-gzip` → `Gzip`; `deflate` → `Deflate`; `zstd` → `Zstd`; case-insensitive, trimmed. A list (`gzip, zstd`), any other value, or more than one `Content-Encoding` header → `UnsupportedEncoding`.
- `read_capped`: a `Content-Length` header above `max` → `TooLarge` without reading. Otherwise collect with `http_body_util::Limited::new(body, max)`; a length-limit error → `TooLarge`, any other error → `Read`.
- `decompress` (sync; the caller runs it in `spawn_blocking`): `Identity` returns `raw` unchanged and never calls `reserve`. Otherwise wrap a reader — `flate2::read::MultiGzDecoder` (concatenated gzip members are valid), `flate2::read::ZlibDecoder` for `deflate` (HTTP `deflate` is the zlib format, RFC 9110 § 8.4.1.2), `zstd::stream::read::Decoder` with `window_log_max(24)` (a 16 MiB window, so a frame cannot make the decoder allocate up to 128 MiB) — and read it in chunks of 64 KiB into a `Vec`:
  - before growing the output by a chunk, call `reserve(chunk_len)`; `false` → `DecompressError::Budget` (the output is dropped);
  - output above `max` → `Body(TooLarge)` (stop at `max + 1` bytes read);
  - any decoder error → `Body(Corrupt)`;
  - an empty result is fine (it decodes as an empty request).

  So the memory a decompression holds is always covered by the in-flight budget (D-G-5); a 100-byte gzip bomb cannot allocate 10 MiB outside it.

### Tests (in `body.rs`)

- Content types: `application/x-protobuf`, `application/json; charset=utf-8`, `APPLICATION/JSON` accepted; `text/plain`, `application/protobuf`, missing → `UnsupportedType`.
- Encodings: each accepted spelling; `br`, `gzip, zstd`, two headers → `UnsupportedEncoding`.
- `read_capped`: exactly `max` bytes passes; `max + 1` → `TooLarge`; a `Content-Length` of `max + 1` with a short body → `TooLarge`; a body stream that errors (`Body::from_stream(futures::stream::iter([Ok(Bytes::from("ab")), Err(std::io::Error::other("boom"))]))`) → `Read`.
- `decompress` round-trips for gzip (also two concatenated members), deflate and zstd, with a `reserve` that always says yes; it reports the sum of reserved bytes ≥ the output length.
- A bomb (64 MiB of zeros compressed, `max` = 1 MiB) → `Body(TooLarge)`, and the sum passed to `reserve` stays ≤ 1 MiB + 64 KiB.
- A `reserve` that refuses after 128 KiB → `Budget`.
- Truncated or garbage input → `Body(Corrupt)`.
- A zstd frame whose window exceeds 2^24 (compress with `zstd::stream::Encoder` and `window_log(27)` plus long-distance matching) → `Body(Corrupt)`.

### Done when

`just check` is green; `cargo tree -d -p llmobs-server` shows no second `flate2` or `zstd`.

**Commit:** `server: read and decompress OTLP request bodies`

---

## G1.5 — OTLP success responses and the reserved environment

**Goal:** the response body for a successful export, and the D-G-7 rewrite, as pure functions.

**Edit:** `crates/server/Cargo.toml` (`prost`, `opentelemetry-proto`), `crates/server/src/ingest/mod.rs` (module declarations), new `crates/server/src/ingest/otlp.rs`, new `crates/server/src/ingest/environment.rs`.

### `otlp.rs`

```rust
/// Body and content type of a `200` for `/v1/traces` (spec 02 § Endpoints).
pub(crate) fn export_response(format: BodyFormat, rejections: &llmobs_otel::Rejections) -> (HeaderValue /* content type */, Vec<u8>);
```

- No rejection (`rejections.total() == 0`): `partial_success` is absent. Protobuf: `ExportTraceServiceResponse { partial_success: None }.encode_to_vec()` (empty bytes). JSON: `{}`.
- With rejections: `rejected_spans = total` (as `i64`, saturating), `error_message = rejections.message()` (`Some` when total > 0; use `unwrap_or_default()`). Protobuf: `ExportTraceServiceResponse { partial_success: Some(ExportTracePartialSuccess { rejected_spans, error_message }) }.encode_to_vec()`. JSON: build it by hand with `serde_json::json!` as `{"partialSuccess":{"rejectedSpans":"<n>","errorMessage":"<message>"}}` — `int64` is a decimal string in proto3 JSON, as OTLP/JSON specifies — and serialize compactly.
- Content type: `application/x-protobuf` or `application/json`.

### `environment.rs`

```rust
/// The OTLP-only rewrite of D-G-7. Returns how many spans were rewritten.
pub(crate) fn rewrite_reserved_environment(spans: &mut [llmobs_core::span::Span]) -> usize;
```

For each span with `environment == Some(names::environment::EVAL)`: set it to `names::environment::EVAL_EXTERNAL`; read `attributes[names::metadata_key::NORMALIZE_WARNING]` as a JSON array of strings (absent, or not a JSON array of strings, counts as empty: the normalizer always writes a valid one, so this is defensive), add `"reserved_environment"`, sort, deduplicate, and write it back with `serde_json::to_string` (compact, as the normalizer writes it). Define `const RESERVED_ENVIRONMENT: &str = "reserved_environment";` in this module.

### Tests

- `otlp.rs`: both formats with and without rejections. Decode the protobuf output with `ExportTraceServiceResponse::decode` and check the fields; parse the JSON and check `rejectedSpans` is the string `"4"` (`edge/rejected-ids` rejects 4 spans; check its `expected.json`) and `errorMessage` equals `Rejections::message()`. Build the `Rejections` by normalizing a tiny request with a bad span id (see `fixtures/otlp/edge/rejected-ids/input.json`), since `Rejections` has no public constructor.
- `environment.rs`: a span with `llmobs-eval` and no warning attribute gets `["reserved_environment"]`; one with `["end_before_start"]` gets `["end_before_start","reserved_environment"]`; one already carrying the code is not duplicated; `production`, `None` and `llmobs-eval-external` are untouched; the return value counts rewrites.

### Done when

`just check` is green.

**Commit:** `server: encode OTLP export responses and rewrite the reserved environment`

---

## G1.6 — `Ingest`: budget, prices, `accept` and `commit`

**Goal:** the ingest core of the design's pipeline, held in `AppState`, independent of HTTP.

**Edit:** `crates/server/Cargo.toml` (`hex`, `futures`), `crates/server/src/ingest/mod.rs`, `crates/server/src/http/mod.rs` (`AppInner.ingest`), `crates/server/src/serve.rs` (build it; `ServeError::Prices`), `crates/server/src/test_support/mod.rs` (build it).

### Types (`ingest/mod.rs`)

```rust
/// Ingest state shared by every request (D-G-1). Holds no `AppState`.
pub(crate) struct Ingest {
    prices: std::sync::RwLock<Arc<PriceTable>>,
    budget: Arc<tokio::sync::Semaphore>,
    max_request_bytes: usize,
    max_spans_per_request: usize,
    limits: IngestLimits,
}

/// Budget held by one request from before its body is read until it is answered.
pub(crate) struct Reservation(tokio::sync::OwnedSemaphorePermit);

/// What `accept` produced. Holds the request's reservation until dropped.
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
    #[error("{0:?}")] Body(BodyError),
    #[error("too many spans in one request")] TooManySpans,
    #[error("invalid OTLP payload")] Decode(#[source] llmobs_otel::DecodeError),
    #[error("ingest budget is full")] Busy,
    #[error("storage refused the append")] Append(#[source] AppendError),
    #[error("storage refused a media blob")] Media(#[source] MediaError),
    #[error("could not build the span batch")] Batch(#[source] llmobs_core::error::CoreError),
    #[error("normalized media has an invalid hash")] MediaHash,
    #[error("ingest task failed")] Join(#[source] tokio::task::JoinError),
}

impl From<IngestError> for ApiError { … }
```

(Check the exact path of `CoreError` in `crates/core/src/error.rs`.)

`From<IngestError> for ApiError`, following the design's error table:

| `IngestError` | `ApiError` |
|---|---|
| `Body(UnsupportedType)` | `UnsupportedMediaType { detail: "expected Content-Type application/x-protobuf or application/json" }` |
| `Body(UnsupportedEncoding)` | `UnsupportedMediaType { detail: "unsupported Content-Encoding" }` |
| `Body(TooLarge)` | `PayloadTooLarge { detail: "request body is too large" }` |
| `TooManySpans` | `PayloadTooLarge { detail: "too many spans in one request" }` |
| `Body(Corrupt)` | `BadRequest { detail: Some("invalid compressed body") }` |
| `Body(Read)` | `BadRequest { detail: Some("could not read the request body") }` |
| `Decode(e)` | `BadRequest { detail: Some(e.to_string()) }` (its text has no payload bytes, `crates/otel/src/error.rs`) |
| `Media(Invalid { .. })` | `BadRequest { detail: Some("invalid media in the request") }` — an over-long MIME type from the request reaches `put_media` (B1 request in the design) |
| `Busy`, `Append(Backpressure)` | `Unavailable { detail: "server is busy, retry later" }` |
| `Append(WalFailed)` | `Unavailable { detail: "storage is unavailable" }` |
| `Append(ShuttingDown)` | `Unavailable { detail: "server is shutting down" }` |
| `Append(ProjectDropped)`, `Media(ProjectDropped)` | `NotFound` |
| `Media(HashMismatch \| NotFound \| InvalidRange \| Fault)`, `MediaHash`, `Append(InvalidBatch { .. })`, `Batch(_)`, `Join(_)` | `Internal(Box::new(error))` (`NotFound` and `InvalidRange` come only from `get_media`; `Fault` only from test hooks) |
| `Media(Object(_) \| Catalog(_) \| Query(_))` | `Unavailable { detail: "storage is unavailable" }` |

Match `AppendError` and `MediaError` exhaustively, naming every variant as above, with no `_` arm, so a new storage variant is a compile error here. `ApiError::Internal` needs `IngestError: Send + Sync + 'static`; every source above is.

### Behaviour

```rust
impl Ingest {
    pub(crate) fn new(config: &IngestConfig, media: &MediaConfig, prices: PriceTable) -> Self;
    /// Swaps the price table (D-G-10). `#[cfg(test)]` in M1: only the golden test calls it.
    /// The track that adds custom prices removes the `cfg`.
    #[cfg(test)]
    pub(crate) fn replace_prices(&self, prices: PriceTable);
    pub(crate) fn max_request_bytes(&self) -> usize;
    /// Reserve budget before reading a body (D-G-5).
    pub(crate) fn reserve(&self, content_length: Option<u64>, encoding: BodyEncoding) -> Result<Reservation, IngestError>;
    /// Decompress, decode, count, normalize, rewrite (D-G-2, D-G-7). OTLP only.
    pub(crate) async fn accept(&self, raw: Bytes, reservation: Reservation, format: BodyFormat, encoding: BodyEncoding, project: &ProjectRow, now_ns: i64) -> Result<Accepted, IngestError>;
    /// Store media, then append (D-G-8, D-G-9).
    pub(crate) async fn commit(&self, storage: &Storage, project_id: ProjectId, normalized: Normalized) -> Result<CommitReceipt, IngestError>;
}
```

`commit` takes a `ProjectId` in M1. M3 replaces it with a `CommitTarget { org_id, project_id, request_bytes }` when the usage counters need those fields, and updates every caller (no unused fields before then, AGENTS.md § No placeholders).

`new`: `budget = Semaphore::new(max_inflight_bytes)`; sizes converted with `usize::try_from` (validated in G1.1). `prices` is wrapped in `RwLock<Arc<…>>`. A poisoned lock is recovered with `PoisonError::into_inner` (the value is an `Arc`, always consistent).

`reserve` (D-G-5: every byte a request holds is covered by the budget):
- the amount is the `Content-Length` when present and ≤ `max_request_bytes`, else `max_request_bytes` (a chunked body, or one the read will refuse anyway); at least 1;
- `budget.clone().try_acquire_many_owned(amount)` (`u32::try_from` is safe after G1.1's validation; map a failed conversion to `Busy` anyway); `Err` → `IngestError::Busy`. It never waits.

`accept`, in order:
1. If the reservation is larger than `raw.len()` (chunked bodies reserve the maximum), give the excess back with `OwnedSemaphorePermit::split(excess)` and drop the split part (`split` exists since tokio 1.40; the workspace pins 1.54). Raw bytes stay covered.
2. Clone the current `Arc<PriceTable>` (read lock, clone, release).
3. Build `IngestContext { project_id: project.id, ingest_time: now_ns, retention_days: project.retention_days, limits: self.limits }`.
4. In one `spawn_blocking` closure (moving `raw`, the `Arc<PriceTable>`, the context, the semaphore `Arc` and the permit):
   1. `decompress(raw, encoding, max_request_bytes)`;
   2. (inside `decompress`) each 64 KiB chunk of output is reserved first through the `reserve` callback, which does `semaphore.clone().try_acquire_many_owned(n)` and `merge`s the result into the reservation's permit; `DecompressError::Budget` → `Busy`, `Body(e)` → `Body(e)`. The raw bytes stay reserved too until the request ends: compressed requests hold raw + decompressed bytes, which is what they use;
   3. `decode_protobuf` or `decode_json` by `format`;
   4. count spans (`resource_spans → scope_spans → spans`, saturating); more than `max_spans_per_request` → `TooManySpans`;
   5. `normalize(&request, &ctx, &prices)`;
   6. `rewrite_reserved_environment(&mut normalized.spans)`;
   7. return `Accepted` with the reservation.
5. A `JoinError` → `IngestError::Join`.

`commit`, in order:
1. Convert each `llmobs_core::media::MediaBlob` to `llmobs_storage::MediaBlob`: `hex::decode` the 64-char `sha256` into `[u8; 32]` (wrong length or bad hex → `MediaHash`), `Bytes::from(blob.bytes)`, same `mime_type`.
2. Put them with `futures::stream::iter(blobs).map(|b| storage.put_media(target.project_id, b)).buffer_unordered(4)`, stopping at the first error (`try_collect` or a loop over `next()`); error → `IngestError::Media`. Nothing is appended after a media error.
3. If `normalized.spans` is empty, return `CommitReceipt { accepted: 0, media }` without appending.
4. Build the batch with `llmobs_core::batch::spans_to_record_batch(&normalized.spans)` inside `spawn_blocking` (a 50 000-span batch takes milliseconds); error → `Batch`.
5. `storage.append_spans(target.project_id, batch).await`; error → `Append`.
6. Return the receipt.

`commit` does not take the budget: its caller (`accept`'s permit, held in `Accepted`) does. Track I's in-process commits are bounded by its own queue.

### Wiring

- `AppInner` gains `pub(crate) ingest: crate::ingest::Ingest`.
- `serve::bind` builds it after `meta.db` opens: `PriceTable::new(builtin_price_rows())`; an error → release storage and `meta.db` as the other early errors do, and return a new `ServeError::Prices(#[source] llmobs_otel::PriceError)` with message `"built-in price table"`.
- `TestApp::open` builds it the same way (`expect` is fine there).

### Tests (in `ingest/mod.rs`, using `TestApp` and `seed()`)

- `accept` on `fixtures/otlp/genai/` any one case's `input.json` (protobuf: decode the JSON with `llmobs_otel::decode_json`, re-encode with `prost::Message::encode_to_vec`) returns the same `Normalized` as calling `normalize` directly with the same context and table.
- `accept` with JSON and each compression gives the same result.
- `TooManySpans`: an `IngestConfig` with `max_spans_per_request = 2` and a 3-span request.
- Budget: with `max_inflight_bytes = max_request_bytes = 1 KiB`, hold one `Reservation` for `Content-Length: 600`; a second `reserve(Some(600), Identity)` returns `Busy`; after dropping the first, it succeeds. `reserve(None, Identity)` reserves the whole 1 KiB and `accept` gives back all but the raw length (a following `reserve(Some(1000), …)` after `accept` of a 20-byte body still fails only while the 20 bytes are held: check with `budget.available_permits()`). With 2 KiB of budget, a 100-byte gzip body that decompresses to 4 KiB returns `Busy`.
- `commit` stores media before the spans: ingest `fixtures/otlp/llmobs/sdk-media-bytes` (with `media.min_bytes` = 1 B in the config), then `storage.get_media(project, &sha, None)` returns the bytes and their MIME type, and the `spans` provider holds the span with that `media_refs` entry.
- `commit` with a blob whose `sha256` does not match its bytes returns `Media(HashMismatch)` and appends nothing (scan the project: 0 rows).
- `commit` with no spans and no media returns `accepted: 0` and appends nothing.
- A blob with a 300-byte MIME type (build `Normalized` by hand) → `Media(Invalid { .. })` → `400`, nothing appended.
- `commit` after `storage.drop_project(project)` returns `Append(ProjectDropped)` (or `Media(ProjectDropped)` when the case has media) and maps to `404`.
- `From<IngestError> for ApiError`: one assertion per table row (status, code, detail, `Retry-After` on 503).

### Done when

`just check` is green.

**Commit:** `server: add the ingest core`

---

## G1.7 — `POST /v1/traces`

**Goal:** the OTLP/HTTP route on the main router, end to end.

**Edit:** new `crates/server/src/routes/otlp.rs`, `crates/server/src/routes/mod.rs`, `crates/server/src/http/access.rs` (`routes()` extends with `crate::routes::otlp::routes()`).

### Route

```rust
pub(crate) fn routes() -> Vec<RouteDef> {
    vec![RouteDef {
        method: Method::POST,
        path: "/v1/traces",
        access: Access::Key { scope: KeyScope::Ingest },
        documented: false,
        handler: post(export),
    }]
}
```

### Handler

```rust
async fn export(
    State(state): State<AppState>,
    Extension(access): Extension<ProjectAccess>,
    headers: HeaderMap,
    body: axum::body::Body,
) -> Result<Response, ApiError>
```

1. `started = Instant::now()`.
2. `format = body_format(&headers)`, `encoding = body_encoding(&headers)` (errors → `IngestError::Body` → `ApiError`). Both checks come before the body is read.
3. `reservation = state.ingest.reserve(content_length, encoding)`, where `content_length` is the parsed `Content-Length` header (absent or unparsable → `None`). A full budget answers `503` before any byte is read.
4. `raw = read_capped(body, &headers, state.ingest.max_request_bytes()).await`.
5. `accepted = state.ingest.accept(raw, reservation, format, encoding, &access.project, state.clock.now_ns()).await`.
6. Destructure `accepted`; clone `normalized.rejections`; `receipt = state.ingest.commit(&state.storage, access.project.id, normalized).await`; drop the reservation only after `commit` returns, so the budget covers the spans until they are appended.
7. Log once (`info`, target `ingest`): `project_id`, `request_bytes`, `decompressed_bytes`, `spans = receipt.accepted`, `rejected = rejections.total()`, `media = receipt.media`, `rewritten`, `duration_ms`.
8. Answer `200` with `export_response(format, &rejections)`.

On every error path, log once at `warn` (`413`, `415`, `400`, `503`, `404`) with `project_id`, `request_bytes` when known, `duration_ms` and `reason` (the `IngestError` variant name as a static string, e.g. `"budget"`, `"too_large"`, `"backpressure"`); `500`s are already logged at `error` by `ApiError::into_response`. Put this in a small `fn log_refusal(project: ProjectId, error: &IngestError, started: Instant)`.

CSRF (D141) does not affect this route: an API key arrives as a bearer, which skips the origin check.

### Tests (in `routes/otlp.rs`; `TestApp`, `seed()`)

Build requests with `app.request(Method::POST, "/v1/traces").header("authorization", &format!("Bearer {}", seeded.ingest_key)).header("content-type", …).body(…)`.

- **Success, protobuf:** a 2-span request → `200`, `content-type: application/x-protobuf`, empty body; both spans readable through `storage.query(project).spans` (scan with DataFusion, see G1.9's helper — or, until G1.9, count rows with a plain `SessionContext` and `register_table`).
- **Success, JSON:** same request in OTLP/JSON → `200`, `application/json`, body `{}`.
- **Encodings:** gzip, deflate and zstd bodies each succeed.
- **Partial success:** `fixtures/otlp/edge/rejected-ids/input.json` → `200` with `partialSuccess.rejectedSpans` and the D111 message; the valid spans are stored.
- **All rejected:** a request whose only span has an all-zero id → `200` with `rejectedSpans: "1"`, nothing appended.
- **Empty request** `{}` → `200`, `{}`.
- **A body above the old 1 MiB default** (2 MiB of valid protobuf, e.g. one span with a 2 MiB attribute) → `200` (pins that the body is read with the route's own cap, not the router's 1 MiB `DefaultBodyLimit`, which only buffering extractors such as `Json` apply).
- **413:** raw body of `max_request_bytes + 1` (config set to 64 KiB for the test); a gzip body that decompresses beyond the limit; more spans than `max_spans_per_request`. Each answers `payload_too_large` with its detail.
- **415:** `text/plain`; `content-encoding: br`.
- **400:** garbage protobuf; malformed JSON; a gzip header with garbage. The `detail` contains none of the body bytes (use a body containing `SECRET-BODY-MARKER` and assert it is absent from the response and from the logs, with `assert_logs_clean`).
- **401/403/404:** no `Authorization` (401); an unknown `sk` key (401); a `read`-only key (403); a `pk` key (403); a session cookie with `Origin` (403); an `ingest` key used once, then its project set to `deleting` (404; a key never used before would get 401, see G1.3).
- **503 (budget):** with `max_inflight_bytes` = `max_request_bytes` = 64 KiB, hold a permit by calling `state.ingest.accept` directly with a 40 KiB request and keeping the `Accepted`, then send a 40 KiB HTTP request → `503`, `retry-after: 1`, code `busy`; after dropping the `Accepted`, the same request → `200`. Storage backpressure is not forced over HTTP: storage's fault hooks are private to `llmobs-storage`, and `storage.flush.max_bytes` must stay below `memtable.hard_limit`, so a flush always starts first. Its mapping is tested in G1.6.
- **Reserved environment:** a span with `deployment.environment.name = "llmobs-eval"` is stored with `environment = "llmobs-eval-external"` and the warning code.
- **Logs:** the `info` line contains the route template `/v1/traces`, the project id and the span count, and no key, header value or attribute value (`assert_logs_clean` with the key and an attribute value as needles).
- **Access suite:** `/v1/traces` now appears in `checked_routes()`; the suite passes. Its `call` sends `POST` with the JSON body `{}`; allowed callers get `200`.

### Done when

`just check` is green; `crates/server/openapi.json` is unchanged; `just run serve` with an `sk` key accepts a request sent with `curl --data-binary @fixtures/otlp/genai/<case>/input.json -H 'content-type: application/json'` (manual check, described in the report).

**Commit:** `server: serve POST /v1/traces`

---

## G1.8 — The OTLP/HTTP port (4318)

**Goal:** `llmobs serve` also listens on `otlp_http_addr` with only `/v1/traces` (D-G-21).

**Edit:** `crates/server/src/http/mod.rs`, `crates/server/src/serve.rs`, `crates/cli/src/login.rs` (its test config).

### Steps

1. `http/mod.rs`: `pub(crate) fn otlp_router(state: AppState) -> Router { serve_router(state, crate::routes::otlp::routes()) }`. It has the same layers as the main router (request log, authentication, origin check, 404/405 problems), and no other route: `/healthz`, `/api/*` and everything else are `404` there.
2. `serve.rs`:
   - after binding `http_addr`, if `config.otlp_http_addr` is `Some(addr)`, bind it (`ServeError::Bind { addr, source }` on failure, releasing state like the main bind) and log `otlp_http_local_addr` at `info`;
   - spawn a second `axum::serve(listener, otlp_router(state).into_make_service_with_connect_info::<SocketAddr>())` with its own graceful-shutdown receiver;
   - `Running` gains `pub otlp_local_addr: Option<SocketAddr>` and holds the second server's `JoinHandle` and shutdown sender;
   - `shutdown` signals both, drains both within the same `DRAIN` budget (`tokio::join!` the two `drain` calls), then continues as today (background tasks, `last_used`, storage, `meta.db`). Report the first error of the two.
   - `Drop for Running` aborts both server tasks.
3. `crates/cli/src/login.rs` (around line 490) writes a test `llmobs.toml` with only `http_addr = "127.0.0.1:0"` and then calls `serve::bind`; with the new default it would bind the real 4318. Add `otlp_http_addr = ""` to that TOML.
4. The startup order of D137 stays: storage, `meta.db`, prices, bootstrap, then the binds. A failed second bind releases everything, so `LOCK` is free after `Err`.

### Tests (in `serve.rs`, over TCP like `healthz_over_tcp_and_shutdown_releases_the_lock`)

- With `otlp_http_addr = Some(127.0.0.1:0)` and `config.init` set (`admin_email`, `admin_password`, `org`, `project`, and `secret_key` = a valid `sk-llmobs-` + 32 base62 characters; bootstrap gives it the full scopes, `ingest` included): `POST /v1/traces` on the OTLP port with that key answers `200`; `GET /healthz` on the OTLP port answers `404`; the same `POST` on the main port answers `200`.
- With `otlp_http_addr = None`: `Running.otlp_local_addr` is `None` and only one listener exists. The same through TOML: `ServerConfig` loaded from `otlp_http_addr = ""` binds no second listener.
- A port already in use for `otlp_http_addr` fails `bind` with `ServeError::Bind` naming that address, and a following `bind` on the same data dir succeeds (the lock was released).
- Shutdown with both listeners open releases the lock (the existing lock test, with the OTLP listener on).

### Done when

`just check` is green.

**Commit:** `server: listen for OTLP/HTTP on otlp_http_addr`

---

## G1.9 — Golden ingest test and concurrency

**Goal:** prove that every fixture ingested over HTTP is stored exactly as `normalize` produces it, and that concurrent ingest and shutdown keep the durability contract.

**Edit:** `crates/server/Cargo.toml` (dev: `arrow`, `datafusion`), new test module `crates/server/src/ingest/fixtures_tests.rs` (declared `#[cfg(test)] mod fixtures_tests;` in `ingest/mod.rs`; it must be a unit-test module because `TestApp` is `pub(crate)`), `docs/roadmap.md`.

### Golden test: `every_fixture_is_stored_as_normalized`

The fixtures root is `Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/otlp")`; walk it with `std::fs::read_dir` (two levels: group, case). For each directory `fixtures/otlp/<group>/<case>/` that contains `expected.json` (cases without it are awaiting bless, D118; skip them):

1. Read `case.json`: `settings` (`max_attribute_bytes`, `media_min_bytes`, `media_max_bytes`, `retention_days`) and `context.ingest_time` (default `2026-01-01T00:00:00Z`, `fixtures/otlp/README.md`).
2. Group cases by their `(max_attribute_bytes, media_min_bytes, media_max_bytes)` triple; build one `TestApp` per group with those values in `IngestConfig`/`MediaConfig` (defaults otherwise), `seed()` it, and `replace_prices(PriceTable::new(rows of fixtures/otlp/prices.json))`.
3. For each case: `add_project(&seeded, <group>-<case> as slug, retention_days)`; `app.state.clock.set(ingest_time)`; send `input.json` as OTLP/JSON to `/v1/traces` with that project's key and assert `200`; compute the expected spans with `normalize(&decode_json(input), &IngestContext { project_id, ingest_time, retention_days, limits }, &prices)`, apply `rewrite_reserved_environment`, sort by `(trace_id, span_id, start_time, end_time, name)`, and convert with `spans_to_record_batch`.
4. Read the stored rows: `let q = app.state.storage.query(project_id, QueryOptions::default())?; let ctx = SessionContext::new(); ctx.register_table("raw_spans", q.spans)?;` then `SELECT * FROM raw_spans ORDER BY trace_id, span_id, start_time, end_time, name`, collect, `concat_batches`.
5. Compare `num_rows`, then each column of the expected schema by name: `expected.column(i).to_data() == stored.column_by_name(name).to_data()` after casting the stored column to the expected column's data type with `arrow::compute::cast` when the types differ (a provider may return an equivalent type). On a difference, fail with the case path and the column name only (never the values: they are fixture payloads, harmless here, but keep the habit).
6. Check the response's `rejectedSpans` equals the fixture's rejected count, and each `expected.json` media entry exists through `storage.get_media`.

Also send each case once as protobuf (re-encoded from the decoded JSON) to a second project of the same app and compare the same way, so both decoders are covered by every fixture. If the whole test takes more than ~60 s in a debug build, keep the protobuf pass for one case per group and say so in the report.

### Concurrency tests

- **Parallel ingest:** 8 tasks each send 20 requests of 50 spans (distinct trace ids) to one project; all answer `200`; the project then holds exactly 8 000 rows.
- **Shutdown during ingest:** bind a real server (`serve::bind`, ports 0, `config.init` with a secret key as in G1.8), start 4 tasks that send requests in a loop (over `tokio::net::TcpStream` with a hand-written HTTP/1.1 request, as `serve.rs`'s existing TCP tests do) and record every span id acknowledged with `200`; after ~200 ms call `Running::shutdown`. Requests after it fail to connect. Reopen storage on the same data dir (`Storage::open`) and check that every acknowledged span id is present (graceful shutdown flushes the memtables, so they come from Parquet files; the WAL path is covered by storage's crash tests). Unacknowledged spans may or may not be present. A `503 ShuttingDown` is not expected here: the HTTP drain ends before `Storage::shutdown` starts.

### Docs

`docs/roadmap.md`: in the Wave 2 table, set G's Status to "**In progress.** M1 implemented." and add the PR numbers once known.

### Done when

`just check` is green, and the golden test ran every blessed fixture case (print the count with `eprintln!` and assert it equals the number of `expected.json` files found, and is above 100, so an empty or partial run fails).

**Commit:** `server: test OTLP ingest against every fixture`

---

## After M1

Report, as AGENTS.md § Commits and reports asks:
- the exact versions of `flate2`, `zstd` and any crate added;
- whether figment needed the `otlp_http_addr` empty-string special case (G1.1);
- whether the backpressure `503` is tested end to end or only by mapping (G1.7);
- the golden test's run time and whether the protobuf pass was reduced (G1.9);
- anything in this plan that turned out wrong, and what was done instead.

Then plan M2 (read API and generated examples), starting from the merged A2 review.
