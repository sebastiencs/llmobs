# M3 — Deletion worker

Read [README.md](README.md) first, especially § Design › Lifecycle, Kinds, Evaluation traces, Cascades, Worker, Audit, and Live streams and caches.

**Goal:** the background worker that takes every open `deletions` row to `applied` (or `failed`), for all five kinds:
- resolve the scope of `trace_ids`, `filter` and `user` rows with the M2 reads;
- register them with storage;
- poll storage until each is applied;
- drop the projects of `project` and `org` rows and delete their `meta.db` rows.

Also the deletion guard, the live-stream `resync` event and the metrics-cache invalidation. No route creates rows yet: M4 and M5 add them. Tests insert rows with the M1 store.

**Edit only:**
- `crates/server/src/deletion/mod.rs`, `deletion/resolve.rs`, `deletion/guard.rs` (new);
- `crates/server/src/lib.rs` (declare `mod deletion;`);
- `crates/server/src/http/mod.rs` (`AppInner.deletions`);
- `crates/server/src/serve.rs` (construct, load the guard, spawn the task);
- `crates/server/src/test_support/mod.rs` (construct `AppInner.deletions`);
- `crates/server/src/meta/deletions.rs` (add `mark_dropped`);
- `crates/server/src/meta/projects.rs` (add `delete_project`, `project_ids_of_org`);
- `crates/server/src/meta/orgs.rs` (add `delete_org`);
- `crates/server/src/usage.rs` (add `delete_org_usage`);
- `crates/server/src/live.rs`, `crates/server/src/routes/streams.rs` (resync events);
- `crates/server/src/routes/metrics.rs` (`MetricsCache::clear_project`).

**Commit:** `Add the deletion worker`

## Read first

- `crates/server/src/meta/deletions.rs` (M1): every function, `DeletionRow`, `RequestDetails`.
- `llmobs_storage`:
  - `Storage::{query, register_deletion, deletion_status, drop_project, apply_deletions}`;
  - `Deletion`, `DeletionScope`, `DeletionStatus`, `QueryOptions`, `QueryContext::new`;
  - `filter_trace_ids`, `user_deletion_scope`, `eval_trace_ids`, `FilterDeletionRequest`, `MAX_FILTER_DELETION_TRACES`, `TimeWindow`;
  - `error::{DeletionError, StorageError, QueryError}` and `RunError` (with `RunError::kind()`).
- `crates/server/src/http/mod.rs`: `AppInner`, `AppState`.
- `crates/server/src/serve.rs`: `spawn_usage` and the `tasks: vec![…]` list in `Running`.
- `crates/server/src/test_support/mod.rs`:
  - `TestApp::{new, with_config, seed, add_project, commit_spans, request}`, `Seeded`, `dataset_span`;
  - `capture_logs`, `assert_logs_clean`;
  - the manual clock (`Clock::Manual`, `clock.advance`).
- `crates/server/src/live.rs`, and `crates/server/src/routes/streams.rs` (`run_span_stream`, `run_trace_stream`, `resync()`, and the tests' `open_traces`).
- `crates/server/src/routes/metrics.rs`: `MetricsCache`.
- `crates/server/src/usage.rs`: `Usage::flush`.
- `crates/server/src/meta/projects.rs`: `project_by_id`, `ProjectRow`. `crates/server/src/meta/orgs.rs`: `org_by_id`, `OrgRow`.

## Store additions

`meta/deletions.rs`:

```rust
/// A `pending` project or org deletion that ran: `applied`, `handed_over_at = applied_at = now`.
pub(crate) async fn mark_dropped(conn: &mut SqliteConnection, id: Uuid, now: i64) -> Result<bool, MetaError>;
```
`UPDATE deletions SET status = 'applied', handed_over_at = ?, applied_at = ? WHERE id = ? AND status = 'pending' AND kind IN ('project', 'org')`.

`meta/projects.rs`:

```rust
/// Deletes the project row; foreign keys cascade to its keys, overrides and every
/// project-owned table (D-M-7). A missing row is not an error.
pub(crate) async fn delete_project(conn: &mut SqliteConnection, id: ProjectId) -> Result<(), MetaError>;
```

```rust
/// Every project id of the org, whatever its state, ordered by id.
pub(crate) async fn project_ids_of_org<'e, E>(executor: E, org: OrgId) -> Result<Vec<ProjectId>, MetaError>
where
    E: sqlx::Executor<'e, Database = sqlx::Sqlite>;
```
`SELECT id FROM projects WHERE org_id = ? ORDER BY id`. A bad id is `Corrupt { table: "projects", column: "id" }`. Callers pass `state.meta.reader()` here and `&mut *conn` inside a transaction (M5).

`meta/orgs.rs`:

```rust
/// Deletes the org row; foreign keys cascade to members, invites, connections and projects.
pub(crate) async fn delete_org(conn: &mut SqliteConnection, id: OrgId) -> Result<(), MetaError>;
```

`usage.rs`:

```rust
/// Deletes every `usage_daily` row of the org (org drop, D-M-7).
pub(crate) async fn delete_org_usage(conn: &mut SqliteConnection, org: OrgId) -> Result<(), MetaError>;
```

Each is one `DELETE … WHERE id = ?` (`org_id = ?` for usage), with a `# Errors` doc.

## Live bus and metrics cache

`live.rs`:
- `LiveEvent` gains `pub(crate) resync: bool`, documented "`true`: the project's data changed in a way spans cannot express (a deletion); streams send `resync`. `spans` is empty." `publish` sets it to `false`.
- New method:

```rust
/// Tells the project's open streams to refetch (D-M-11). Builds nothing when no stream is open.
pub(crate) fn publish_resync(&self, project_id: ProjectId) {
    if self.sender.receiver_count() == 0 {
        return;
    }
    let id = self.next_id.fetch_add(1, Ordering::Relaxed).saturating_add(1);
    let _ = self.sender.send(Arc::new(LiveEvent { id, project_id, spans: Vec::new(), resync: true }));
}
```

`routes/streams.rs`, in both `run_span_stream` and `run_trace_stream`: right after the `if event.project_id != project { continue; }` check, add:

```rust
if event.resync {
    if !send(&tx, resync(), timing.send).await {
        return;
    }
    continue;
}
```

`send` is the stream module's existing timeout helper (`async fn send(tx: &mpsc::Sender<Event>, event: Event, timeout: Duration) -> bool`); every send in these loops goes through it so a stalled client cannot hold the stream.

In `run_trace_stream`, put it after `last_id = event.id;`, and clear `pending` before sending, as the lag branch does. Any other code that builds a `LiveEvent` literal must set `resync: false`. Search the crate for `LiveEvent {`.

`routes/metrics.rs`:

```rust
impl MetricsCache {
    /// Drops every cached answer of `project` (D-M-11).
    pub(crate) fn clear_project(&self, project: ProjectId) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries.retain(|key, _| key.project != project);
    }
}
```

## Guard

`deletion/guard.rs`:

```rust
//! Trace ids and end users of recent deletions, so a judge run that started before a
//! deletion does not write a copy of deleted data after its cut-off (D-M-6).
#![cfg_attr(not(test), allow(dead_code))] // `blocks` is called by I's executor from M8 on

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use llmobs_core::ids::{ProjectId, TraceId};

use crate::meta::{MetaDb, MetaError};

/// How long an entry is kept after its deletion's `created_at` (24 h).
pub(crate) const GUARD_NS: i64 = 24 * 60 * 60 * 1_000_000_000;

#[derive(Debug, Default)]
pub(crate) struct DeletionGuard {
    inner: Mutex<GuardState>,
}

#[derive(Debug, Default)]
struct GuardState {
    traces: HashMap<ProjectId, HashMap<TraceId, i64>>,
    users: HashMap<ProjectId, HashMap<String, i64>>,
}

impl DeletionGuard {
    /// Records `traces` of `project` from a deletion created at `created_at`. An existing entry
    /// keeps the later `created_at`.
    pub(crate) fn add_traces(&self, project: ProjectId, traces: &[TraceId], created_at: i64);
    /// Records an end user of `project`.
    pub(crate) fn add_user(&self, project: ProjectId, user_id: &str, created_at: i64);
    /// Whether a judge run about `trace` must not go on (D-M-6). Only entries with
    /// `created_at > now - GUARD_NS` count:
    /// - a trace-id entry for `trace` blocks;
    /// - a user entry for one of `users` blocks only when `newest_ingest_ns <= entry.created_at`
    ///   (the trace's data predates that user's deletion; newer data is not covered, spec S5).
    /// `users` are the distinct user ids of the trace's loaded spans; `newest_ingest_ns` their
    /// largest `ingest_time`. Callers that know neither pass `&[]` and `0`.
    pub(crate) fn blocks(&self, project: ProjectId, trace: TraceId, users: &[&str], newest_ingest_ns: i64, now: i64) -> bool;
    /// Removes entries with `created_at <= now - GUARD_NS`, and empty project maps.
    pub(crate) fn prune(&self, now: i64);
    /// Adds `meta::deletions::guard_entries(now - GUARD_NS)`.
    pub(crate) async fn load(&self, meta: &MetaDb, now: i64) -> Result<(), MetaError>;
}
```

Lock with `.lock().unwrap_or_else(PoisonError::into_inner)`, as `live.rs` does. `load` reads from `meta.reader()` before taking the lock. Never hold the lock across an `.await`.

## Worker

`deletion/mod.rs`:

```rust
//! Deletion requests after creation: resolve, register, poll; project and org drops
//! (spec 03 § 7, D-M-3).

pub(crate) mod guard;
mod resolve;

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::Notify;
use uuid::Uuid;

/// Time between two runs when nothing wakes the worker.
pub(crate) const TICK: Duration = Duration::from_secs(2);
/// Time a failed row waits before its next attempt (60 s).
pub(crate) const RETRY_NS: i64 = 60_000_000_000;
/// Consecutive failures after which a `trace_ids`, `filter` or `user` row fails.
pub(crate) const MAX_ATTEMPTS: u32 = 5;
/// Time allowed to each resolution read.
pub(crate) const RESOLVE_TIMEOUT: Duration = Duration::from_secs(600);

/// Worker state shared through `AppInner.deletions`.
#[derive(Debug)]
pub(crate) struct Deletions {
    /// Notified by every route that creates a deletion. Producers call `notify_one()`
    /// (never `notify_waiters()`), so a wake sent while `run_once` runs is kept for the next wait.
    pub(crate) wake: Notify,
    pub(crate) guard: guard::DeletionGuard,
    attempts: Mutex<HashMap<Uuid, Attempt>>,
    /// `MAX_FILTER_DELETION_TRACES`; tests lower it.
    filter_cap: AtomicUsize,
}

#[derive(Debug, Clone, Copy)]
struct Attempt {
    failures: u32,
    retry_at: i64,
}

impl Deletions {
    pub(crate) fn new() -> Self;   // filter_cap = llmobs_storage::MAX_FILTER_DELETION_TRACES
    /// The filter cap in force (M4's POST count uses it too).
    pub(crate) fn filter_cap(&self) -> usize;
    #[cfg(test)]
    pub(crate) fn set_filter_cap(&self, cap: usize);
    /// Whether `id` may be attempted at `now`.
    fn due(&self, id: Uuid, now: i64) -> bool;
    /// Records a failure; returns the consecutive count.
    fn failed(&self, id: Uuid, now: i64) -> u32;
    /// Forgets `id`'s failures (after a success or a final state).
    fn settled(&self, id: Uuid);
}
```

`Deletions` also implements `Default` (calling `new`), because clippy's `new_without_default` asks for it.

### `run_once`

```rust
/// Processes every open deletion once, oldest first. Errors on one row are logged and
/// counted; they do not stop the others. Returns `Err` only when the open rows cannot be listed.
pub(crate) async fn run_once(state: &AppState) -> Result<(), MetaError> {
    state.deletions.guard.prune(state.clock.now_ns());
    let rows = crate::meta::deletions::open_deletions(state.meta.reader()).await?;
    for row in rows {
        // Per row: a resolution read can take minutes.
        let now = state.clock.now_ns();
        if !state.deletions.due(row.id, now) {
            continue;
        }
        let result = match row.kind {
            DeletionKind::TraceIds | DeletionKind::Filter | DeletionKind::User => data_step(state, &row, now).await,
            DeletionKind::Project => drop_project_step(state, &row, now).await,
            DeletionKind::Org => drop_org_step(state, &row, now).await,
        };
        match result {
            Ok(()) => state.deletions.settled(row.id),
            Err(Step::Retry(label)) => retry(state, &row, now, label, true).await,
            Err(Step::RetryForever(label)) => retry(state, &row, now, label, false).await,
            Err(Step::Fail(reason)) => fail(state, &row, now, reason).await,
        }
    }
    Ok(())
}
```

```rust
/// Why a step stopped.
enum Step {
    /// Try again later; a data row in `resolving` fails after `MAX_ATTEMPTS`.
    /// The label is a static error kind for the log.
    Retry(&'static str),
    /// Try again later, never fail: storage already holds the deletion (any error after
    /// `register_deletion` returned `Ok`), or the row is a drop.
    RetryForever(&'static str),
    /// Give up: the row becomes `failed` (data kinds only).
    Fail(FailReason),
}
```

Conversions into `Step`: private functions in `mod.rs` named `from_meta`, `from_query`, `from_run`, `from_deletion` and `from_storage`, not `From` impls, so each call site shows its choice. `resolve.rs` uses them and `Step` through `super::` (a child module sees the parent's private items).

| Error | `Step` |
|---|---|
| `MetaError` | `Retry("meta")` |
| `QueryError::Busy` | `Retry("busy")` |
| `QueryError::DataFusion(_)` | `Retry("query")` |
| `RunError::Timeout` | `Retry("timeout")` |
| `RunError::Internal(_)` | `Retry("internal")` |
| `RunError::Invalid(_)`, `InvalidRequest(_)`, `Failed(_)`, `TooLarge` | `Fail(ResolutionFailed)` (`Failed` is an execution error in the caller's filter, such as a failed cast, so it will fail again) |
| `DeletionError::Invalid { .. }` | `Fail(ResolutionFailed)` |
| `DeletionError::Catalog(_)` | `Retry("catalog")` |
| `DeletionError::NotFound` | handled where it occurs (poll) |
| `StorageError` (from `drop_project`) | `Retry("storage")` |

The table covers every variant of `RunError` (6), `QueryError` (2) and `DeletionError` (3).

The drop steps return `RetryForever` for every error. So do the data steps for every error that happens after `register_deletion` returned `Ok`: the `mark_handed_over` write and what follows.

`retry(state, row, now, label, may_fail)`:
- `let failures = state.deletions.failed(row.id, now);`
- Log `tracing::warn!(deletion_id = %row.id, kind = row.kind.as_str(), error = label, failures, "deletion step failed")`.
- If `may_fail`, the kind is a data kind, `row.state == DeletionState::Resolving`, `row.trace_count.is_none()` (no scope stored yet, so no cascade ran) and `failures >= MAX_ATTEMPTS`, call `fail(state, row, now, FailReason::ResolutionFailed)`. A row whose scope is stored keeps retrying registration: `failed` must keep meaning "nothing was hidden or deleted". A `Pending` data row (storage holds it) and every drop keep retrying, once a minute.

`fail(state, row, now, reason)`: `meta.write` → `mark_failed(row.id, reason)`.
- `Ok(true)`: log the audit event `tracing::info!(target: "audit", deletion_id = %row.id, error = reason.as_str(), "deletion failed")`, then `state.deletions.settled(row.id)`.
- `Ok(false)` (the row is not `resolving`, e.g. a `pending` row whose registration storage refused): `tracing::warn!(deletion_id = %row.id, error = reason.as_str(), "deletion step failed on a row that cannot fail")` and `state.deletions.failed(row.id, now)`, so it waits a minute; do not call `settled`.
- `Err(error)`: log `error = %error`, call `state.deletions.failed(row.id, now)`; the row stays for later.

### Data kinds: `data_step`

```rust
async fn data_step(state: &AppState, row: &DeletionRow, now: i64) -> Result<(), Step>
```
`row.project_id` is `Some` for these kinds (M1 decoding guarantees it). Bind it with `let Some(project) = row.project_id else { return Err(Step::Fail(FailReason::ResolutionFailed)) };`.

1. **Superseded.** Load `project_by_id(reader, project)`. If it is missing, or its `state != "active"`, or its org (`org_by_id`) is missing or not `active`:
   - `meta.write` → `mark_applied(row.id, now)`;
   - log `deletion applied` (audit, below);
   - return `Ok(())`.
2. For a `User` row, `state.deletions.guard.add_user(project, user_id, row.created_at)` (idempotent; M4 also adds it at creation, and this covers rows left by a previous process or copied by M7). For a `TraceIds` row whose `trace_count` is `None`, add its requested ids (`scope_traces`) with `add_traces` the same way.
3. Then by state:
   - `Resolving` with `trace_count == None`: `if !resolve::resolve(state, row, project).await? { return Ok(()) }`, then `register(state, row, project, now)`. `resolve` returns `Ok(false)` when `store_counts` found the row no longer `resolving`.
   - `Resolving` with `trace_count == Some(_)`, or `Pending` with `handed_over_at == None`: `register(state, row, project, now)`.
   - `Pending` with `handed_over_at == Some(_)`: `poll(state, row, project)`.
   - `Applied` / `Failed`: unreachable from `open_deletions`. Return `Ok(())`.

`resolve::resolve(state, row, project) -> Result<bool, Step>` (in `deletion/resolve.rs`):

1. `let source = state.storage.query(project, QueryOptions { around: None }).await` (`QueryError` → `Step` per the table), then `let ctx = QueryContext::new(source, None)` (`RunError` → `Step`).
2. Targets by kind:
   - `TraceIds`: `crate::meta::deletions::scope_traces(reader, row.id)`. M4 stored the requested ids at creation.
   - `Filter`: take `filter`, `search`, `from`, `to`, `include_eval`, `include_experiments` from `row.request`.
     - `from` or `to` missing → `Step::Fail(ResolutionFailed)`.
     - `TimeWindow::new(from, to)` → on error `Fail(ResolutionFailed)`.
     - Call `filter_trace_ids(&ctx, FilterDeletionRequest { filter: request.filter.as_deref(), search: request.search.as_deref(), window, include_eval: request.include_eval.unwrap_or(false), include_experiments: request.include_experiments.unwrap_or(false), limit: state.deletions.filter_cap() + 1, timeout: RESOLVE_TIMEOUT })`.
     - If more than `filter_cap()` ids come back → `Err(Step::Fail(FailReason::TooManyTraces))`.
   - `User`: `user_deletion_scope(&ctx, user_id, row.created_at, RESOLVE_TIMEOUT)`. Targets are its `trace_ids`; keep its `session_ids`.
3. `let eval = eval_trace_ids(&ctx, &targets.iter().copied().collect::<HashSet<_>>(), RESOLVE_TIMEOUT)`.
4. `scope = targets ∪ eval` (a `Vec`, sorted, deduped).
5. One `meta.write`:
   - `insert_traces(row.id, &scope)`;
   - `insert_sessions(row.id, &sessions)` (empty for non-user kinds);
   - `store_counts(row.id, count(scope.len()), session_count)`. `session_count` is `Some(count(sessions.len()))` for `User` and `None` otherwise. `fn count(n: usize) -> u64 { u64::try_from(n).unwrap_or(u64::MAX) }` (cannot saturate on 64-bit targets).
   - Return the `store_counts` result: `false` means the row is no longer `resolving` (commit anyway; nothing else was written).
   - M8 adds the `meta.db` cascade call here, in this same transaction.

`register(state, row, project, now) -> Result<(), Step>` (in `mod.rs`):

1. `traces = scope_traces(reader, row.id)`; for `User`, `sessions = scope_sessions(reader, row.id)`.
2. If the kind is `TraceIds` or `Filter` and `traces` is empty:
   - `mark_applied(row.id, now)`;
   - log `deletion applied`;
   - return. Storage refuses an empty `Traces` scope.

   A `User` row always registers: its `user_id` covers spans even with no trace.
3. `state.deletions.guard.add_traces(project, &traces, row.created_at)`.
4. Build `llmobs_storage::Deletion { id: row.id, project, cutoff: row.created_at, scope }`:
   - `TraceIds` and `Filter`: `scope = DeletionScope::Traces { trace_ids: traces }`;
   - `User`: `scope = DeletionScope::User { user_id, trace_ids: traces, session_ids: sessions }`.
5. `state.storage.register_deletion(deletion).await`. `DeletionError` → `Step` per the table.
6. `meta.write` → `mark_handed_over(row.id, now)`. From here on, every error is `Step::RetryForever`: storage already hides the rows.
7. `state.live.publish_resync(project)` and `state.metrics_cache.clear_project(project)`.
8. Audit: `tracing::info!(target: "audit", deletion_id = %row.id, trace_count = traces.len(), session_count = sessions.len(), "deletion registered")`. `sessions` is empty for non-user kinds. (The `row` snapshot's counts may still be `None`, so log the lengths.)

Known races, accepted:
- A `GET /metrics` that read before the tombstone can `put` its answer just after `clear_project`; it lives at most 10 s.
- A commit already past its project check can `usage.add` after the org drop's `flush`; the 10 s usage flusher then writes a `usage_daily` row for the deleted org. That row holds counts only.

`poll(state, row, project) -> Result<(), Step>`:
- `Ok(DeletionStatus::Pending)` → `Ok(())`.
- `Ok(DeletionStatus::Applied { applied_at })` → `mark_applied(row.id, applied_at)`, then log `deletion applied`.
- `Err(DeletionError::NotFound)` → `clear_handover(row.id)`, then `tracing::warn!(deletion_id = %row.id, "storage does not know a handed-over deletion; registering it again")`, then `Ok(())`. The next tick registers it.
- Other errors → `Step` per the table.

Audit for applied, in every place above: `tracing::info!(target: "audit", deletion_id = %row.id, "deletion applied")`. Log it only when `mark_applied` / `mark_dropped` returned `true`.

### Drops

`drop_project_step(state, row, now)`:
1. `let Some(project) = row.project_id`; `None` → log an error with the id and return `Err(Step::RetryForever("corrupt"))`. It cannot happen after M1's decoding.
2. Only `Pending` rows with `handed_over_at == None` reach here. Any other state returns `Ok(())`.
3. `state.storage.drop_project(project).await` → on error `Err(Step::RetryForever("storage"))`. `drop_project` is idempotent (spec 03 § 7).
4. One `meta.write`: `crate::meta::projects::delete_project(conn, project)`, then `mark_dropped(conn, row.id, now)`.
5. `state.live.publish_resync(project)`; `state.metrics_cache.clear_project(project)`.
6. When `mark_dropped` returned `true`: audit `deletion applied`, then `on_dropped(state, &Dropped { deletion_id: row.id, org_id: row.org_id, projects: vec![project], whole_org: false })`.

`drop_org_step(state, row, now)`:
1. `projects = scope_projects(reader, row.id)` ∪ `project_ids_of_org(reader, row.org_id)` (sorted, deduped). The second list catches a project created by a request that passed the access check just before the org became `deleting` (D-M-7).
2. For each project, `drop_project`. On the first error, return `Err(Step::RetryForever("storage"))`. The next attempt starts again; dropped ones are no-ops.
3. `state.usage.flush(&state.meta).await` → on error `RetryForever("meta")`. Usage counted in memory for these projects must not land in `usage_daily` after the rows are deleted.
4. One `meta.write`: `crate::usage::delete_org_usage(conn, row.org_id)`, then `crate::meta::orgs::delete_org(conn, row.org_id)`, then `mark_dropped(conn, row.id, now)`.
5. For each project: `publish_resync`, `clear_project`.
6. When `mark_dropped` returned `true`: audit `deletion applied`, then `on_dropped(state, &Dropped { deletion_id: row.id, org_id: row.org_id, projects, whole_org: true })`.

### The `dropped` hook (D-M-17)

```rust
/// A project or org whose data and `meta.db` rows are gone.
#[derive(Debug, Clone)]
pub(crate) struct Dropped {
    pub(crate) deletion_id: Uuid,
    pub(crate) org_id: OrgId,
    /// The dropped projects (one for a project drop; every project of the org for an org drop).
    pub(crate) projects: Vec<ProjectId>,
    /// The org itself is gone.
    pub(crate) whole_org: bool,
}

/// Called exactly once per drop, after its `meta.db` transaction committed (D-M-17).
/// Track Q adds the control-plane notification here (org → cell mapping, D170), through
/// its own outbox. Today it records the audit event.
pub(crate) fn on_dropped(_state: &AppState, dropped: &Dropped) {
    if dropped.whole_org {
        tracing::info!(target: "audit", deletion_id = %dropped.deletion_id, org_id = %dropped.org_id, projects = dropped.projects.len(), "org dropped");
    } else {
        for project in &dropped.projects {
            tracing::info!(target: "audit", deletion_id = %dropped.deletion_id, org_id = %dropped.org_id, project_id = %project, "project dropped");
        }
    }
}
```

`_state` is unused until Q; the leading underscore keeps clippy quiet. Keep the parameter: Q needs the state, and changing every caller later would be churn.

The `deletions` rows of those projects and of the org stay (no foreign keys, M1).

## Wiring

- `lib.rs`: `mod deletion;` in alphabetical order. Check how the other modules are declared (`pub(crate) mod` or `mod`) and do the same.
- `http/mod.rs`: add to `AppInner`, after `usage`:

  ```rust
  /// Deletion worker state: wake-up, guard, attempts (D-M-3).
  pub(crate) deletions: crate::deletion::Deletions,
  ```
- `serve.rs`, `test_support/mod.rs`: construct `deletions: crate::deletion::Deletions::new(),`.
- `serve.rs`:
  - After `crate::bootstrap::prepare(&state)` succeeds, call `state.deletions.guard.load(&state.meta, state.clock.now_ns()).await`. On error, log `tracing::error!(error = %error, "deletion guard load failed")` and continue. The guard is a safety net; the server must still start.
  - Add `spawn_deletions(state.clone())` to `tasks`, before `spawn_usage(state)`:

  ```rust
  fn spawn_deletions(state: AppState) -> JoinHandle<()> {
      tokio::spawn(async move {
          loop {
              if let Err(error) = crate::deletion::run_once(&state).await {
                  tracing::error!(error = %error, "deletion run failed");
              }
              tokio::select! {
                  () = state.deletions.wake.notified() => {}
                  () = tokio::time::sleep(crate::deletion::TICK) => {}
              }
          }
      })
  }
  ```

  The first run happens at startup and handles every row left open by a previous process.

## Tests

`deletion/mod.rs` `#[cfg(test)] mod tests` (worker), `deletion/guard.rs` tests (guard), and one test each in `live.rs`, `routes/streams.rs`, `routes/metrics.rs`.

Test helpers in `deletion/mod.rs` tests:

```rust
/// A span of trace `trace` (32 hex), span `span` (16 hex), with `name`, committed to `project`.
async fn commit(app: &TestApp, project: &ProjectRow, trace: &str, span: &str, edit: impl FnOnce(&mut Span));
/// Inserts a deletion row with `NewDeletion { id: Uuid::now_v7(), created_at: app.state.clock.now_ns(), … }`.
async fn insert(app: &TestApp, new: NewDeletion, traces: &[TraceId]) -> Uuid;
/// `GET /api/v1/projects/{project}/traces/{trace}` with the seeded cookie; returns the status.
async fn trace_status(app: &TestApp, seeded: &Seeded, project: ProjectId, trace: &str) -> StatusCode;
```

To build a span, take `dataset_span(0)`, then set `trace_id`, `span_id`, `parent_span_id = None`, `name`, `user_id`, `session_id`, `environment` and `metadata` as the test needs, **and `ingest_time = app.state.clock.now_ns()`**. `commit_spans` passes spans through unchanged: without that line the span keeps the fixture's `ingest_time` (2026-01-01T10:00Z), which is after the manual clock's `now`, so no deletion would cover it. `commit_spans` sets `project_id`. Give each test its own trace ids, `a2…` plus 30 hex digits. Then advance the manual clock by 1 s (`app.state.clock.advance(Duration::from_secs(1))`) before inserting the deletion, so `ingest_time < cutoff`.

1. `trace_ids_are_hidden_once_registered`:
   - Two traces, A and B. A `trace_ids` row whose scope is A.
   - `run_once` → the row is `pending` with `trace_count == Some(1)` and `handed_over_at == Some(now)`.
   - `trace_status(A) == 404` and `trace_status(B) == 200`.
   - `storage.deletion_status(id) == Pending`.
2. `eval_traces_join_the_scope`:
   - Trace A, plus trace E with `environment = Some("llmobs-eval")` and `metadata[EVAL_TARGET_TRACE_ID] = A`'s hex.
   - `trace_status(E) == 200` first. Then a `trace_ids` row for A, then `run_once`.
   - `trace_count == Some(2)`, `scope_traces` is `[A, E]` sorted, and `trace_status(E) == 404`.
3. `filter_resolves_matching_traces`:
   - Traces A (`name = "keep"`) and B (`name = "drop"`).
   - A `filter` row with `filter: Some("name = 'drop'")`, `from: Some(0)`, `to: Some(i64::MAX)`.
   - After `run_once`, the scope is `[B]`, and A is still 200.
4. `filter_with_no_match_is_applied_at_once`: filter `name = 'nothing'` → `applied`, `trace_count == Some(0)`, `handed_over_at == None`, and `storage.deletion_status(id)` is `Err(DeletionError::NotFound)`.
5. `filter_over_the_cap_fails`: `app.state.deletions.set_filter_cap(1)`, two matching traces → `failed` with `error == Some(TooManyTraces)`; both traces are still 200.
6. `invalid_filter_fails`: `filter: Some("name = ")` → `failed` with `ResolutionFailed` after **one** run.
7. `user_resolves_traces_and_sessions`:
   - Trace A has a span of `u1` with session `s1`. Trace B has a span of `u1` with no session. Trace C has a span of `u2`.
   - A `user` row for `u1`. After `run_once`: `trace_count == Some(2)`, `session_count == Some(1)`, `scope_sessions == ["s1"]`.
   - A and B are 404, C is 200.
   - `GET /api/v1/projects/{p}/users/u1` is 404.
8. `registered_scope_is_reused_after_a_restart`:
   - A `trace_ids` row whose scope is trace X, which does not exist in storage.
   - `store_counts(id, 1, None)` already done, as if a previous process had resolved it.
   - `run_once` → `pending`, and `storage.deletion_status(id) == Pending`.
9. `lost_hand_over_registers_again`:
   - A `trace_ids` row for trace A, moved to `pending` with `mark_handed_over` **without** registering it in storage.
   - `run_once` → `handed_over_at == None`. A second `run_once` → `handed_over_at` is set, and `storage.deletion_status == Pending`.
10. `reaches_applied_after_flush`:
    - `TestApp::with_config(|config| config.storage.flush.max_age = Duration::from_millis(100))` (storage refuses less than 100 ms), one trace A, a `trace_ids` row, `run_once`.
    - Then loop at most 100 times: `app.state.storage.apply_deletions(wall_now_ns()).await.unwrap()` with the real wall clock (`SystemTime::now()` since the epoch, in ns; storage registers deletions and runs its passes on wall time, not on the server's manual clock), then `run_once`, then stop when the row is `applied`, else sleep 100 ms.
    - The row ends `applied` with `applied_at` set.
    - Check the field path to `flush.max_age` in `config.rs` (`ServerConfig` → `storage: StorageConfig` → `flush: FlushConfig`).
11. `superseded_when_the_project_is_not_active`: `UPDATE projects SET state = 'deleting'` on the seeded project, then a `user` row → after `run_once`, it is `applied` and `storage.deletion_status(id)` is `NotFound`.
12. `project_drop_removes_the_project`:
    - `app.add_project(&seeded, "doomed", None)` gives a project and its ingest key. Commit trace A to it, and trace S to the seeded project.
    - A `project` row (`Pending`, `request.name`/`slug`) → `run_once`.
    - The row is `applied` with `handed_over_at == applied_at == Some(now)`.
    - `project_by_id` is `None`; `SELECT COUNT(*) FROM api_keys WHERE project_id = ?` is 0.
    - Appending to it fails: `app.state.storage.append_spans(project, batch)` with `batch = llmobs_core::batch::spans_to_record_batch(&[span]).unwrap()` (as `test_support/mod.rs` imports it; do not use `commit_spans`, which unwraps) returns `Err(AppendError::ProjectDropped)`.
    - The seeded project is untouched: `trace_status(seeded, S) == 200`. (`trace_status(A)` is not checked: the project route is 404 once the row is gone, which proves nothing about storage.)
13a. `drop_hook_logs_once`: wrap test 12's `run_once` in `capture_logs`; exactly one log line contains both `audit` and `project dropped` (storage logs its own non-audit `project dropped` line too; `capture_logs` prints targets, so filter on `audit`). A second `run_once` adds nothing (the row is applied).
13b. `org_drop_includes_projects_created_late`: an org row whose `deletion_projects` lists only project P1, while the org also has P2 (inserted after the row, as a racing `create_project` would). `run_once` drops both: P2's row is gone and appending to P2 fails with `ProjectDropped`.
13. `org_drop_removes_the_org_and_its_usage`:
    - Seed. Create a second org with `insert_org` and two projects with `insert_project`, then commit one span to each project and one to the seeded project.
    - `app.state.usage.flush(&app.state.meta).await.unwrap()` makes `usage_daily` hold rows for both orgs.
    - An `org` row (`project_id: None`) with `insert_projects` for both projects → `run_once`.
    - The row is `applied`. The second org, its two projects and its `usage_daily` rows are gone. The seeded org's `usage_daily` row is still there.
14. `failed_steps_wait_and_then_fail` (unit test of `Deletions` only, no app):
    - `due(id, 0)` is true. `failed(id, 0)` returns 1. `due(id, RETRY_NS - 1)` is false and `due(id, RETRY_NS)` is true.
    - Four more `failed` calls return 2…5. `settled(id)` makes `due(id, 0)` true again.
15. `audit_logs_carry_no_scope`:
    - A span of `user-secret-7` in trace `a2…ff` with session `session-secret-9`, and a `user` row for it.
    - `let ((), logs) = capture_logs(run_once(&app.state).map(|r| r.unwrap())).await;` (adapt to `capture_logs`'s signature). Assert that some line contains both `audit` and `deletion registered` (storage logs a non-audit `deletion registered` too), and that `logs` contains none of `user-secret-7`, `session-secret-9` and the trace hex. A needle that shows up through a dependency's debug event is a real leak: find and fix it.

`deletion/guard.rs` tests:

16. `blocks_known_traces_and_users_for_a_day`:
    - `add_traces(P, [T], 100)` and `add_user(P, "u", 100)`.
    - `blocks(P, T, &[], 0, 101)` is true. `blocks(P, T2, &["u"], 100, 101)` is true (data ingested at the cut-off).
    - `blocks(P, T2, &["u"], 101, 102)` is false: that trace's data is newer than the deletion.
    - `blocks(P2, T, &[], 0, 101)` is false. `blocks(P, T2, &[], 0, 101)` is false.
    - `blocks(P, T, &[], 0, 100 + GUARD_NS)` is false: expired.
17. `prune_drops_expired_entries`: after `prune(100 + GUARD_NS)`, the internal maps are empty. Check this through `blocks` and a `#[cfg(test)] fn len(&self) -> usize` (total entries).
18. `load_reads_recent_deletions`: using `TestApp::new()`, insert a `user` row at `now` with scope `[T]` and a `trace_ids` row at `now - GUARD_NS - 1` with scope `[T2]`. `guard.load(&meta, now)` → `blocks(P, T, &[], 0, now)` and `blocks(P, X, &["user-1"], now - 1, now)` are true, and `blocks(P, T2, &[], 0, now)` is false.

Other modules:

19. `live.rs`: `publish_resync_reaches_subscribers`. Subscribe, then `publish_resync(p)`. The received event has `resync == true`, empty `spans` and `project_id == p`. With no subscriber, `publish_resync` does not move `last_id()`.
20. `routes/streams.rs`, in its tests module:
    - `a_deletion_resyncs_the_trace_list_stream`: open the traces stream with `open_traces(&app, &seeded, "")`, then `app.state.live.publish_resync(seeded.project.id)`. `next_non_ping(Duration::from_secs(2))` returns an event named `resync`.
    - `a_resync_for_another_project_is_ignored`: publish for another project id. No `resync` arrives within 300 ms (`next_non_ping` returns `None`).
21. `routes/metrics.rs`: `clear_project_drops_only_that_project`. Build two `MetricsKey` literals that differ only by `project` (P, Q; copy the other field values from an existing test in the module, `bucket` included) and one `MetricsResponse` (build it the way the existing metrics tests do, e.g. `serde_json::from_value`). `put` both, `clear_project(P)`; `get(P key, now)` is `None` and `get(Q key, now)` is `Some`.

## Done when

- [ ] All five kinds go from open to `applied` (or `failed`) through `run_once`, as § Worker says.
- [ ] `serve.rs` loads the guard and spawns the worker; `TestApp` constructs `Deletions`.
- [ ] Logs carry no scope value (test 15).
- [ ] `on_dropped` exists and is called once per drop.
- [ ] The 24 tests pass; `just check` is green; one commit `Add the deletion worker`.
- [ ] Report any deviation from this file.
