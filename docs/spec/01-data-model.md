# 01 — Data model

## Hierarchy

```
Organization ─┬─ Project ─┬─ Trace ── Span (tree via parent_span_id)
              │           ├─ Session (derived: traces sharing session_id)
              │           ├─ End user (derived: traces sharing user_id)
              │           ├─ Score (target: span | trace | session)
              │           ├─ Dataset ── DatasetVersion ── DatasetItem
              │           ├─ Experiment ── ExperimentItem (→ dataset item + trace)
              │           ├─ Evaluator · OnlineRule · AlertRule · ScoreDefinition
              │           └─ Media blob (content-addressed)
              └─ Members · LLM connections · API keys live at org or project level (see 04)
```

High-volume, append-mostly data (spans, trace summaries, scores) lives in the **columnar store** (03). Everything else lives in **SQLite** (04).

## Span kinds

A single closed enum, `span_kind`, stored as a dictionary-encoded string.

| `span_kind` | Meaning | Typical sources |
|---|---|---|
| `llm` | A call to a generative model (chat, completion, responses, generate_content) | GenAI `chat`/`text_completion`/`generate_content`, OI `LLM`, Vercel `doGenerate`/`doStream` |
| `embedding` | Embedding call | GenAI `embeddings`, OI `EMBEDDING` |
| `tool` | Tool / function execution | GenAI `execute_tool`, OI `TOOL`, `traceloop.span.kind=tool` |
| `agent` | Agent invocation / loop | GenAI `invoke_agent`/`create_agent`, OI `AGENT` |
| `chain` | Generic step of an application pipeline | OI `CHAIN`, traceloop `task`/`workflow` |
| `retriever` | Retrieval from a vector store / search | OI `RETRIEVER` |
| `reranker` | Reranking | OI `RERANKER` |
| `guardrail` | Guardrail check | OI `GUARDRAIL` |
| `evaluator` | Evaluation step (incl. our own judges) | OI `EVALUATOR` |
| `span` | Anything else (HTTP, DB, unknown) | default |

## Span (columnar table `spans`)

One row per OTel span, after normalization. Columns marked **P** are the "light" projection read by list views; heavy columns are only read when a span/trace is opened or searched.

Type notation in this document: "Utf8 (dict)" means a `Utf8` column in memory that is **dictionary-encoded in Parquet** (low cardinality); it is not an Arrow `Dictionary` type (D62). The Rust structs and Arrow schemas implementing these tables live in `llmobs-core` (D60).

| Column | Arrow type | P | Notes |
|---|---|---|---|
| `project_id` | Utf8 (dict) | P | Partition key (constant per file). |
| `trace_id` | FixedSizeBinary(16) | P | Bloom filter. |
| `span_id` | FixedSizeBinary(8) | P | |
| `parent_span_id` | FixedSizeBinary(8), nullable | P | Null ⇒ root of the trace *as received*. |
| `name` | Utf8 | P | |
| `span_kind` | Utf8 (dict) | P | Enum above. |
| `start_time` | Timestamp(ns, UTC) | P | The partition day is the trace's anchor day, the UTC date of this or the day before or after (03 § Trace anchor). |
| `end_time` | Timestamp(ns, UTC) | P | |
| `duration_ns` | Int64 | P | `end - start`. |
| `status` | Utf8 (dict) | P | `unset` \| `ok` \| `error`. |
| `status_message` | Utf8, nullable | P | |
| `service_name` | Utf8 (dict), nullable | P | Resource `service.name`. |
| `session_id` | Utf8, nullable | P | Bloom filter. |
| `user_id` | Utf8, nullable | P | Bloom filter. GDPR key. |
| `environment` | Utf8 (dict), nullable | P | e.g. `production`. Reserved: `llmobs-eval` (evaluator executions, 06), `experiment` default for experiment runs. |
| `release` | Utf8 (dict), nullable | P | Git SHA / version string. |
| `tags` | List(Utf8) | P | |
| `metadata` | Map(Utf8, Utf8) | P | User metadata; values are JSON-encoded scalars or strings. |
| `provider` | Utf8 (dict), nullable | P | Normalized lowercase: `openai`, `anthropic`, `gcp.gemini`, `aws.bedrock`, … |
| `request_model` | Utf8 (dict), nullable | P | |
| `response_model` | Utf8 (dict), nullable | P | |
| `model` | Utf8 (dict), nullable | P | `coalesce(response_model, request_model)`, used for cost + grouping. |
| `operation` | Utf8 (dict), nullable | | GenAI `operation.name` if any. |
| `input_tokens` | Int64, nullable | P | Includes cached tokens (OTel semantics). |
| `output_tokens` | Int64, nullable | P | Includes reasoning tokens. |
| `cache_read_tokens` | Int64, nullable | P | |
| `cache_write_tokens` | Int64, nullable | P | |
| `reasoning_tokens` | Int64, nullable | P | |
| `total_tokens` | Int64, nullable | P | |
| `cost_input` | Float64, nullable | P | USD. |
| `cost_output` | Float64, nullable | P | USD. |
| `cost_total` | Float64, nullable | P | USD; includes cache and reasoning components. |
| `cost_source` | Utf8 (dict), nullable | | `computed` \| `reported` \| `none`. |
| `ttft_ns` | Int64, nullable | P | Time to first token. |
| `request_params` | Utf8 (JSON), nullable | | temperature, top_p, max_tokens, … |
| `input` | Utf8, nullable | | See *Payloads* below. |
| `input_format` | Utf8 (dict), nullable | | `messages` \| `json` \| `text`. |
| `output` | Utf8, nullable | | |
| `output_format` | Utf8 (dict), nullable | | |
| `input_preview` | Utf8, nullable | P | First 512 chars of plain-text rendering. |
| `output_preview` | Utf8, nullable | P | Same. |
| `system_instructions` | Utf8 (JSON), nullable | | |
| `tool_definitions` | Utf8 (JSON), nullable | | |
| `attributes` | Map(Utf8, Utf8) | | All span attributes not consumed by normalization; values JSON-encoded. Includes `llmobs.normalize_warning` (02 rule 18) and `llmobs.normalize_error`. |
| `resource_attributes` | Map(Utf8, Utf8) | | Minus `service.name`. |
| `scope_name` | Utf8 (dict), nullable | | Instrumentation scope. |
| `scope_version` | Utf8 (dict), nullable | | |
| `events` | List(Struct{time: Timestamp, name: Utf8, attributes: Utf8 JSON}) | | Exceptions etc. GenAI message events are consumed. |
| `links` | List(Struct{trace_id, span_id, attributes JSON}) | | |
| `media_refs` | List(Utf8) | | SHA-256 hex of extracted blobs; used by media GC. |
| `truncated` | List(Utf8) | | Names of attributes truncated at ingest. |
| `conventions` | List(Utf8 dict) | | Detected conventions (`genai`, `openinference`, `openllmetry`, `vercel`, `llmobs`, …). Debug aid. |
| `ingest_time` | Timestamp(ns, UTC) | | Server receive time. Used by deletion cut-offs. |
| `schema_version` | UInt16 | | Row-level normalization version. |

Uniqueness: `(project_id, trace_id, span_id)`. Duplicate re-sends are tolerated at write time and removed at compaction (keep the row with greatest `ingest_time`); queries over not-yet-compacted data may briefly see duplicates — list views de-duplicate by `span_id`.

### Payloads (`input` / `output`)

- `input_format = messages`: JSON array in the **canonical message format** below.
- `input_format = json`: any JSON value (tool args, chain inputs).
- `input_format = text`: plain string, stored as-is (not JSON-quoted).
- For `llm` spans, `input` holds the input messages; `system_instructions` and `tool_definitions` are separate columns so the chat view can collapse them.
- Media inside payloads is replaced by references (see 02 §Media).

### Canonical message format

We adopt the OTel GenAI semantic-conventions JSON schema for `gen_ai.input.messages` / `gen_ai.output.messages` as our canonical format, so GenAI-native data needs no conversion and our SDK emits the standard.

```json
[
  {"role": "system" | "user" | "assistant" | "tool",
   "name": "optional",
   "parts": [
     {"type": "text", "content": "…"},
     {"type": "reasoning", "content": "…"},
     {"type": "tool_call", "id": "call_1", "name": "get_weather", "arguments": {…}},
     {"type": "tool_call_response", "id": "call_1", "response": …},
     {"type": "uri",  "modality": "image", "mime_type": "image/png", "uri": "https://…"},
     {"type": "blob", "modality": "image", "mime_type": "image/png", "content": "<base64>"}
   ],
   "finish_reason": "stop"            // output messages only
  }
]
```

A `tool_call` without arguments omits `arguments` (as in the GenAI schema). An explicit JSON `null` on an optional field (`name`, `finish_reason`, `id`, `modality`, `mime_type`) is preserved as-is. A message without `parts` is read as an empty list and written back with `"parts": []`. This is the only case where the JSON does not round-trip exactly.

After media extraction, `blob` parts become `{"type": "uri", "modality": …, "mime_type": …, "uri": "media://<sha256>"}`. Unknown part types are preserved verbatim; the UI renders them as JSON.

## Trace (columnar table `traces`)

Traces are not sent by clients; they are **derived**. Each flush writes one *partial* summary row per `(project_id, trace_id)` present in the flushed batch; queries merge partials with `GROUP BY trace_id`. This is the same idea as ClickHouse's AggregatingMergeTree. `traces` files are always computed from `spans` files (D88, 03 §3): when compaction merges a trace's spans into one file, its partials are recomputed from them and collapse into one row, and duplicate spans stop counting twice.

Rows of `spans` and `traces` are partitioned by the trace's **anchor day** (03 § Trace anchor), so all rows of a trace that crosses midnight share one partition (D86).

| Column | Type | Merge rule |
|---|---|---|
| `project_id` | Utf8 | key |
| `trace_id` | FixedSizeBinary(16) | key |
| `start_time` | Timestamp | min |
| `end_time` | Timestamp | max |
| `name` | Utf8 | from root partial (`has_root`), else first non-null |
| `root_span_id` | FixedSizeBinary(8) | from root partial |
| `has_root` | Boolean | bool_or |
| `status` | Utf8 | `error` if any partial has `error_count > 0`, else root status, else `unset` (no root seen yet) |
| `session_id` | Utf8 | first non-null (root preferred) |
| `user_id` | Utf8 | first non-null (root preferred) |
| `environment` | Utf8 | first non-null |
| `release` | Utf8 | first non-null |
| `tags` | List(Utf8) | distinct union |
| `metadata` | Map | root's metadata, keys from other partials added if absent |
| `input_preview` / `output_preview` | Utf8 | root partial; if no root, first `llm` span's |
| `span_count` | Int64 | sum |
| `error_count` | Int64 | sum |
| `llm_count` | Int64 | sum |
| `input_tokens` … `total_tokens` | Int64 | sum |
| `cost_total` | Float64 | sum, in integer units of 1e-18 USD so the result does not depend on merge order (D-A2-4) |
| `models` | List(Utf8) | distinct union |
| `last_ingest_time` | Timestamp | max |
| `partial_count` | Int32 | sum (1 per partial; = 1 after full compaction) |

`duration_ns` is computed at query time as `end_time - start_time`.

**Order.** Results never depend on the order in which spans were flushed.
- **Inside a partial**, "first non-null", the earliest span and the choice among several root spans follow `(start_time, span_id)` ascending.
- **When partials are merged**, the root partial is the one with `has_root`. Among several, it is the one with the smallest `(start_time, root_span_id)`. It comes first, and the other partials follow in `start_time` order.
- **Ties.** Non-root partials tied on `start_time` contribute the smallest value: the smallest string, the smallest name, and for `metadata`, per absent key, the smallest value. The root partial always comes first, so a tie between it and another partial doesn't matter.

A partial keeps its values but not the `(start_time, span_id)` of the span each value came from. Merging the partials of a split trace therefore equals applying these rules to all of the trace's spans at once for counts, tokens, `cost_total` (within one rounding to 1e-18 USD per value), `tags`, `models`, `start_time`, `end_time`, `last_ingest_time` and `has_root`; for the context fields when each is set on every span of the trace or on none; and for `metadata` when every span that has a key has the same value for it. It differs for `name` when there is no root and spans in different partials tie on the earliest `start_time`, for the previews when the root's preview is null and `llm` spans are in different partials, for `metadata` and the context fields when values differ between spans, and for `root_span_id` and everything taken from the root when a trace has several root spans. Those results are still deterministic.

**Root semantics.** The root is the span with null `parent_span_id`. If an application's root span is not exported (e.g. a parent span from an un-instrumented HTTP server), the trace has `has_root = false` and the UI shows the earliest span as the entry point.

## Session (derived)

No table. A session is `SELECT … FROM traces WHERE session_id = ? GROUP BY session_id`. Session list aggregates: first/last time, trace count, total cost/tokens, error count, user_id, tags. Session-level scores attach to `(project_id, session_id)`.

## Score (columnar table `scores`)

Scores are high-volume (online judges can score every trace) and must be joinable with spans/traces in SQL, so they live in the columnar store. Updates are last-writer-wins on `score_id` (by `updated_at`); deletes are rows with `deleted = true` until compaction drops them.

| Column | Type | Notes |
|---|---|---|
| `project_id` | Utf8 | partition |
| `score_id` | Utf8 (UUID) | LWW key; client-provided or derived deterministically per (target, name, source, author) — 06 |
| `target` | Utf8 dict | `span` \| `trace` \| `session` |
| `trace_id` | FixedSizeBinary(16), nullable | set for span/trace targets |
| `span_id` | FixedSizeBinary(8), nullable | span target |
| `session_id` | Utf8, nullable | session target (and copied for trace targets when known) |
| `user_id` | Utf8, nullable | set when the writer knows it (SDK context, judges, which read the trace); GDPR deletes also match via resolved trace/session IDs, so it is an optimization only |
| `name` | Utf8 dict | e.g. `hallucination`, `user_feedback` |
| `data_type` | Utf8 dict | `numeric` \| `boolean` \| `categorical` \| `text` |
| `value_num` | Float64, nullable | numeric value; boolean as 0/1 |
| `value_str` | Utf8, nullable | categorical label or text |
| `reason` | Utf8, nullable | explanation |
| `source` | Utf8 dict | `api` \| `sdk` \| `human` \| `end_user` \| `llm_judge` \| `check` \| `webhook` |
| `evaluator_id` | Utf8, nullable | |
| `rule_id` | Utf8, nullable | online rule that produced it |
| `experiment_id` | Utf8, nullable | |
| `dataset_item_id` | Utf8, nullable | |
| `author_user_id` | Utf8, nullable | for `human` |
| `metadata` | Map(Utf8, Utf8) | |
| `created_at` | Timestamp | partition day |
| `updated_at` | Timestamp | LWW version |
| `deleted` | Boolean | |
| `ingest_time` | Timestamp | |

Score **definitions** (name, type, allowed categories, numeric range, description) live in SQLite and drive the annotation UI; scores are not required to match a definition.

## Media blob

Object `media/<project_id>/<sha256>` in the object store; metadata row in the storage catalog's `media` table (03 §9: content type, size, first and last seen). Referenced from payloads as `media://<sha256>`; served at `GET /api/v1/projects/{p}/media/{sha256}` (auth required, `Cache-Control: private, immutable`).

## Normalization version

`schema_version` (spans) is bumped whenever normalization rules change in a way that affects stored values. Old rows are never rewritten for normalization changes; readers must handle every version ≥ 1.
