# Phase 0 · Step 4 — API contract (OpenAPI skeleton)

Implementation plan, split into small sub-phases. Each sub-phase is done by one agent in one go and ends with `just check` green and one commit.

**Prerequisites:** steps 1–3 merged (`llmobs-core` types, the OTLP fixtures and `fixtures/otlp/prices.json`).

**Read first:**
- `AGENTS.md`
- `docs/spec/09-api.md`, especially **§ Conventions** and **§ Response shapes**. They are the rules; this plan gives the field lists.
- `docs/spec/05-query.md`: views, default exclusions, filter DSL, dashboard metrics.
- `docs/spec/01-data-model.md`: span, trace and score columns, trace merge rules.
- `docs/spec/11-ui.md`: what the screens show (why each field exists).
- `docs/spec/08-auth.md` § Sign-up policy, § Web sessions, § OAuth/OIDC (auth config and `/me`).
- Decisions D64, D65 and D69–D73 in `docs/mvp-decisions.md`.
- The `utoipa` docs for the exact version you add (docs.rs). This plan names utoipa 5-style items (`ToSchema`, `PartialSchema`, `IntoParams`, `OpenApiBuilder`, `PathsBuilder`, `OperationBuilder`, `ComponentsBuilder`); if 6.x renamed one, use the new name and say so in your report.

## Why this step exists

The Wave 1 UI track builds the trace list, trace view, sessions, users, dashboard and saved views **before any handler exists**. It needs:
1. the request and response types, as TypeScript generated from an OpenAPI file;
2. realistic mock responses that are guaranteed to match those types.

Step 4 delivers both, from Rust:
- response/request types in `llmobs-server::api`, deriving `utoipa::ToSchema`;
- the OpenAPI document assembled from them, committed as `crates/server/openapi.json` and kept up to date by a test (D69);
- a few pure functions the handlers will need and that can be fully tested now: payload parsing (D70), span-tree assembly, parameter parsing;
- an example project in `fixtures/api/` and the responses for it, checked by a test (D72).

## Scope

**In:**
- `utoipa` in `llmobs-core`: schemas for IDs, stored enums, canonical messages, `Score` (D71).
- `crates/server/src/api/`: the types, the pure functions, the OpenAPI document.
- `crates/server/openapi.json` and its up-to-date test.
- `fixtures/api/`: README, dataset, static responses, generated responses, manifest, media.
- `just` recipes `openapi` and `api-examples`.

**Out:**
- handlers, routing, axum, auth, storage queries (Wave 2/3);
- endpoints outside the 22 operations below (their tracks add them);
- the TypeScript client generation (step 5, `web/` scaffold);
- CI (step 6).

Do not add stub handlers, empty modules or enum variants for later endpoints (AGENTS.md: no placeholders).

## The 22 operations

`{project}` = project UUID, `org-slug/project-slug` (URL-encoded), or `current`. Project-scoped paths are under `/api/v1/projects/{project}`, abbreviated `…` below.

| # | operation_id | Method & path | Tag | Request | 2xx response |
|---|---|---|---|---|---|
| 1 | `get_auth_config` | `GET /api/auth/config` | auth | — | `AuthConfig` |
| 2 | `get_me` | `GET /api/auth/me` | auth | — | `Me` |
| 3 | `list_projects` | `GET /api/v1/orgs/{org}/projects` | projects | `PageParams` | `ProjectPage` |
| 4 | `get_project` | `GET /api/v1/projects/{project}` | projects | — | `Project` |
| 5 | `list_traces` | `GET …/traces` | traces | `TraceListParams` | `TracePage` |
| 6 | `stream_traces` | `GET …/traces/stream` | traces | `TraceStreamParams` | SSE: `trace` → `TraceSummary` |
| 7 | `get_trace` | `GET …/traces/{trace_id}` | traces | `TraceDetailParams` | `TraceDetail` |
| 8 | `stream_trace` | `GET …/traces/{trace_id}/stream` | traces | — | SSE: `span` → `SpanSummary` |
| 9 | `get_span` | `GET …/traces/{trace_id}/spans/{span_id}` | spans | — | `SpanDetail` |
| 10 | `list_spans` | `GET …/spans` | spans | `SpanListParams` | `SpanPage` |
| 11 | `list_sessions` | `GET …/sessions` | sessions | `SessionListParams` | `SessionPage` |
| 12 | `get_session` | `GET …/sessions/{session_id}` | sessions | — | `SessionDetail` |
| 13 | `list_users` | `GET …/users` | users | `UserListParams` | `EndUserPage` |
| 14 | `get_user` | `GET …/users/{user_id}` | users | — | `EndUserDetail` |
| 15 | `list_scores` | `GET …/scores` | scores | `ScoreListParams` | `ScorePage` |
| 16 | `get_schema` | `GET …/schema` | schema | — | `SchemaResponse` |
| 17 | `list_saved_views` | `GET …/saved-views` | saved_views | `SavedViewListParams` | `SavedViewPage` |
| 18 | `create_saved_view` | `POST …/saved-views` | saved_views | body `CreateSavedView` | 201 `SavedView` |
| 19 | `update_saved_view` | `PATCH …/saved-views/{id}` | saved_views | body `UpdateSavedView` | `SavedView` |
| 20 | `delete_saved_view` | `DELETE …/saved-views/{id}` | saved_views | — | 204 |
| 21 | `get_metrics` | `GET …/metrics` | metrics | `MetricsParams` | `MetricsResponse` |
| 22 | `get_media` | `GET …/media/{sha256}` | media | header `Range` | 200/206 binary |

Error responses (all `application/problem+json`, schema `Problem`):
- every operation except `get_auth_config` and `get_me`: 401, 403, 404, 429;
- `get_me`: 401, 429;
- every operation with query parameters or a body (3, 5, 6, 7, 10, 11, 13, 15, 17, 18, 19, 21): 400;
- `get_media`: also 416.

Security: `get_auth_config` has none (`security: []`). Every other operation accepts either the `session_cookie` scheme (API key in cookie `llmobs_session`) or the `bearer` scheme (HTTP bearer: a PAT or an `sk` key). Build the cookie name from the brand macro (`concat!(llmobs_core::llmobs_name!(), "_session")`) so the rename (D46) stays mechanical.

## Final layout

```
crates/core/src/                     ToSchema added to ids, enums, message, score (O1)
crates/server/
  Cargo.toml                         + utoipa, serde, serde_json, uuid, chrono (O2)
  openapi.json                       committed contract (O6)
  src/lib.rs                         pub mod api;
  src/api/
    mod.rs                           module docs + `pub mod` lines
    common.rs                        Page, Problem, ProblemCode, FieldError, TextPosition,
                                     Limit, TimeParam, SortOrder, PageParams          (O2)
    payload.rs                       Payload + Payload::from_stored                    (O2)
    account.rs                       AuthConfig, Me, Project, OrgRole, …                  (O3)
    traces.rs                        TraceSummary, SpanSummary, TreeSpan, TraceDetail,
                                     SpanDetail, ScoreBrief, build_tree, params        (O4)
    sessions.rs  users.rs  scores.rs  schema.rs  saved_views.rs  metrics.rs           (O5)
    openapi.rs                       pub fn openapi() -> OpenApi                       (O6)
  tests/
    openapi.rs                       contract file + document lint                     (O6)
    api_examples.rs                  example generation and validation                 (O8)
    support/mod.rs                   dataset loader + reference aggregations           (O8)
fixtures/api/
  README.md                          (O7)
  dataset/spans.json  dataset/scores.json                                             (O7)
  static/*.json                      hand-written responses                            (O7)
  media/<sha256>.png                                                                   (O7)
  responses/…                        generated responses                               (O8)
  manifest.json                      generated                                         (O8)
```

## Rules for every type in `crates/server/src/api/`

- Derive `Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema`. Responses also derive `Deserialize`: the example test and the future CLI client (`llmobs traces list`) read them.
- Field order = the order in this plan (it becomes the JSON order).
- `Option<scalar>` fields: `#[serde(default, skip_serializing_if = "Option::is_none")]`. `Vec` and maps: always serialized, **no** `skip_serializing_if`, **no** `#[serde(default)]` on response types (so the schema marks them required).
- Instants: `i64` nanoseconds with `#[serde(with = "llmobs_core::time_serde")]` and `#[schema(value_type = String, format = DateTime)]`. Durations (`*_ns`): plain `i64`.
- Maps: `BTreeMap` (sorted, deterministic).
- Doc comment on every type and field: one sentence, citing the spec section where useful. utoipa turns them into `description`s, which end up in the TS types.
- Enums: `#[serde(rename_all = "snake_case")]`; unit-only enums are closed sets.
- Request bodies (`CreateSavedView`, `UpdateSavedView`) and params use `#[serde(deny_unknown_fields)]`.
- No `unwrap`/`expect` outside tests; nothing panics on input.

---

## O1 — OpenAPI schemas for core types

**Goal:** core types used in API responses implement `utoipa::ToSchema`, with schemas that match their serde JSON exactly (D71).

### Dependency

Root `Cargo.toml` `[workspace.dependencies]`: `utoipa = { version = "6.0.0", features = ["uuid"] }` (latest at plan time; check with `cargo info utoipa`). Do **not** enable `preserve_order` or `preserve_path_order`: the default `BTreeMap`s keep the document sorted and deterministic. If 6.0.0 does not build with the pinned toolchain, use the latest 5.x and report it.

`crates/core/Cargo.toml`: `utoipa = { workspace = true }`; append "OpenAPI schemas" to the crate `description` (`AGENTS.md` already lists it).

### Schemas

| Type | How | Schema |
|---|---|---|
| `TraceId` | manual `PartialSchema` + `ToSchema` | `string`, `pattern: ^[0-9a-f]{32}$`, description "Trace id, 16 bytes as lowercase hex." |
| `SpanId` | manual | `string`, `pattern: ^[0-9a-f]{16}$` |
| `ProjectId`, `OrgId` (the `uuid_id!` macro) | manual, in the macro | `string`, `format: uuid` |
| every `stored_enum!` enum | add `utoipa::ToSchema` to the macro's derive list | string enum of the snake_case values (utoipa reads `rename_all`) |
| `Role` (component name **`MessageRole`**), `Modality` (message) | manual | `string`; description lists the known values and says other strings are kept. The Rust type stays `llmobs_core::message::Role`; only the schema name is `MessageRole`, because the org role owns no `Role` component either (see O3) |
| `Part` | manual | `oneOf` of 7 object schemas, see below |
| `Message` | manual | object: `role` (ref `MessageRole`, required), `name` (string), `parts` (array of `Part`, required), `finish_reason` (string); `additionalProperties: true` |
| `ScoreTarget` | derive | utoipa handles `#[serde(tag = "type")]`; check the output is a `oneOf` of three objects each with a `type` enum of one value |
| `ScoreValue` | derive if utoipa supports `tag` + `content` (adjacent tagging), else manual | `oneOf`: `{data_type: "numeric", value: number}`, `{data_type: "boolean", value: boolean}`, `{data_type: "categorical", value: {label: string, value?: number}}`, `{data_type: "text", value: string}` |
| `Score` | derive | instants overridden to `String`/`DateTime` with `#[schema(value_type = String, format = DateTime)]` |

`Part` sub-schemas, registered as their own components so TS gets named types: `TextPart {type: "text", content}`, `ReasoningPart {type: "reasoning", content}`, `ToolCallPart {type: "tool_call", id?, name, arguments: any}`, `ToolCallResponsePart {type: "tool_call_response", id?, response: any}`, `UriPart {type: "uri", modality?, mime_type?, uri}`, `BlobPart {type: "blob", modality?, mime_type?, content}`, `UnknownPart` (object, `type` string optional, description "A part type this version does not model; render as JSON."). Every known part has `type` as a single-value enum, `type` and its modelled non-optional fields required, `additionalProperties: true` (the D65 `extra` fields). `PartialSchema::schema()` of `Part` returns the `oneOf` of refs; `ToSchema::schemas()` pushes the seven sub-schemas so `schema_from::<Part>()`-style registration pulls them in. Do not add Rust types for the sub-schemas: build them with utoipa's `ObjectBuilder` inside the `Part` impl.

`Span`, `SpanEvent`, `SpanLink` do **not** get `ToSchema`: the API exposes `SpanDetail`/`SpanSummary` instead (O4).

### Tests (`crates/core`)

- One insta JSON snapshot per manual schema (`TraceId`, `SpanId`, `ProjectId`, `Role`, `Modality`, `Part` incl. its sub-schemas, `Message`), plus `ScoreTarget`, `ScoreValue`, `Score`, `SpanKind` (as a representative stored enum). Snapshot `serde_json::to_value(<T as PartialSchema>::schema())`.
- A test that serializes one example of each `Part` variant and each `ScoreValue` variant and checks the JSON's `type`/`data_type` value appears in the matching sub-schema's enum (a cheap guard against schema/serde drift).

**Commit:** `core: add OpenAPI schemas for shared types`

---

## O2 — API foundation: common types and payload parsing

**Goal:** the module skeleton of `llmobs-server::api` with the types every operation shares, and `Payload`.

### Dependencies (`crates/server/Cargo.toml`)

`utoipa`, `serde`, `serde_json`, `uuid`, `chrono` (all `workspace = true`). Dev: `insta`, `serde_urlencoded` (new workspace dependency; look up the version axum 0.8 uses with `cargo info axum` / `cargo tree` later, else latest 0.7.x) to test query-string parsing the way axum's `Query` does it.

`src/lib.rs`: `pub mod api;` with a crate doc comment of one paragraph describing what the crate contains today (the HTTP API types and OpenAPI document), not what it will contain. `src/api/mod.rs`: module doc pointing to spec 09 and the contract file, then `pub mod` lines for the modules that exist at this sub-phase (add each module in the sub-phase that creates it).

### `api/common.rs`

| Item | Definition |
|---|---|
| `Page<T>` | `{data: Vec<T>, next_cursor: Option<String>}`. `next_cursor` is **always serialized** (`null` when absent): no `skip_serializing_if` on this one field; schema: nullable, required. Generic over `T: ToSchema`. |
| `Problem` | `{type: String, title: String, status: u16, code: ProblemCode, detail: Option<String>, errors: Vec<FieldError>}`. Serialize `type` via `#[serde(rename = "type")]` on a field named `problem_type`. `errors` omitted when empty (`skip_serializing_if = "Vec::is_empty"`, `default`): this is the one list exception, because RFC 7807 clients expect the member to be absent. Constructor `Problem::new(code: ProblemCode, detail: Option<String>) -> Problem` sets `type = "about:blank"`, `status = code.status()`, `title` = the canonical reason phrase of the status. |
| `ProblemCode` | enum `BadRequest, InvalidFilter, Unauthorized, Forbidden, NotFound, RateLimited, Internal`; `fn status(self) -> u16` = 400, 400, 401, 403, 404, 429, 500. |
| `FieldError` | `{field: String, message: String, position: Option<TextPosition>}` |
| `TextPosition` | `{line: u32, column: u32}`, 1-based, in characters. |
| `Limit` | newtype over `u32`, `Default` = 50. `Deserialize`: deserialize a `u32`, reject 0 and > 1000 with a serde custom error "limit must be between 1 and 1000". `#[schema(value_type = u32, minimum = 1, maximum = 1000, default = 50)]` where it is used as a param (see params below). `fn get(self) -> u32`. |
| `TimeParam` | newtype over `i64` ns. `Deserialize` from a string: if it matches `^-?[0-9]+$` it is epoch **milliseconds** (multiply by 1 000 000 with `checked_mul`, overflow → error); otherwise parse RFC 3339 with chrono (any offset), convert to UTC ns (`timestamp_nanos_opt`, `None` → error). Error messages say what is accepted and never echo the input. Schema: `string`, description "RFC 3339 timestamp or epoch milliseconds." `fn ns(self) -> i64`. |
| `SortOrder` | `Asc`, `Desc`; `Default` = `Desc`. |
| `PageParams` | `IntoParams` (query): `limit: Limit` (`#[serde(default)]`), `cursor: Option<String>`. Used by `list_projects` and `list_saved_views`. |

`ToSchema` for `Page<T>`: components must be named `TracePage`, `SpanPage`, `SessionPage`, `EndUserPage`, `ScorePage`, `ProjectPage`, `SavedViewPage` (O6 registers them by name). Whatever name utoipa generates for the generic itself is not used.

### `api/payload.rs`

```rust
#[serde(tag = "format", content = "value", rename_all = "snake_case")]
pub enum Payload {
    Messages(Vec<Message>),
    Json(serde_json::Value),
    Text(String),
}
```

- Tag values are exactly `PayloadFormat`'s spellings; add a unit test that walks `PayloadFormat::iter()` (strum) and checks each spelling round-trips through `Payload`'s tag, so the two enums cannot drift.
- `pub fn from_stored(format: PayloadFormat, text: String) -> Payload`, the rule of spec 09 § Response shapes:
  - `Messages`: `serde_json::from_str::<Vec<Message>>` → `Messages`; on error, `serde_json::from_str::<Value>` → `Json`; on error → `Text(text)`.
  - `Json`: parse `Value` → `Json`; on error → `Text(text)`.
  - `Text` → `Text(text)`.
- Never panics, never logs the text.
- Schema: `oneOf` of three objects `{format: "messages", value: Message[]}`, `{format: "json", value: any}`, `{format: "text", value: string}`. Derive if utoipa handles adjacent tagging correctly, else implement manually like `ScoreValue`.

### Tests

- `Limit`: absent → 50; `"1"`, `"1000"` ok; `"0"`, `"1001"`, `"-1"`, `"abc"` errors. Parse through `serde_urlencoded::from_str::<PageParams>("limit=…")`, the same deserializer path axum uses.
- `TimeParam`: `2026-01-01T10:00:00Z`, `2026-01-01T11:00:00+01:00` (same instant), `2026-01-01T10:00:00.123456789Z` (ns kept), `1767261600000` (ms), `-1000` (before epoch), `""`, `"yesterday"`, `"99999999999999999"` (overflow) → errors. Check error messages don't contain the input.
- `Payload::from_stored`: valid messages; messages text that is a JSON object (→ `Json`); messages text that is not JSON (→ `Text`); valid json; invalid json (→ `Text`); text that looks like JSON stays `Text`. Serialization snapshot (insta) of each variant.
- `Problem::new(ProblemCode::InvalidFilter, …)` with one `FieldError` with position: insta snapshot; without errors: `errors` absent.
- `Page<u32>` with `next_cursor: None` serializes `"next_cursor": null`.

**Commit:** `server: add API common types and payload parsing`

---

## O3 — Account and navigation types (`api/account.rs`)

| Type | Fields |
|---|---|
| `AuthConfig` | `setup_required: bool` (no user exists yet), `password_enabled: bool`, `signup: SignupPolicy`, `providers: Vec<AuthProvider>`, `smtp_enabled: bool`, `email_verification_required: bool` |
| `SignupPolicy` | `Open`, `InviteOnly`, `Disabled` (spec 08 `LLMOBS_AUTH_SIGNUP`) |
| `AuthProvider` | `id: String` (`github`, `google`, `oidc:<name>`, as in `identities.provider`), `display_name: String` |
| `Me` | `user: User`, `orgs: Vec<OrgMembership>` |
| `User` | `id: Uuid`, `email: String`, `name: Option<String>`, `is_instance_admin: bool` |
| `OrgMembership` | `id: OrgId`, `slug: String`, `name: String`, `role: OrgRole` |
| `OrgRole` (Rust and OpenAPI name) | `Owner`, `Admin`, `Member`, `Viewer` (spec 04 `org_members.role`; a project override `none` hides the project, so it never appears in a response) |
| `Project` | `id: ProjectId`, `org_id: OrgId`, `slug: String`, `name: String`, `retention_days: Option<u32>`, `role: OrgRole` (the caller's effective role on this project), `created_at` (instant) |

Tests: one insta snapshot per type from a literal value (these lock the JSON shapes). `OrgRole` and `SignupPolicy` spellings are covered by the snapshots.

**Commit:** `server: add account and project API types`

---

## O4 — Trace and span types (`api/traces.rs`)

### Constants

`pub const MAX_TRACE_SPANS: usize = 20_000;` (spec 09 § Response shapes).

### Types

`ScoreBrief` — the latest score of one name on a row: `score_id: Uuid`, `name: String`, `value: ScoreValue`, `source: ScoreSource`.

`TraceSummary` (spec 01 § Trace; one row of the trace list):

| Field | Type |
|---|---|
| `trace_id` | `TraceId` |
| `root_span_id` | `Option<SpanId>` |
| `has_root` | `bool` |
| `name` | `String` |
| `start_time`, `end_time` | instants |
| `duration_ns` | `i64` |
| `status` | `SpanStatus` |
| `session_id`, `user_id`, `environment`, `release` | `Option<String>` |
| `tags` | `Vec<String>` |
| `metadata` | `BTreeMap<String, String>` |
| `input_preview`, `output_preview` | `Option<String>` |
| `span_count`, `error_count`, `llm_count` | `i64` |
| `input_tokens`, `output_tokens`, `cache_read_tokens`, `cache_write_tokens`, `reasoning_tokens`, `total_tokens` | `Option<i64>` |
| `cost_total` | `Option<f64>` |
| `models` | `Vec<String>` |
| `scores` | `Vec<ScoreBrief>` |

`SpanSummary` (light columns; spec 09 § Response shapes):

| Field | Type |
|---|---|
| `trace_id`, `span_id` | `TraceId`, `SpanId` |
| `parent_span_id` | `Option<SpanId>` |
| `name` | `String` |
| `span_kind` | `SpanKind` |
| `start_time`, `end_time` | instants |
| `duration_ns` | `i64` |
| `status` | `SpanStatus` |
| `status_message`, `service_name`, `session_id`, `user_id`, `environment`, `release` | `Option<String>` |
| `tags` | `Vec<String>` |
| `metadata` | `BTreeMap<String, String>` |
| `provider`, `model` | `Option<String>` |
| `input_tokens` … `total_tokens` (the six) | `Option<i64>` |
| `cost_total` | `Option<f64>` |
| `ttft_ns` | `Option<i64>` |
| `input_preview`, `output_preview` | `Option<String>` |

`TreeSpan`: `depth: u32`, `detached: bool`, `span: SpanSummary`.

`TraceDetail`: `trace: TraceSummary`, `spans: Vec<TreeSpan>`, `truncated: bool`.

`SpanDetail` (every column except `project_id`, in spec 01 order, with computed fields inserted where shown):

`trace_id`, `span_id`, `parent_span_id?`, `name`, `span_kind`, `start_time`, `end_time`, **`duration_ns`**, `status`, `status_message?`, `service_name?`, `session_id?`, `user_id?`, `environment?`, `release?`, `tags`, `metadata`, `provider?`, `request_model?`, `response_model?`, **`model?`**, `operation?`, the six token fields `?`, `cost_input?`, `cost_output?`, `cost_total?`, `cost_source?` (`CostSource`), `ttft_ns?`, `request_params?: Value`, `input?: Payload`, `output?: Payload`, `input_preview?`, `output_preview?`, `system_instructions?: Value`, `tool_definitions?: Value`, `attributes: BTreeMap<String, String>`, `resource_attributes: BTreeMap<String, String>`, `scope_name?`, `scope_version?`, `events: Vec<SpanEventView>`, `links: Vec<SpanLinkView>`, `media_refs: Vec<String>`, `truncated: Vec<String>`, `conventions: Vec<Convention>`, `ingest_time`, `schema_version: u16`.

`SpanEventView`: `time` (instant), `name: String`, `attributes: Value`. `SpanLinkView`: `trace_id`, `span_id`, `attributes: Value`. Absent stored attributes → `{}`.

### Conversions (production code; the handlers will read `llmobs_core::Span` values)

- `impl From<&Span> for SpanSummary` and `impl From<Span> for SpanDetail`.
- `duration_ns = end_time - start_time` (use `saturating_sub`; the producer guarantees order, but disk data is not trusted). `model = response_model.or(request_model)`. Check whether `llmobs-core` already exposes these computations (the Arrow conversion computes them); reuse them if public, otherwise compute here — do not change core in this sub-phase.
- Payloads: `input`/`output` become `Payload::from_stored(format, text)` when both the text and its format are present; text without format → `Payload::Text`; format without text → absent.
- `request_params`, `system_instructions`, `tool_definitions`, event and link `attributes`: private helper `parse_json_or_string(text: String) -> Value` (parse, else `Value::String(text)`).

### `pub fn build_tree(spans: Vec<SpanSummary>) -> Vec<TreeSpan>`

Rules (spec 09 § Response shapes):
1. Drop rows whose `span_id` was already seen (keep the first).
2. Index children by `parent_span_id`. A span is a **root** when `parent_span_id` is `None` (`detached: false`) or names a span not in the set (`detached: true`).
3. Sort roots, and each node's children, by `(start_time, span_id)`.
4. Emit in pre-order with an explicit stack (no recursion: a 20 000-deep chain must not overflow the stack). `depth` = 0 for roots, parent depth + 1 below.
5. Spans never reached (they sit in a parent cycle, including a self-parent) are then emitted, in `(start_time, span_id)` order, each as a root with `detached: true` followed by its own unreached subtree; a span is never emitted twice.

### Params (`IntoParams`, query)

| Struct | Fields |
|---|---|
| `TraceListParams` | `filter: Option<String>`, `search: Option<String>`, `from: Option<TimeParam>`, `to: Option<TimeParam>`, `sort: TraceSort` (default `StartTime`), `order: SortOrder` (default `Desc`), `limit: Limit`, `cursor: Option<String>`, `include_eval: bool` (default false), `include_experiments: bool` (default false) |
| `TraceSort` | `StartTime`, `Duration`, `CostTotal`, `TotalTokens` |
| `TraceStreamParams` | `filter`, `include_eval`, `include_experiments` (filter restricted to light columns; validation is the handler's job) |
| `TraceDetailParams` | `around: Option<TimeParam>` |
| `SpanListParams` | same fields as `TraceListParams`, with `sort: SpanSort` (same four variants) |

Each param field gets a doc comment (it becomes the parameter description), e.g. `filter`: "Filter DSL expression over the `traces` view (spec 05)."

### Tests

- `From<Span>`: build a `Span` with every field set (reuse values from `fixtures/otlp/genai/chat-cache-tokens/expected.json` by deserializing it in the test) and snapshot the resulting `SpanDetail` and `SpanSummary` (insta JSON). A second span with only required fields: optional fields absent, lists/maps present and empty.
- Payload cases through the conversion: text without format; format without text.
- `parse_json_or_string`: valid object, invalid text.
- `build_tree`:
  - a 4-level tree with siblings out of order → correct pre-order and depths;
  - an orphan (missing parent) → root, `detached: true`, its children keep depth 1;
  - two roots ordered by start time, ties broken by `span_id`;
  - a duplicate `span_id` → kept once (the first);
  - a 2-cycle and a self-parent → each span emitted exactly once, as detached roots;
  - a 20 000-deep chain → completes, last depth = 19 999;
  - empty input → empty output;
  - property-style check over the above: output length = number of distinct span ids; every non-detached, non-root span appears after its parent.

**Commit:** `server: add trace and span API types`

---

## O5 — Sessions, users, scores, schema, saved views, metrics

### `api/sessions.rs`

- `SessionSummary`: `session_id: String`, `start_time`, `end_time`, `duration_ns: i64`, `trace_count: i64`, `error_count: i64`, `total_tokens: Option<i64>`, `cost_total: Option<f64>`, `user_id: Option<String>`, `scores: Vec<ScoreBrief>`.
- `SessionTurn`: `trace: TraceSummary`, `input: Option<Payload>`, `output: Option<Payload>`.
- `SessionDetail`: `session: SessionSummary`, `turns: Vec<SessionTurn>`, `truncated: bool`. `pub const MAX_SESSION_TRACES: usize = 1_000;`
- `SessionListParams`: `filter` (over the `sessions` view), `from`, `to`, `sort: SessionSort` (`StartTime` default, `EndTime`, `TraceCount`, `CostTotal`), `order`, `limit`, `cursor`.

### `api/users.rs`

- `EndUserSummary`: `user_id: String`, `first_seen`, `last_seen` (instants: min `start_time`, max `end_time`), `trace_count: i64`, `session_count: i64`, `error_count: i64`, `total_tokens: Option<i64>`, `cost_total: Option<f64>`.
- `EndUserDetail`: `user: EndUserSummary`, `sessions: Vec<SessionSummary>`. `pub const MAX_USER_SESSIONS: usize = 100;`
- `UserListParams`: `filter` (over the `traces` view, applied before grouping), `from`, `to`, `sort: UserSort` (`LastSeen` default, `TraceCount`, `CostTotal`), `order`, `limit`, `cursor`.

### `api/scores.rs`

- `ScoreListParams`: `target: Option<ScoreTargetKind>`, `trace_id: Option<TraceId>`, `span_id: Option<SpanId>`, `session_id: Option<String>`, `name: Option<String>`, `source: Option<ScoreSource>`, `from`, `to` (on `created_at`), `limit`, `cursor`. Responses are `Page<llmobs_core::score::Score>` (no new response type).
- Test that `trace_id=<hex>&span_id=<hex>&target=span&source=llm_judge` parses via `serde_urlencoded` (this exercises core's `Deserialize` for the IDs and enums from a query string).

### `api/schema.rs`

- `SchemaResponse`: `views: Vec<ViewSchema>`, `functions: Vec<FunctionSchema>`.
- `ViewSchema`: `name: ViewName`, `description: String`, `columns: Vec<ColumnSchema>`.
- `ViewName`: `Spans`, `Traces`, `Sessions`, `Scores`, `ExperimentItems`, `DatasetItems` (spec 05 § Logical views).
- `ColumnSchema`: `name: String`, `column_type: ColumnType`, `nullable: bool`, `description: String`.
- `ColumnType`: `String`, `Integer`, `Float`, `Boolean`, `Timestamp`, `StringList`, `StringMap`, `Json` (string columns holding JSON text). Our own small set: the UI picks operators per type, it does not need Arrow types. Binary ID columns are `String` (views expose hex, spec 05).
- `FunctionSchema`: `name: String`, `signature: String` (e.g. `score(name: string) -> float`), `description: String`, `in_filter: bool` (allowed in the filter DSL; all are allowed in SQL).

### `api/saved_views.rs`

- `SavedViewKind`: `Traces`, `Spans`, `Sessions`, `Users`, `Sql` (spec 04 `saved_views.kind`).
- `SavedView`: `id: Uuid`, `kind`, `name: String`, `state: Value` (schema: `type: object`, description "Opaque UI state: list-page URL params, or `{query, params}` for SQL."), `owner_user_id: Option<Uuid>`, `shared: bool`, `is_default: bool`, `created_at`, `updated_at`.
- `CreateSavedView`: `kind`, `name`, `state: serde_json::Map<String, Value>` (typed as a map so a non-object is rejected by serde), `shared: bool` (default false), `is_default: bool` (default false).
- `UpdateSavedView`: `name: Option<String>`, `state: Option<Map<String, Value>>`, `shared: Option<bool>`, `is_default: Option<bool>`.
- `SavedViewListParams`: `kind: Option<SavedViewKind>`, `limit`, `cursor`.
- Tests: `CreateSavedView` rejects `state: []` and unknown fields; minimal body gets defaults.

### `api/metrics.rs` (spec 05 § Dashboard metrics)

- `MetricsParams`: `from: TimeParam` (required), `to: TimeParam` (required), `bucket: MetricsBucket` (default `Auto`), `filter: Option<String>`, `group_by: Option<MetricsGroupBy>`.
- `MetricsBucket`: `Auto`, `1m`, `5m`, `10m`, `15m`, `30m`, `1h`, `6h`, `12h`, `1d` (variants `Auto, M1, M5, M10, M15, M30, H1, H6, H12, D1` with `#[serde(rename = "1m")]` etc.).
- `MetricsGroupBy`: `Model`, `Environment`, `Release`.
- `MetricsResponse`: `from`, `to` (instants), `bucket_ns: i64` (the resolved bucket), `group_by: Option<MetricsGroupBy>`, `kpis: Vec<Kpi>`, `series: Vec<MetricSeries>`, `top_models: Vec<ModelUsage>`.
- `Kpi`: `metric: KpiMetric`, `value: Option<f64>`, `previous: Option<f64>` (same-length period just before `from`). `KpiMetric`: `Traces`, `ErrorRate`, `LatencyP95`, `CostTotal`, `TotalTokens`.
- `MetricSeries`: `metric: MetricName`, `group: Option<String>`, `points: Vec<MetricPoint>`. Every series has one point per bucket from `from` to `to`.
- `MetricPoint`: `time` (bucket start), `value: Option<f64>` (absent = no data in the bucket).
- `MetricName` and the meaning of `group` (doc comment on each variant; spec 05 § Dashboard metrics):

| Variant | Source | `group` |
|---|---|---|
| `Traces`, `Errors`, `ErrorRate`, `LatencyP50`, `LatencyP95`, `LatencyP99`, `Cost` | `traces` | `group_by` value, or absent |
| `InputTokens`, `OutputTokens`, `CacheReadTokens`, `CacheWriteTokens` | `spans` (llm, embedding) | `group_by` value, or absent |
| `TtftP50`, `TtftP95` | `spans` (llm) | `group_by` value, or absent |
| `LlmLatencyP50`, `LlmLatencyP95`, `CostByModel` | `spans` (llm) | model |
| `ScoreAvg` | `scores` | score name |
| `EvalCost` | `traces` with environment `llmobs-eval` | absent |

  Latencies and TTFT are in nanoseconds (as `f64`); costs in USD.
- `ModelUsage`: `model: String`, `llm_count: i64`, `total_tokens: Option<i64>`, `cost_total: Option<f64>`. At most 10, by cost descending.

### Tests

One insta snapshot per response type from a literal (sessions, users, schema, saved view, metrics); the `MetricsBucket` spellings round-trip; `MetricsParams` without `from` fails to parse.

**Commit:** `server: add session, user, score, schema, saved view and metrics API types`

---

## O6 — The OpenAPI document and the contract file

### `api/openapi.rs`

`pub fn openapi() -> utoipa::openapi::OpenApi`, built with the builder API:

- `info`: title `concat!(llmobs_name!(), " API")`, version `env!("CARGO_PKG_VERSION")`, description one sentence pointing to spec 09. No license (D9 open). No `servers` (relative).
- `tags`: `auth`, `projects`, `traces`, `spans`, `sessions`, `users`, `scores`, `schema`, `saved_views`, `metrics`, `media`, each with a one-line description.
- `components.security_schemes`: `session_cookie` (API key in cookie, name from the brand macro), `bearer` (HTTP bearer, description "A personal access token (`pat-…`) or a secret key (`sk-…`)."). Global `security`: either scheme; `get_auth_config` overrides with `[]`.
- `components.schemas`: every type of O2–O5 that appears in a request or response, plus the core types they reference (pulled in through `ToSchema::schemas`), plus the seven named pages: insert `("TracePage", <Page<TraceSummary> as PartialSchema>::schema())`, etc.
- `paths`: the 22 operations of the table above. Use small private helper functions to keep it readable (for example `fn operation(id, tag, summary) -> OperationBuilder`, `fn json(schema_name) -> Response`, `fn problems(codes: &[u16]) -> Responses`), not a macro.
  - Each operation: `operation_id`, one tag, a `summary` (short imperative, e.g. "List traces"), a `description` when the operation has rules worth stating (default exclusions; trace detail order and cap; SSE event names and data schemas; media `Range` support and `Cache-Control: private, immutable`).
  - Path params: `org` (string: org UUID or slug), `project` (string, description of the three accepted forms), `trace_id` (ref `TraceId`), `span_id` (ref `SpanId`), `session_id`, `user_id` (strings), `id` (uuid), `sha256` (string, `pattern: ^[0-9a-f]{64}$`).
  - Query params: from the `IntoParams` structs (`T::into_params(|| Some(ParameterIn::Query))`).
  - SSE responses: content type `text/event-stream`, schema `string`; the description names each event and the component its data follows (`span` → `SpanSummary`; `trace` → `TraceSummary`; `resync`, `ping` → `{}`).
  - `get_media`: 200 and 206 with content `application/octet-stream` (schema `string`, `format: binary`), a description saying the real `Content-Type` is the blob's MIME type; header param `Range` (optional).
  - `delete_saved_view`: 204 without content. `create_saved_view`: 201.
  - Problem responses: per the error table above, content type `application/problem+json`, schema ref `Problem`.

### Contract file and test (`crates/server/tests/openapi.rs`)

- Rendering: `serde_json::to_string_pretty(&openapi())` + `"\n"`.
- `contract_file_is_up_to_date`: compare with `crates/server/openapi.json`. When the env var `LLMOBS_OPENAPI_WRITE=1` is set, write the file instead and pass. On mismatch, fail with a message naming `just openapi`.
- `document_lint` (on the parsed JSON, not on Rust types, so it checks what clients see):
  1. the set of `(method, path, operation_id)` equals the 22 rows above, written out in the test (this locks the scope; adding an endpoint later means editing this list on purpose);
  2. every `$ref` anywhere resolves to an existing `#/components/schemas/…` entry;
  3. every operation has exactly one tag from the tag list, and a non-empty summary;
  4. security as specified (empty only on `get_auth_config`);
  5. the 4xx responses of each operation are exactly those of the error list above; every listed 4xx response uses `application/problem+json` with `$ref` `Problem`;
  6. every path containing `{x}` declares a path parameter `x`;
  7. every property name in every component schema is snake_case (`^[a-z][a-z0-9_]*$`), and every component name is PascalCase without `_`, `<` or `>` (catches leaked generic names);
  8. the seven page components exist and their `data.items` refer to the right item schema;
  9. no two schemas share a component name: OpenAPI keeps only one, so a clash silently replaces a schema (this is how `Role` would have collided). Check it where the components are collected (for example, `openapi.rs` builds them through one function that a unit test calls to assert no name is inserted twice), and assert that `MessageRole` and `OrgRole` both exist and differ.

### `justfile`

```
# Regenerate crates/server/openapi.json
openapi:
    LLMOBS_OPENAPI_WRITE=1 cargo test -p llmobs-server --test openapi -- contract_file_is_up_to_date
```

Read the generated `openapi.json` once by eye before committing: field descriptions present, required lists right (lists/maps required; options not), nullable `next_cursor`, enums complete.

**Commit:** `server: add the OpenAPI document and contract file`

---

## O7 — Example project: dataset, static responses, media

**Goal:** a small, coherent example project that exercises every response shape. Hand-written, in canonical pretty JSON (2-space indent, trailing newline; the O8 test enforces it).

### `fixtures/api/README.md`

Document:
- purpose (UI mocks; contract examples; future storage/API tests may load the dataset);
- layout (as in § Final layout);
- the fixed identifiers below;
- which files are generated (`responses/`, `manifest.json`: never edit by hand, run `just api-examples`) and which are hand-written (`dataset/`, `static/`, `media/`);
- the manifest format (O8);
- content rules: synthetic only; no real keys, emails other than `@example.com`, or personal data.

### Fixed identifiers

| Thing | Value |
|---|---|
| org | id `01900000-0000-7000-8000-000000000010`, slug `acme`, name `Acme` |
| project | id `01900000-0000-7000-8000-000000000001` (same as the OTLP fixtures), slug `support-bot`, name `Support bot`, retention 30 days |
| second project | id `01900000-0000-7000-8000-000000000002`, slug `playground`, name `Playground`, no retention |
| user | id `01900000-0000-7000-8000-000000000100`, `ada@example.com`, `Ada Example`, instance admin, org owner |
| trace ids | `a1000000000000000000000000000001` … `…006` |
| span ids | `b10000000000000` + trace digit + span digit, e.g. T1 S2 = `b100000000000012` |
| missing parent of T6 | `b1000000000000ff` |
| PNG blob | sha256 `497790947d4666760ce38f3c00e852c71fdb66cae849bae8e9ede352719e1581` (the 1×1 PNG of `fixtures/otlp/edge/media-truncation-rejection`) |

Session and user ids are `[a-z0-9-]+` so they can be file names.

### `dataset/spans.json`

A JSON array of `llmobs_core::Span` (exactly the format of the OTLP fixtures' `expected.json` spans), sorted by `(trace_id, start_time, span_id)`. All spans: `project_id` = the project, date 2026-01-01, `service_name` `support-bot`, `release` `v1.4.0`, `schema_version` 1, `ingest_time` = `end_time` + 500 ms, `conventions` `["genai"]`, environment `production` unless stated. Costs use `fixtures/otlp/prices.json` and the spec 02 formula; they are given below so nobody recomputes them differently.

| Trace | Span | Parent | Kind, name | Start → end (10:mm:ss.mmm) | Status | Model, tokens (in/out/cache_read), cost in/out/total | Payloads |
|---|---|---|---|---|---|---|---|
| T1 `…001` session `sess-001`, user `user-42`, tags `["support"]` | S1 | — | agent `support-agent` | 00:00.000 → 00:04.200 | ok | — | input text "My order 1234 hasn't arrived.", output text "Your order 1234 shipped and arrives tomorrow." |
| | S2 | S1 | llm `chat claude-sonnet-4-5` | 00:00.100 → 00:01.900, ttft 600 ms | ok | anthropic `claude-sonnet-4-5`; 1200/40/1000; total 1240; 0.0009 / 0.0006 / 0.0015 | system_instructions (one text part), tool_definitions (`get_order_status`, JSON schema with `order_id`), input messages [user text], output [assistant tool_call `get_order_status` `{"order_id":"1234"}`, finish_reason `tool_calls`], request_params `{"max_tokens":1024,"temperature":0.2}` |
| | S3 | S1 | tool `get_order_status` | 00:01.950 → 00:02.300 | ok | — | input json `{"order_id":"1234"}`, output json `{"status":"shipped","eta":"2026-01-02"}` |
| | S4 | S1 | llm `chat claude-sonnet-4-5` | 00:02.350 → 00:04.100, ttft 400 ms | ok | 1300/60/1200; total 1360; 0.00066 / 0.0009 / 0.00156 | input [user, assistant tool_call, tool tool_call_response], output [assistant text, finish_reason `stop`] |
| T2 `…002` same session and user | S1 | — | agent `support-agent` | 05:00.000 → 05:02.500 | ok | — | input text "Can I change the delivery address?", output text |
| | S2 | S1 | llm `chat claude-sonnet-4-5` | 05:00.050 → 05:02.400, ttft 350 ms | ok | 1500/80/1200; total 1580; 0.00126 / 0.0012 / 0.00246 | messages in/out (text only) |
| T3 `…003` session `sess-002`, user `user-7` | S1 | — | chain `answer-question` | 10:00.000 → 10:00.800 | error, message "upstream model error" | — | input text |
| | S2 | S1 | llm `chat gpt-4o-mini` | 10:00.050 → 10:00.750 | error, message "429 Too Many Requests" | openai `gpt-4o-mini`, no tokens, `cost_source` `none` | input messages; one event `exception` at 10:00.750 with attributes `{"exception.type":"RateLimitError","exception.message":"rate limited"}` |
| T4 `…004` user `user-42`, no session, environment `staging` | S1 | — | chain `describe-image` | 20:00.000 → 20:03.000 | ok | — | input text "What does this shipping label say?", output text "It is addressed to warehouse 7."  |
| | S2 | S1 | retriever `vector-search` | 20:00.010 → 20:00.210 | ok | — | input text "shipping label", output json array of 2 documents `{id, score, text}` |
| | S3 | S1 | llm `chat gpt-4o-mini` | 20:00.250 → 20:02.950, ttft 600 ms | ok | 850/120/—; total 970; 0.00085 / 0.00048 / 0.00133 | input [user with text part + `uri` part `{modality: image, mime_type: image/png, uri: media://<png sha256>}`], output text; `media_refs` = [png sha256] |
| T5 `…005` environment `llmobs-eval` (use the core constant's value) | S1 | — | evaluator `judge helpfulness` | 01:00.000 → 01:01.500 | ok | — | attributes `llmobs.eval.target_trace_id` = T1, `llmobs.eval.evaluator_id` = `01900000-0000-7000-8000-000000000201` (attribute names from `llmobs_core::names`) |
| | S2 | S1 | llm `chat gpt-4o-mini` | 01:00.020 → 01:01.480 | ok | 600/50/—; total 650; 0.0006 / 0.0002 / 0.0008 | messages in/out |
| T6 `…006` no session, no user | S1 | `b1000000000000ff` (missing) | llm `chat claude-sonnet-4-5` | 30:00.000 → 30:02.000 | ok | 400/100/—; total 500; 0.0012 / 0.0015 / 0.0027 | messages in/out |

Every `llm` span with a model has `provider`, `request_model`, `response_model` (same value), `operation` `chat`, `input_format`/`output_format` `messages`, `input_preview`/`output_preview` per spec 02 § Normalization rules rule 9, and `cost_source` `computed` when it has a cost. Non-LLM spans have no tokens and no cost (D68 rule 3). Root spans carry `session_id`, `user_id`, `tags`; their children carry the same `session_id`/`user_id` (as the SDK propagates them).

### `dataset/scores.json`

A JSON array of `llmobs_core::score::Score`, project as above, `deleted: false`, `ingest_time` = `updated_at`. Score ids `01900000-0000-7000-8000-0000000003NN`.

| # | Target | Name | Value | Source | Notes |
|---|---|---|---|---|---|
| 01 | trace T1 | `helpfulness` | numeric 0.5 | `api` | created/updated 10:00:30 — older; must lose to #02 |
| 02 | trace T1 | `helpfulness` | numeric 0.9 | `llm_judge` | updated 10:01:02; reason "Resolved the question with tool data."; `evaluator_id` …0201, `rule_id` `01900000-0000-7000-8000-000000000202`; `metadata` `{"judge.trace_id": "a1000000000000000000000000000005"}` |
| 03 | span T1 S4 | `tone` | categorical `{label: "friendly", value: 1.0}` | `llm_judge` | |
| 04 | trace T2 | `thumbs` | boolean true | `end_user` | |
| 05 | session `sess-001` | `resolved` | boolean true | `human` | `author_user_id` = Ada |
| 06 | trace T3 | `note` | text "Needs retry with backoff." | `human` | `author_user_id` = Ada |

Set `session_id`/`user_id` on each score when the target's trace has them.

### `static/` (hand-written responses)

| File | Type | Content |
|---|---|---|
| `auth-config.json` | `AuthConfig` | setup not required, password on, signup `invite_only`, providers `github` ("GitHub") and `oidc:okta` ("Okta"), SMTP off, verification off |
| `me.json` | `Me` | Ada, one org membership (acme, owner) |
| `projects.json` | `ProjectPage` | both projects, role owner, `next_cursor` null |
| `project.json` | `Project` | support-bot |
| `schema.json` | `SchemaResponse` | views `traces`, `spans`, `sessions`, `scores` with **every** column of spec 01/05 as exposed by the views (`project_id` hidden; IDs as `String`). Traces view: the spec 01 trace columns **without** the merge bookkeeping `last_ingest_time` and `partial_count`, plus computed `duration_ns` and `duration_ms`. Spans view: plus computed `duration_ns`, `duration_ms`, `model`. Sessions view: exactly the columns of spec 05 § Logical views (`session_id`, `start_time`, `end_time`, `duration_ns`, `duration_ms`, `trace_count`, `error_count`, `total_tokens`, `cost_total`, `user_id`, `tags`). Scores view: spec 01 score columns. `events` and `links` are `json`, descriptions from spec 01 "Notes"; functions: the filter DSL allow-list of spec 05 (`now`, `lower`, `upper`, `length`, `coalesce`, `array_has`, `array_length`, `date_trunc`, `abs`, `score`, `score_label`, `has_score`) with `in_filter: true` |
| `saved-views.json` | `SavedViewPage` | a shared default traces view "Errors" (`state` `{"filter":"status = 'error'","time_range":"last_24h"}`), and a private SQL query "Cost per model per day" (`state` `{"query":"SELECT …","params":[]}` with a real query over the `spans` view) |
| `metrics.json` | `MetricsResponse` | 10:00–11:00, bucket 10 min (`bucket_ns` 600000000000), no group_by; KPIs for the five metrics; series `traces`, `errors`, `error_rate`, `latency_p95`, `cost`, `cost_by_model` (one series per model), `score_avg` (`helpfulness`); `top_models` for the two models. Use values consistent with the dataset's non-eval traces (counts and costs per 10-minute bucket); latency percentiles may be approximations |

### `media/497790…1581.png`

The decoded PNG bytes (the base64 string is in plan step 3, F6, and in that fixture's `input.json`).

**No test in this sub-phase** beyond `just check` staying green; O8 validates everything. Keep the JSON canonical so O8 does not rewrite it.

**Commit:** `fixtures: add the API example project`

---

## O8 — Example responses: generation, manifest and validation

### `crates/server/tests/support/mod.rs`

- Loaders: `dataset_spans() -> Vec<Span>`, `dataset_scores() -> Vec<Score>`, path helpers relative to `CARGO_MANIFEST_DIR` (`../../fixtures/api`).
- `canonical_json<T: Serialize>(&T) -> String`: pretty, 2 spaces, trailing `\n`.
- **Reference aggregations**, written straight from spec 01/05/09 (test code: storage will implement the real ones; their outputs will later be compared with these files):
  - `trace_summary(spans: &[Span], scores: &[Score]) -> TraceSummary` for one complete trace: root = span without parent (if several, earliest by `(start_time, span_id)`); `name` = root's, else earliest span's; `status` = `error` if any span errors, else root's, else `unset`; session/user/environment/release = root's if set, else first non-null by `(start_time, span_id)`; `tags` = sorted distinct union; `metadata` = root's, plus keys from other spans (in start order) when absent; previews = root's, else first `llm` span's; counts; token sums and `cost_total` sum (`None` when every span is `None`); `models` = sorted distinct `model` values; `scores` = latest per name among non-deleted trace-target scores of this trace (greatest `updated_at`, then `score_id`), sorted by name.
  - `is_default_visible(trace)`: environment ≠ `llmobs-eval` (core constant) and no `llmobs.experiment_id` metadata key (core constant).
  - `session_summary`, `user_summary` over visible trace summaries, as defined in spec 05/09.

### `crates/server/tests/api_examples.rs`

**Generation** (`examples_are_up_to_date`): from the dataset, build every generated response and the manifest; with `LLMOBS_API_EXAMPLES_WRITE=1` write them (deleting stale files under `responses/` first), else compare file by file (missing, extra and different files all fail, naming `just api-examples`).

| Operation | Query in manifest | File | Content |
|---|---|---|---|
| `list_traces` | `{}` | `responses/traces.json` | visible traces, `start_time` desc then `trace_id` desc, `next_cursor` null |
| `list_traces` | `{"include_eval": "true"}` | `responses/traces-include-eval.json` | all traces |
| `get_trace` | path `trace_id` | `responses/traces/<trace_id>.json` | every trace incl. T5; `spans` = `build_tree` of the trace's `SpanSummary`s; `truncated` false |
| `get_span` | path `trace_id`, `span_id` | `responses/spans/<trace_id>/<span_id>.json` | every span, `SpanDetail::from` |
| `list_spans` | `{}` | `responses/spans.json` | spans of visible traces, `start_time` desc then `span_id` desc |
| `list_sessions` | `{}` | `responses/sessions.json` | `start_time` desc |
| `get_session` | path `session_id` | `responses/sessions/<id>.json` | turns by `start_time`; root payloads via `SpanDetail` conversion rules |
| `list_users` | `{}` | `responses/users.json` | `last_seen` desc |
| `get_user` | path `user_id` | `responses/users/<id>.json` | sessions `start_time` desc |
| `list_scores` | `{}` | `responses/scores.json` | all non-deleted scores, `created_at` desc then `score_id` desc |

Plus manifest entries for the static files: `get_auth_config`, `get_me`, `list_projects` (path `org` = `acme`), `get_project`, `get_schema`, `list_saved_views`, `get_metrics`, and `get_media` (path `sha256`, file `media/<sha>.png`, `content_type` `image/png`).

**Manifest** (`fixtures/api/manifest.json`), an array sorted by `(operation_id, file)`:

```json
{
  "operation_id": "get_trace",
  "path_params": {"trace_id": "a1000000000000000000000000000001"},
  "query": {},
  "status": 200,
  "content_type": "application/json",
  "file": "responses/traces/a1000000000000000000000000000001.json"
}
```

`project` is never in `path_params`: mocks match any project. Document this in the README.

**Validation tests:**
1. `static_files_match_their_types`: each static file deserializes into its type and re-serializes to exactly the same text (canonical, and no field dropped or added).
2. `generated_files_roundtrip`: same check for every generated file (guards `Deserialize`/`Serialize` symmetry).
3. `manifest_covers_the_read_operations`: every `GET` operation in `openapi()` (O6) appears in the manifest at least once, except `stream_traces` and `stream_trace` (SSE); no manifest entry names an unknown operation; every `file` exists.
4. `dataset_is_consistent`: span ids unique per trace; every `media_refs` entry has `media/<sha>.png` whose SHA-256 matches; every score target exists in the dataset; ids in `static/` (project, org, user) match the fixed identifiers; dataset session/user ids match `^[a-z0-9-]+$`; dataset files are canonical (sorted spans, canonical formatting).
5. `example_scenarios`: spot checks that pin the interesting cases, so a regression is obvious:
   - T1's summary has `helpfulness` 0.9 (score #02 beats #01);
   - T5 is absent from `traces.json`, `spans.json`, sessions and users, present in `traces-include-eval.json`;
   - T6's tree has one span with `detached: true`, and its summary `has_root: false`;
   - T3's summary status is `error` and `error_count` 2;
   - T4 S3's `SpanDetail.input` is `messages` with a `uri` part.

### `justfile`

```
# Regenerate fixtures/api responses and manifest
api-examples:
    LLMOBS_API_EXAMPLES_WRITE=1 cargo test -p llmobs-server --test api_examples -- examples_are_up_to_date
```

**Commit:** `fixtures: generate API example responses and manifest`

---

## O9 — Final review and report

1. `just check` passes; `just openapi` and `just api-examples` leave the tree unchanged.
2. Read `crates/server/openapi.json` end to end against spec 09 § Conventions and § Response shapes. Run `npx openapi-typescript@7 crates/server/openapi.json -o /tmp/…/api.d.ts` once (pin the latest 7.x; do not commit the output, do not add a `package.json`) and check the TS is usable: discriminated unions for `Payload`, `Part`, `ScoreValue`, `ScoreTarget`; optional fields `?:`; lists required. Report anything awkward.
3. Check that `fixtures/api/README.md` matches what the tests enforce.

**Report back:**
- the utoipa version, and anything that had to be implemented manually because the derive could not express the serde shape;
- every place where you had to interpret this plan or spec 09;
- the openapi-typescript check result;
- versions of new dependencies (`utoipa`, `serde_urlencoded`).

**Commit:** only if O9 changed something (`api: final review fixes`).
