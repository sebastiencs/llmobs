# Phase 0 · Step 2 — `llmobs-core`: shared contracts

Implementation plan, split into small sub-phases. Each sub-phase is done by one agent in one go and ends with `just check` green and one commit.

**Prerequisite:** step 1 (`01-workspace-skeleton.md`) is merged.

**Read first:**
- `AGENTS.md`
- `docs/spec/01-data-model.md`: the source of every column below.
- `docs/spec/02-ingestion.md`: attribute names and the canonical message format.
- `docs/spec/06-evaluation.md` § Scores.
- `docs/spec/README.md` § Cross-cutting conventions.
- Decisions D59–D63 in `docs/mvp-decisions.md`.

## What `llmobs-core` is (and is not)

`llmobs-core` holds the **contracts shared by more than one crate**, nothing else.

| In core | Not in core (owner) |
|---|---|
| `TraceId`, `SpanId`, `ProjectId`, `OrgId` | other entity IDs: plain `uuid::Uuid` in `llmobs-server` (D61) |
| enums that appear in stored data (span kind, status, payload format, cost source, convention, score enums) | config types: each crate owns its section (D59) |
| the canonical message types (`Message`, `Part`, …) | OTLP decoding, normalization, previews, cost computation: `llmobs-otel` |
| the `Span` and `Score` Rust structs (the otel → storage and API → storage contract) (D60) | trace partial computation, WAL, partition keys, day strings: `llmobs-storage` |
| the Arrow schemas of `spans`, `traces`, `scores`, plus column-name constants | the trace partial Rust type: none, storage computes partials with Arrow/DataFusion |
| `Span` → `RecordBatch` and `Score` → `RecordBatch` conversion | `RecordBatch` → `Span` (no consumer yet) |
| attribute names, reserved metadata keys and reserved values (`llmobs.*`, `llmobs-eval`, `media://`) | time formatting/parsing helpers: added where first needed |

**Rules** (on top of `AGENTS.md`):
- Every public item has a doc comment that references the spec section it implements.
- No item without a consumer in Phase 0–Wave 1. Everything listed in this plan has one: otel produces `Span`, storage consumes batches and schemas, the server writes `Score`, and fixtures serialize `Span`.
- Struct field names equal the Arrow column names whenever the mapping is 1:1, so readers never have to translate.
- Do not change the spec while implementing. If something in this plan contradicts the spec, stop and report it.

## Final module layout

```
crates/core/src/
  lib.rs          crate doc + `pub mod` declarations, nothing else
  error.rs        CoreError
  ids.rs          TraceId, SpanId, ProjectId, OrgId
  enums.rs        SpanKind, SpanStatus, PayloadFormat, CostSource, Convention,
                  ScoreSource, ScoreDataType, ScoreTargetKind
  names.rs        product name macro, attribute names, reserved keys/values
  message.rs      Message, Role, Part, Modality (canonical GenAI message format)
  span.rs         Span, SpanEvent, SpanLink
  score.rs        Score, ScoreTarget, ScoreValue
  schema.rs       Arrow schemas + column-name constants + light projection
  batch.rs        spans_to_record_batch, scores_to_record_batch
crates/core/tests/
  snapshots/      insta snapshots (committed)
```

Each sub-phase adds its own `pub mod x;` line. Do not declare modules ahead of time.

---

## C1 — Dependencies, error type, IDs

**Goal:** the four ID types, with strict parsing, hex/UUID formatting and serde.

**Dependencies:**
- Add to root `[workspace.dependencies]` if missing, at the latest version (look it up):
  - `hex = "<latest>"`
  - `strum = { version = "<latest>", features = ["derive"] }` (used in C2, add it now)
  - `insta = { version = "<latest>", features = ["json"] }`
- `crates/core/Cargo.toml`:
  - `[dependencies]`: `serde`, `serde_json`, `thiserror`, `uuid`, `hex` (all `workspace = true`)
  - `[dev-dependencies]`: `insta`, `serde_json`

**`error.rs`:**
```rust
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error("invalid {kind}: {reason}")]
    InvalidId { kind: &'static str, reason: &'static str },
    #[error("unknown {kind} value: {value:?}")]
    UnknownVariant { kind: &'static str, value: String },
    #[error(transparent)]
    Arrow(#[from] arrow::error::ArrowError),   // add this variant in C7, not now
}
```
The error message must never contain user payload. `value` in `UnknownVariant` is acceptable because it is an enum token. Truncate it to 64 chars.

**`ids.rs`:**

| Type | Repr | Constructors | Formatting | Rules |
|---|---|---|---|---|
| `TraceId` | `pub struct TraceId([u8; 16])` | `from_bytes([u8;16]) -> Result<Self, CoreError>`, `from_slice(&[u8]) -> Result<…>` (len must be 16), `FromStr` (32 lowercase or uppercase hex chars) | `Display`/`Debug`/`to_hex()`: 32 lowercase hex chars; `as_bytes() -> &[u8;16]` | all-zero is **invalid** (OTel spec) and rejected by every constructor |
| `SpanId` | `pub struct SpanId([u8; 8])` | same with 8 bytes / 16 hex chars | 16 lowercase hex chars | all-zero invalid |
| `ProjectId` | `pub struct ProjectId(uuid::Uuid)` | `new_v7()`, `from_uuid(Uuid)`, `FromStr` (hyphenated UUID) | `Display`: lowercase hyphenated; `as_uuid()` | any UUID version accepted when parsing (tests may use v4); `new_v7` for creation |
| `OrgId` | same as `ProjectId` | same | same | same |

- Derive `Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord` on all four.
- `Debug` for `TraceId`/`SpanId` prints `TraceId(4bf92f…)` with the full hex. Implement it by hand, not with derive, which would print a byte array.
- **serde:** all four serialize as strings (hex or hyphenated UUID) and deserialize with the same validation. Implement `Serialize`/`Deserialize` by hand via `Display`/`FromStr`. Do not use `serde(transparent)` for TraceId/SpanId.
- No `Default` impl: there is no valid default ID.
- No random generation for `TraceId`/`SpanId` yet; it has no consumer.

**Tests** (`ids.rs` `#[cfg(test)]`):
- hex round-trip
- uppercase input accepted, lowercase output
- wrong length rejected
- non-hex rejected
- all-zero rejected (bytes and hex)
- serde JSON round-trip
- `ProjectId::new_v7()` is version 7
- `ProjectId` parse/format round-trip

**Commit:** `core: add ID types`

---

## C2 — Stored enums

**Goal:** every enum that appears as a string in stored data or the API, with **one** canonical spelling shared by Display, FromStr and serde.

**Dependencies:** `strum` (workspace).

**Pattern for every enum:**
```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash,
         serde::Serialize, serde::Deserialize,
         strum::EnumString, strum::IntoStaticStr, strum::Display, strum::EnumIter)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum SpanKind { Llm, Embedding, Tool, Agent, Chain, Retriever, Reranker, Guardrail, Evaluator, Span }
```
Add `pub fn as_str(self) -> &'static str` (via `IntoStaticStr`). Parsing failures map to `CoreError::UnknownVariant { kind: "span_kind", … }`: add a small `parse(s: &str) -> Result<Self, CoreError>` per enum. Do not use strum's error type in public signatures.

| Enum | Variants → strings | Spec |
|---|---|---|
| `SpanKind` | `llm, embedding, tool, agent, chain, retriever, reranker, guardrail, evaluator, span` | 01 § Span kinds |
| `SpanStatus` | `unset, ok, error` | 01 `status` |
| `PayloadFormat` | `messages, json, text` | 01 § Payloads |
| `CostSource` | `computed, reported, none` | 01 `cost_source` |
| `Convention` | `llmobs, genai, openinference, openllmetry, vercel, langfuse, laminar, google_adk` | 01 `conventions`, 02 § Convention detection |
| `ScoreTargetKind` | `span, trace, session` | 01 § Score `target` |
| `ScoreDataType` | `numeric, boolean, categorical, text` | 01 § Score `data_type` |
| `ScoreSource` | `api, sdk, human, end_user, llm_judge, check, webhook` | 01 § Score `source` |

`SpanStatus` gets `impl Default` → `Unset`. No other enum has a default.

**Tests:**
- For each enum, iterate all variants (`EnumIter`) and check that `as_str` ↔ `parse` ↔ serde JSON agree.
- Unknown strings are rejected with `UnknownVariant`.
- One insta snapshot `enums.snap` listing `kind: [values…]` for every enum, so any rename shows up in review.

**Commit:** `core: add stored enums`

---

## C3 — Names: attributes, reserved keys and values

**Goal:** a single place for every `llmobs`-branded string, so the future rename (D46) touches one macro.

**`names.rs`:**
```rust
/// Product name used in every branded identifier. Renaming the product = changing this macro.
#[macro_export]
macro_rules! llmobs_name {
    () => { "llmobs" };
    ($suffix:literal) => { concat!("llmobs", $suffix) };
}
pub const PRODUCT: &str = llmobs_name!();
```

Then constants grouped in sub-modules. All are `pub const …: &str = llmobs_name!("…")` where branded, or plain literals for standard attribute names. Values come from specs 02, 06 and 10:

```rust
pub mod attr {          // span attributes emitted by our SDKs / read by otel (spec 02, 10)
    SPAN_KIND          = "llmobs.span.kind"
    INPUT              = "llmobs.input"
    INPUT_FORMAT       = "llmobs.input.format"
    OUTPUT             = "llmobs.output"
    OUTPUT_FORMAT      = "llmobs.output.format"
    SESSION_ID         = "llmobs.session.id"
    USER_ID            = "llmobs.user.id"
    TAGS               = "llmobs.tags"
    METADATA_PREFIX    = "llmobs.metadata."      // one attribute per metadata key
    ENVIRONMENT        = "llmobs.environment"
    RELEASE            = "llmobs.release"
    TTFT_NS            = "llmobs.ttft_ns"
    COST_TOTAL         = "llmobs.cost.total"
    EXPERIMENT_ID      = "llmobs.experiment.id"
    DATASET_ITEM_ID    = "llmobs.dataset_item.id"
}
pub mod metadata_key {  // reserved keys inside the `metadata` map (spec 02 context fields, 06)
    EXPERIMENT_ID         = "llmobs.experiment_id"
    DATASET_ITEM_ID       = "llmobs.dataset_item_id"
    EVAL_TARGET_TRACE_ID  = "llmobs.eval.target_trace_id"
    EVAL_TARGET_SPAN_ID   = "llmobs.eval.target_span_id"
    EVAL_RULE_ID          = "llmobs.eval.rule_id"
    EVAL_EXPERIMENT_ID    = "llmobs.eval.experiment_id"
    EVAL_EVALUATOR_ID     = "llmobs.eval.evaluator_id"
    NORMALIZE_ERROR       = "llmobs.normalize_error"   // spec 02 § message conversion; stored in `attributes`
}
pub mod score_metadata_key { // keys in Score.metadata written by evaluators (spec 06)
    JUDGE_TRACE_ID = "judge.trace_id", JUDGE_MODEL = "judge.model",
    JUDGE_INPUT_TOKENS = "judge.input_tokens", JUDGE_OUTPUT_TOKENS = "judge.output_tokens",
    JUDGE_COST_USD = "judge.cost_usd", JUDGE_LATENCY_MS = "judge.latency_ms"
}
pub mod environment {   // reserved environment values (spec 01, 06)
    EVAL                = "llmobs-eval"
    EVAL_EXTERNAL       = "llmobs-eval-external"   // what external data claiming EVAL is rewritten to
    EXPERIMENT          = "experiment"
}
pub mod media {         // spec 01 § Media blob, 02 § Media extraction
    URI_SCHEME          = "media://"
    DROPPED_URI         = "llmobs:dropped"
}
pub mod marker {        // truncation texts (spec 02 § Truncation)
    TRUNCATED_STRING_SUFFIX_PREFIX = "…[llmobs: truncated, original "   // followed by "<N> bytes]"
    TRUNCATED_MESSAGES_PREFIX      = "[llmobs: "                        // followed by "<K> messages truncated]"
}
```
The pseudo-syntax above shows names and values. Write real `pub const NAME: &str = …;` items, with a doc comment on each pointing to the spec. Truncation markers are only prefixes; otel formats the full text.

**Tests:**
- An insta snapshot `names.snap` of every constant as `module::NAME = "value"`.
- A test asserting every branded constant starts with `PRODUCT`. This catches a hand-typed `"llmobs"`.

**Commit:** `core: add branded names and reserved values`

---

## C4 — Canonical message format

**Goal:** Rust types for the OTel GenAI message JSON (spec 01 § Canonical message format). They are used by otel converters, server judges and the UI contract. Serialization must produce **exactly** the GenAI JSON shape. Unknown content must survive a round trip unchanged.

**`message.rs`:**
```rust
pub struct Message {
    pub role: Role,
    pub name: Option<String>,          // omitted from JSON when None
    pub parts: Vec<Part>,
    pub finish_reason: Option<String>, // omitted when None; output messages only
}

pub enum Role { System, User, Assistant, Tool, Other(String) }

pub enum Modality { Image, Video, Audio, Other(String) }

pub enum Part {
    Text { content: String },
    Reasoning { content: String },
    ToolCall { id: Option<String>, name: String, arguments: Option<serde_json::Value> },   // None = no `arguments` key (D65)
    ToolCallResponse { id: Option<String>, response: serde_json::Value },
    Uri { modality: Option<Modality>, mime_type: Option<String>, uri: String },
    Blob { modality: Option<Modality>, mime_type: Option<String>, content: String }, // base64
    /// Any part whose `type` is unknown, or whose known type fails to parse; kept verbatim.
    Unknown(serde_json::Value),
}
```

**Serde requirements:**
- `Role` and `Modality` serialize as lowercase strings (`"system"`, `"image"`). `Other(s)` serializes as `s`. Deserializing an unknown string gives `Other(s)`. Hand-write both impls (`as_str` plus `From<&str>`), about 20 lines each. Do not use `#[serde(other)]`, which drops the value.
- `Part` is internally tagged by `"type"` with values `text`, `reasoning`, `tool_call`, `tool_call_response`, `uri`, `blob`. Implement `Deserialize` by hand:
  1. deserialize to `serde_json::Value`;
  2. if it is an object with a known `"type"`, try the typed variant;
  3. on any failure (unknown type, missing field, wrong field type), return `Part::Unknown(value)`.
  `Serialize`: typed variants write `"type"` first, then their fields, and omit `None` fields. `Unknown(v)` writes `v` unchanged.
- **Extra fields are preserved, never dropped (D65).** A part with a known `type` plus fields we do not model (e.g. a provider-specific `"cache_control"` on a text part) stays **typed**, and the unmodelled fields go into an `extra` map:
  - Every typed `Part` variant and `Message` get a field `extra: serde_json::Map<String, serde_json::Value>`, empty by default.
  - Deserialize: remove the known keys (and `type`) from the object; whatever remains goes into `extra`.
  - Serialize: the known fields come first, in declared order, then the `extra` entries in their original order. Enable `serde_json`'s `preserve_order` feature in the workspace if `Map` order is not already insertion order, and say so in the report. Omit `extra` entirely when it is empty.
  - Rationale: falling back to `Unknown` would hide real text, tool calls and blobs from the chat view, the judges and media extraction. Dropping the extras would silently lose data. With `extra`, the part stays typed and the JSON round-trips exactly.
  - `Unknown` remains only for an unknown `type`, a missing `type`, or a known type whose *modelled* fields are missing or of the wrong type.
- `Message`: same pattern. Known fields `role, name, parts, finish_reason`, everything else in `extra`. `parts` defaults to empty when missing.
- Field order in output: `role, name, parts, finish_reason`, then `extra`. For parts: `type`, then fields in the order declared above, then `extra`.

**Helpers** (all have consumers):
- `impl Part { pub fn is_media(&self) -> bool }`: true for `Uri` and `Blob`.
- `pub fn parse_messages(json: &str) -> Result<Vec<Message>, serde_json::Error>`
- `pub fn messages_to_json(messages: &[Message]) -> String`: compact JSON. This is what is stored in `Span.input` when the format is `messages`. It cannot fail, because the types only contain JSON-safe data. Use `serde_json::to_string`; on the impossible error path, return `"[]"` with a comment explaining why.

**Tests:**
- Round-trip each part type.
- An unknown part type is preserved byte-for-byte (compare `Value`s).
- A known type with a missing required field becomes `Unknown`.
- A known type with extra fields stays typed and the extras land in `extra`. Re-serializing produces the same JSON `Value` as the input; this holds for parts and for `Message`.
- `Role::Other("developer")` round-trips.
- Insta snapshot of a realistic conversation: system, user with text and image URI, assistant with reasoning and tool call, tool response, assistant with finish reason.
- Deserializing the example JSON from spec 01 works. Put it in `tests/data/genai_messages.json`.

**Commit:** `core: add canonical message types`

---

## C5 — `Span` and `Score` structs

**Goal:** the owned Rust representations written to storage, with serde (used by golden fixtures in step 3 and by the API).

**`span.rs`.** Field names equal column names (spec 01 table). The two **computed** columns are not fields: `duration_ns` and `model` are computed in C7.

```rust
pub struct Span {
    pub project_id: ProjectId,
    pub trace_id: TraceId,
    pub span_id: SpanId,
    pub parent_span_id: Option<SpanId>,
    pub name: String,
    pub span_kind: SpanKind,
    pub start_time: i64,                 // ns since epoch, UTC
    pub end_time: i64,                   // ns; producer guarantees end_time >= start_time
    pub status: SpanStatus,
    pub status_message: Option<String>,
    pub service_name: Option<String>,
    pub session_id: Option<String>,
    pub user_id: Option<String>,
    pub environment: Option<String>,
    pub release: Option<String>,
    pub tags: Vec<String>,
    pub metadata: BTreeMap<String, String>,          // values JSON-encoded scalars or strings
    pub provider: Option<String>,
    pub request_model: Option<String>,
    pub response_model: Option<String>,
    pub operation: Option<String>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    pub reasoning_tokens: Option<i64>,
    pub total_tokens: Option<i64>,
    pub cost_input: Option<f64>,
    pub cost_output: Option<f64>,
    pub cost_total: Option<f64>,
    pub cost_source: Option<CostSource>,
    pub ttft_ns: Option<i64>,
    pub request_params: Option<String>,              // JSON object text
    pub input: Option<String>,                       // see PayloadFormat
    pub input_format: Option<PayloadFormat>,
    pub output: Option<String>,
    pub output_format: Option<PayloadFormat>,
    pub input_preview: Option<String>,
    pub output_preview: Option<String>,
    pub system_instructions: Option<String>,         // JSON text
    pub tool_definitions: Option<String>,            // JSON text
    pub attributes: BTreeMap<String, String>,        // values JSON-encoded
    pub resource_attributes: BTreeMap<String, String>,
    pub scope_name: Option<String>,
    pub scope_version: Option<String>,
    pub events: Vec<SpanEvent>,
    pub links: Vec<SpanLink>,
    pub media_refs: Vec<String>,                     // sha256 lowercase hex
    pub truncated: Vec<String>,
    pub conventions: Vec<Convention>,
    pub ingest_time: i64,
    pub schema_version: u16,
}

pub struct SpanEvent { pub time: i64, pub name: String, pub attributes: Option<String> /* JSON object text */ }
pub struct SpanLink  { pub trace_id: TraceId, pub span_id: SpanId, pub attributes: Option<String> }
```

- Derive `Debug, Clone, PartialEq, Serialize, Deserialize` on all three.
- `BTreeMap` (not `HashMap`) gives deterministic output for snapshots and fixtures.
- serde: field names as declared.
  - **Timestamps** (D64): the instant fields (`start_time`, `end_time`, `ingest_time`, `SpanEvent.time`, and in `Score`: `created_at`, `updated_at`, `ingest_time`) stay `i64` ns in Rust and Arrow. In JSON they are **RFC 3339 strings, UTC, always 9 fractional digits, `Z` suffix**, e.g. `"2026-11-09T10:12:03.123456789Z"`, as the spec README requires.
    - Implement one module, `crate::time_serde`, with `serialize`/`deserialize` functions, plus an `option` submodule if needed, and use it via `#[serde(with = "crate::time_serde")]`.
    - Formatting goes through `chrono` (add `chrono` from the workspace to core's dependencies).
    - Parsing accepts any valid RFC 3339 offset and 0–9 fractional digits, converts to UTC ns, and rejects values outside the `i64` ns range with a serde error.
    - **Durations stay integers:** `ttft_ns` (and the computed `duration_ns`) are JSON numbers, because they are lengths of time, not instants.
    - Tests: round-trip at ns precision, a non-`Z` offset normalized to UTC, out-of-range rejected, and the pre-1970 negative case.
  - `Option` fields: `#[serde(default, skip_serializing_if = "Option::is_none")]`.
  - `Vec`/`BTreeMap` fields: `#[serde(default, skip_serializing_if = "…::is_empty")]`.
  - Fixtures then only contain meaningful fields.
- Payload invariant (documented on the fields, not enforced by the type): `input.is_some() == input_format.is_some()`, and the same for output. C7's conversion does not check it: spans can be read back from disk, and nothing may panic on disk input (AGENTS.md).
- No constructor, builder or `Default` derive: a struct literal is the API.
- Exception: provide `#[cfg(test)]`-only test helpers in `span.rs` (`fn minimal_span() -> Span`). Tests in other crates write their own literals.

**`score.rs`:**
```rust
pub enum ScoreTarget {
    Span { trace_id: TraceId, span_id: SpanId },
    Trace { trace_id: TraceId },
    Session { session_id: String },
}
pub enum ScoreValue {
    Numeric(f64),                                   // must be finite
    Boolean(bool),
    Categorical { label: String, value: Option<f64> },
    Text(String),
}
pub struct Score {
    pub project_id: ProjectId,
    pub score_id: uuid::Uuid,
    pub target: ScoreTarget,
    /// For span/trace targets: the session the target belongs to, when known. Ignored for session targets.
    pub session_id: Option<String>,
    pub user_id: Option<String>,
    pub name: String,
    pub value: ScoreValue,
    pub reason: Option<String>,
    pub source: ScoreSource,
    pub evaluator_id: Option<uuid::Uuid>,
    pub rule_id: Option<uuid::Uuid>,
    pub experiment_id: Option<uuid::Uuid>,
    pub dataset_item_id: Option<uuid::Uuid>,
    pub author_user_id: Option<uuid::Uuid>,
    pub metadata: BTreeMap<String, String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub deleted: bool,
    pub ingest_time: i64,
}
```
- Methods (they have consumers in C7 and the API):
  - `ScoreTarget::kind() -> ScoreTargetKind`
  - `ScoreValue::data_type() -> ScoreDataType`
  - `ScoreValue::value_num() -> Option<f64>`: numeric → `Some(v)`; boolean → `Some(0.0/1.0)`; categorical → `value`; text → `None`
  - `ScoreValue::value_str() -> Option<&str>`: categorical → label; text → text; others → `None`
- serde, same skip rules as `Span`:
  - `ScoreTarget`: internally tagged `{"type": "span", "trace_id": "…", "span_id": "…"}`.
  - `ScoreValue`: adjacently tagged `{"data_type": "numeric", "value": 0.8}`; categorical `{"data_type": "categorical", "value": {"label": "correct", "value": 1.0}}`.
- `Numeric` must be finite. Deserialization rejects NaN/±inf with a serde error; JSON cannot carry them anyway, but check explicitly in a hand-written `Deserialize`, or by validating through `#[serde(try_from = …)]` on a helper type.

**Tests:**
- Serde round-trip of a full `Span` (every field set) and of a minimal `Span`; insta snapshot of both JSONs.
- A minimal span's JSON contains only required fields.
- Score round-trip for each target × value combination.
- `value_num` / `value_str` / `data_type` mapping table test.
- Non-finite numeric rejected.

**Commit:** `core: add Span and Score structs`

---

## C6 — Arrow schemas

**Goal:** the exact Arrow schemas of the three tables, as the single definition used by otel tests, storage, query and the API.

**Dependencies:** `arrow` (workspace).
- Enable only what is needed: `arrow` with default features off, plus whatever `arrow-schema`, `arrow-array` and `arrow-ipc` require for C7.
- If feature trimming is awkward, depend on the umbrella `arrow` with defaults and note it in the report.

**Global type choices** (D62, reflected in spec 01):

| Spec type | Arrow type in memory |
|---|---|
| `Utf8` and `Utf8 (dict)` | `DataType::Utf8`. "(dict)" means dictionary encoding **in Parquet** (storage's writer setting), not an Arrow `Dictionary` type in memory. |
| `Timestamp` | `Timestamp(TimeUnit::Nanosecond, Some("UTC".into()))` |
| `FixedSizeBinary(16/8)` | `FixedSizeBinary(16)` / `FixedSizeBinary(8)` |
| `List(X)` | `List(Field::new("item", X, false))`: non-null items |
| `Map(Utf8, Utf8)` | `Map(Field::new("entries", Struct[Field::new("keys", Utf8, false), Field::new("values", Utf8, true)], false), false)`. These are the arrow-rs `MapBuilder` defaults; keys are not sorted. |
| `UInt16`, `Int64`, `Int32`, `Float64`, `Boolean` | as named |

**`schema.rs`:**
- `pub fn spans_schema() -> SchemaRef`, `pub fn traces_schema() -> SchemaRef`, `pub fn scores_schema() -> SchemaRef`. Each is backed by a `static …: LazyLock<SchemaRef>` and returns a cheap `Arc` clone.
- Column-name constants: `pub mod spans { pub const PROJECT_ID: &str = "project_id"; … }`, and the same for `traces` and `scores`, one constant per column in table order. Every schema field uses these constants; no string literals in field definitions.
- `pub const SPANS_LIGHT_COLUMNS: &[&str]`: the columns marked **P** in spec 01, in table order.
- Column order: exactly the spec 01 table order for each table.
  - `spans`: `duration_ns` is right after `end_time`, and `model` right after `response_model`.
  - `traces`: the spec writes `input_tokens … total_tokens`. Expand it to `input_tokens, output_tokens, cache_read_tokens, cache_write_tokens, reasoning_tokens, total_tokens`, and `input_preview / output_preview` to two columns, in that order.

**Nullability:**

| Table | Non-nullable columns | Everything else |
|---|---|---|
| `spans` | `project_id, trace_id, span_id, name, span_kind, start_time, end_time, duration_ns, status, tags, metadata, attributes, resource_attributes, events, links, media_refs, truncated, conventions, ingest_time, schema_version` | nullable |
| `traces` | `project_id, trace_id, start_time, end_time, name, has_root, status, tags, metadata, span_count, error_count, llm_count, models, last_ingest_time, partial_count` | nullable (`root_span_id`, `session_id`, `user_id`, `environment`, `release`, previews, token sums, `cost_total`) |
| `scores` | `project_id, score_id, target, name, data_type, source, metadata, created_at, updated_at, deleted, ingest_time` | nullable |

**Struct fields:**
- `events` item: `Struct{ time: Timestamp(ns,UTC) non-null, name: Utf8 non-null, attributes: Utf8 nullable }`.
- `links` item: `Struct{ trace_id: FixedSizeBinary(16) non-null, span_id: FixedSizeBinary(8) non-null, attributes: Utf8 nullable }`.

**Tests:**
- Insta snapshots `spans_schema.snap`, `traces_schema.snap`, `scores_schema.snap`, using the schema's `Debug`. If that is too noisy, use a custom one-line-per-field rendering: `name: Type [nullable]`.
- Every column constant appears in the schema, and every schema field has a constant. Check via a list in the test.
- Every name in `SPANS_LIGHT_COLUMNS` exists in `spans_schema()`.

**Commit:** `core: add Arrow schemas`

---

## C7 — Struct → `RecordBatch` conversion

**Goal:** convert `&[Span]` and `&[Score]` into batches that match the schemas exactly. This is the hot path of ingestion (spec 03 § WAL payload), so it is columnar and allocation-conscious.

**`batch.rs`:**
```rust
pub fn spans_to_record_batch(spans: &[Span]) -> Result<RecordBatch, CoreError>;
pub fn scores_to_record_batch(scores: &[Score]) -> Result<RecordBatch, CoreError>;
```
Add the `CoreError::Arrow` variant now.

**Implementation requirements:**
- One arrow builder per column, created with capacity `spans.len()`. Use `StringBuilder::with_capacity(len, estimated_bytes)` where cheap to estimate, e.g. the sum of string lengths for the heavy columns.
- Iterate the slice once, appending to every builder per row, then `RecordBatch::try_new(spans_schema(), columns)`.
- **Computed columns:**
  - `duration_ns = end_time - start_time`;
  - `model = response_model.or(request_model)`.
- **Enums:** append `as_str()`.
- **IDs:** `FixedSizeBinaryBuilder` with `as_bytes()`.
- **Score IDs and optional UUIDs:** hyphenated lowercase strings.
- **Scores:**
  - `target` = `target.kind().as_str()`;
  - `trace_id`/`span_id` set from the target;
  - `session_id` = the target's session for session targets, else `score.session_id`;
  - `data_type`, `value_num`, `value_str` from `ScoreValue` helpers.
- **Maps:** a `MapBuilder<StringBuilder, StringBuilder>` with field names matching C6. Keys in `BTreeMap` order.
- **Lists of structs** (`events`, `links`): a `ListBuilder<StructBuilder>`, or build child arrays directly and assemble `ListArray`/`StructArray` with offsets. Choose the simpler one and verify the resulting `DataType` equals the schema's exactly (field names and nullability included).
- An empty slice returns an empty batch with the right schema.

**Tests:**
- **Exact schema:** for a mixed batch (minimal + full spans), `batch.schema() == spans_schema()`. Same for scores.
- **Values:** spot-check every column for row 0 and row 1, e.g. `duration_ns`, `model` fallback, map entries in order, list contents, null vs empty list (empty list ≠ null).
- **Arrow IPC round-trip** (what the WAL does): write the batch with `arrow::ipc::writer::StreamWriter` to a `Vec<u8>`, read it back, and assert equality.
- **Parquet round-trip:** add `parquet` with the `arrow` feature as a **dev-dependency**. Write with `ArrowWriter` to a `Vec<u8>`, read back with `ParquetRecordBatchReaderBuilder`, and assert that the schema (after timezone/metadata normalization if needed) and the data are equal. This catches Map and List naming problems before storage hits them. If parquet changes a field name on read, report it; do not paper over it.
- **Performance smoke test**, `#[ignore]`d: convert 100 000 synthetic spans with ~2 KiB input/output each and print the elapsed time, so the implementer can sanity-check (target: well under 1 s in release). No benchmark harness yet.

**Commit:** `core: add Span/Score to RecordBatch conversion`

---

## C8 — Final review and report

**Do (no new features):**
1. Run `just check`.
2. Run `cargo doc -p llmobs-core --no-deps` and fix warnings. Every public item must be documented; add `#![warn(missing_docs)]` to `crates/core/src/lib.rs`, a lint local to this crate.
3. Check `cargo tree -p llmobs-core -e normal` for unexpected heavy dependencies. `arrow` is expected. `datafusion` and `parquet` must **not** be normal dependencies of core.
4. Re-read spec 01's three tables against `schema.rs` column by column. Every name, type, nullability and the order must match the plan above.

**Report back:**
- the versions chosen for new dependencies (`hex`, `strum`, `insta`, and the arrow features used);
- whether Parquet round-trips the Map/List types unchanged;
- the timing of the ignored performance test (release);
- any deviation, and why.

**Commit:** `core: docs and final checks` (only if C8 changed anything)
