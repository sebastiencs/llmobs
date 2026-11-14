# G · M4 — Live updates

Implementation plan for milestone M4 of track G. It has 5 sub-phases, G4.1 to G4.5. Each one is done by one agent in one go and ends with `just check` green and one commit.

**Order:** G4.1 (storage) and G4.2 (live bus) touch different crates and run at the same time. G4.3 needs G4.2. G4.4 needs G4.1 and G4.3. G4.5 is last.

**Prerequisites:** M1, M2 and M3 are on the branch you start from (`git log` shows `server: accept OTLP over gRPC`). Node `26.13.0` is at `$HOME/.local/share/nvm/v26.13.0/bin` (not on the non-interactive `PATH`).

**Read first** (every sub-phase):
- `AGENTS.md`, all of it.
- This file's sections up to § G4.1, then your own sub-phase.
- `docs/plans/g-spine/00-design.md` § Live bus and SSE (M4), § Commit API.
- `docs/spec/09-api.md` § Observability data (the two `stream` rows) and § SSE; `docs/spec/02-ingestion.md` § Live bus; `docs/spec/05-query.md` § Full-text search.
- `docs/mvp-decisions.md`: D134, D-C-5, D-G-12, D-G-13, D-G-22, D-G-31 to D-G-34.
- The web client this must work with: `web/src/lib/sse.ts`. It listens for `span`, `trace`, `resync` and `ping` events; a 45 s gap without `ping` makes it reconnect. On an error it closes the `EventSource` itself and opens a new one after a backoff, and sends itself a `resync` when it reopens; it never sends `Last-Event-ID`.

## What M4 is, and what it is not

When M4 is done, the UI's live mode works against the real server:
- `GET /api/v1/projects/{project}/traces/{trace_id}/stream` sends `span` events for new spans of one trace;
- `GET /api/v1/projects/{project}/traces/stream` sends `trace` events with the merged summaries of traces that new spans touched, honouring `filter`, `search`, `include_eval` and `include_experiments`.

**Out of scope:** any web change; replay (D-G-12); multi-node fan-out; re-checking access while a stream is open (a stream outlives a revoked session, key or membership until it ends; accepted for M4, D-G-34).

## Decisions this plan relies on

| # | Summary |
|---|---|
| D-G-12 | In-process `tokio::sync::broadcast`, no replay; monotonic `id:`; `Last-Event-ID` or lag → `resync`; `ping` every 15 s. |
| D-G-13 | The trace stream re-reads merged summaries of touched traces once a second, at most 500 ids per tick, else `resync`. |
| D-G-22 | The trace stream validates `filter` at open (any `search` text is valid): an invalid filter is `400 invalid_filter`, not a stream. A failed re-read sends `resync`. |
| D-C-5 | Search on the live stream: a trace is sent from the first arriving span that matches, then on every later update while the stream is open. |
| D-G-31 | **Storage read `trace_refresh`** (A2's directory): the traces-view rows of up to 500 trace ids, with the default exclusions, the filter, and an optional search over a time window. An empty id list only parses the filter. |
| D-G-32 | **Bus bounds.** Capacity 64 events; an event carries at most 500 span summaries (a bigger commit publishes several); nothing is built when no stream is open. Every stream receives every project's events and filters them. Worst case about 64 × 500 summaries (each up to ~2 KiB with its two 512-char previews) held by the channel, ~64 MiB. |
| D-G-33 | **Live search window.** A tick re-reads not-yet-matched traces with the search limited to `start_time ∈ [min, max + 1)` of the spans that arrived for them, the window narrowed to its last 24 h when wider. Matched trace ids are remembered (at most 10 000, then `resync` and the set is cleared) and re-read without search. |
| D-G-34 | **Stream slots, failures and shutdown.** At most 100 open streams per user (per key for an API key; a PAT counts as its user); the 101st is `429 rate_limited`, `Retry-After: 1`. A re-read refused as busy or timed out (10 s) is retried next tick without `resync`; other failures send `resync`. Shutdown closes every stream before the HTTP drain. Responses add `X-Accel-Buffering: no` (axum's `Sse` already sends `Cache-Control: no-cache`). Access is not re-checked while a stream is open. |

## Conventions for every sub-phase

- **Branch.** You run in your own git worktree. First run `pwd`, `git branch --show-current` and `git log --oneline -5`. The brief names the branch you start from; when your branch is not at it, run `git reset --hard <that branch>` (your branch has no work of its own yet), then check `git log` again. Commit only on your own branch; no push, merge or rebase beyond that reset. Never bare `git stash`.
- **Node.** Before `just check` or `just openapi`: `export PATH="$HOME/.local/share/nvm/v26.13.0/bin:$PATH"` in the same command.
- **Checks.** Iterate with `just check-rust`. Before the commit, `just check` exactly once; it must pass. A foreground command may run 10 minutes; when it may take longer, start it once with `run_in_background`, writing its output to a file in the session scratchpad, and wait for the completion notice. Never poll, never start a second run, leave no loop behind.
- **Errors and panics.** No `unwrap()`/`expect()` outside tests (locks: `.lock().unwrap_or_else(PoisonError::into_inner)`). Handlers return `ApiError`. No arithmetic that can overflow on network input (use `saturating_*`).
- **Privacy.** Never log filter or search text, span content, or a key. Log project ids, counts, `RunError::kind()` or a fixed text.
- **Tests.** `TestApp` + `seed()`, `#[tokio::test(flavor = "multi_thread")]`, except tests using `assert_logs_clean`, which use `#[tokio::test(flavor = "current_thread")]` (the log capture is per thread). Every wait has a timeout. Read stream events with the G4.3 helpers that skip `ping`.
- **Names.** Storage items as `llmobs_storage::…`.
- **STOP rules.** Stop and report without committing when the plan is wrong about the code in a way you cannot resolve without guessing, or an earlier milestone's test fails for a reason outside your change.
- **Commit.** One commit, the subject given, no trailer lines.
- **Report.** Branch and worktree path; commit hash; what you did; deviations and why; new dependencies (none are expected); anything unverified.

## Final layout after M4

```
crates/storage/src/reads/traces.rs   trace_refresh, TraceRefreshRequest, MAX_REFRESH_IDS (G4.1)
crates/server/src/
  live.rs              LiveBus, LiveEvent, StreamTiming, StreamOwner, StreamSlot (G4.2)
  ingest/mod.rs        commit publishes (G4.2)
  ingest/grpc.rs       passes the bus to commit (G4.2)
  routes/otlp.rs       passes the bus to commit (G4.2)
  routes/read.rs       open_query_on (G4.3)
  routes/streams.rs    SSE plumbing, span stream (G4.3), trace stream (G4.4)
  api/openapi.rs       stream error sets (G4.3)
  http/mod.rs          AppInner.live (G4.2)
  http/access.rs       registers streams::routes() (G4.3)
  http/access_suite.rs stream bodies are not collected (G4.3)
  serve.rs             closes the bus at shutdown (G4.2)
  test_support/mod.rs  StreamTiming::TEST, commit_spans, dataset_span, send_stream, TestStream (G4.2, G4.3)
```

## Shared reference

### SSE wire format

| Event | `id` | `data` |
|---|---|---|
| `span` | the bus event id | `serde_json` of `api::traces::SpanSummary` |
| `trace` | the largest bus event id folded into this tick | `serde_json` of `api::traces::TraceSummary` (with `scores`) |
| `resync` | none | `{}` |
| `ping` | none | `{}` |

Built with `axum::response::sse::Event::default().event(name)`, `.id(..)` when there is one, `.data(json)`. `ping` is a real event, not an SSE comment (the client's watchdog listens for the `ping` event): do not use `Sse::keep_alive`. The response is `(headers, Sse::new(body)).into_response()` with `X-Accel-Buffering: no` in `headers`.

### Stream task pattern (both streams)

The handler validates, takes a slot, subscribes, spawns one task and returns at once:
```rust
let (tx, rx) = tokio::sync::mpsc::channel::<Event>(16);
tokio::spawn(run_…(…, tx));        // owns the slot, the bus receiver, the closed receiver, an Arc<Storage>
let body = futures::stream::unfold(rx, |mut rx| async move {
    rx.recv().await.map(|event| (Ok::<_, std::convert::Infallible>(event), rx))
});
```
- The task never holds an `AppState` (a lingering clone would keep `Running::shutdown` from closing `meta.db`). It holds what it needs: `Arc<Storage>` (`state.storage.clone()`), the `ProjectId`, the params, the `broadcast::Receiver`, the `watch::Receiver<bool>`, the `StreamSlot`, the `StreamTiming`.
- Shutdown is awaited through one helper (a `watch::Ref` must not be held across an `.await`, it is `!Send`):
  ```rust
  async fn shut(rx: &mut watch::Receiver<bool>) { let _ = rx.wait_for(|closed| *closed).await; }
  ```
- Every `select!` is `biased;` with arms in this order: `shut(&mut closed)` → return; `tx.closed()` → return (the client left; frees the slot at once); then the stream's own arms.
- Intervals use `MissedTickBehavior::Delay`.
- The task returns when a `tx.send(..).await` fails, on shutdown, when the client leaves, on `RecvError::Closed`, and (span stream only) after the idle timeout. It never panics.

### Timing

`crate::live::StreamTiming { tick: Duration, ping: Duration, idle: Duration }`. `StreamTiming::DEFAULT` (production, `serve.rs`) = tick 1 s, ping 15 s, idle 10 min. `#[cfg(test)] StreamTiming::TEST` (`test_support`) = tick 20 ms, ping 200 ms, idle 1 s. The re-read timeout is `routes::streams::REFRESH_TIMEOUT = Duration::from_secs(10)` (D-G-34).

## G4.1 — Storage: `trace_refresh`

**Start from:** the branch the brief names. **Files:** `crates/storage/src/reads/{traces.rs,mod.rs}`, `crates/storage/src/lib.rs`. This edits A2's directory (the user allowed storage reads for M3; M4 follows the same rule, D-G-31).

### Steps

1. In `reads/traces.rs` (add `TimeWindow` to its `crate::views` import), add:
   ```rust
   /// Traces the live stream re-reads (D-G-13, D-G-31).
   #[derive(Debug, Clone, Copy)]
   pub struct TraceRefreshRequest<'a> {
       /// Filter text over `traces`. Empty or whitespace means no filter.
       pub filter: Option<&'a str>,
       /// Search text and the `start_time` window its spans must fall in (D-G-33).
       pub search: Option<(&'a str, TimeWindow)>,
       /// Trace ids, 32 hex digits, either case. At most [`MAX_REFRESH_IDS`].
       pub trace_ids: &'a [&'a str],
       /// Keep rows whose environment is the eval environment.
       pub include_eval: bool,
       /// Keep rows that belong to an experiment.
       pub include_experiments: bool,
       /// Time allowed for planning and running.
       pub timeout: Duration,
   }

   /// Max ids of one [`trace_refresh`].
   pub const MAX_REFRESH_IDS: usize = 500;

   /// The traces-view rows (every column, no order) of `trace_ids` that pass the default
   /// exclusions, the filter and the search. No time window applies to the traces: the query
   /// scans the project's traces once per call.
   ///
   /// An empty `trace_ids` parses the filter and returns no batches without running a query:
   /// the live stream validates its filter this way when it opens.
   ///
   /// # Errors
   ///
   /// [`RunError::Invalid`] for filter text; [`RunError::InvalidRequest`] for more than
   /// [`MAX_REFRESH_IDS`] ids or an id that is not 32 hex digits; [`RunError::Timeout`].
   pub async fn trace_refresh(ctx: &QueryContext, request: TraceRefreshRequest<'_>) -> Result<Vec<RecordBatch>, RunError>
   ```
2. Body (read `list_frame` first; it is the model):
   1. Filter as `list_frame` does: `present(request.filter)` → `ctx.parse_filter(FilterView::Traces, text)?`, keeping `references_environment`.
   2. More than `MAX_REFRESH_IDS` ids → `InvalidRequest("at most 500 trace ids")`; each id through `traces.rs`'s private `hex_id(id, 32, "trace_id")` (it lowercases); empty ids → `Ok(vec![])` now.
   3. Under `tokio::time::timeout(request.timeout, …)` (`Err` → `RunError::Timeout`): `ctx.session().table("traces")` (errors through `classify`), conjuncts:
      - `exclusions(ctx, FilterView::Traces, include_eval, include_experiments, references_environment)?`;
      - `col(traces::TRACE_ID).in_list(<the lowercase hex literals>, false)`;
      - when `search` is `Some((text, window))` and `present(Some(text))` is some: `search(FilterView::Traces, text, Some(window), ctx).await?`;
      `frame.filter(<conjuncts and-ed>)`, then `filter.apply(frame, ctx)`, then `frame.collect().await.map_err(|error| ctx.fail(error))` (as `list_frame`/`finish` do).
3. Re-export `trace_refresh`, `TraceRefreshRequest` and `MAX_REFRESH_IDS` in `reads/mod.rs` and `lib.rs` (keep the lists sorted).

### Tests (`reads/traces.rs` test module, on the `test_support` dataset — `fixtures/api/dataset`)

- Ids of traces `a1…01`, `a1…03`, `a1…05`, defaults → rows for 01 and 03 (05 is `llmobs-eval`); with `include_eval` → 01, 03, 05.
- Filter `name = 'answer-question'` with ids 01, 03 → only 03.
- Search `delivery` (only trace 02's span text has it: "Can I change the delivery address?") with window `[2026-01-01T10:05:00Z, 2026-01-01T10:06:00Z)` and ids 01, 02 → only 02; the same search with window `[10:00:00, 10:01:00)` → none.
- Empty ids with filter `name = 'x'` → `Ok(vec![])`; empty ids with filter `name =` → `RunError::Invalid`.
- 501 ids → `InvalidRequest`; `"zz"` → `InvalidRequest`.

### Done when

`just check` passes. Commit: `storage: re-read traces for the live stream`.

## G4.2 — The live bus

**Start from:** the branch the brief names. **Files:** `crates/server/src/{lib.rs,live.rs,serve.rs}` (new `live.rs`), `crates/server/src/http/mod.rs`, `crates/server/src/ingest/{mod.rs,grpc.rs}`, `crates/server/src/routes/otlp.rs`, `crates/server/src/test_support/mod.rs`.

### Steps

1. **`crate::live`** (`pub(crate) mod live;` in `lib.rs`), starting with `#![cfg_attr(not(test), allow(dead_code))]` (G4.4 removes it, once every item has a caller):
   ```rust
   //! In-process live bus for SSE (D-G-12, D-G-32, D-G-34).

   /// One committed batch, or a part of one (at most [`MAX_EVENT_SPANS`] spans).
   #[derive(Debug)]
   pub(crate) struct LiveEvent {
       /// Monotonic over the process's life, from 1.
       pub(crate) id: u64,
       pub(crate) project_id: ProjectId,
       /// Light rows, in commit order.
       pub(crate) spans: Vec<SpanSummary>,
   }

   pub(crate) const BUS_CAPACITY: usize = 64;
   pub(crate) const MAX_EVENT_SPANS: usize = 500;
   pub(crate) const MAX_STREAMS_PER_OWNER: u32 = 100;

   #[derive(Debug, Clone, Copy, PartialEq, Eq)]
   pub(crate) struct StreamTiming { pub(crate) tick: Duration, pub(crate) ping: Duration, pub(crate) idle: Duration }
   impl StreamTiming {
       pub(crate) const DEFAULT: Self = Self { tick: Duration::from_secs(1), ping: Duration::from_secs(15), idle: Duration::from_secs(600) };
       #[cfg(test)]
       pub(crate) const TEST: Self = Self { tick: Duration::from_millis(20), ping: Duration::from_millis(200), idle: Duration::from_secs(1) };
   }

   /// Who holds a stream slot.
   #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
   pub(crate) enum StreamOwner { User(Uuid), Key(Uuid) }

   type Slots = Arc<Mutex<HashMap<StreamOwner, u32>>>;   // std::sync::Mutex

   pub(crate) struct LiveBus {
       sender: broadcast::Sender<Arc<LiveEvent>>,
       next_id: AtomicU64,
       slots: Slots,
       closed: watch::Sender<bool>,
       pub(crate) timing: StreamTiming,
   }

   /// A held stream slot. Dropping it frees the slot.
   pub(crate) struct StreamSlot { slots: Slots, owner: StreamOwner }
   ```
   - `LiveBus::new(timing)`: `broadcast::channel(BUS_CAPACITY).0`, `watch::channel(false).0`, `next_id` 0.
   - `publish(&self, project_id: ProjectId, spans: &[Span])`: when `self.sender.receiver_count() == 0` return at once. Otherwise, for each `spans.chunks(MAX_EVENT_SPANS)`: `id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1`; `let _ = self.sender.send(Arc::new(LiveEvent { id, project_id, spans: chunk.iter().map(SpanSummary::from).collect() }));` (an error only means no receiver).
   - `subscribe(&self) -> broadcast::Receiver<Arc<LiveEvent>>`.
   - `acquire(&self, owner) -> Option<StreamSlot>`: under the lock, a count below `MAX_STREAMS_PER_OWNER` is incremented and a slot returned; otherwise `None`. `impl Drop for StreamSlot`: lock, decrement, remove the key at 0.
   - `close(&self)`: `self.closed.send_replace(true);`. `closed(&self) -> watch::Receiver<bool>`: `self.closed.subscribe()`.
   - `#[cfg(test)] pub(crate) fn last_id(&self) -> u64`.
   - Manual `Debug` for `LiveBus` (prints `receiver_count` and the last id) and `StreamSlot` (prints the owner kind only).
2. **State.** `AppInner.live: crate::live::LiveBus` (doc `/// Live bus and stream slots (D-G-12).`): `LiveBus::new(StreamTiming::DEFAULT)` in `serve.rs`, `LiveBus::new(StreamTiming::TEST)` in `test_support`.
3. **Commit publishes.** `Ingest::commit(&self, storage: &Storage, usage: &Usage, live: &LiveBus, target: CommitTarget, normalized: Normalized)`. In the `spawn_blocking` closure keep the spans: `spans_to_record_batch(&spans).map(|batch| (batch, spans))` (the `IngestError::Batch` mapping is unchanged). Inside the branch where spans were appended, right after `append_spans` succeeds: `live.publish(target.project_id, &spans);`. Callers: `routes/otlp.rs`, `ingest/grpc.rs`, and the ingest tests in `ingest/mod.rs` (pass `&app.state.live`).
4. **Shutdown.** At the very start of `Running::shutdown`, before any shutdown signal (HTTP, OTLP/HTTP, gRPC): `if let Some(state) = &self.state { state.live.close(); }`.
5. **Test helpers** in `test_support` (G4.3 and G4.4 use them):
   - `pub(crate) async fn commit_spans(&self, project: &ProjectRow, spans: Vec<Span>)`: `self.state.ingest.commit(&self.state.storage, &self.state.usage, &self.state.live, CommitTarget { org_id: project.org_id, project_id: project.id, request_bytes: 0, now_ns: self.state.clock.now_ns() }, Normalized { spans, media: Vec::new(), rejections: Rejections::default() }).await.unwrap()` (check `Rejections` has `Default`; if not, build it the way `llmobs_otel` tests do).
   - `pub(crate) fn dataset_span(index: usize) -> Span`: the `index`-th span of `fixtures/api/dataset/spans.json` (factor the file reading out of `load_example_dataset`). Tests clone it and set `project_id`, `trace_id`, `span_id`, `parent_span_id: None`, `name`, `input`, `environment`, `start_time`/`end_time` as they need.

### Tests (`live.rs`)

- `publish` with no subscriber builds nothing: `last_id()` unchanged.
- With a subscriber: 1200 spans → three events (500, 500, 200) with consecutive ids, the right `project_id`, summaries in order.
- `acquire` 100 times for one owner → 100 slots; the 101st → `None`; drop one → `acquire` succeeds; another owner is independent; after dropping every slot the map is empty.
- `close()` → `shut(&mut bus.closed())` returns (define `shut` in `live.rs` as `pub(crate)`, per § Stream task pattern); a receiver taken after `close()` returns at once too.
- In `routes/otlp.rs`: subscribe, export one JSON fixture → one event with the stored span count and the project id.

### Done when

`just check` passes. Commit: `server: publish committed spans on a live bus`.

## G4.3 — SSE plumbing and the span stream

**Start from:** the branch the brief names (it has G4.2). **Files:** `crates/server/src/routes/{mod.rs,streams.rs,read.rs}` (new `streams.rs`), `crates/server/src/http/{access.rs,access_suite.rs}`, `crates/server/src/api/openapi.rs`, `crates/server/tests/openapi.rs`, `crates/server/openapi.json` and `web/src/api/schema.gen.ts` (regenerated), `crates/server/src/test_support/mod.rs`.

### Steps

1. **Stream test helpers** (`test_support`):
   - Factor the request building of `TestRequest::send` into a private `fn build(self) -> (Router, Request<Body>)`; `send` uses it.
   - `TestRequest::send_stream(self) -> TestStream`: `build`, `oneshot`, keep the body: `TestStream { pub(crate) status: StatusCode, pub(crate) headers: HeaderMap, body: Body, buffer: String }`.
   - `SseEvent { pub(crate) event: String, pub(crate) id: Option<String>, pub(crate) data: String }`.
   - `TestStream::next_event(&mut self, wait: Duration) -> Option<SseEvent>`: read frames (`http_body_util::BodyExt::frame`) under one `tokio::time::timeout(wait, …)` until `buffer` holds a full event (up to `\n\n`); parse `event: `, `id: `, `data: ` lines, ignore lines starting with `:` and blocks without `event:`; `None` on timeout or end of body.
   - `next_non_ping(&mut self, wait) -> Option<SseEvent>`: like `next_event`, skipping `ping` events, within the same overall `wait`.
   - `body_closed(&mut self, wait) -> bool`: keep reading frames (ignoring their content) until the body ends (`true`) or `wait` elapses (`false`).
2. **Read helper** (`routes/read.rs`): `pub(crate) async fn open_query_on(storage: &Storage, project: ProjectId, around: Option<i64>) -> Result<QueryContext, ApiError>`; `open_query` calls it with `&state.storage`.
3. **Contract.** In `api/openapi.rs`, `stream_trace` and `stream_traces` use `READ_ERRORS` (they can answer 400, 422 and 503 too). In `tests/openapi.rs`, add both ids to `READ_OPERATIONS` (and remove `stream_traces` from `QUERY_OR_BODY`). Do not change their descriptions (the lint pins them). Run `export PATH=… && just openapi`; commit the regenerated files.
4. **`routes/streams.rs`** — shared pieces:
   - `pub(crate) const REFRESH_TIMEOUT: Duration = Duration::from_secs(10);` (used in G4.4).
   - `fn sse_response(rx: mpsc::Receiver<Event>) -> Response` (§ Stream task pattern; adds `X-Accel-Buffering: no`).
   - `fn owner(principal: &Principal) -> StreamOwner`: `Session { user, .. }` / `Pat { user, .. }` → `User(user.id)`, `ApiKey { key_id, .. }` → `Key(*key_id)`.
   - `fn take_slot(state: &AppState, principal: &Principal) -> Result<StreamSlot, ApiError>`: `None` → `ApiError::RateLimited { retry_after_s: 1 }`.
   - `fn ping() -> Event`, `fn resync() -> Event` (`data("{}")`), `fn json_event(name, id: u64, value: &impl Serialize) -> Option<Event>` (`serde_json::to_string` error → `None`, the caller skips it and logs a fixed text).
   - `fn has_last_event_id(headers: &HeaderMap) -> bool` (`last-event-id` present).
5. **Span stream.** `routes()` in `streams.rs`, registered in `access::routes()` right after `traces::routes()`: `GET /api/v1/projects/{project}/traces/{trace_id}/stream`, `Access::Project { perm: ProjectPerm::View, session_only: false }`, `documented: true`, handler `stream_trace(State, Extension<Principal>, Extension<ProjectAccess>, error::Path<StreamTracePath { trace_id: String }>, HeaderMap)` (a new private path struct):
   1. `trace_id = TraceId::from_str(&path.trace_id)` (either case) → else `ApiError::BadRequest { detail: Some("invalid trace_id".to_owned()), errors: Vec::new() }`.
   2. `slot = take_slot(..)?`; `bus = state.live.subscribe()`; `closed = state.live.closed()`; `timing = state.live.timing`.
   3. Spawn `run_span_stream(project_id, trace_id, has_last_event_id, bus, closed, timing, slot, tx)`:
      - when `Last-Event-ID` was sent, send `resync()` first;
      - `ping = interval_at(Instant::now() + timing.ping, timing.ping)` with `MissedTickBehavior::Delay`; `idle = Instant::now() + timing.idle`, reset to `now + timing.idle` after each sent `span`;
      - loop `select! { biased; _ = shut(&mut closed) => return, _ = tx.closed() => return, _ = sleep_until(idle) => return, _ = ping.tick() => send ping(), r = bus.recv() => … }` where `Ok(event)` with `event.project_id == project_id` sends one `json_event("span", event.id, span)` per span with `span.trace_id == trace_id`; `Err(RecvError::Lagged(_))` → `resync()`; `Err(RecvError::Closed)` → return. A failed send → return.
   An idle-closed stream ends its body; the client reconnects after its backoff and refetches through its own `resync`.
6. **Access suite.** `http/access_suite.rs`'s `send` helper collects bodies, which never ends for a stream. When `path.ends_with("/stream")`, use `send_stream()` and return `TestResponse { status, headers, body: Bytes::new() }` (dropping the `TestStream` ends the stream task: `tx.closed()`). The suite reads only the status and `x-test-access`.
7. Until G4.4, `GET …/traces/stream` matches `/traces/{trace_id}` and answers 400; that is expected.

### Tests (`routes/streams.rs`)

Spans are added with `app.commit_spans(&seeded.project, vec![span])`, where `span` is `dataset_span(0)` with `project_id = seeded.project.id`, a chosen `trace_id`/`span_id`, `parent_span_id: None`.
- Open the stream for `a1000000000000000000000000000001` with the seeded cookie → 200, `content-type` starts with `text/event-stream`, `cache-control: no-cache`, `x-accel-buffering: no`; commit a span of that trace → `next_non_ping(2 s)` is `span` with an `id` and `data` that parses as `SpanSummary` with that trace and span id; then commit a span of trace `…02` → no non-ping event within 300 ms.
- A `ping` arrives within 1 s, data `{}`.
- With header `Last-Event-ID: 7` → the first event is `resync`.
- No span for TEST idle (1 s) → `body_closed(3 s)` is true.
- `app.state.live.close()` → `body_closed(1 s)` is true.
- Slots: hold 100 slots from `app.state.live.acquire(StreamOwner::User(user_id))` (`user_id` from `users::user_by_email(app.state.meta.reader(), "ada@example.com")`), then open → 429, `code == "rate_limited"`, `retry-after: 1`; drop one slot → open succeeds.
- `…/traces/zz/stream` → 400; the read key → 200; no credential → 401.
- Dropping the `TestStream` frees its slot: after drop, within 1 s, `acquire` succeeds 100 times again (poll every 20 ms up to 1 s; this is a test, not a server loop).

### Done when

`just check` passes. Commit: `server: stream the spans of one trace`.

## G4.4 — The trace list stream

**Start from:** the branch the brief names (it has G4.1 and G4.3). **Files:** `crates/server/src/routes/streams.rs`, `crates/server/src/live.rs` (remove the `dead_code` allowance).

### Steps

1. Route `GET /api/v1/projects/{project}/traces/stream` in `streams.rs`'s `routes()` (before the span stream row), `ProjectPerm::View`, `documented: true`. Handler `stream_traces(State, Extension<Principal>, Extension<ProjectAccess>, error::Query<TraceStreamParams>, HeaderMap)`:
   1. `slot = take_slot(..)?`; `bus = state.live.subscribe()` (before validation, so no event is missed; both drop on a validation error).
   2. Validate (D-G-22): `ctx = open_query(&state, project_id, None).await?`; `llmobs_storage::trace_refresh(&ctx, TraceRefreshRequest { filter: params.filter.as_deref(), search: None, trace_ids: &[], include_eval: params.include_eval, include_experiments: params.include_experiments, timeout: READ_TIMEOUT }).await?` — an invalid filter is `ApiError::Query` → `400 invalid_filter`. Drop `ctx`.
   3. Spawn `run_trace_stream(storage: Arc<Storage>, project_id, params, has_last_event_id, bus, closed, timing, slot, tx)`; return `sse_response(rx)`.
2. **`run_trace_stream`** keeps `pending: HashMap<TraceId, (i64, i64)>` (min and max `start_time` of the arriving spans per trace), `last_id: u64`, and `matched: HashSet<TraceId>` (used only when `params.search` is set and not blank).
   - Send `resync()` first when `Last-Event-ID` was sent.
   - `tick` and `ping` intervals (`MissedTickBehavior::Delay`); loop `select! { biased; shut → return; tx.closed() → return; ping.tick() → ping(); r = bus.recv() → …; tick.tick() → refresh }`.
   - `Ok(event)` of this project: per span, `pending.entry(span.trace_id)` min/max with `span.start_time`; `last_id = event.id`; when `pending.len() > MAX_REFRESH_IDS` → send `resync()`, clear `pending`. `Err(Lagged(_))` → send `resync()`, clear `pending`. `Err(Closed)` → return.
   - Tick with a non-empty `pending`: `batch = std::mem::take(&mut pending)`; run `refresh(&storage, project_id, &params, &batch, &mut matched)` raced against shutdown and the client: `select! { biased; shut → return; tx.closed() → return; r = refresh(..) => r }`. Then:
     - `Ok(rows)` → for each `TraceSummary` send `json_event("trace", last_id, &row)`; when `matched` was cleared for overflow (see below), send `resync()` first.
     - `Err(Retry)` → merge `batch` back into `pending` (min of mins, max of maxes); no event.
     - `Err(Resync)` → send `resync()`.
3. **`refresh`** returns `Result<RefreshRows, RefreshError>` with `enum RefreshError { Retry, Resync }`:
   1. `ctx = open_query_on(&storage, project_id, None).await` — `ApiError::Unavailable` (busy admission) → `Retry`; any other error → `Resync`.
   2. Without search: one `trace_refresh` with every id of `batch`, `search: None`, timeout `REFRESH_TIMEOUT`.
   3. With search: ids already in `matched` → one `trace_refresh` without search (skip when none); the others → one `trace_refresh` with `search: Some((text, window))` (skip when none), where `window = TimeWindow::new(lo, max.saturating_add(1))`, `max` the largest of their maxes, `lo = max(min of their mins, max.saturating_sub(86_400_000_000_000))` (D-G-33); every trace returned by that search read joins `matched`; when `matched` would exceed 10 000, clear it and report it (the caller sends `resync()`).
   4. `RunError::Timeout` or `RunError::TooLarge` from a read → `Retry`; any other `RunError` → `Resync`. Log `RunError::kind()` (warn) — never the text.
   5. Rows: `convert::trace_summaries(&batches)` (`ConvertError` → `Resync`, log a fixed text), then `attach_trace_scores(&ctx, &mut rows)` (`ApiError` → `Resync`).
   6. Drop `ctx` before returning (no admission is held between ticks).
4. Remove `live.rs`'s `dead_code` allowance (`tick` and every item now have callers; `TEST` and `last_id` are `#[cfg(test)]`).

### Tests (`routes/streams.rs`)

Spans are committed with `app.commit_spans` as in G4.3 (`dataset_span(0)` clones with chosen ids, `parent_span_id: None`, `start_time`/`end_time` set to the manual clock's day, `environment: Some("production")` unless the test says otherwise).
- Open `…/traces/stream` → 200; commit one span of new trace `a1…0a` → `next_non_ping(3 s)` is `trace` with that `trace_id`, `span_count` 1 and `id` set; commit a second span of it → `trace` with `span_count` 2 (merged, D-G-13).
- `?filter=name%20%3D%20'never'` → no non-ping event within 500 ms after a commit.
- `?filter=name%20%3D` (invalid) → 400 `invalid_filter`, not a stream.
- Exclusions: a span with `environment: Some("llmobs-eval")` → nothing within 500 ms; with `?include_eval=true` → its `trace`.
- Search (D-C-5): `?search=needle-x` → a span named `needle-x tool` sends its trace; a later span of the same trace named `plain` sends it again (matched); a span of another trace named `plain` sends nothing.
- Overflow: one `commit_spans` call with 1 100 spans of 1 100 distinct traces → a `resync` event within 3 s.
- `Last-Event-ID` → first event `resync`; `app.state.live.close()` → `body_closed(1 s)`.
- Privacy (`#[tokio::test(flavor = "current_thread")]`): `assert_logs_clean(&["needle-x"], async { open a `?search=needle-x` stream, commit a matching span, read its `trace` event })`.

### Done when

`just check` passes; the web's tests still pass unchanged. Commit: `server: stream live trace summaries`.

## G4.5 — Status

**Files:** `docs/plans/g-spine/00-design.md` (Status line: add "M4 implemented (branch `<branch>`)"), `docs/roadmap.md` (G row status: "**In progress.** M1–M4 implemented."). The decisions and spec edits of M4 were written with this plan.

### Done when

`just check` passes. Commit: `docs: mark G M4 implemented`.

## After M4

M5: the embedded UI, the minimal login page, the web on the real API, the end-to-end test.
