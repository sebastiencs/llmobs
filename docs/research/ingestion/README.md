# OTLP ingestion research (track B1)

How other systems decode and normalize OTLP traces, and what `llmobs-otel` (spec 02) should adopt or avoid. Written 2026-11-10 while planning B1.

| File | Scope |
|---|---|
| [greptimedb.md](greptimedb.md) | GreptimeDB: OTLP decode, attribute storage models, validation, partial success. No LLM handling. |
| [openobserve.md](openobserve.md) | OpenObserve: OTLP decode, flattening, its ~8.8k-line GenAI layer, cost. |
| [genai-mapping.md](genai-mapping.md) | Laminar, Langfuse, Phoenix, Opik, LangWatch: convention mapping against spec 02, with a 24-row gap table. |

No code was copied. Citations point at the local checkouts named in each file.

## Synthesis

### Decoding

- Everyone uses `opentelemetry-proto`'s prost types. GreptimeDB refuses OTLP/JSON for traces. OpenObserve accepts it through `with-serde`, and one bad value fails the whole batch (#14599, #15999).
- `with-serde` (0.33.1, our pin) decodes hex IDs of **any length**. It accepts int64 as a string or a number, but not enum names (`"SPAN_KIND_CLIENT"`). We keep it, and B1 adds the checks it doesn't do:
  - ID length, enforced by rule 15;
  - fixtures pinning each JSON quirk we accept or reject.
- Decompression and body limits belong to the server's HTTP layer, not `llmobs-otel`.

### Attribute model

- Typed columns per attribute key create a type-reconciliation subsystem (GreptimeDB v1). It breaks on `temperature = 0` sent as `intValue` (#7860).
- Flattening to strings loses types (OpenObserve).
- Spec 02's maps with compact JSON for non-string values (rule 1) avoid both problems. Our typed normalized fields must accept `intValue` and `doubleValue` interchangeably wherever the value is a number.
- Neither system caps nesting depth or attribute count, and neither defines duplicate-key behaviour. We must.

### Validation and partial success

- Neither validates IDs; all-zero IDs are stored as-is. Rule 15 is right.
- Timestamp handling is buggy in both:
  - GreptimeDB has `u64` underflow on `end - start` and unchecked `as i64` casts;
  - OpenObserve clamps negative durations to 0 and blames retention for every rejection.
- What we take from this:
  - D85 bounds;
  - checked conversion of `u64` nanoseconds to `i64`;
  - an explicit `end < start` policy pinned in a fixture.
- GreptimeDB's late partial-success design is worth following:
  - rejection reasons form a closed enum;
  - the message is bounded and deduplicated, showing at most N causes with counts;
  - the message never contains attribute values.

### Truncation

Neither system truncates attribute values or payloads. Spec 02 § Truncation is ahead of both.

### GenAI normalization

- GreptimeDB does none.
- OpenObserve rewrites the attribute map in place to canonical `gen_ai.*` keys. Its precedence is per convention or last-write-wins, which leads to:
  - double counting (#16272);
  - 0 instead of null (#16024);
  - a prefix-match bug that deleted messages (#16271);
  - estimated tokens stored as if reported.
- Spec 02's per-field precedence, null-when-unknown, and "tokens only on `llm`/`embedding`" rules avoid all of these.
- Spec 02 is missing a set of keys and rules. See genai-mapping.md § Gap table. The important ones:
  - `gen_ai.usage.cache_write.input_tokens` (the semconv rename);
  - underscore-style cache keys;
  - Vercel v6 token detail keys;
  - the Vercel v5 + Anthropic exclusive-input semantics;
  - an exclusive-input safety net;
  - modality token subsets;
  - `gen_ai.response.time_to_first_chunk` for TTFT;
  - more `tool_definitions` sources;
  - Laminar and Langfuse context, usage and cost keys.

### Code structure

- OpenObserve uses a struct of extractors with no trait. LangWatch runs one class per convention in a fixed order, over an attribute bag where reading a key removes it. That bag maps directly onto rule 2's consumed-attribute tracking.
- Numbered attributes (`gen_ai.prompt.N.*`, `llm.input_messages.N.*`) must be parsed by index, never matched by raw prefix (OpenObserve #16271).

### Tests

- Neither system has golden fixtures from real SDKs. Two OpenObserve bugs would have been caught by captured fixtures: TTFT stored as epoch seconds, and #16271.
- Neither fuzzes decode. A fuzz or proptest target would have caught GreptimeDB's underflow.
- B2's captured fixtures are the main defence against wrong mapping tables.

## Adopt / avoid for B1

**Adopt:**
- prost types as the input;
- resource and scope borrowed per span group;
- one ordered pass per convention over a consuming attribute bag;
- per-field precedence;
- numbers accepted as int or double;
- checked time conversion;
- a closed rejection-reason enum and a bounded partial-success message;
- a per-source token-semantics flag (inclusive or exclusive cache);
- the gap-table keys;
- golden fixtures plus proptest and fuzzing of decode and normalize.

**Avoid:**
- trusting `with-serde` without fixtures;
- per-key typed columns;
- flattening values to strings;
- convention-wide or last-write-wins precedence;
- raw prefix matching on numbered keys;
- 0 for unknown values;
- estimated tokens;
- network-synced prices (cost would depend on sync time);
- unbounded recursion on `AnyValue`.
