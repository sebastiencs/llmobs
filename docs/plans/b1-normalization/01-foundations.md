# B1 · Foundations (N1–N4)

See [README.md](README.md) for the decisions, the boundaries, the public API and the rules that apply to every sub-phase.

## N1 — Spec 02 and the decisions

**Edit only:**
- `docs/spec/02-ingestion.md`;
- `docs/spec/01-data-model.md` (only the `attributes` row note below);
- `docs/mvp-decisions.md`;
- `fixtures/otlp/README.md`;
- `docs/plans/b1-normalization/` (renumbering only).

**Commit:** `docs: settle B1 normalization decisions in spec 02`

**Prerequisite.** A1's spec edits (rule 15 with the D85 bounds, media step 4 through `llmobs-storage`) are on branch `worktree-a1-storage-design`.
- If they are on `main`, build on them.
- If not, write rule 15 and media step 4 exactly as below, then report the overlap so whoever merges second resolves it.

Check the D numbers in `docs/mvp-decisions.md` on `main`. If D100–D111 are taken, renumber this plan's decisions to the next free numbers, in this directory too.

### 1. `docs/mvp-decisions.md`

Add D100–D111 from the README's decisions table, one row each, in the file's existing format (`| D… | Area | Decision |`). Keep the text short and point at spec 02 for details.

### 2. Spec 02 § Pipeline

Replace the paragraph under the diagram with:

> Normalization is a pure function in `llmobs-otel`: `normalize(request, ingest_context, price_table) -> Normalized { spans, media, rejections }` (D100, D101, D103). It does no I/O. The server stores each returned media blob through `llmobs-storage` before the WAL append. Re-sends are safe because blobs are content-addressed. `ingest_context` carries the project ID, the ingest time, the project's `retention_days` and the limits.

In the diagram, add a step `store media` after `compute cost`.

### 3. Spec 02 § Endpoints

After the encodings table, add:

> OTLP/JSON follows the OTLP specification strictly (D106): lowerCamelCase fields, hex trace and span IDs, enums as integers, 64-bit integers as decimal strings or numbers. Enum names, base64 IDs and unknown fields fail decoding, and the request gets `400`.

Check that unknown fields really fail with `with-serde` 0.33.1; N2 has a test for it. If they don't fail, write "unknown fields are ignored" instead, and say so in the report.

### 4. Spec 02 § Convention detection

Add after the precedence paragraph:

> Within one field, the first convention that yields a value wins as a whole. Message lists from two conventions are never merged (D100). Numbered attributes (`gen_ai.prompt.N.*`, `llm.input_messages.N.*`, …) are parsed by index, never matched by a raw key prefix: `gen_ai.prompt.name` is not part of `gen_ai.prompt.N`.

Add a **detection table**: a convention is listed in `conventions` when the span has at least one attribute or event matching it.

| Convention | Present when |
|---|---|
| `llmobs` | any `llmobs.*` attribute |
| `genai` | any `gen_ai.*` attribute **except** the OpenLLMetry ones below, or an event named `gen_ai.*` |
| `openinference` | `openinference.span.kind`, or any `llm.*`, `input.value`, `output.value`, `embedding.*`, `retrieval.*`, `tool.*` attribute |
| `vercel` | any `ai.*` attribute |
| `openllmetry` | any `traceloop.*` attribute, or `gen_ai.prompt.N.*` / `gen_ai.completion.N.*` (N a decimal index), or `llm.request.type`, or `llm.request.functions.N.*` |
| `langfuse` | any `langfuse.*` attribute |
| `laminar` | any `lmnr.*` attribute |
| `google_adk` | any `gcp.vertex.agent.*` attribute |

`llm.request.type` and `llm.request.functions.*` count for both `openinference` (prefix `llm.`) and `openllmetry`. That's harmless, since `conventions` is a debug aid. Add it as a note.

### 5. Spec 02 § Mapping tables

Apply the rows below from `docs/research/ingestion/genai-mapping.md` § Gap table. The gap number is in parentheses. Precedence inside each cell is left to right.

**Span kind** table:
- `gen_ai.operation.name`: add `retrieval` → `retriever`; `invoke_workflow`, `invoke_runner`, `plan` → `chain`. Any other value → no decision from this source (10).
- `fallback`: `gen_ai.tool.name` or `gen_ai.tool.call.id` present → `tool`, checked **before** the model fallback (11).

**Input / output** table:
- GenAI row: add the span event `gen_ai.client.inference.operation.details`. Its attributes `gen_ai.input.messages`, `gen_ai.output.messages` and `gen_ai.system_instructions` are read when the span attributes don't have them (12).
- GenAI legacy events row: note that these are log records in the semconv and appear on spans only through bridges such as Strands and Logfire (13).
- New column or footnote, **tool definitions** (14):
  - `gen_ai.tool.definitions`;
  - OpenInference `llm.tools.N.tool.json_schema`;
  - Vercel `ai.prompt.tools` (array of JSON strings);
  - OpenLLMetry `llm.request.functions.N.{name,description,parameters}`.

  All are normalized to one element per tool (rule 7).
- Vercel row (15):
  - tool-call parts use `args` (v4) or `input` (v5+);
  - tool-result parts use `result` (v4) or `output`, where v5+ sends `{type, value}` and the converter takes `value`;
  - `image`/`file` parts carry raw base64 in `image`/`data` with `mimeType`/`mediaType`, and become `blob` parts.
- Add a paragraph **Payload encodings** (16). A message- or JSON-bearing attribute may arrive in any of these forms:
  - a JSON string;
  - a string containing a JSON string that itself contains JSON, decoded at most twice;
  - a structured `arrayValue`/`kvlistValue`, converted to JSON with rule 1's encoding.

**Model, provider, usage** table:
- `cache_read_tokens` (2): `gen_ai.usage.cache_read.input_tokens`, `llm.token_count.prompt_details.cache_read`, `ai.usage.inputTokenDetails.cacheReadTokens`, `ai.usage.cachedInputTokens`, `gen_ai.usage.cache_read_input_tokens`, `gen_ai.usage.details.cache_read_tokens`, `gen_ai.usage.input_tokens.cached`.
- `cache_write_tokens` (1, 2, 3): `gen_ai.usage.cache_write.input_tokens`, `gen_ai.usage.cache_creation.input_tokens`, `llm.token_count.prompt_details.cache_write`, `ai.usage.inputTokenDetails.cacheWriteTokens`, `gen_ai.usage.cache_creation_input_tokens`, `gen_ai.usage.cache_write_input_tokens`, `gen_ai.usage.details.cache_write_tokens`.
- `reasoning_tokens` (6): `gen_ai.usage.reasoning.output_tokens`, `llm.token_count.completion_details.reasoning`, `ai.usage.outputTokenDetails.reasoningTokens`, `ai.usage.reasoningTokens`, `gen_ai.usage.reasoning_tokens`, `gen_ai.usage.output_tokens.reasoning`.
- `input_tokens`: add `ai.usage.inputTokens` (v5+) before `ai.usage.promptTokens` (v4), as today. `output_tokens`: the same for `outputTokens`/`completionTokens`.
- `total_tokens` (24): `llm.token_count.total`, `gen_ai.usage.total_tokens`, `ai.usage.totalTokens`, `ai.usage.tokens`, `llm.usage.total_tokens`, else rule 4.
- `ttft_ns` (18):
  - `llmobs.ttft_ns`;
  - `gen_ai.response.time_to_first_chunk` (seconds, ×1e9);
  - `ai.response.msToFirstChunk`, then `ai.stream.msToFirstChunk` (ms, ×1e6);
  - `langfuse.observation.completion_start_time` (an instant: `ttft = instant − start_time`, null when negative).

  Event-based sources (`ai.stream.firstChunk`, OpenInference "First Token Stream Event") are added only once a captured fixture shows them (D111).
- `request_params` (22): every `gen_ai.request.*` attribute except `gen_ai.request.model`, plus `llm.invocation_parameters` (a JSON object string, expanded), `ai.settings.*` and `langfuse.observation.model.parameters` (a JSON object string, expanded). The key is the suffix after the prefix, as in rule 8.
- `provider` (9): extend the rule 12 alias table with:
  - the semconv well-known values, kept as they are: `aws.bedrock`, `azure.ai.openai`, `azure.ai.inference`, `gcp.vertex_ai`, `gcp.gemini`, `gcp.gen_ai`, `x_ai`, `ibm.watsonx.ai`, `mistral_ai`, `groq`, `deepseek`, `perplexity`, `cohere`, `anthropic`, `openai`;
  - the Vercel IDs, mapped: `google.generative-ai` → `gcp.gemini`, `google.vertex.*` → `gcp.vertex_ai`, `amazon-bedrock` → `aws.bedrock`, `azure.*` → `azure.ai.openai`, `xai.*` → `x_ai`, `mistral.*` → `mistral_ai`, `<name>.<anything>` → `<name>` otherwise;
  - `xai` → `x_ai`.
- Reported cost (8): `llmobs.cost.total`, `llm.cost.total`, `gen_ai.usage.cost`, `gen_ai.usage.total_cost` fill `cost_total`. `llm.cost.prompt` and `gen_ai.usage.input_cost` fill `cost_input`. `llm.cost.completion` and `gen_ai.usage.output_cost` fill `cost_output`. Non-numeric values are ignored and stay in `attributes`. D107 applies.

**Context fields** table:
- `session_id` (19, 20): add `lmnr.association.properties.session_id`, after `langfuse.session.id`.
- `user_id`: add `lmnr.association.properties.user_id`.
- `tags` (19, 21):
  - add `ai.telemetry.metadata.tags` and `lmnr.association.properties.tags`;
  - each source may be a string array or a string containing a JSON array of strings;
  - any other form, such as a comma-separated list, is not a tag source and stays in `attributes` as a consumed attribute.
- `metadata`: add `lmnr.association.properties.metadata.*`.
- `release`: add `lmnr.span.release` (span) after `langfuse.release`.

**New subsection "Token semantics"**, replacing the current "Token semantics" paragraph, after the usage table (D108). It contains the D108 text plus a per-source table:

| Source | Input includes cache | Output includes reasoning |
|---|---|---|
| GenAI `gen_ai.usage.*` | yes (semconv) | yes |
| OpenInference `llm.token_count.*` | yes | yes |
| Vercel v6 (`inputTokenDetails` present) | yes (`noCacheTokens` + cache) | yes |
| Vercel ≤ v5 + provider `anthropic` | **no** (pinned by a captured fixture, D111) | yes |
| Vercel other | yes | yes |
| OpenLLMetry Anthropic | unverified (D111); treat as yes and rely on the safety net | yes |
| Google ADK / Gemini native | yes | **no**: thoughts are outside the output, so add `reasoning` to the output |
| Langfuse `usage_details` | the `input` key excludes `input_cached_tokens`/`cache_read_input_tokens` | yes |

The Vercel ≤ v5 + Anthropic rule keys on the span's `ai.*` attributes plus the normalized provider. It applies even when the count was read from `gen_ai.usage.input_tokens` on that same span, because Vercel mirrors its own count there.

Add: per-modality counts (`gen_ai.usage.{audio,image,text}.*`, `llm.token_count.*_details.audio`) are subsets. They never add to any column and stay in `attributes` as unconsumed attributes.

**Finish reasons** (23): extend rule 16 into a general rule.
- In a GenAI output message list, each message keeps its own `finish_reason`. A missing one is filled from `gen_ai.response.finish_reasons[i]`.
- For OpenLLMetry, `gen_ai.completion.N.finish_reason` goes on message N.
- For OpenInference, `llm.output_messages.N.message.finish_reason` goes on message N, if present.

### 6. Spec 02 § Normalization rules

Add rules 17–21:

17. **Timestamps** (D104). `start_time_unix_nano` and `end_time_unix_nano` above `i64::MAX` reject the span. `end < start` sets `end_time = start_time` and records the warning `end_before_start`. A zero `end_time_unix_nano` (an unended span) counts as `end < start`.
18. **Warnings.** Recoverable oddities are recorded as warning codes in the attribute `llmobs.normalize_warning`: a JSON array of strings, sorted, without duplicates. Codes: `duplicate_key`, `end_before_start`, `input_excludes_cache`, `invalid_event_time`, `invalid_number`, `max_depth`. The set is closed. Adding a code means updating this rule.
19. **AnyValue limits** (D105). Nesting deeper than 32 levels (array or kvlist inside array or kvlist) is replaced at depth 33 by the string `[llmobs: max depth]`. The attribute name goes into `truncated`, and the `max_depth` warning is recorded. Duplicate keys in one key-value list: the last one wins, and `duplicate_key` is recorded. This applies to attributes, resource attributes, event and link attributes, and nested kvlists.
20. **Numbers.** A field typed as an integer accepts `intValue`, a `doubleValue` with no fractional part, or a string holding a decimal integer. A field typed as a float accepts `intValue`, `doubleValue` or a numeric string. Anything else does not fill the field. The attribute stays in `attributes`, unconsumed, and `invalid_number` is recorded. Negative token counts are invalid numbers.
21. **Rejection reasons** (D109). The rejection reasons are `invalid_trace_id`, `invalid_span_id`, `timestamp_overflow` and `start_time_out_of_range`. `partial_success.error_message` lists at most 4 of them with counts, for example `rejected 3 spans: invalid_span_id ×2, start_time_out_of_range ×1`, and never any value from the request.

Edit rule 15: reference rule 21 for the reasons. Edit the rule 2 bullet "Payload attributes of lower-precedence conventions…": events consumed as payload sources are removed from `events`. That covers `gen_ai.client.inference.operation.details`, the legacy `gen_ai.*.message` events and `gen_ai.choice`.

### 7. Spec 02 § Media extraction

- Step 1: add Vercel `image`/`file` parts (raw base64 in `image`/`data`) to the candidates. The Vercel converter turns them into `blob` parts.
- Step 3: "sniff MIME if missing" uses a fixed magic-byte table (N14): PNG, JPEG, GIF, WebP, PDF, WAV, MP3, OGG, FLAC, MP4/M4A, WebM. Otherwise it uses the declared type, else `application/octet-stream`.
- Step 4: as in A1's text, the server stores the blob through `llmobs-storage`. Add "(normalization returns the blob; D101)".

### 8. Spec 02 § Cost computation

- Step 1: reference the reported-cost keys and D107.
- After the built-in prices paragraph, add D102: "The snapshot is converted by `just prices-update` and committed as `crates/otel/prices/builtin.json`. It is never fetched at build time or at runtime."

### 9. Spec 01

`attributes` row: append "Includes `llmobs.normalize_warning` (02 rule 18) and `llmobs.normalize_error`."

### 10. `fixtures/otlp/README.md`

- `settings`: add `retention_days` (integer ≥ 1). Absent means no retention.
- `context`: unchanged.
- Add a paragraph: a case whose spans are rejected for D85 uses `context.ingest_time` and `settings.retention_days` to place them out of range.
- Note the warning attribute (rule 18): `expected.json` includes it in `attributes` when present.

### Checks and report

`just check` passes; the change is docs only. Report the final D numbers, whether A1's text was already on `main`, and any gap-table row you left out and why.

---

## N2 — Decoding

**Edit only:**
- `crates/otel/Cargo.toml`;
- root `Cargo.toml` (add `prost`);
- `crates/otel/src/{lib.rs,error.rs,decode.rs}`;
- `crates/otel/tests/decode.rs`.

**Commit:** `otel: decode OTLP protobuf and JSON requests`

### Dependencies

- `opentelemetry-proto`: move from `[dev-dependencies]` to `[dependencies]`, still `workspace = true`.
- `prost`: add to the workspace with the version that `opentelemetry-proto` 0.33.1 uses (0.14.x; take the exact minor from `Cargo.lock`, `default-features = false, features = ["std"]`). Then add it to `[dependencies]`. `cargo deny` must not report a second `prost`.
- `thiserror` and `serde_json` become dependencies (workspace).

### Code

`error.rs`:

```rust
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("invalid OTLP protobuf: {0}")]
    Protobuf(#[source] prost::DecodeError),
    #[error("invalid OTLP JSON at line {line}, column {column}: {category}")]
    Json { line: usize, column: usize, category: JsonErrorCategory },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonErrorCategory { Syntax, Data, Eof }   // Display: "syntax", "data", "eof"
```

`serde_json::Error`'s message can echo input (unknown field names, invalid values). Keep only `line()`, `column()` and `classify()`. Never store the message. Does `prost::DecodeError`'s message contain payload bytes? Check its `Display`: it describes the wire problem and the field path, which is generated code, not data. If it can include input data, replace it with a fixed message plus the description category. Report what you found.

`decode.rs`:

```rust
pub fn decode_protobuf(body: &[u8]) -> Result<ExportTraceServiceRequest, DecodeError>; // prost::Message::decode
pub fn decode_json(body: &[u8]) -> Result<ExportTraceServiceRequest, DecodeError>;     // serde_json::from_slice
```

`lib.rs`: module docs pointing at spec 02, `pub use` of `ExportTraceServiceRequest`, `decode_*` and `DecodeError`.

### Tests (`tests/decode.rs`)

Use inline JSON or protobuf built from the prost types. No fixtures.

- A protobuf round trip: build a request with one span, encode it, decode it, compare.
- Garbage bytes → `DecodeError::Protobuf`.
- JSON quirks, each its own test. Assert the outcome and add a comment on why.
  - hex IDs → OK;
  - int64 as a string and as a number → OK;
  - `kind: "SPAN_KIND_CLIENT"` → `Json` error;
  - base64 IDs (24 chars) → assert what actually happens and pin it. If `with-serde` accepts them as hex-decoding failures, they're an error. If it decodes garbage-length IDs, rule 15 catches them later (N4).
  - a 3-byte hex trace ID → decodes; N4 rejects it;
  - an unknown field → assert the actual behaviour and pin it. This settles the N1 spec sentence; fix the spec in this commit if N1 guessed wrong.
  - empty `{}` → OK, with no resource spans;
  - an `AnyValue` with no value key (`{}`) → pin it. OpenObserve #15999 shows some versions reject the whole batch.
- Error messages contain no input. Decode `{"resourceSpans":[{"scopeSpans":[{"spans":[{"name":"SECRET-123","kind":"SECRET-456"}]}]}]}` and assert that `err.to_string()` contains neither `SECRET`.

### Report

What `with-serde` does for each quirk; the `prost` version.

---

## N3 — Attribute values and the attribute bag

**Edit only:** `crates/otel/src/{lib.rs,value.rs,bag.rs,warn.rs}`.

**Commit:** `otel: encode attribute values and track consumed attributes`

**Prerequisite:** core request C3 (`marker::MAX_DEPTH`) is on `main`. If it isn't, stop and report.

### `warn.rs`

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) enum Warning { DuplicateKey, EndBeforeStart, InputExcludesCache, InvalidEventTime, InvalidNumber, MaxDepth }
impl Warning { pub(crate) fn code(self) -> &'static str }  // snake_case codes of rule 18
#[derive(Debug, Default)]
pub(crate) struct Warnings(BTreeSet<Warning>);
impl Warnings {
    pub(crate) fn add(&mut self, w: Warning);
    /// `None` when empty; else the JSON array string of codes in `Ord` order (= alphabetical: keep the enum order alphabetical and test it).
    pub(crate) fn to_attribute(&self) -> Option<String>;
}
```

### `value.rs` (rule 1, rule 19, rule 20)

```rust
pub(crate) const MAX_DEPTH: usize = 32;
/// Compact JSON for any AnyValue (rule 1: int → number, double → number, bool, string → JSON string,
/// array → array, kvlist → object (last duplicate wins), bytes → base64 JSON string, empty AnyValue → null).
/// Deeper than MAX_DEPTH: the subtree becomes the JSON string marker::MAX_DEPTH and `*hit_depth = true`.
pub(crate) fn to_json(value: &AnyValue, hit_depth: &mut bool, dup: &mut bool) -> serde_json::Value;
/// Map value encoding (rule 1): stringValue verbatim, everything else compact JSON text.
pub(crate) fn to_map_string(value: &AnyValue, hit_depth: &mut bool, dup: &mut bool) -> String;
pub(crate) fn as_str(value: &AnyValue) -> Option<&str>;
pub(crate) fn as_i64(value: &AnyValue) -> Result<Option<i64>, InvalidNumber>;  // rule 20; Ok(None) = no value
pub(crate) fn as_f64(value: &AnyValue) -> Result<Option<f64>, InvalidNumber>;  // rule 20; non-finite = invalid
pub(crate) fn as_string_list(value: &AnyValue) -> Option<Vec<String>>; // arrayValue of strings, or a string holding a JSON array of strings
```

- **No recursion without a depth bound.** Recursing up to 32 levels is fine, since the bound is enforced.
- **A double is not finite JSON** (NaN or inf): encode it as `null` in JSON and record `invalid_number`. Pass `&mut Warnings` instead of the two bools if that reads better.
- **Rule 20 strings:** `"12"` → 12. `"12.0"` → invalid for an integer field and 12.0 for a float field. `" 12"` → invalid. The exponent form `"1e3"` → valid as a float, invalid as an integer.

### `bag.rs` (rule 2)

```rust
pub(crate) struct AttrBag<'a> { /* key → (&'a AnyValue, Use) ; insertion order of the request; duplicates resolved last-wins */ }
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Use { Unread, Consumed, Payload }
impl<'a> AttrBag<'a> {
    pub(crate) fn new(attrs: &'a [KeyValue], warnings: &mut Warnings) -> Self;  // records DuplicateKey
    pub(crate) fn get(&self, key: &str) -> Option<&'a AnyValue>;                 // peek, no state change
    pub(crate) fn take(&mut self, key: &str) -> Option<&'a AnyValue>;           // marks Consumed (never downgrades Payload)
    pub(crate) fn take_payload(&mut self, key: &str) -> Option<&'a AnyValue>;   // marks Payload
    pub(crate) fn drop_payload(&mut self, key: &str);                            // marks Payload even if not used (lower-precedence payloads, rule 2)
    pub(crate) fn keys_with_prefix(&self, prefix: &str) -> impl Iterator<Item = &'a str> + '_; // for ai.settings.*, metadata.* sweeps
    pub(crate) fn indexed(&self, prefix: &str) -> BTreeMap<usize, Vec<(&'a str /*rest after "N."*/, &'a AnyValue)>>; // gen_ai.prompt.N.*, by index; non-decimal N ignored
    /// Rule 2: Unread → kept; Consumed → kept iff encoded value ≤ 256 bytes; Payload → removed.
    pub(crate) fn into_attributes(self, warnings: &mut Warnings, truncated: &mut Vec<String>) -> BTreeMap<String, String>;
}
pub(crate) const SMALL_ATTRIBUTE_BYTES: usize = 256;
```

- `indexed("gen_ai.prompt.")` must not match `gen_ai.prompt.name`, because `name` isn't a decimal index. It must not match `gen_ai.prompt.01.role` either, since leading zeros aren't an index. Add a test for each (OpenObserve #16271).
- "Encoded value" means the rule 1 map string. Its length is measured in bytes.

### Tests (unit, in each module)

- **Rule 1:** one test per `AnyValue` variant, plus nested values.
- **Rule 19:** 33-level nesting gives the marker at depth 33 and sets the flag. 32 levels is unchanged.
- **Duplicate keys:** the last one wins, and the warning is recorded.
- **Rule 20:** a table of inputs → outputs for `as_i64` and `as_f64`, including NaN, negatives, strings and bool.
- **Rule 2 cases:** a 256-byte consumed value is kept; a 257-byte one is removed; a payload is always removed; an unread value is always kept.
- **`indexed`:** gaps (0, 2), non-decimal suffixes, nested `tool_calls.0.function.name` keys returned as the rest of the key.

---

## N4 — The span skeleton, rejections and the golden harness

**Edit only:**
- `crates/otel/src/{lib.rs,normalize.rs,reject.rs}`;
- `crates/otel/tests/golden.rs`;
- `crates/otel/tests/support/mod.rs`;
- `crates/otel/tests/fixtures.rs` (only for the `retention_days` setting);
- new fixtures under `fixtures/otlp/edge/`;
- `justfile` (one recipe).

**Commit:** `otel: normalize span skeletons and reject invalid spans`

**Prerequisite:** core request C1 (`NORMALIZE_WARNING`) is on `main`.

### `reject.rs`

`RejectReason` (public, `Copy + Ord + Hash`), with snake_case codes per rule 21, `as_str()` and `Display`. `Rejections` has a private `BTreeMap<RejectReason, u64>`, `total()`, `count()` and `message()` per D109:
- `None` when the total is 0;
- else `rejected {total} spans: {code} ×{n}, …`, with reasons sorted by count descending then code, at most 4 of them, then `, … ×{rest}` when more exist. There are only 4 reasons today, but keep the cap;
- the word is `span` when total is 1.

### `normalize.rs`

The public `IngestContext`, `IngestLimits` (with the spec's defaults) and `Normalized` (README § Public API). `media` stays an empty `Vec` until N14. Its type is `Vec<MediaBlob>` from core once C2 lands. Until then, N4 declares the field after C2. If C2 isn't on `main`, leave the field out and add it in N14. Say which you did.

`pub fn normalize(request, ctx, prices: &PriceTable) -> Normalized`. `PriceTable` doesn't exist until N16. Until then the signature is `normalize(request, ctx)`, and N16 adds the parameter. This is the one planned API change, and N16 updates every caller.

For each `ResourceSpans` → `ScopeSpans` → `Span`:

1. **IDs (rule 15).**
   - The trace ID must be 16 bytes, and not all zero. Otherwise reject with `InvalidTraceId`.
   - The span ID must be 8 bytes, and not all zero. Otherwise reject with `InvalidSpanId`.
   - `parent_span_id`: empty or all zero → `None`. 8 bytes → `Some`. Any other length → `None`. The fixture pins this. `llmobs-otel` does no logging.

   Use core's `TraceId`/`SpanId` constructors from bytes.
2. **Timestamps (rule 17, D85).**
   - `start`/`end` `u64` > `i64::MAX` → reject with `TimestampOverflow`.
   - `start_time` < 2000-01-01T00:00:00Z (946684800·1e9 ns), or < `ingest_time − retention_days·86400·1e9` (saturating), or > `ingest_time + 86400·1e9` → reject with `StartTimeOutOfRange`.
   - `end < start`, or `end == 0` → set `end = start` and add warning `EndBeforeStart`.
3. **Plain fields.**
   - `name`.
   - `status`: map the OTLP code (0 unset, 1 ok, 2 error; anything else → unset). `status_message`: non-empty message only.
   - `scope_name` and `scope_version`: `None` if empty.
4. **Resource.**
   - `resource_attributes` is the rule 1 map minus `service.name`.
   - `service_name` comes from resource `service.name` as a string.
   - Encode the resource once per `ResourceSpans` and clone it per span. Don't re-encode it per span.
5. **Attributes:** an `AttrBag` over `span.attributes`. Nothing consumes keys yet. `bag.into_attributes(...)` fills `attributes`.
6. **Events:** each becomes a `SpanEvent { time, name, attributes: Some(JSON object text) or None if empty }`. Event attributes use rule 1 with nested JSON values (`to_json`), not map strings. An event time above `i64::MAX` is clamped to `start_time`, and the warning `InvalidEventTime` is recorded.
7. **Links:** `SpanLink`. A link with an invalid trace or span ID is dropped. It's not a span rejection.
8. **Fixed values.**
   - `span_kind = Span`;
   - `project_id` and `ingest_time` come from `ctx`;
   - `schema_version = 1`, the current constant in core: use it if core has one, else request it;
   - `conventions` is empty (N5).
9. **Warnings:** if not empty, insert `NORMALIZE_WARNING` into `attributes`, overriding any client-sent attribute with the same key.
10. Sort nothing. The harness sorts.

### Golden harness (`tests/golden.rs`)

- Load every case with `support::load_all_cases()`. Build an `IngestContext` from `case.json` (`context`, plus `settings` including `retention_days`).
- Decode `input.json` with `decode_json`. Normalize, sort the spans like `support::sort_spans`, and compare with `expected.json` using the README rules (1e-12 on the cost fields).
- Compare media as `{sha256, mime_type, size_bytes}`, and `rejected_spans` as `rejections.total()`.
- **Bless mode:** with `LLMOBS_BLESS=1`, write `expected.json` in canonical format (`support::canonical_json`) instead of comparing. It never runs in CI.
- **`const PENDING: &[&str]`:** case paths (`group/case`) the normalizer can't produce yet.
  - Pending cases are run and must **fail**. A pending case that passes fails the test with "remove it from PENDING".
  - Start with all 6 existing cases, unless `edge/zero-parent-span-id` passes, in which case it's not pending.
  - Report the list.
- **Failure messages:** the case name plus the first differing field path, from a JSON diff of the two `Span` values. Fixtures are test data, so values may appear in test output.

`justfile`: add a `fixtures-bless` recipe that runs `LLMOBS_BLESS=1 cargo test -p llmobs-otel --test golden`.

`support/mod.rs` and `fixtures.rs`: accept `retention_days` in `settings` (integer ≥ 1).

### New fixtures (`fixtures/otlp/edge/`)

Each is hand-written. The expected output is the skeleton only: spans without GenAI attributes, so they pass now.
- `rejected-ids`:
  - a 3-byte trace ID, an all-zero trace ID, a 7-byte span ID, an all-zero span ID, a valid span;
  - a parent ID of the wrong length, which is kept as absent.
- `timestamps-out-of-range`: `retention_days: 7`; spans at 1999-12-31, at `ingest − 8 days`, at `ingest + 25 h`, and a valid one.
- `end-before-start`: one span with `end < start`, one with `end = 0`; both are kept with the warning.
- `resource-scope-events-links`: resource and scope attributes, two events (one with attributes), a link with a valid ID and one with an invalid ID, status error with a message.
- `duplicate-keys-and-depth`: duplicate span attribute keys, and a 33-deep kvlist; `settings` unchanged.

These cases need span times inside the README's 10-second window, except those of `timestamps-out-of-range`. Document that exception in its `case.json` `notes`, and relax the window check in `fixtures.rs` for **rejected** spans only, since only accepted spans must be in the window. If that check can't tell rejected spans from accepted ones, have it skip spans that aren't in `expected.json`.

### Unit tests

- `Rejections::message()`: total 0, 1 and several; ordering; no value leakage.
- `normalize` on a request with zero spans.
- One resource shared by two scopes: encoded once, which can be tested through the outcome only.

### Report

- the initial `PENDING` list;
- what you did about `MediaBlob`/C2;
- whether core exposes a schema-version constant.
