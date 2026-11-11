# B1 · OTLP normalization (`llmobs-otel`)

Track B1 of Wave 1 implements spec 02 inside `crates/otel`. It turns an OTLP `ExportTraceServiceRequest` into normalized `llmobs_core::Span`s, extracted media blobs, and a count of rejected spans. It covers:
- decoding;
- convention detection;
- the mapping tables and normalization rules;
- media extraction;
- truncation;
- cost.

HTTP and gRPC handlers, authentication, rate limits, the WAL append, the live bus and online-eval triggers belong to the server (Wave 2).

The plan has 23 sub-phases, **N1–N23**. Each is done by one agent, in order, and ends with `just check` green and one commit.

**Read first, for every sub-phase:**
- `AGENTS.md`;
- `docs/spec/02-ingestion.md` (after N1, with this plan's decisions);
- `docs/spec/01-data-model.md` § Span and § Payloads;
- `fixtures/otlp/README.md`;
- `docs/research/ingestion/README.md`;
- the sub-phase's own section.

## Files

| File | Sub-phases |
|---|---|
| [01-foundations.md](01-foundations.md) | N1 spec and decisions · N2 decode · N3 attribute values and the attribute bag · N4 the span skeleton, rejections and golden harness |
| [02-fields.md](02-fields.md) | N5 field tables and detection · N6 span kind · N7 model, provider, params, context · N8 usage and TTFT |
| [03-payloads.md](03-payloads.md) | N9 payload model and previews · N10 OpenAI converter · N11 Anthropic converter · N12 Gemini converter · N13 llmobs and GenAI payloads |
| [04-media-truncation-cost.md](04-media-truncation-cost.md) | N14 media extraction · N15 truncation · N16 price table and cost · N17 built-in price snapshot |
| [05-conventions.md](05-conventions.md) | N18 OpenInference · N19 Vercel · N20 OpenLLMetry · N21 Langfuse and Laminar · N22 Google ADK |
| [06-hardening.md](06-hardening.md) | N23 property tests |

## Decisions

These were settled with the user on 2026-11-10. N1 writes them into `docs/mvp-decisions.md` and spec 02. **Numbering:** A1 uses D83–D90 and A2 uses D89–D101. D100 and D101 were already A2's on this branch (DSL casts, SQL time window), so B1 is D102–D113.

| # | Decision | Source |
|---|---|---|
| D102 | **The normalizer is a hybrid; there is no convention trait.** The span's attributes go into an `AttrBag` that records which keys were read and how (payload or not), which implements rule 2. **Scalar fields** are declarative precedence tables of `(key, convention, value type)`: model, provider, tokens, context, params, reported cost. Spec 02's tables become data in `fields.rs`. **Span kind, payloads (input, output, system instructions, tool definitions) and TTFT** are plain functions, one per convention, called in precedence order. The first one that returns a value wins, all-or-nothing per field: message lists from two conventions are never merged. | User |
| D103 | **Normalization is pure and returns its media.** `normalize` returns `Normalized { spans, media, rejections }`. `media` holds the extracted blobs (`llmobs_core::media::MediaBlob`: SHA-256, MIME type, bytes as `Vec<u8>`). The server stores them with `Storage::put_media` before the WAL append, and storage owns the `media` table (A1 draft). Spec 02 § Media extraction step 4 moves to the server. | User |
| D104 | **The built-in price table is a converted snapshot checked into the repo** (`crates/otel/prices/builtin.json`), embedded with `include_str!`. No network access at build time. `just prices-update <litellm.json>` regenerates it through an env-gated test, like `fixtures-fmt`. It is refreshed by hand each release, and the diff is reviewed. | User |
| D105 | **The output is `Vec<Span>`.** The server converts it with `llmobs_core::batch` before `append_spans`. Building Arrow directly is deferred until a benchmark asks for it. | User |
| D106 | **Timestamps.** OTLP `u64` nanoseconds above `i64::MAX` reject the span (`timestamp_overflow`). The D85 bounds reject the span (`start_time_out_of_range`). `end < start` is **clamped**: `end_time = start_time`, and the attribute `llmobs.normalize_warning` gets `end_before_start`. The span is kept. | User |
| D107 | **AnyValue limits.** Nesting deeper than 32 levels is replaced by the string `[llmobs: max depth]`, and the attribute name is added to `truncated`. Duplicate keys in one key-value list: the **last** one wins, which is what the OTel SDKs do. There is no attribute-count cap: the request limits bound it. | User |
| D108 | **Strict OTLP/JSON.** Only the standard encoding is accepted: integer enums, hex IDs, int64 as a string or a number. Enum names fail decoding, and the server answers 400. Unknown fields are ignored (`opentelemetry-proto` 0.33.1 `with-serde` has no `deny_unknown_fields`). ID length is checked per span by rule 15, because `with-serde` accepts any length. Fixtures pin each quirk. | User |
| D109 | **Reported cost covering only some parts.** Any reported cost attribute makes `cost_source = reported`. Reported parts fill their columns and missing parts stay null. `cost_total` is the reported total, else the sum when both parts are present, else null. A span's cost is never part price table and part reported. | User |
| D110 | **Token semantics.** Every usage source declares whether its input count includes cache tokens and whether its output count includes reasoning tokens. Exclusive sources are added back. Safety net: if `input_tokens < cache_read + cache_write` after resolution, the input is treated as exclusive, the cache is added back, and the warning `input_excludes_cache` is recorded. Per-modality counts (`gen_ai.usage.{audio,image,text}.*`, `…_details.audio`) are subsets: they are never summed and stay in `attributes`. | Research (genai-mapping gaps 4–7) |
| D111 | **Rejections.** `RejectReason` is a closed enum. `Rejections::message()` is bounded: at most 4 reasons with counts, sorted by count then name, never any value or key from the payload. The server puts it in `partial_success.error_message`. | Research (GreptimeDB #7892) |
| D112 | **Robustness tests use proptest** in `just test`. There is no cargo-fuzz: it needs nightly, and the toolchain is pinned. | User |
| D113 | **Spec 02 additions from the research.** Rows backed by the GenAI semconv or by several competitors go into spec 02 now (N1). Behaviours reported by one source only are written as "pinned by a captured fixture": the Vercel v5 + Anthropic exclusive input, event-based TTFT, and the OpenLLMetry Anthropic and ADK token semantics. They get hand-written fixtures until B2 captures exist. | User |

## Boundaries

```
server ── body bytes ──▶ llmobs_otel::decode_protobuf / decode_json ──▶ ExportTraceServiceRequest
server ── request, IngestContext, &PriceTable ──▶ llmobs_otel::normalize ──▶ Normalized { spans, media, rejections }
server ── media ──▶ storage.put_media          server ── spans → RecordBatch (core) ──▶ storage.append_spans
server ── model_prices rows (meta.db) ──▶ PriceTable::new          server ◀── builtin_price_rows() (seeds meta.db)
```

- **Server (Wave 2):**
  - HTTP and gRPC endpoints, decompression, body and gRPC size limits, and the spans-per-request limit (413);
  - authentication and rate limits;
  - `put_media`, the WAL append, the live bus, online-eval triggers;
  - the OTLP response, built from `Rejections`;
  - building the `PriceTable` from `model_prices` (scope priority comes from `PriceRow.scope`);
  - seeding the built-in rows at startup (spec 04 § Pricing).
- **A1 (storage):**
  - partials come from spans at flush (D88), so B1 copies nothing to trace partials;
  - storage owns media blobs and the `media` table;
  - D85 is implemented by B1 (N4), with `retention_days` passed in `IngestContext`.
- **A2 (query):** none. A2 reads the columns B1 fills.
- **B2 (fixture capture, started later by the user):**
  - B1 writes hand-written fixtures for every rule it implements;
  - B2's captured cases are blessed with `LLMOBS_BLESS=1` (N4) and reviewed;
  - each D113 "pinned by a captured fixture" behaviour gets re-checked against them;
  - the golden test's `PENDING` list (N4) must be empty when B1 ends; B2's cases never go into it.
- **Core (`llmobs-core`)** is a shared contract. B1 does not change it. The requests below were made by the coordinating session and are on `main`.

### Requests for `llmobs-core`

| # | Needed by | Request |
|---|---|---|
| C1 | N4 | `metadata_key::NORMALIZE_WARNING` (`llmobs.normalize_warning`) in `names.rs`. The value is a JSON array string of warning codes, sorted and deduplicated. **Done.** |
| C2 | N14 | `llmobs_core::media::MediaBlob { sha256: String /* 64 lowercase hex */, mime_type: String, bytes: Vec<u8> }`, whose `Debug` prints the size, never the bytes. Used by `llmobs-otel` (produced) and `llmobs-storage` (`put_media`). **Done.** |
| C3 | N3 | `marker::MAX_DEPTH` (`[llmobs: max depth]`) next to the truncation markers. **Done.** |

## Public API at the end of B1

The whole public surface of `llmobs-otel`. Sub-phases add items in this shape. Changing it means updating this section in the same commit.

```rust
pub use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;

pub fn decode_protobuf(body: &[u8]) -> Result<ExportTraceServiceRequest, DecodeError>;
pub fn decode_json(body: &[u8]) -> Result<ExportTraceServiceRequest, DecodeError>;

pub struct IngestContext {
    pub project_id: ProjectId,
    pub ingest_time: i64,               // ns
    pub retention_days: Option<u32>,    // None = keep forever
    pub limits: IngestLimits,
}
pub struct IngestLimits { pub max_attribute_bytes: usize, pub media_min_bytes: usize, pub media_max_bytes: usize }
impl Default for IngestLimits { /* 8 MiB, 4 KiB, 32 MiB (spec 02 § Limits) */ }

pub fn normalize(request: ExportTraceServiceRequest, ctx: &IngestContext, prices: &PriceTable) -> Normalized;
pub struct Normalized { pub spans: Vec<Span>, pub media: Vec<MediaBlob>, pub rejections: Rejections }
pub struct Rejections { /* private counts */ }
impl Rejections { pub fn total(&self) -> u64; pub fn count(&self, reason: RejectReason) -> u64; pub fn message(&self) -> Option<String>; }
pub enum RejectReason { InvalidTraceId, InvalidSpanId, TimestampOverflow, StartTimeOutOfRange }

pub struct PriceTable { /* compiled */ }
pub struct PriceRow { /* spec 04 model_prices columns + scope */ }
pub enum PriceScope { Builtin, Org, Project }
pub struct PriceTier { /* min_input_tokens + prices */ }
impl PriceTable { pub fn new(rows: Vec<PriceRow>) -> Result<Self, PriceError>; }
pub fn builtin_price_rows() -> Vec<PriceRow>;

pub enum DecodeError { /* thiserror */ }
pub enum PriceError { /* thiserror */ }
```

`normalize` cannot fail. Problems with individual spans go into `rejections` or into the span's warnings and `normalize_error`.

## Module layout at the end of B1

```
crates/otel/
  src/
    lib.rs            re-exports, crate docs
    error.rs          DecodeError, PriceError
    decode.rs         N2
    value.rs          N3 AnyValue → compact JSON / string, depth limit
    bag.rs            N3 AttrBag (consumed tracking, rule 2)
    normalize.rs      N4 request/resource/scope loop, IngestContext, Normalized, span skeleton
    reject.rs         N4 RejectReason, Rejections
    warn.rs           N4 warning codes
    detect.rs         N5 conventions present (rule 13)
    fields.rs         N5 Source/Field tables and the resolver
    kind.rs           N6
    context.rs        N7 model, provider (+ alias table), operation, params, session/user/tags/metadata/env/release
    usage.rs          N8 tokens, semantics, total, TTFT
    payload.rs        N9 Payload, formats, previews, tool args, JSON-in-string decoding
    convert/          N10–N12 openai.rs, anthropic.rs, gemini.rs (+ vercel.rs N19, openinference.rs N18)
    conventions/      N13 llmobs.rs, genai.rs; N18–N22 openinference.rs, vercel.rs, openllmetry.rs, langfuse.rs, laminar.rs, adk.rs
    media.rs          N14
    truncate.rs       N15
    cost.rs           N16 PriceTable, matching, computation, reported cost
    prices/builtin.json  N17 (outside src/, include_str!)
  tests/
    fixtures.rs, otlp_json.rs, support/   (existing; N4 extends support)
    golden.rs        N4
    decode.rs        N2
    prices.rs        N17
    proptest.rs      N23
```

The `convert/` and `conventions/` files may be merged or split if the code reads better. Say so in the report.

## Rules for every sub-phase

- **Edit only** the files the sub-phase lists. `crates/otel/Cargo.toml` and the root `Cargo.toml` may be edited when the sub-phase adds a listed dependency.
- **Dependencies:** each version is declared once in `[workspace.dependencies]`. The versions below were checked on 2026-11-10 (`cargo info`); recheck at the time:
  - `prost` 0.14 (must match `opentelemetry-proto` 0.33.1; the lock has 0.14.4);
  - `proptest` 1.12;
  - `regex`, `sha2`, `base64`, `serde_json`, `thiserror` and `opentelemetry-proto` are already in the workspace;
  - `opentelemetry-proto` moves from dev-dependency to dependency of `llmobs-otel` in N2;
  - no MIME sniffing crate: N14 uses a small magic-byte table.
- **No panics on input.** No `unwrap`/`expect`/indexing that can fail on data from the request. Integer conversions are checked, and arithmetic on token counts saturates.
- **Privacy:** errors, warnings, rejection messages and `tracing` output never contain attribute values, payloads, keys from the payload or IDs. Codes, counts and sizes only.
- **Fixtures:** each hand-written case follows `fixtures/otlp/README.md`. Run `just fixtures-fmt` then `just fixtures-check`. A new case comes with a `case.json` whose `spec_refs` point at the rule it pins.
- **Golden test:** from N4 on, `cargo test -p llmobs-otel --test golden` must pass. A sub-phase that makes a `PENDING` case pass removes it from the list. The test fails if a pending case passes.
- **Checks:** `just check` green (the PATH needs Node; see AGENTS.md § Commands).
- **Report:**
  - what was done;
  - any deviation from this plan or the spec, and why;
  - new dependency versions;
  - which `PENDING` cases were removed;
  - anything unverified.
- **One commit** per sub-phase, with the given subject, ending with the attribution line of AGENTS.md.

## Order and parallelism

N1 → N2 → N3 → N4 → N5 → (N6, N7, N8, N9 in any order) → N10, N11, N12 (any order, after N9) → N13 → N14 → N15 → N16 → N17 → N18…N22 (any order) → N23.

They're meant to run one at a time. N6–N8 and N10–N12 touch separate files and could run in parallel worktrees, but they share `fields.rs`, the fixtures and `golden.rs`'s `PENDING` list, so merge conflicts are likely. Run them in parallel only if a merge step is accepted.
