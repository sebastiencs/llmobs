# B1 · Scalar fields (N5–N8)

See [README.md](README.md) for the decisions, the boundaries and the rules that apply to every sub-phase. These sub-phases fill the non-payload columns. Spec 02's tables, as amended by N1, are the source of truth: when this plan and the spec disagree, the spec wins and the report says so.

## N5 — Field tables, the resolver and convention detection

**Edit only:** `crates/otel/src/{lib.rs,fields.rs,detect.rs,normalize.rs}`.

**Commit:** `otel: resolve fields from precedence tables and detect conventions`

### `fields.rs`

The declarative half of D100.

```rust
#[derive(Clone, Copy)]
pub(crate) enum Scope { Span, Resource }          // where the key is read (environment/release read resource keys)
#[derive(Clone, Copy)]
pub(crate) struct Source { pub key: &'static str, pub convention: Convention, pub scope: Scope }
pub(crate) const fn span(key: &'static str, convention: Convention) -> Source;
pub(crate) const fn resource(key: &'static str, convention: Convention) -> Source;

pub(crate) struct Resolved<T> { pub value: T, pub source: Source }

/// First source with a usable value wins; marks the winning span key Consumed (resource keys are never consumed:
/// resource_attributes keep everything except service.name). Sources whose value is present but unusable
/// (wrong type, invalid number) are skipped, left Unread, and record InvalidNumber when numeric.
pub(crate) fn first_string(bag: &mut AttrBag, resource: &ResourceView, sources: &[Source]) -> Option<Resolved<String>>;
pub(crate) fn first_i64(bag: &mut AttrBag, resource: &ResourceView, sources: &[Source], warnings: &mut Warnings) -> Option<Resolved<i64>>;
pub(crate) fn first_f64(…) -> Option<Resolved<f64>>;
pub(crate) fn first_string_list(…) -> Option<Resolved<Vec<String>>>;
```

- `ResourceView` is a read-only key → `&AnyValue` lookup over the resource attributes, built once per `ResourceSpans` in `normalize.rs`.
- An empty string is not a value: it's skipped and left Unread.
- Leave consumption of lower-precedence **non-payload** sources alone. Rule 2 only removes lower-precedence *payload* sources, and those are handled in N9/N13.
- The tables themselves are added by the sub-phases that use them (N6–N8). This sub-phase adds the resolver and its tests, using test-only tables.

### `detect.rs`

`pub(crate) fn detect(bag: &AttrBag, events: &[Event]) -> Vec<Convention>` implements spec 02's detection table (N1 § 4) and returns conventions in precedence order (rule 13):
- a single pass over the keys, with prefix checks;
- `gen_ai.prompt.N.*` and `gen_ai.completion.N.*` are told apart from GenAI by a decimal index right after the prefix. Reuse the index parser from `bag.rs`.

`normalize.rs` sets `span.conventions`.

### Tests

- **Resolver:**
  - precedence;
  - an empty string skipped;
  - an invalid number skipped, with a warning, the next source used, and the bad attribute kept;
  - a resource source;
  - the winning key ends up `Consumed` and the losing keys `Unread`.
- **Detection:** one test per convention, plus `gen_ai.prompt.0.role` → OpenLLMetry only, `gen_ai.prompt.name` → GenAI only, a mixed Vercel span (`ai.*` + `gen_ai.*`) → `[genai, vercel]`, and an empty span → `[]`.

Golden: `PENDING` cases may now differ only in other fields. They remain pending.

---

## N6 — Span kind

**Edit only:** `crates/otel/src/{kind.rs,normalize.rs,lib.rs}`, plus fixtures under `fixtures/otlp/{genai,edge}/`.

**Commit:** `otel: map span kinds across conventions`

`kind.rs`: `pub(crate) fn span_kind(bag: &mut AttrBag) -> SpanKind`. It follows spec 02 § Span kind in table order, so the first row with a decision wins, and the source attribute becomes Consumed. Decisions:

- `llmobs.span.kind`: any valid `SpanKind` string (core's parser), verbatim. An invalid value gives no decision.
- `gen_ai.operation.name`: the N1 table, including the new rows. Any other value gives no decision.
- `openinference.span.kind`, `ai.operationId`, `traceloop.span.kind`, `llm.request.type`, `langfuse.observation.type`, `lmnr.span.type`: as in the spec.
  - Vercel uses a suffix match on `ai.operationId`: `*.doGenerate` and `*.doStream` → `llm`; exact names for `ai.generateText`, `ai.streamText`, `ai.generateObject` and `ai.streamObject` → `chain`; `ai.toolCall` → `tool`; a prefix `ai.embed` → `embedding`.
  - Matching is case-sensitive except where the spec lists uppercase sources (OpenInference, Laminar). Use exact matches against those spellings.
- The tool-key fallback, then the model fallback (`gen_ai.request.model` or `llm.model_name` present → `llm`). Fallback keys are only peeked (`get`) and left as they are: the model key is consumed later by N7.
- Otherwise `span`.

Add one hand-written fixture per new N1 row: `genai/operation-retrieval` (`retrieval` → `retriever`), `genai/operation-workflow` (`invoke_workflow` → `chain`) and `edge/tool-key-fallback` (`gen_ai.tool.name` only → `tool`). Keep each fixture minimal, with only the kind-relevant attributes. Their expected outputs may have to include fields that later sub-phases fill: for example, `gen_ai.operation.name` also fills `operation` in N7. If a new case can't pass yet, put it in `PENDING` and say why in the report.

Unit tests: one per table row, plus "an invalid `llmobs.span.kind` falls through to the next row".

---

## N7 — Model, provider, operation, params and context

**Edit only:** `crates/otel/src/{context.rs,fields.rs,normalize.rs,lib.rs}`, plus fixtures under `fixtures/otlp/{genai,llmobs,edge}/`.

**Commit:** `otel: fill model, provider, request params and context fields`

`context.rs` holds the tables (as `const &[Source]` in `fields.rs`, or local to `context.rs`; pick one place and say which) and the functions that fill these columns:

- `request_model`, `response_model`, `operation`, `service_name` (already done in N4).
- `provider`: precedence table, then rule 12. Lowercase it, then apply the alias table from N1 § 5, as a `match` or a sorted static slice, with exact keys first, then `<prefix>.*` rules, then the `<name>.<anything>` → `<name>` fallback for Vercel-style IDs. The fallback only applies when the value came from `ai.model.provider`: a semconv value like `aws.bedrock` must stay as it is. Test that explicitly.
  - When no source exists, the spec says "inferred from model name via price table". That needs N16, so the inference is added in N16, not here.
- `request_params` (rule 8, N1 § 5):
  - sweep `gen_ai.request.*` except `.model`, `ai.settings.*`, and the expanded `llm.invocation_parameters` and `langfuse.observation.model.parameters` (JSON object strings; a value that isn't an object is ignored and the attribute stays);
  - keys are the suffix;
  - on a duplicate key across sources, the higher-precedence convention wins (GenAI > OpenInference > Vercel > Langfuse);
  - values are JSON: a string attribute is a JSON string, and numbers keep their type;
  - the result is a compact JSON object with sorted keys, or `None` when empty;
  - every source attribute becomes Consumed.
- `session_id`, `user_id`, `tags`, `metadata`, `environment`, `release`, with the N1 tables. Rule 11: context keys consumed from a metadata-like source (`ai.telemetry.metadata.sessionId`, …) are not copied into `metadata`. Sibling keys go into `metadata` with the source prefix stripped. On a metadata key collision, the higher-precedence source wins.
- Experiment linkage: `llmobs.experiment.id` and `llmobs.dataset_item.id` go into `metadata` under the reserved keys (core `metadata_key`).
- `tags`: deduplicated, keeping first-occurrence order. Only the first source that yields tags is used.

Rule 3: `provider`, `request_model`, `response_model`, `operation` and `request_params` are filled for **every** kind.

Fixtures:
- `llmobs/context-fields` (every `llmobs.*` context key, plus experiment linkage);
- `genai/request-params` (several `gen_ai.request.*` keys of mixed types);
- `edge/provider-aliases` (several spans: `openai.chat` via Vercel, `aws.bedrock` via GenAI, `xai` → `x_ai`, uppercase `OpenAI`);
- `edge/tags-json-string` (tags as a JSON-array string, plus a CSV string that must stay in `attributes`).

Remove any `PENDING` case that now passes.

Unit tests: one per table, plus the alias table.

---

## N8 — Usage, token semantics and TTFT

**Edit only:** `crates/otel/src/{usage.rs,fields.rs,normalize.rs,lib.rs}`, plus fixtures under `fixtures/otlp/{genai,vercel,edge}/`.

**Commit:** `otel: normalize token usage and time to first token`

`usage.rs`: `pub(crate) fn usage(bag, kind, provider, warnings) -> Usage` and `pub(crate) fn ttft(bag, start_time) -> Option<i64>`.

- **Rule 3.** Only `llm` and `embedding` spans get token columns. On other kinds, usage attributes are not read: they stay Unread, so they stay in `attributes`.
- **Tables (N1 § 5).** Resolve `input`, `output`, `cache_read`, `cache_write`, `reasoning` and `total` independently with `first_i64`. Each `Source` has a `semantics` tag in a parallel table, or a field added on a usage-specific source type. Use a usage-specific type; don't add a field to the generic `Source`:

  ```rust
  pub(crate) struct UsageSource { pub source: Source, pub input_includes_cache: Inclusive, pub output_includes_reasoning: Inclusive }
  pub(crate) enum Inclusive { Yes, No, VercelLegacy /* decided per span: see below */ }
  ```

- **Vercel v6 `noCacheTokens`.** If `ai.usage.inputTokenDetails.noCacheTokens` is present and there's no inclusive input source, `input = noCacheTokens + cacheRead + cacheWrite`.
- **`VercelLegacy`.** The span has `ai.*` attributes, has **no** `ai.usage.inputTokenDetails.*`, and its normalized provider is `anthropic`. Then the input count is exclusive, wherever it was read from: `gen_ai.usage.input_tokens` or `ai.usage.inputTokens`/`promptTokens` (N1 token semantics table).
- **Add-back.** Exclusive input becomes `input + cache_read + cache_write`. Exclusive output becomes `output + reasoning`. Use saturating arithmetic.
- **Safety net (D108).** After add-back, if `input < cache_read + cache_write`: `input += cache_read + cache_write`, and record `InputExcludesCache`.
- **Total, rule 4.** A reported total, else `input + output` when both are known, else `None`.
- **Modality subsets.** Never read them: they stay Unread.
- **TTFT** (any kind; it's not usage).
  - `llmobs.ttft_ns` (integer ns).
  - `gen_ai.response.time_to_first_chunk` (float seconds × 1e9, rounded, rejected if negative or not finite).
  - `ai.response.msToFirstChunk`, `ai.stream.msToFirstChunk` (ms × 1e6).
  - `langfuse.observation.completion_start_time`: an RFC 3339 string or integer ns, parsed with core's strict `parse_rfc3339`. `ttft = t − start_time`; when negative or unparseable, `None` and the attribute stays.

  Each one that wins becomes Consumed.
- **Embedding spans:** `output_tokens` is read too, if reported. D82 handles their cost in N16.

Fixtures (hand-written, D111):
- `genai/cache-write-semconv` (`gen_ai.usage.cache_write.input_tokens` plus `cache_read`);
- `genai/underscore-cache-keys` (OpenLLMetry-style `gen_ai.usage.cache_read_input_tokens`);
- `vercel/v6-token-details` (`inputTokenDetails`, `outputTokenDetails`);
- `vercel/v5-anthropic-exclusive-input`: `notes` says "pinned by hand from LangWatch's finding; re-check against a B2 capture";
- `edge/input-excludes-cache-safety-net`;
- `edge/modality-tokens-not-summed`;
- `genai/ttft-time-to-first-chunk`;
- `edge/usage-on-chain-span` (usage attributes on a `chain` span stay in `attributes`).

Expected costs: these cases use the test price table (D67) and need N16 for cost. Until N16 lands they stay in `PENDING`, or they use a model the price table doesn't know (`cost_source: none`, as in `edge/media-truncation-rejection`). **Prefer an unknown model** so the case passes now. Use known models only in the dedicated cost cases of N16.

`genai/chat-cache-tokens` may pass now if it only needs N4–N8 and cost. Check, and remove it from `PENDING` if so.

Unit tests:
- every semantics combination;
- the safety net, with and without total;
- saturating overflow (`i64::MAX` tokens);
- TTFT conversions and negative values.
