# Competitor trace storage — research notes for the llmobs storage design

Scope: how Phoenix, MLflow and LangWatch (in depth), and Langfuse, Opik, Helicone and Laminar (briefly) store and query traces and spans. Each finding is mapped to our spec (`docs/spec/01`, `03`, `05`).

All paths are relative to `/home/sebastien/github/<repo>/`. Nothing in those repos was edited and no code was copied.

## Snapshot dates

`git log` on these repos is blocked by this session's worktree sandbox. The dates below come from two other sources:
- the clone date in `.git/logs/HEAD`, which is 2026-11-09 for all seven repos;
- the newest CHANGELOG entry:
  - Phoenix 21.0.0, 2026-10-30;
  - MLflow 3.18.0, 2026-11-02;
  - LangWatch 3.23.0, 2026-11-03.

For the same reason, the performance history below was mined from CHANGELOGs, ADRs and migration headers instead of `git log --grep`.

---

## 1. Phoenix (Arize), Python, SQLAlchemy on SQLite or Postgres

### Data model
- **`traces` table** (`src/phoenix/db/models.py:811-867`):
  - Columns: rowid, `project_rowid`, `trace_id` (hex string), `project_session_rowid`, `start_time`, `end_time`.
  - `trace_id` is **globally unique**, not unique per project.
  - Index on `(project_rowid, start_time DESC)`.
  - The trace row stores **no** aggregates: no tokens, cost, status, name or root.
- **`spans` table** (`models.py:870-1069`):
  - Typed columns: `span_id`, `parent_id`, `name`, `span_kind`, times, `status_code`, `status_message`.
  - Everything else, including the LLM input and output (`input.value`, `output.value`, messages), sits in **one JSON `attributes` column**. Events are a JSON list.
  - Promoted columns: `llm_token_count_prompt/completion` and three `cumulative_*` counters.
  - Hybrid properties read JSON paths: `input_value_first_101_chars` is a `substr` over the JSON (`models.py:914-928`), and `user_id` and `session_id` are JSON paths too.
  - Partial **expression indexes** on `attributes->session.id` and `attributes->user.id` (`models.py:1055-1068`).
  - `span_id` is **globally unique** with `sqlite_on_conflict="IGNORE"`, so the first write wins.
- **`project_sessions`** (`models.py:781-808`): `session_id` (unique), `project_id`, `start_time`, `end_time`, indexed `(project_id, start_time DESC)` and `(project_id, end_time DESC)`.
- **`span_costs`** is a separate table keyed by span and trace.
- Large payloads are not handled specially. Postgres rejects NUL characters, so NUL is replaced by U+FFFD in descriptive fields only; identifiers are left as they are (`db/insertion/span.py:126-178`).

### Trace aggregates and late spans
Phoenix maintains almost nothing at write time.
- **Per span** (`db/insertion/span.py:181-378`), each span:
  1. finds or creates the trace;
  2. widens the trace's `[start,end]` with `CASE` updates (`_extend_time_range`, :63-92);
  3. attaches a session on first sight (**first session ID wins**: `coalesce(project_session_rowid, new)`, :247-264);
  4. widens the session's time range.
- **Cumulative counters** are pushed up the tree. A new span sums its children's cumulative counters, which handles children that arrived first. It then adds its own counts to all ancestors through a **recursive CTE `UPDATE`** (:297-377), which handles a parent that arrived first.
  - The comment says this is "usually a no-op, since the parent usually arrives after the child" (:345-348).
  - This is per-row read-modify-write. It only works because ingestion is serialized and each span runs in its own savepoint.
- **Trace-level aggregates** (span count, error count, LLM tokens, cost, span-kind counts) are computed **on read** by `GROUP BY trace_rowid` over spans (`db/trace_aggregates.py:91-145`).
  - Tokens are summed **only over `span_kind = LLM`** (:112-123), the same idea as our spec 02 §123.
- **Root detection** (`representative_root_span_by_trace`, `trace_aggregates.py:148-190`):
  - roots are spans with `parent_id IS NULL`, **plus orphans** (spans whose parent was never received, `orphan_span_as_root_span`);
  - the representative root is the earliest by `start_time`, with tie-break `id DESC`.
  - So **multiple roots** are expected, and the choice between them is deterministic.
- **Session aggregates** are also computed on read, by `GROUP BY traces.project_session_rowid` (`db/session_aggregates.py:1-6`). The `project_sessions` table only holds identity and time range.

### Write path
- `BulkInserter` (`db/bulk_inserter.py:115-420`) is an **in-memory deque**:
  - It drains up to `max_ops_per_transaction=100` spans per transaction, with one savepoint per span.
  - Failures are retried with exponential backoff and jitter on deadlock or serialization failure.
  - On a non-retryable error the batch is released with a metric, and the log message admits that "accepted spans may be missing" (:398-404).
- **There is no WAL.** The OTLP endpoint acknowledges before the database write, so spans can be lost on a crash.
- Backpressure: the queue is capped at 20 000 spans (`PHOENIX_MAX_SPANS_QUEUE_SIZE`, `config.py:3578`). When it is full the API answers 503 (`server/api/routers/v1/utils.py:41`).
- Duplicates: `INSERT … ON CONFLICT DO NOTHING` on `span_id`. A re-sent span is ignored, and its counters are not double-propagated because propagation only happens when a row was actually inserted (:336-338).

### Read path
- **Span list** (`server/api/types/Project.py:623-718`):
  - **Keyset cursor** `(sort_value, rowid)` with a separate case for NULL sort values; overfetch by one to detect the next page.
  - The time range is on `span.start_time`.
  - Trace filters apply to spans through `trace_rowid IN (subquery)`.
- **Session list** uses **interval-overlap semantics**: a session matches if `[start,end]` intersects the window (`Project.py:211-238`, 721-729). The index `(project_id, end_time DESC)` was added for it (migration `eaf1907ae453`).
- There is no real full-text search. The DSL supports `'x' in input.value`, which becomes a string contains over JSON.

### Filter language
- A **Python expression**, parsed with `ast` and validated, then translated to SQLAlchemy and evaluated against a dictionary of bindings (`trace/dsl/filter.py`, 3 900 lines; `trace_filter.py`; `session_filter.py`).
- Separate vocabularies for spans, traces and sessions. Trace filters expose aggregates (`num_spans`, `error_count`, `token_count_*`, `total_cost`, `llm_span_count`) plus root-span `input`/`output` (`trace_filter.py:54-75`, 311-351).
- **Quantifiers and reductions over a trace's spans**: `any`, `all`, `len`, `max`, `min`, `sum` over `spans` (`filter.py:208-209`).
- Two compilation shapes, `"scan"` (grouped subquery) and `"probe"` (correlated scalar). The choice depends on selectivity (`trace_filter.py:45`, 694-708, 892-916).
  - The comment explains why `NOT IN` was avoided: Postgres does not turn it into an anti-join and times out.
- Safety work visible in `_validate_python_surface` (`filter.py:2868-2914`):
  - The file admits the DSL "began as a Python-evaluated filter and only later gained a SQL backend".
  - Rules were added for NFKC-normalised identifiers (`ｎａｍｅ` silently becoming `name`), NUL characters, bitwise operators that compile to unrelated SQL, and recursion depth (`filter.py:1298-1306`).

### Deletes and retention
- Per-trace delete and per-project delete by time range (`server/api/routers/v1/traces.py:384-430`, 827-880). Hard `DELETE` cascades by foreign key to spans, costs and annotations.
- **Orphaned sessions must be swept explicitly**:
  - `delete_traces_and_orphan_sessions` (`server/api/utils.py:12-40`) deletes sessions with no traces left.
  - It uses `FOR UPDATE SKIP LOCKED` to avoid deleting a session that ingestion is resuming.
  - An hourly sweeper removes the leftovers.
- An earlier version used `RETURNING` on bulk deletes and ran out of memory (#13906 / #13909).
- **Retention policies** (`db/types/trace_retention.py`): `max_days`, `max_count`, or both. Each policy runs on its own cron and can be assigned to many projects. `max_count` uses a `ROW_NUMBER` per project.
- There is no per-user (GDPR) delete.

### Performance history (CHANGELOG and migration headers)
- Token columns were promoted out of JSON "to improve projects page query performance" (#4135).
- A session ID index was built over JSON (#11028), with a workaround for large tables (#11526) and `PHOENIX_MIGRATE_INDEX_CONCURRENTLY` (#11543).
- `ix_latency` was dropped because no query used that expression, and on SQLite it collapsed every row to one key (`migrations/versions/4aad9107d196…py:1-18`).
- Composite `(project_id, start_time)` sorting (#13752).
- Descendant-span depth limited to 3 (#6578).
- Dataloaders added to avoid N+1 queries (#6490).
- Sessions filter rewritten without the join (#8028).

---

## 2. MLflow tracing, Python, SQL tracking store

Report from a sub-agent, spot-checked for paths. `SAS` = `mlflow/store/tracking/sqlalchemy_store.py`.

### Data model
- **`trace_info`** (`dbmodels/models.py:791-894`):
  - Primary key `request_id`. Columns: `timestamp_ms`, `execution_time_ms`, `status`, and `request_preview`/`response_preview` at 1 000 characters.
  - Added on 2026-07 in migration `75868b020152`: `trace_name`, `session_id`, token and cost columns.
- Tags and metadata are **EAV tables** with no `(key, value)` index (`models.py:936-993`).
- **`spans`** table (migration `a1b2c3d4e5f6`, 2025-08; `models.py:2263-2416`):
  - Primary key `(trace_id, span_id)`. A few typed columns.
  - The **whole span is one JSON `content` TEXT**, with all attributes and IO. A comment notes "we should improve this by saving only the attributes" (SAS:11171).
- Older traces kept spans in a `traces.json` artifact. A tag records where the spans live (`TRACKING_STORE`, artifact, or `ARCHIVE_REPO`), and readers dispatch on it (`mlflow/tracing/client.py:229-245`).
- **Previews** take the last chat message, replace NUL, and are cut to 1 000 characters (`mlflow/tracing/utils/truncation.py:27-58`). There is no span size cap.

### Trace aggregates and late spans
Two writers merge into one `trace_info` row:
- **`log_spans`**, per export batch (SAS:5849-6479):
  - `SELECT … FOR UPDATE` the trace, or create it as `IN_PROGRESS`.
  - Upsert spans with `ON CONFLICT DO UPDATE`.
  - Widen the time range with `CASE` expressions.
  - Set previews, session and user only if still NULL (**first writer wins**).
  - **Re-read and re-parse every stored span's JSON to recompute token usage and cost** on every batch.
- **`start_trace`**, at trace end with client values, writes an `infoFinalized` marker that freezes these fields (SAS:3992-4234).

Problems this design ran into:
- Token double-counting on roll-up parent spans (CHANGELOG #24339). The fix counts a span's usage only if no ancestor has usage (`mlflow/tracing/utils/__init__.py:236-340`).
- A Postgres deadlock between the two writers (#24338).
- **Spans dropped with only a warning** when the trace row could not be created after 10 retries (SAS:6060-6137).

The OTLP endpoint (`mlflow/server/otel_api.py:133-344`) has no finalize step, so OTLP traces are always "partial" by that definition.

### Write path
The client async queue (`mlflow/tracing/export/async_export_queue.py:38-194`) holds 1 000 items with 10 workers, **drops tasks when full**, and does not retry.

### Read path
- `search_traces` uses **OFFSET pagination** (the page token is base64 `{"offset": n}`, `mlflow/utils/search_utils.py:987-1032`), ordered by `timestamp_ms DESC, request_id`.
- The full-text filter `trace.text` is `EXISTS (spans.content LIKE …)`, pruned only by experiment and a time bound (SAS:11282-11293).
- `batch_get_traces` **silently omits** traces with fewer stored spans than the client-reported `num_spans` (SAS:6717-6728).
- Sessions are a denormalized `session_id` column, aggregated with `GROUP BY` on read (SAS:4461-4721).

### Filter language
- Parsed with sqlparse; **AND only, no OR or parentheses** (`search_utils.py:2257-2275`).
- Prefixes: `attributes.`/`trace.`, `tags.`, `metadata.`, `span.name|type|status|content`, `span.attributes.X` (a `LIKE` on the raw JSON), `feedback.X`.
- All span predicates share **one EXISTS**, so they must match the same span (SAS:10993-11301).

### Deletes and retention
- `delete_traces` by `max_timestamp_millis`/`max_traces` or by IDs. Hard delete with foreign-key cascade (SAS:4913-5013). No tombstones.
- "Retention" is **archival**: old traces move to an OTLP protobuf file in the artifact store, `spans.content` is blanked, and a generation counter guards against concurrent writes (SAS:6889-7506).

### Performance history
- 2026-07: a denormalization migration moved token, cost, session and name out of EAV tables, because EAV did not scale for analytics.
- A daily roll-up layer was added ("Faster Analytics for Large Trace Datasets").
- An N+1 query fix (#22431).
- A trace-ID fast path to avoid scanning `spans.content` (#22433).
- Indexes `(experiment_id, start_time_unix_nano)` and `(timestamp_ms, request_id)`.

---

## 3. LangWatch, TypeScript, ClickHouse event sourcing (Elasticsearch removed)

### Store
Elasticsearch is gone: "ClickHouse is the sole data store; ElasticSearch writes are fully disabled" (`packages/prisma-client/prisma/migrations/20260403120000_remove_elasticsearch_clickhouse_feature_flags/migration.sql:1-2`; CHANGELOG.md:1683).

The architecture is event-sourced: `event_log` is the source of truth, and fold/map "projections" are built from it. Postgres holds only product entities.

### Data model
All tables are in `packages/clickhouse-migrations/migrations/00002_create_schema.sql`.
- **`event_log`** (L15-37): ReplacingMergeTree, ordered by `(Tenant, AggregateType, AggregateId, IdempotencyKey)`, where `IdempotencyKey = tenant:trace:span`.
- **`stored_spans`** (L44-119): ReplacingMergeTree, ordered by `(Tenant, TraceId, SpanId)`, weekly partitions on `StartTime`.
  - Attributes are `Map(LowCardinality(String), String)` with bloom indexes on keys and values.
- **`trace_summaries`** (L126-181): one row per trace, ReplacingMergeTree on `UpdatedAt`.
  - Holds computed IO, TTFT, duration, span count, error flags, models, cost and tokens.
  - Ngram skip indexes on lowercased IO for search (`00011_add_io_search_indexes.sql`).
- **`trace_analytics`**: a slim table sorted by time (`00039`). Its own comment warns that the time key moves when an earlier span arrives late, so readers must dedup by `max(UpdatedAt)`.
- **Roll-ups** come from **per-span increments** in an AggregatingMergeTree, because "a span is immutable, a trace mutates N times". Crash double-counts are accepted (`dev/docs/adr/034-event-sourced-analytics-materialization.md`).
- **Large payloads** (ADR-022):
  - commands over 256 KiB are spooled to object storage;
  - projections keep a preview plus a reference when IO exceeds 64 KiB, and the full value is read back from `event_log` on demand (`trace-projection-lean.rules.ts:30`);
  - each attribute is capped at 8 MiB.

### Trace assembly and late spans
- The summary is a **stateful fold in arrival order**, with `refoldOnOutOfOrder: false` (`modules/trace/process/src/eventing/trace-summary.projection.ts:134-143`, 622-734).
  - The previous state comes from Redis, or from a ClickHouse point read on a cache miss.
  - Re-reading `event_log` on the hot path "starved ClickHouse merges and caused the 2026-07-23 outage" (ADR-066/034).
  - After 512 spans, later spans are only counted (:235-241).
- **Root**: `parentSpanId === null` is sticky. Until a root arrives, the earliest span stands in, and a real root takes over when it arrives (`trace-name-resolution.ts:24-170`).
- **Dedup**:
  - a 30 s command dedup on `tenant:trace:span`;
  - the last 1 000 applied event IDs are stored with the fold state;
  - the code warns that a retry without that set "will double-count".
- **Moving time key (ADR-087, `migrations/00072_trace_summaries_storage_anchor.sql:1-40`)**:
  - `OccurredAt = min(span start)` was the partition key and the TTL anchor.
  - Late, earlier-starting spans moved it.
  - Log-only traces got `1970-01-01`, landed in an already-expired partition, and made detail reads run unbounded across all partitions, which caused production out-of-memory kills.
  - The fix freezes the storage anchor at first contribution and keeps a separate `EarliestSpanStartMs`.

### Read path
- **Trace list** (`trace-list.repository.ts`):
  - Dedup is an `(id, max(UpdatedAt)) IN (…)` computed **on base predicates only**. Filters apply after the dedup, "or a re-annotated trace answers to both annotation buckets" (L141-171, 263-271).
  - Light columns are fetched first, heavy IO only for the page.
  - Keyset cursor with `TraceId` as tie-break.
- **Trace detail**: the span read window comes from the summary's anchor. A 2 GiB per-query memory cap was added after out-of-memory errors.
- **Threads/sessions**: `GROUP BY conversation_id` on read over deduped summaries (`session-groups.repository.ts:17-41`).

### Filter language
- A Liqe (Lucene-like) language compiled to parameterised ClickHouse SQL (`features/query/rules/trace-query.rules.ts`).
- Span attribute predicates become `TraceId IN (SELECT … FROM stored_spans WHERE time window AND attr = v)`.
- Limits: 50 parameters, 8 content terms, and OR disables the content branch.

### Deletes and retention
- **No per-trace delete.**
- Retention is a per-row `_retention_days` column stamped at ingest (project > team > org) plus a merge-time TTL (`dev/docs/adr/022-data-retention.md`).
- Annotations are not cascaded: dangling references are tolerated (ADR-025).

---

## 4. Secondary repos

**Langfuse** (ClickHouse; `packages/shared/clickhouse/migrations/canonical/`)
- `traces`, `observations` and `scores` are ReplacingMergeTree(`event_ts`, `is_deleted`) with monthly partitions.
- Updates are read-modify-write in the worker (`worker/src/services/IngestionService/index.ts:770-849`).
- Trace aggregates (cost, usage, latency) and sessions are computed **on read** from `observations` (`repositories/traces.ts:1660-1690`).
- They **tried an AggregatingMergeTree of partial trace aggregates and dropped it** (`0023_traces_aggregating_merge_trees.up.sql` was added and later removed in `0028`/`0029`).
- The new "events" model (`0039`-`0041`):
  - one span-level table with trace fields **denormalised onto every span** and an `is_app_root` flag;
  - sort key `(project, toStartOfMinute(start), xxHash32(trace_id), span_id, start)`;
  - a propagation job copies trace properties onto spans.
- Deletes are lightweight `DELETE … WHERE id IN … AND timestamp BETWEEN min/max`, plus a cleaner that runs `APPLY DELETED MASK`.

**Opik** (ClickHouse; `apps/opik-backend/.../db-app-analytics/migrations/`)
- `traces` and `spans` are ReplacingMergeTree(`last_updated_at`), deduped on read with `LIMIT 1 BY id`.
- Trace usage, cost and span count are computed **on read** in a `spans_agg` CTE (`TraceDAO.java:745-770`).
- Threads are `GROUP BY thread_id` on read.
- **`span_weeks`** (`000132`) is an index of which weekly partitions hold each trace's spans, so trace reads prune partitions. "A missing [week] makes it skip rows."
- The **`deletion_events`** log (`000096`) exists because a lightweight DELETE does not bump the version column, so a version-based copy during table migrations would bring deleted rows back.

**Helicone** (ClickHouse)
- No trace or span model: one row per LLM request (`request_response_rmt`).
- Sessions are header properties, aggregated **on read**. A `session_rmt` materialized table was created and later dropped (schema_49/50, then 53/54).

**Laminar / lmnr** (ClickHouse, Rust ingest). This is the closest design to ours.
- **`traces_agg`** (`frontend/lib/clickhouse/migrations/54_traces_agg.sql:1-4`): an AggregatingMergeTree where "each ingest batch inserts one partial row per trace … reads always re-aggregate with GROUP BY".
  - Ordered by `(project_id, id)`, with a `start_time` projection.
  - The partial is built in Rust (`app-server/src/ch/traces.rs:10-192`).
- Set-once fields (input, session, user, root, metadata) go in **`traces_static`**, a CoalescingMergeTree.
- **Padded bounds** are mandatory: "a tight `start_time` filter can clip a trace's partials" (54_traces_agg.sql, the comment before the view).
  - The query engine pads the user's window by **±3 h on both sides** (`app-server/src/query_engine/validator.rs:54-58`, 1894-1906).
- **Spans are a plain MergeTree with no dedup**, and the sums are not idempotent: "two replicas walking the same windows permanently inflate tokens/costs" (`frontend/lib/clickhouse/scripts/backfill-lock.ts:44-48`).
- Session and trace deletes remove spans only (`frontend/lib/actions/traces/index.ts:188-200`).

---

## 5. Cross-cutting lessons, mapped to our spec

### A. Things our spec gets right, confirmed by others
- **Partial trace rows merged with `GROUP BY` and collapsed at compaction (01 §Trace, 03 §6).** Laminar runs exactly this in production. Phoenix, Langfuse and Opik compute aggregates on read from spans, which works but became a hot spot for all of them: Langfuse tried AggregatingMergeTree, and MLflow and Phoenix promoted columns.
- **Sessions derived on read (01 §Session, 05).** Every product does this. Helicone dropped a materialized session table. Phoenix keeps a `project_sessions` row, and it costs them first-wins session attachment, orphan sweeps and `SKIP LOCKED` races.
- **Typed columns for hot fields, plus maps for the rest.** MLflow (JSON `content`) and Phoenix (JSON `attributes`) both spent years promoting tokens, cost, session, user and model out of JSON. LangWatch uses typed columns plus maps with bloom indexes.
- **Keyset cursors (05 §Trace list).** MLflow's OFFSET pagination is the counter-example.
- **Tokens only on `llm`/`embedding` spans (02 §123).** Phoenix does the same. MLflow needed an "ancestor has usage" rule after double-counting roll-up parents.
  - Residual risk: two nested `llm` spans, for example a LangChain ChatModel span wrapping an OpenAI-instrumentor span, still double count. Consider a normalization rule, or document it.
- **A real WAL with group commit.** Phoenix (in-memory deque, 503 at 20k), MLflow (client queue drops when full; server drops spans after create retries) and Laminar have no durable acknowledgement. This is a genuine differentiator.
- **Tombstones plus a rewriter for GDPR (03 §7).** Nobody else has a per-user delete. LangWatch has no per-trace delete at all.

### B. Likely bugs or gaps in our spec
1. **The trace window only pads backwards (03 §6, `trace_lookback`).** We scan partials on `[a − 1h, b]`.
   - The lookback correctly stops a trace that started before `a` from looking as if it started inside the window.
   - It does not catch partials that start **after `b`** for a trace that started inside the window.
   - The root span ends last, so it is often exported in the last flush, but its own `start_time` is the earliest, so the root itself is never the problem. The problem is the later children of long agent runs.
   - The effect: traces near the upper edge of a historical window get truncated aggregates (`end_time`, tokens, cost, `span_count`, error status).
   - Laminar pads **both sides** (±3 h). Recommendation: scan `[a − lookback, b + lookahead]`, filter merged rows on `[a, b]`, and tie the padding to a documented maximum trace duration. The "latest N" fast path is unaffected because `b = now`.
2. **Filter pushdown into `TracesProvider` must not touch partials.** A filter such as `session_id = X`, `status = 'error'`, `cost_total > 0.1` or `tags @> …` evaluated per partial gives wrong answers:
   - a non-root partial usually has a NULL session ID or user ID;
   - a cost threshold can fail on each partial yet pass on the merged row.
   - LangWatch hit the same problem with dedup (dedup on base predicates first, filter afterwards).
   - Rule to write into 03 §6: push down only the time window and `trace_id`. For selective predicates such as session ID or user ID, run two phases: a bloom or stats scan finds candidate `trace_id`s, then **all** partials of those traces are fetched, merged, and then filtered.
3. **Duplicate spans inflate trace sums permanently.**
   - 01 says duplicate spans are "tolerated at write time and removed at compaction". That holds for `spans`, but each flush already wrote a `traces` partial that **summed** the duplicate, and merging partials (03 §5) has no span IDs to dedup on.
   - OTLP exporters retry after timeouts even when the server had already persisted the data, so duplicates are realistic.
   - Laminar documents the exact failure ("permanently inflate tokens/costs"). LangWatch carries a dedup ID set and warns that otherwise it "will double-count".
   - Options:
     - (a) a bounded recent-span-ID set at ingest (LangWatch uses a 30 s TTL), which catches the common retry case;
     - (b) at day close, **recompute** `traces` rows from the deduped `spans` instead of merging partials;
     - (c) both. Recommendation: (b) as the correctness backstop and (a) for freshness.
4. **Compaction is per day, but traces are not.**
   - Compaction and day close work per `(table, project, day)`. A trace crossing midnight, or a late partial landing in another day, keeps ≥ 2 partials forever, so "`partial_count = 1` after full compaction" is false for those traces.
   - The query merge still gives the right answer, but the `fully_merged` style shortcut cannot be assumed for `traces`.
   - Retention (03 §8) drops day partitions, so such a trace becomes **half-deleted**: its old spans and partials disappear while the newer ones remain.
   - LangWatch solved both problems by partitioning and applying TTL on a **frozen anchor** (first-seen time), not on `min(start)`.
   - Decide whether `traces` partials (and possibly spans) partition by span `start_time`, by the trace's first-seen day, or by ingest day, and document the cross-day behaviour.
5. **Timestamp sanity at ingest.**
   - LangWatch's `1970-01-01` incident (`00072` header): a zero `start_time` landed in an already-expired partition and made detail reads unbounded.
   - Our day partition is derived from `start_time`, and nothing in 01, 02 or 03 clamps zero, pre-epoch or far-future timestamps (a grep found no rule).
   - Add a rule: for example, when `start_time` is 0 or more than N days from `ingest_time`, store it but partition by `ingest_time`, or clamp it and record it in `truncated`/`conventions`.
6. **Order-dependent merge rules.**
   - 01 uses "first non-null" for `session_id`, `user_id`, `environment`, `release` and for `name` when there is no root. Across partials "first" needs a defined order; use earliest `start_time`, tie-break `span_id`, so the result is identical whatever the flush or compaction order.
   - **Multiple roots** need the same tie-break; Phoenix uses earliest start.
   - MLflow's first-writer-wins previews and LangWatch's arrival-order fold are both arrival-order dependent. Ours can be deterministic.
7. **Trace filters over span properties are missing from the DSL (05).**
   - Phoenix has `any(...)`, `all(...)`, `len`, `sum` and `min`/`max` over a trace's spans; LangWatch has span-attribute IN-subqueries; MLflow has `span.*` EXISTS.
   - Our `traces` view only sees merged columns, and full-text search is the only spans semi-join.
   - Users will want "traces containing a `tool` span named X" or "traces where any LLM span has `attributes['k'] = v`". Consider a `has_span(<span predicate>)` UDF, compiled to `trace_id IN (SELECT trace_id FROM spans WHERE … AND <padded window>)`.
8. **Session time-window semantics are undefined (05 `sessions` view).** Grouping `traces WHERE start_time ∈ window` yields aggregates of the part of the session inside the window. Phoenix deliberately lists sessions by interval overlap: a session appears if it was active in the window, using a stored per-session `[start, end]`. Its aggregates are a grain shift over the session's traces (`session_aggregates.py`). Without a session table, overlap listing needs `min(start)` and `max(end)` per session over a wider scan. Choose one semantic and document it.
9. **Single-trace lookup for long traces.** The ±1-day time hint misses spans of traces longer than a day. Opik built `span_weeks` (trace → partitions) for this; our spec already lists a post-MVP `trace_index`. Note the multi-day case explicitly.

### C. Filter DSL safety (05)
The checks below come from Phoenix's hardening and LangWatch's limits.
- Cap expression depth. sqlparser recursion and our own validator walk both recurse; Phoenix catches `RecursionError`.
- Cap the expression size and the number of `IN` list items and literals (LangWatch: 50 parameters).
- Reject NUL characters in literals.
- Make the ASCII-only identifier rule explicit, so Unicode look-alikes cannot sneak in. sqlparser does not NFKC-normalise the way Python does, but quoted identifiers deserve a check.
- Make search terms with `OR` predictable. LangWatch disables content search under `OR`, and MLflow forbids `OR` entirely. We support `OR` fully, which is better, but the full-text conjunct must stay ANDed.

### D. Smaller design notes
- **Heavy payload split.**
  - LangWatch keeps IO over 64 KiB out of projections, as a preview plus a pointer.
  - Our `input_preview`/`output_preview` (P columns) plus heavy columns that are only projected on open is the same idea, done with columnar projection instead.
  - Keep `traces` partials free of heavy IO, as specified.
- **Light list, then heavy page** (LangWatch). It matches 05 §Trace detail; apply it to the trace list too, so heavy previews are fetched only for the page's IDs.
- **Per-query memory caps.** LangWatch needed 2 GiB after out-of-memory errors on unbounded detail reads. We already have `query.max_memory`.
- **Retention.** LangWatch stamps `_retention_days` on each row and uses merge-time TTL. Phoenix adds a `max_count` rule. Day-granular partition drop (ours) is simpler and fine, given the cross-day caveat in B.4.
- **Deletes vs compaction.** Opik's `deletion_events` lesson: any copy or rewrite path (compaction, deletion rewriter, a future migration tool) must apply tombstones explicitly. Our 03 §5 already applies tombstones in every merge; keep that as an invariant with a test.
- **Cross-project traces.** Phoenix keys traces globally, and "existing trace's memberships win" across projects. With our `(project_id, trace_id)` key, a distributed trace whose services send to different projects is split into two traces. This is acceptable but should be documented.
