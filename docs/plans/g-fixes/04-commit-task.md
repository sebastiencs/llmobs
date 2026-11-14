# F4 — Finish an ingest commit even when the client goes away (M-4)

Read [README.md](README.md) first. Independent of the other phases.

**Goal:** an OTLP request (HTTP or gRPC) whose client disconnects while its spans are being appended still publishes them to the live bus and counts them in usage. Today hyper drops the handler future when the client goes away; if that happens while `Ingest::commit` awaits `append_spans`, the append may finish inside storage while `live.publish` and `usage.add` never run. The fix runs the commit in its own task, which the handler awaits, and shutdown waits for such tasks before the final usage flush.

**Edit only:**
- `crates/server/src/ingest/mod.rs`;
- `crates/server/src/routes/otlp.rs`;
- `crates/server/src/ingest/grpc.rs`;
- `crates/server/src/serve.rs`;
- `docs/mvp-decisions.md` (new row D-G-38).

**Commit:** `Finish an ingest commit even when the client goes away`

## Background

- `crates/server/src/ingest/mod.rs`:
  - `pub(crate) struct Ingest { prices, budget: Arc<Semaphore>, max_request_bytes, max_spans_per_request, limits }`, built by `Ingest::new(config, media, prices)`. Its doc says it holds no `AppState`.
  - `pub(crate) struct Reservation(OwnedSemaphorePermit);` — the request's budget.
  - `pub(crate) async fn commit(&self, storage: &Storage, usage: &Usage, live: &LiveBus, target: CommitTarget, normalized: Normalized) -> Result<CommitReceipt, IngestError>`: stores media, appends spans, then `live.publish(…)`, then `usage.add(…)`.
  - `IngestError::Join(tokio::task::JoinError)` exists (`"ingest task failed"`).
  - Tests (`mod tests`) have the helpers `target(&app, &project) -> CommitTarget`, `accepted_fixture(&app, &project, "llmobs/<case>/input.json") -> Normalized`, `stored_spans(&app, project_id) -> usize`.
- `crates/server/src/routes/otlp.rs` (HTTP) and `crates/server/src/ingest/grpc.rs` (`async fn export_request(state: &AppState, …)`) both do:

  ```rust
      let receipt = state
          .ingest
          .commit(
              &state.storage,
              &state.usage,
              &state.live,
              target,
              normalized,
          )
          .await?;
      // The budget covers the spans until they are appended (D-G-5).
      drop(reservation);
  ```

  where `reservation: Reservation` came out of `Accepted`.
- `AppState` (`crate::http::AppState`) is `Clone` (an `Arc`), with fields `storage: Arc<Storage>`, `usage: Usage`, `live: LiveBus`, `ingest: Ingest`.
- `crates/server/src/serve.rs`, `impl Running { pub async fn shutdown(mut self) }`: closes the live bus, signals the listeners, drains them (`tokio::join!(drain_optional(…), …)`), aborts `self.tasks`, then inside `if let Some(state) = self.state.take() {` flushes `last_used`, `token_last_used`, `usage`, then shuts storage down within `STORAGE_SHUTDOWN`.

## Changes

### 1. `ingest/mod.rs`: count running commits

Add `use std::sync::atomic::{AtomicUsize, Ordering};` and `use tokio::sync::Notify;` (extend the existing `tokio::sync` import).

Add, after the `Reservation` struct:

```rust
/// Commits running in their own task, so shutdown can wait for them (D-G-38).
#[derive(Debug, Default)]
struct Commits {
    running: AtomicUsize,
    done: Notify,
}

/// One running commit. Dropping it wakes [`Ingest::wait_for_commits`] when it was the last.
struct CommitGuard(Arc<Commits>);

impl Commits {
    fn enter(self: &Arc<Self>) -> CommitGuard {
        self.running.fetch_add(1, Ordering::SeqCst);
        CommitGuard(Arc::clone(self))
    }

    async fn idle(&self) {
        loop {
            let notified = self.done.notified();
            tokio::pin!(notified);
            // Registered before the check, so a guard dropped in between still wakes it.
            notified.as_mut().enable();
            if self.running.load(Ordering::SeqCst) == 0 {
                return;
            }
            notified.await;
        }
    }
}

impl Drop for CommitGuard {
    fn drop(&mut self) {
        if self.0.running.fetch_sub(1, Ordering::SeqCst) == 1 {
            self.0.done.notify_waiters();
        }
    }
}
```

Add the field `commits: Arc<Commits>,` to `Ingest` (last field) and `commits: Arc::default(),` to the struct literal in `Ingest::new`.

Add to `impl Ingest`:

```rust
    /// Waits until no commit started by [`commit_in_task`] is running (D-G-38).
    pub(crate) async fn wait_for_commits(&self) {
        self.commits.idle().await;
    }
```

### 2. `ingest/mod.rs`: the task

Add, after `impl Ingest` (a free function, so `Ingest` itself still holds no `AppState`):

```rust
/// Runs [`Ingest::commit`] in its own task and waits for it (D-G-38). When the request is
/// dropped while waiting (the client went away), the task still appends, publishes and counts
/// the spans. `reservation` moves into the task: the budget covers the spans until they are
/// appended (D-G-5).
pub(crate) async fn commit_in_task(
    state: &AppState,
    target: CommitTarget,
    normalized: Normalized,
    reservation: Reservation,
) -> Result<CommitReceipt, IngestError> {
    let guard = state.ingest.commits.enter();
    let state = state.clone();
    let task = tokio::spawn(async move {
        let _guard = guard;
        let result = state
            .ingest
            .commit(&state.storage, &state.usage, &state.live, target, normalized)
            .await;
        drop(reservation);
        result
    });
    task.await.map_err(IngestError::Join)?
}
```

Import `crate::http::AppState`. The existing `Ingest::commit` stays as it is (its tests call it directly).

### 3. Callers

In `routes/otlp.rs` and `ingest/grpc.rs`, replace the block quoted in Background (from `let receipt = state` to `drop(reservation);`, including the D-G-5 comment) with:

```rust
    let receipt = commit_in_task(state, target, normalized, reservation).await?;
```

In `grpc.rs` import it with `use super::commit_in_task;` and call `commit_in_task(state, …)`; in `otlp.rs` add `commit_in_task` to the existing `use crate::ingest::{…}` list. `state` is already a `&AppState` in both functions.

### 4. `serve.rs`: wait before the final flush

In `Running::shutdown`, make this the first statement inside `if let Some(state) = self.state.take() {`:

```rust
            // Commits whose request was dropped still run (D-G-38): they append and count
            // before the final flushes.
            if tokio::time::timeout(STORAGE_SHUTDOWN, state.ingest.wait_for_commits())
                .await
                .is_err()
            {
                tracing::error!("ingest commits did not finish before shutdown");
            }
```

## Tests (`ingest/mod.rs`, `mod tests`)

1. `a_commit_finishes_after_its_request_is_dropped` (`#[tokio::test(flavor = "multi_thread")]`):
   - `let app = TestApp::new().await; let seeded = app.seed().await;`
   - `let normalized = accepted_fixture(&app, &seeded.project, "llmobs/sdk-observe-nested/input.json").await; let spans = normalized.spans.len(); assert!(spans > 0);`
   - `let mut bus = app.state.live.subscribe();` (a receiver must exist, or `publish` builds nothing).
   - `let reservation = app.state.ingest.reserve_exact(1).expect("budget");`
   - Poll the commit **once** and drop it, as a disconnecting client does: `let _polled = tokio::time::timeout(Duration::ZERO, commit_in_task(&app.state, target(&app, &seeded.project), normalized, reservation)).await;` (tokio's `timeout` polls the future before checking the deadline; keep the `_polled` binding, so clippy sees no unused value, and do not assert on it).
   - `tokio::time::timeout(Duration::from_secs(10), app.state.ingest.wait_for_commits()).await.expect("the commit finishes");`
   - Assert `stored_spans(&app, seeded.project.id).await == spans`.
   - Assert the bus got the spans: `let event = bus.try_recv().expect("an event was published"); assert_eq!(event.spans.len(), spans);`.
   - Flush usage and read it: `app.state.usage.flush(&app.state.meta).await.unwrap();` then `let counted: i64 = sqlx::query_scalar("SELECT spans FROM usage_daily WHERE project_id = ?").bind(seeded.project.id.to_string()).fetch_one(app.state.meta.reader()).await.unwrap(); assert_eq!(counted, i64::try_from(spans).unwrap());`

   Without the change (calling `app.state.ingest.commit(…)` the same way), the single poll stops at the first await and nothing is stored: confirm this once by temporarily swapping the call, then restore it.
2. `wait_for_commits_returns_at_once_when_idle`: `tokio::time::timeout(Duration::from_millis(100), app.state.ingest.wait_for_commits()).await.expect("idle");` on a fresh `TestApp`.

Add `use std::time::Duration;` to the test module if it is missing.

## Docs

### `docs/mvp-decisions.md`, section `## Track G — Integration spine`

Add right after the D-G-37 row:

```
| D-G-38 | Commit task | **An OTLP request's commit (media, append, live publish, usage counters) runs in its own task, which the handler awaits.** A client that disconnects mid-commit drops the handler, not the commit, so stored spans are always published and counted. The request's budget reservation moves into the task. Shutdown waits for these tasks (bounded by the storage shutdown timeout) before the final usage flush (G review M-4). |
```

## Done when

- `cargo test -p llmobs-server ingest` passes, with the two tests above.
- `cargo test -p llmobs-server routes::otlp` and `cargo test -p llmobs-server ingest::grpc` pass unchanged.
- `just check` passes.
