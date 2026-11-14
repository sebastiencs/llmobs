# F2 — Stream maximum age, send timeout, project isolation tests (M-2, m-1, m-4, m-5, m-12)

Read [README.md](README.md) first. Independent of F1 and F4. F3 runs after it (both edit `routes/streams.rs`).

**Goal:**
- Every live stream ends 15 minutes after it opened (M-2). A revoked session or key then keeps an open stream at most 15 minutes. The web client (`web/src/lib/sse.ts`) treats an ended stream as an error: it reconnects with backoff, which authenticates again, and sends `resync` to its page on reopen, which refetches. No web change is needed.
- A stream whose client has not made room for an event within 30 s ends, so a stalled client does not hold a slot and a task for ever (m-1).
- Tests show that a stream never sends another project's spans (m-4).
- Spec 09 says that a score write sends no stream event (m-5) and which spans a search stream checks (m-12).

**Edit only:**
- `crates/server/src/live.rs`;
- `crates/server/src/routes/streams.rs`;
- `docs/spec/09-api.md`;
- `docs/mvp-decisions.md` (row D-G-34 only).

**Commit:** `End live streams after 15 minutes or a stalled client`

## Background

`crates/server/src/live.rs`:

```rust
pub(crate) struct StreamTiming {
    pub(crate) tick: Duration,
    pub(crate) ping: Duration,
    pub(crate) idle: Duration,
}
impl StreamTiming {
    pub(crate) const DEFAULT: Self = Self { tick: 1 s, ping: 15 s, idle: 600 s };
    #[cfg(test)]
    pub(crate) const TEST: Self = Self { tick: 20 ms, ping: 200 ms, idle: 1 s };
}
```

`TestApp` builds its bus with `StreamTiming::TEST` (`test_support/mod.rs`), `serve.rs` with `StreamTiming::DEFAULT`.

`crates/server/src/routes/streams.rs`:
- `async fn run_span_stream(stream: SpanStream, tx: mpsc::Sender<Event>)` destructures `timing`, then loops on a `tokio::select! { biased; shut…, tx.closed()…, sleep_until(idle)…, ping.tick()…, bus.recv()… }`.
- `async fn run_trace_stream(stream: TraceStream, tx: mpsc::Sender<Event>)` loops on `tokio::select! { biased; shut…, tx.closed()…, ping.tick()…, bus.recv()…, tick.tick()… }`. Inside the tick arm, a nested `select!` runs `refresh`.
- Every event goes out with `tx.send(<event>).await.is_err()` followed by `return`. There are 4 such sends in `run_span_stream` (the resume `resync`, the ping, each `span` message, the lagged `resync`) and 7 in `run_trace_stream` (the resume `resync`, the ping, the `resync` when `pending.len() > MAX_REFRESH_IDS`, the lagged `resync`, the `overflowed` `resync`, each `trace` message, the `RefreshError::Resync` `resync`).
- `mpsc::channel(CLIENT_BUFFER)` with `CLIENT_BUFFER = 16`: a send waits once 16 events are queued and the client has not read them.
- Test helpers in `mod tests`: `stream_path`, `commit_one(app, seeded, trace, span)`, `user_id`, `traces_stream_path`, `open_traces(app, seeded, query)`, `commit_span(app, seeded, trace, span, name, environment)`, `next_trace`, `until_resync`, constants `TRACE_1`, `TRACE_2`, `WAIT` (2 s), `TRACE_WAIT` (3 s). From `test_support`: `TestApp::new`, `seed`, `add_project(&seeded, slug, None) -> (ProjectRow, String)`, `commit_spans(&project, rows)`, `dataset_span(i)`, `send_stream`, `TestStream::{next_non_ping, next_event, body_closed}`.

## Changes

### 1. `live.rs`: two new timings

Replace `StreamTiming` and its `impl` with:

```rust
/// Cadences and limits of the streams (D-G-34).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct StreamTiming {
    pub(crate) tick: Duration,
    pub(crate) ping: Duration,
    /// The span stream ends after this long without a span.
    pub(crate) idle: Duration,
    /// Every stream ends this long after it opened (G review M-2).
    pub(crate) max_age: Duration,
    /// A stream ends when its client has not made room for an event within this (m-1).
    pub(crate) send: Duration,
}

impl StreamTiming {
    pub(crate) const DEFAULT: Self = Self {
        tick: Duration::from_secs(1),
        ping: Duration::from_secs(15),
        idle: Duration::from_secs(600),
        max_age: Duration::from_secs(900),
        send: Duration::from_secs(30),
    };

    #[cfg(test)]
    pub(crate) const TEST: Self = Self {
        tick: Duration::from_millis(20),
        ping: Duration::from_millis(200),
        idle: Duration::from_secs(1),
        max_age: Duration::from_secs(4),
        send: Duration::from_millis(100),
    };
}
```

### 2. `streams.rs`: a bounded send

Add, above `fn take_slot`:

```rust
/// Sends `event` to the client. `false` when the client is gone, or has not made room for it
/// within `timeout` (G review m-1): the stream then ends.
async fn send(tx: &mpsc::Sender<Event>, event: Event, timeout: Duration) -> bool {
    matches!(tokio::time::timeout(timeout, tx.send(event)).await, Ok(Ok(())))
}
```

In `run_span_stream` and `run_trace_stream`, replace each of the 11 `tx.send(X).await.is_err()` expressions with `!send(&tx, X, timing.send).await`, keeping the surrounding `if … { return; }` and `&&` conditions. For example `if resume && tx.send(resync()).await.is_err() {` becomes `if resume && !send(&tx, resync(), timing.send).await {`. After the change, `rg 'tx\.send\(' crates/server/src/routes/streams.rs` finds only the line inside `fn send`.

### 3. `streams.rs`: the maximum age

In `run_span_stream`, after `let mut idle = Instant::now() + timing.idle;`, add:

```rust
    let end = Instant::now() + timing.max_age;
```

and in its `select!`, add after `() = tx.closed() => return,`:

```rust
            () = sleep_until(end) => return,
```

In `run_trace_stream`, after `let mut last_id = 0_u64;`, add the same `let end = Instant::now() + timing.max_age;`, and the same arm after `() = tx.closed() => return,` in the **outer** `select!` (not in the nested one around `refresh`: a re-read is bounded by `REFRESH_TIMEOUT`).

Update the module doc comment's first line to:

```rust
//! Live SSE streams (spec 09 § SSE; D-G-12, D-G-34). Every stream ends after
//! `StreamTiming::max_age`, or when its client stops reading for `StreamTiming::send`.
```

(keep the rest of the module doc as it is).

## Tests (in `streams.rs`, `mod tests`)

Each test uses `TestApp::new()` and `app.seed()`, like the existing ones.

1. `the_trace_stream_ends_at_its_maximum_age`: open `open_traces(&app, &seeded, "")`; assert `stream.status == StatusCode::OK`; then `assert!(!stream.body_closed(Duration::from_secs(3)).await, "open before the maximum age");` then `assert!(stream.body_closed(Duration::from_secs(3)).await, "closed after the maximum age");` (`TEST.max_age` is 4 s, so the stream ends between those two waits; `body_closed` reads and discards pings).
2. `the_span_stream_ends_at_its_maximum_age_even_while_spans_arrive`: open the span stream for `TRACE_1` (as in `an_idle_stream_ends`) and record `let opened = tokio::time::Instant::now();`. Then loop at most 20 times: `commit_one(&app, &seeded, TRACE_1, &format!("b1{n:014x}"))` with the loop counter `n`, then `if stream.body_closed(Duration::from_millis(300)).await { break; }`. A span every ~300 ms keeps the 1 s idle end from firing. After the loop, assert that the body closed (keep the `bool`), and that `opened.elapsed() >= Duration::from_millis(3_500)`.
3. `a_client_that_stops_reading_loses_its_stream_and_slot`: take `let owner = StreamOwner::User(user_id(&app).await);`. Open the span stream for `TRACE_1` and **do not read it**. Take the other 99 slots and keep them: `let mut held: Vec<StreamSlot> = (0..99).map(|_| app.state.live.acquire(owner).expect("a slot is free")).collect();` and assert `app.state.live.acquire(owner).is_none()`. Commit 40 spans of `TRACE_1` in **one** `app.commit_spans(&seeded.project, rows)` call (rows built like `commit_one` does, span ids `format!("b1{n:014x}")` for `n` in `0..40`). The task queues 16 events, the 17th send waits, and `TEST.send` (100 ms) ends the task. Then poll a single `app.state.live.acquire(owner)` every 20 ms with a `tokio::time::Instant` deadline 600 ms after the commit, exactly as `dropping_the_stream_frees_its_slot` does; it must return `Some` (push it into `held`) before the deadline. Without the change the task blocks inside the send and never frees its slot. `drop(stream)` last.
4. `the_span_stream_ignores_other_projects`: `let (other, _key) = app.add_project(&seeded, "other", None).await;`. Open the span stream for `TRACE_1` on the seeded project. Commit a span of `TRACE_1` to `other` (`app.commit_spans(&other, vec![row])`, row built as in `commit_one`). Assert `stream.next_non_ping(Duration::from_millis(300)).await.is_none()`. Then `commit_one(&app, &seeded, TRACE_1, "b100000000000009")` and assert the next non-ping event is a `span` whose span id is `b100000000000009`.
5. `the_trace_stream_ignores_other_projects`: same shape with `open_traces(&app, &seeded, "")`: a span of `trace_id(70)` committed to `other` gives no event within 300 ms; then `commit_span(&app, &seeded, &trace_id(71), "b100000000000071", "mine", "production")` gives `next_trace` with `trace_id` `trace_id(71)`.

If an **existing** test in the crate starts failing because a stream now ends after 4 s or after a 100 ms stalled send, stop and report it with the test name; do not loosen the new timings.

## Docs

### `docs/spec/09-api.md`

In the routes table, in the `GET /traces/{trace_id}/stream` row, replace `closes after 10 min idle` with `closes after 10 min idle or 15 min open`.

In the `GET /traces/stream` row, replace `a trace is sent once any of its spans matches, then on every update while the stream stays open (D-C-5)` with `a trace is sent once a span of it that arrives while the stream is open matches, then on every update while the stream stays open (D-C-5, D-G-33)`.

In the same row, replace `An invalid filter or search is a `400` problem before any event (D-G-22)` with `An invalid filter, or a search over 1,024 bytes, is a `400` problem before any event (D-G-22, D-G-39)` (F3 implements the length check; the spec is briefly ahead of the code).

In § SSE, replace the two sentences `Every stream ends when the server shuts down; the span stream also ends after 10 minutes without a span. The trace stream's `search` is checked over the arriving spans' time range (D-G-33).` with:

```
Every stream ends when the server shuts down, 15 minutes after it opened, and when its client has not read for 30 s; the span stream also ends after 10 minutes without a span (D-G-34). The client reconnects, which authenticates again, and refetches. A score write sends no event: an open list or trace shows a new score after the trace's next span or a refetch. The trace stream's `search` is checked over the arriving spans' time range: only spans that arrive while the stream is open can match (D-G-33).
```

### `docs/mvp-decisions.md`, row D-G-34

Replace its sentence `A stream does not re-check access while open.` with:

```
A stream does not re-check access while open, but every stream ends 15 minutes after it opened, so a revoked session or key keeps a stream at most that long; the client reconnects and authenticates again. A stream whose client has not made room for an event within 30 s ends (G review M-2, m-1).
```

## Done when

- `cargo test -p llmobs-server routes::streams` passes, with the five tests above, twice in a row.
- `just check` passes.
