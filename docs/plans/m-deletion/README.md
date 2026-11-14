# M — Deletion and retention

Implementation plan for track M: GDPR deletion requests (by trace ids, by filter, by end user, by project, by org) and per-project retention, end to end. That covers the API, the background worker that hands requests to storage, the cascades in `meta.db`, the audit trail, re-applying deletions after a backup restore, and the UI. It has 12 sub-phases, M1 to M12. One agent (Haiku) does each sub-phase in one go, in order, and nothing outside this README and its own sub-phase file. Each sub-phase ends with `just check` green and one commit.

**Why a plan and not a design doc (D-M-1).** Storage already does the hard part (A1 M5):
- tombstones, so pending rows are hidden;
- the deletion rewriter, compaction and the applied status;
- retention watermarks;
- `drop_project`;
- media GC.

What is left is server work: resolving which traces a request covers, a status lifecycle, `meta.db` cascades, routes and pages. It fits in § Design below.

**Prerequisites** (checked when this plan was written, 2026-11-14):
- A1 M5 (deletion, retention, project drop) and G M1–M5 are merged into `main`. L is implemented.
- H (console) is merged. I (online evaluation) is being implemented; its M1 (connections, migration `0004_connections.sql`) is merged. M1–M7 and M9–M12 do not depend on I. **M8 needs I's M3 merged** (the `eval_jobs` table and the job executor). If it has not merged when you reach M8, stop and report.
- Track P (plans and entitlements, D179) is not built. M leaves two call sites for it (§ Plans and cloud). Track Q (cloud control plane, D170) is not built either; M leaves one hook for it.
- All of M is MIT core code (D172): nothing goes under `ee/`.
- Node `26.13.0` is installed at `$HOME/.local/share/nvm/v26.13.0/bin`. It is not on the non-interactive `PATH`.

## Read first (every sub-phase)

- `AGENTS.md`, all of it.
- This README, all of it. Then read your own sub-phase file. Do not read the other sub-phase files unless yours says so.
- `docs/spec/03-storage.md` § 7 Deletion (GDPR) and § 8 Retention.
- `docs/spec/04-metadata-db.md` § Deletion requests.
- `docs/spec/09-api.md` § Conventions and § Pricing & GDPR.
- `docs/spec/08-auth.md` § Authorization (RBAC).
- `docs/mvp-decisions.md`:
  - D13, D24, D83 and D-A1-3 to D-A1-7;
  - every `D-M-n` (§ Track M — Deletion and retention).
- Your sub-phase file lists the code to read.

## Scope

**In:**
- `meta.db` tables `deletions`, `deletion_traces`, `deletion_sessions`, `deletion_projects` (M1).
- Three storage reads that resolve a request's scope: traces matching a filter, one end user's traces and sessions, and the evaluation traces that target a set of traces (M2).
- The deletion worker: resolve, register with storage, poll until applied, and the cascades. Also the deletion guard, the live-stream `resync` and the metrics-cache invalidation (M3).
- Routes:
  - `POST /api/v1/projects/{project}/deletions` (kinds `trace_ids`, `filter`, `user`);
  - `POST …/deletions/preview`;
  - `GET …/deletions` and `GET …/deletions/{id}` (M4).
- `DELETE /api/v1/projects/{project}`, `DELETE /api/v1/orgs/{org}` and `GET /api/v1/orgs/{org}/deletions`, plus the worker's project and org kinds (M5).
- Retention: an hourly `apply_retention` for every project, and an immediate one when a `PATCH` changes retention (M6).
- `llmobs admin reapply-deletions --from <data_dir>` (M7).
- The evaluation cascade: delete `eval_jobs` of deleted traces, and the guard in I's executor (M8).
- Web:
  - the project settings **Data** tab, with deletion by user id and the deletion history (M9);
  - "Delete matching traces" on the traces list, "Delete trace" in the trace header, "Delete user data" on the user page (M10);
  - "Delete project" and "Delete organization" (M11).
- An end-to-end test and the roadmap closeout (M12).
- The hooks tracks P and Q need: a retention cap per org (D175), a retention-management entitlement check (D177), and a "dropped" notification point for the control plane (D170). M places the calls; P and Q fill them (§ Plans and cloud).

**Out:**
- Deleting instance user accounts (people who log in). That is account management, not end-user data. D149 lets instance admins disable accounts.
- Personal data typed by project members into saved views, saved SQL queries, rule filters or alert names. Those are texts written by the customer's own staff, not ingested data (D-M-14).
- Datasets and experiment items copied from traces (D24). Track J creates those tables and adds its own cascade to `deletion::cascade` (§ Coordination).
- Alert history: `alert_events` hold aggregates only and are kept (spec 03 § Cascades). Track K's tables cascade with the project row (§ Coordination).
- Plan lookup, the data-access window that hides old data from reads, and the upsell copy beyond one message: track P (D175, D177, D179). The control-plane side of org and project deletion: track Q (D170).
- `Idempotency-Key` on the new POSTs. Spec 09 lists it for every create, but no route implements it yet; it stays a cross-track gap (§ API contract gaps).

## Design

### Lifecycle (D-M-3)

Every request is a row in `meta.db` `deletions`. `POST` answers `202` with that row. A background worker does the rest:

```
             POST (trace_ids | filter | user)                   DELETE project | org
                         │                                              │
                         ▼                                              ▼
                    resolving ──(scope stored, register_deletion)──▶ pending ──(storage: applied)──▶ applied
                         │                                              │   (project/org: drop_project, rows deleted)
                         └──(too_many_traces | resolution_failed)──▶ failed
```

- **`resolving`**: the row exists, `trace_count` is NULL. For `trace_ids` the requested ids are already in `deletion_traces`. The worker resolves the scope:
  - the filter's traces;
  - the user's traces and sessions;
  - plus, for every kind, the evaluation traces that target those traces.

  It stores the scope and `trace_count`/`session_count` in one transaction, together with the `meta.db` cascades. Then it calls `Storage::register_deletion`. Rows stay visible while a request is `resolving` (D-M-3, amending D13's "immediately": normally seconds).
- **`pending`**: `register_deletion` returned, `handed_over_at` is set. Storage hides the covered rows from every read (tombstones). The worker polls `Storage::deletion_status` every tick.
- **`applied`**: storage reported the deletion applied (spec 03 § 7: bytes gone from Parquet files within `storage.deletion.max_delay`, 24 h by default). `applied_at` is set.
- **`failed`**: resolution gave up. `error` is `too_many_traces` (a filter matched more than 100,000 traces at resolution time) or `resolution_failed` (5 failed attempts in a row, or an error that cannot succeed on retry). Nothing was hidden or deleted. The caller may submit a new request.
- **Project and org kinds** are created in `pending` with `handed_over_at` NULL. The project or org is set to `state = 'deleting'` in the same transaction, so every route answers 404 for it at once and ingest is refused. The worker calls `drop_project` (once per project), deletes the `meta.db` rows (foreign-key cascades), and sets `applied` in one step. These kinds never fail. A storage error is retried every minute, forever, and logged.
- **Restart**: the worker's first tick handles every open row:
  - a `resolving` row with `trace_count` NULL is resolved again;
  - a `resolving` row with `trace_count` set is registered with its stored scope;
  - a `pending` row with `handed_over_at` NULL is registered or dropped;
  - a `pending` row that was handed over is polled.

  Registration is idempotent by id (spec 03 § 7), so a crash between `register_deletion` and the `pending` write is safe.
- **Superseded**: a `trace_ids`, `filter` or `user` row whose project is no longer `active` (being dropped, or gone) is set `applied` with `applied_at = now` and no storage call. The drop erases everything.
- **Cut-off**: `created_at`, taken from `state.clock` when the request is accepted. It is passed as `Deletion::cutoff`. Rows ingested after it are new data and are not deleted (spec README S5).

### Kinds (D-M-2)

| `kind` | Created by | Scope stored | Storage call |
|---|---|---|---|
| `trace_ids` | `POST …/deletions` `{kind, trace_ids}` (1–1,000 ids) | `deletion_traces`: the requested ids, plus their evaluation traces | `register_deletion(Traces)` |
| `filter` | `POST …/deletions` `{kind, filter?, search?, from, to, include_eval?, include_experiments?}` | `deletion_traces`: the matched traces (≤ 100,000), plus their evaluation traces; the request in `request` | `register_deletion(Traces)`; no call when nothing matched (the row goes straight to `applied`, `trace_count = 0`) |
| `user` | `POST …/deletions` `{kind, user_id}` | `deletions.user_id`; `deletion_traces`: the traces with a span of that user ingested up to the cut-off, plus their evaluation traces; `deletion_sessions`: those spans' session ids | `register_deletion(User)` |
| `project` | `DELETE /api/v1/projects/{project}?confirm=<slug>` | `project_id`; `request` keeps the name and slug | `drop_project` |
| `org` | `DELETE /api/v1/orgs/{org}?confirm=<slug>` | `deletion_projects`: every project of the org at request time; `request` keeps the name and slug | `drop_project` for each |

The `filter` kind matches the traces list exactly. It takes the same filter DSL, full-text `search`, `[from, to)` window and default exclusions (`include_eval`, `include_experiments`). So "delete what I see" deletes what the list shows. D24's "trace / search filter" covers it.

### Evaluation traces (D-M-6)

A judge trace (track I) is a trace in the same project, with environment `llmobs-eval`. Its spans carry `metadata['llmobs.eval.target_trace_id']`, the hex id of the trace it evaluated. It holds copies of the evaluated data (prompt, variables, webhook body), so spec 03 § 7 extends every scope with them.
- **Lookup.** `eval_trace_ids` (M2) scans the project's eval spans once (`environment = 'llmobs-eval'` and the key present) and keeps the ids whose target is in the scope. The intersection is done in memory, streaming batch by batch, so the scope size does not change the query plan.
- **Late judge traces.** A judge run that loaded its target before the deletion and commits after the cut-off would write a judge trace that no tombstone covers. The **deletion guard** (`deletion::DeletionGuard`, M3) closes that gap. It keeps in memory, for 24 hours after `created_at`:
  - the user ids of `user` deletions, from creation;
  - the trace ids of every deletion, from the moment they are known.

  I's executor asks `guard.blocks(project, target_trace_id, users, newest_ingest, now)` before the judge call and again before it commits a judge trace or a score; `users` are the trace's span user ids and `newest_ingest` its newest span's `ingest_time`. A user entry only blocks a trace whose data was ingested at or before the deletion's cut-off: a user who keeps using the product after an erasure is still evaluated. When blocked, the job ends `skipped` (`deleted`) and commits nothing (M8). The guard is loaded from `meta.db` at startup. 24 h is longer than a job can live (D-I-28 expires queued rule jobs after 24 h, and a running job has a timeout of minutes).

### Cascades in `meta.db` (D-M-7, D-M-11)

| Table / state | `trace_ids`, `filter`, `user` | `project` | `org` |
|---|---|---|---|
| `eval_jobs` (I) | rows of the scope's traces whose status is not `running` are deleted, in the scope transaction (M8) | `ON DELETE CASCADE` from `projects` | cascade |
| `pending_traces` (I) | kept; the sweeper reads through storage, which hides the traces | cascade | cascade |
| `saved_views` (H), evaluators, rules, score definitions, API keys, members, alert tables (K) | kept | cascade | cascade |
| `llm_connections`, `invites`, `org_members` | kept | kept | cascade from `orgs` |
| `usage_daily` | kept | kept: aggregates, needed for the org's billing (N) | deleted, after one `Usage::flush` |
| `deletions` and its scope tables | kept forever (audit, D-M-8) | kept | kept |
| live streams (SSE) | `resync` event on the project's streams when registered | `resync` | `resync` per project |
| `GET /metrics` cache | entries of the project dropped | dropped | dropped |
| API-key cache | kept: every request re-checks the project's state | entries expire in ≤ 60 s; the project is 404 at once | same |

Deleting the `projects` row (or the `orgs` row) is what runs the foreign-key cascades. `meta.db` has `foreign_keys=ON` (D136).

### Access (D-M-4)

| Route | Access | Session / PAT (read, write) | `sk` key |
|---|---|---|---|
| `POST /projects/{p}/deletions` kind `trace_ids` | `Access::Project { perm: ProjectPerm::Delete, session_only: false }` | effective project role `admin` | scope `write` |
| `POST /projects/{p}/deletions` kind `filter` or `user` | same route | `admin` | `403` (checked in the handler) |
| `POST /projects/{p}/deletions/preview` | same perm | `admin` | `403` (handler) |
| `GET /projects/{p}/deletions`, `GET …/deletions/{id}` | same perm | `admin` | scope `write` |
| `DELETE /projects/{p}` | `Access::Project { perm: ProjectPerm::Drop, session_only: false }` | **org** role `admin` or `owner` (handler check; a project-admin override is not enough) | `403` |
| `DELETE /orgs/{org}` | `Access::Org { perm: OrgPerm::Delete, session_only: false }` | org `owner` | `404` (keys never see org routes) |
| `GET /orgs/{org}/deletions` | `Access::Org { perm: OrgPerm::ViewDeletions, session_only: false }` | org `admin` | `404` |

A read-only PAT is refused on every non-GET route by the existing check in `http/access.rs::prepare` (403). These rules are spec 08 unchanged; M only names the permissions.

### Limits (D-M-5)

- `trace_ids`: 1–1,000 distinct ids per request. Each is 32 hex digits, either case, and not all zero.
- `filter`: at most 100,000 matched traces (`MAX_FILTER_DELETION_TRACES` in storage).
  - `POST` counts first, with `limit = 100,001` and the 30 s read timeout. More than 100,000 is `422 too_many_traces`.
  - The worker re-checks at resolution. If the data grew past the cap in between, the row ends `failed` with `error = too_many_traces`.
  - `POST …/preview` returns `{matched_traces, capped}`, where `capped` means "more than 100,000".
- `user`: no cap (D-M-5). Spans are already covered by `user_id`. The resolved trace ids only add that user's spans without a user id and the `traces` rows.
- `user_id`: 1–255 bytes, compared exactly (no trimming). That is the bound D-G-28 uses for score user ids.
- At most 1,000 open (`resolving` or `pending`) deletions per project. One more is `409 conflict` with detail `too many open deletions`.
- `confirm` (project and org drops) must equal the slug exactly. Otherwise it is `400` with field `confirm`.

### Worker (D-M-3, D-M-13)

`deletion::run_once(&AppState)` processes every open row, oldest first. The task `spawn_deletions` (in `serve.rs`) runs it:
- when `state.deletions.wake` is notified (every create notifies it);
- otherwise every 2 s.

Rows are processed one at a time. A large `user` resolution can delay the others, but a project or org being dropped is already hidden.

Failures are counted per row in memory (`attempts: HashMap<Uuid, Attempt>`). After a failure the row is skipped until `now + 60 s`.
- `RunError::Timeout`, `QueryError::Busy` and `DeletionError::Catalog`/`StorageError` are retried.
- `trace_ids`, `filter` and `user` rows end `failed` / `resolution_failed`:
  - after 5 consecutive failures;
  - at once on `RunError::Invalid`, `RunError::InvalidRequest`, `RunError::Failed` or `RunError::TooLarge`;
  - only while no scope is stored (`trace_count` NULL): once the scope and its cascades are stored, the row retries registration until storage takes it, so `failed` always means nothing was hidden or deleted.
- `project` and `org` rows are retried forever.

Timeouts: `RESOLVE_TIMEOUT = 600 s` for each resolution read; the `POST` count and the preview use `routes::read::READ_TIMEOUT` (30 s).

### Retention (D-M-10)

- `retention::retention_once(&AppState)` runs every hour, and once 60 s after startup. For every project with `state = 'active'` in an active org, it computes the **effective retention** `retention::effective_days(project.retention_days, cap)`: the smaller of the two when both are set, the one that is set otherwise, none when neither is (D-M-15). `cap` is the org's retention cap from its plan (D175: data-access window + 30 days). Until track P lands, `retention_once` passes `None`, so effective retention is the project's own. When there is an effective retention, it calls `Storage::apply_retention(project, keep_from)`, where `keep_from = UTC date of now − effective days`. Errors are logged per project and do not stop the loop.
- `PATCH /api/v1/projects/{project}` that sets `retention_days` to a number calls `apply_retention` for that project right after the commit. An error is logged, and the response is still `200`: the hourly run retries.
- Setting retention back to null ("keep forever") calls nothing. The storage watermark never moves back.
- Setting `retention_days` to a number (on `PATCH` or on project creation) passes the entitlement check `may_manage_retention` first (D177, D-M-16). Until P lands it always allows. Setting it back to `null` (forever) is never gated. When it refuses, the answer is `403` with code `plan_required`, and the settings page shows "Data retention management is part of the Pro plan." (M11). GDPR deletions are never gated.
- Retention does not clear the metrics cache or send a live `resync`: dropped days are old data, and the 10 s cache and the next refetch catch up.
- What it covers:
  - spans, traces and scores: whole days, in storage;
  - judge traces: they are spans of the project;
  - media: daily media GC, so a blob outlives its last span by at most a day.

  `deletions` rows are audit and are kept. `eval_jobs` are pruned by I after 7 days. `usage_daily` is aggregate and is kept.
- New projects keep data forever unless created with `retention_days` (D-M-10; unchanged from D).

### Audit (D-M-8)

- `deletions` rows and their scope tables are never deleted, not even when the project or org is dropped. They have no foreign keys.
  - `request` keeps the request details: the filter, or the dropped project's or org's name and slug.
  - `requested_by_kind` + `requested_by` name the user or the API key.
- `tracing` events at `info` with `target: "audit"`, field names exactly as below. They carry ids only, never the user id of a `user` deletion, a trace id, a filter text or a slug (spec 08 § Audit, D150):
  - `deletion requested`: `deletion_id`, `kind`, `org_id`, `project_id` (absent for org), `actor_kind`, `actor_id`;
  - `deletion registered`: `deletion_id`, `trace_count`, `session_count`;
  - `deletion applied`: `deletion_id`;
  - `deletion failed`: `deletion_id`, `error`;
  - `retention changed`: `project_id`, `actor_id`, `retention_days` (number or `none`);
  - `project dropped`: `deletion_id`, `org_id`, `project_id`; `org dropped`: `deletion_id`, `org_id`, `projects` (a count) — both from `deletion::on_dropped` (D-M-17).
- `llmobs admin reapply-deletions --from <data_dir>` (M7) copies the deletions that the current `meta.db` lacks from another data directory's `meta.db`, with their scope, as open rows. The server then registers or drops them again (D-M-9). This is how a backup restore keeps erasures done after the backup (spec 12 § Backups).

### Live streams and caches (D-M-11)

- `LiveEvent` gains `resync: bool`. `LiveBus::publish_resync(project_id)` sends one event with no spans and `resync = true`, and builds nothing when no stream is open. Both stream loops in `routes/streams.rs` send the SSE `resync` event when they receive a resync event for their project. The web pages already refetch on `resync`.
- `MetricsCache::clear_project(project_id)` drops that project's cached answers.
- Both are called after a deletion is registered and after a project is dropped.

### Plans and cloud (D-M-15, D-M-16, D-M-17)

Three call sites for tracks P and Q. Each is one plain function call in M's code. M builds no plan lookup, no entitlement module and no control-plane client.

| Hook | Where (M) | M's behaviour | Who replaces it, with what |
|---|---|---|---|
| Retention cap (D175) | `retention::retention_once`: `let cap: Option<u32> = None;` passed to `effective_days` | no cap | P: `state.entitlements.of(org).retention_cap_days()` (plan data-access window + 30 days; `None` without a plan, e.g. self-hosted without a licence) |
| Retention management (D177) | `routes/projects.rs::may_manage_retention(state, org) -> bool`, called by `create_project` and `update_project` when the body sets `retention_days` to a number | returns `true` | P: the body becomes `state.entitlements.of(org).allows(Feature::RetentionManagement)` (free when self-hosted, Pro on the cloud) |
| Dropped (D170) | `deletion::on_dropped(state, &Dropped)`, called once by the worker after a project or org drop committed (at most once: a crash right after the commit skips it) | emits the audit event `org dropped` / `project dropped` | Q: also sends the control-plane notification (org → cell mapping cleanup); for durability Q reconciles from `deletions` rows of kind `project`/`org` in status `applied`, which are never deleted |

### Code layout (end state)

```
crates/storage/src/reads/deletions.rs     filter_trace_ids, user_deletion_scope, eval_trace_ids (M2)
crates/server/migrations/meta/NNNN_deletions.sql                                           (M1)
crates/server/src/meta/deletions.rs       rows, inserts, updates, queries                  (M1, M3, M7)
crates/server/src/deletion/mod.rs         Deletions (wake + guard + attempts), run_once, on_dropped (M3)
crates/server/src/deletion/resolve.rs     scope resolution over the M2 reads              (M3)
crates/server/src/deletion/guard.rs       DeletionGuard                                    (M3)
crates/server/src/deletion/cascade.rs     meta.db cascades of a stored scope              (M8)
crates/server/src/retention.rs            retention_once, keep_from                        (M6)
crates/server/src/api/deletions.rs        request and response types                       (M4, M5)
crates/server/src/routes/deletions.rs     project deletion routes                          (M4)
crates/server/src/routes/deletions.rs     + DELETE project, DELETE org, org deletion list (M5)
crates/server/src/routes/projects.rs      PATCH/POST apply retention, may_manage_retention (M6)
crates/server/src/admin.rs                + reapply_deletions                               (M7)
crates/cli/src/main.rs                    + admin reapply-deletions                         (M7)
web/src/components/type-to-confirm-dialog.tsx                                              (M9)
web/src/routes/$org/$project/settings/data.tsx                                             (M9)
web/src/components/deletions/*                                                             (M9, M10, M11)
```

`NNNN` is the next free migration number when M1 runs: list `crates/server/migrations/meta/`, take the highest four-digit prefix and add one (`0005` when this plan was updated, after H's `0003` and I's `0004`). H and I are adding migrations in parallel. A number already merged is never changed. If a rebase brings in a migration with the same number as yours, renumber yours.

## Decisions

The decisions are in `docs/mvp-decisions.md` § Track M — Deletion and retention, D-M-1 to D-M-17. D-M-15 to D-M-17 apply D169–D179 (cloud and plans). The user chose D-M-3, D-M-4, D-M-5, D-M-7, D-M-8, D-M-10 and D-M-12's confirmations (2026-11-14). The others follow from the spec and the code.

## Sub-phases

| # | File | What | Depends on |
|---|---|---|---|
| M1 | [`01-store.md`](01-store.md) | Migration and `meta/deletions.rs` | — |
| M2 | [`02-storage-reads.md`](02-storage-reads.md) | `filter_trace_ids`, `user_deletion_scope`, `eval_trace_ids` in `crates/storage` | — |
| M3 | [`03-worker.md`](03-worker.md) | `deletion` module: worker for `trace_ids`/`filter`/`user`, guard, live `resync`, metrics cache, task | M1, M2 |
| M4 | [`04-api.md`](04-api.md) | Project deletion routes, types, OpenAPI, fixtures, `ProblemCode::TooManyTraces` | M3 |
| M5 | [`05-drop.md`](05-drop.md) | `DELETE` project and org, org deletion list, worker kinds `project`/`org` | M4 |
| M6 | [`06-retention.md`](06-retention.md) | Hourly retention, retention on `PATCH`, the cap and entitlement call sites | M3 (both edit `serve.rs`'s task list) |
| M7 | [`07-reapply.md`](07-reapply.md) | `llmobs admin reapply-deletions` | M5 |
| M8 | [`08-eval-cascade.md`](08-eval-cascade.md) | `eval_jobs` cascade and the guard in I's executor | M3, **I's M3** |
| M9 | [`09-web-data-tab.md`](09-web-data-tab.md) | Type-to-confirm dialog, Data tab (user deletion, history) | M4 |
| M10 | [`10-web-actions.md`](10-web-actions.md) | Delete matching traces, delete trace, delete user data | M9 |
| M11 | [`11-web-drop.md`](11-web-drop.md) | Delete project, delete organization | M5, M9 |
| M12 | [`12-e2e-closeout.md`](12-e2e-closeout.md) | End-to-end test, spec and roadmap closeout | all |

Run them in this order. M6 may run any time after M3. M8 may wait for I and run after M12. If it does, M12's closeout says M8 is the one left.

## Conventions

- **Commit**: one commit per sub-phase, with the subject line given in the sub-phase file. No attribution lines.
- **Errors**: server code returns `ApiError` (`crates/server/src/http/error.rs`); store code returns `MetaError`; storage reads return `RunError`. No `unwrap()`/`expect()` outside tests unless a comment states the invariant (`AGENTS.md`).
- **Privacy**: never log or put in an error message a trace id, user id, session id, filter text, search text or slug from a deletion request. Log `deletion_id`, `project_id`, `org_id`, counts and the error kind.
- **Time**: `i64` ns from `state.clock.now_ns()`. JSON instants use `llmobs_core::time_serde` and the `optional_time` helper that `api/keys.rs` uses.
- **Ids**: `Uuid::now_v7()` for deletion ids. Trace ids are `llmobs_core::ids::TraceId`, stored in `meta.db` as 16-byte BLOBs (`trace_id.as_bytes()` / `TraceId::from_bytes`), and written in JSON as lowercase hex.
- **Tests**: Rust unit tests next to the code (`#[cfg(test)] mod tests`). Server tests use `crate::test_support::TestApp` with its manual clock (`2026-01-01T00:00:00Z`). Web tests use Vitest + Testing Library on the MSW server, with `server.use(...)` to override an endpoint; never `vi.mock`.
- **Contract**: when `openapi.json` changes, run `just openapi`. It regenerates `crates/server/openapi.json` and `web/src/api/schema.gen.ts`, and both are committed. When the API examples change, run `just api-examples`.

## Commands

```bash
export PATH=$HOME/.local/share/nvm/v26.13.0/bin:$PATH
```

```bash
just fmt
```

```bash
just check
```

`just check` must pass before every commit. For a quick loop while working:
- `cargo test -p llmobs-server <name>`;
- `cargo test -p llmobs-storage <name>`;
- `cd web && pnpm vitest run <file>`.

## Coordination with other tracks

These go to the coordinating session, which forwards them. They are also in the report of the sub-phase named.

- **I (online evaluation)**:
  - Judge spans must carry `metadata['llmobs.eval.target_trace_id']` as lowercase hex (spec 06 already says so). M2's lookup depends on it.
  - M8 adds `DeletionGuard::blocks` calls to I's executor and deletes `eval_jobs` rows. I should keep the guard calls when it changes the executor.
  - D-I-28 already deletes jobs of projects that are not `active`.
- **K (alerts)**: every K table that holds a `project_id` must declare `REFERENCES projects(id) ON DELETE CASCADE`, or reach a table that does, so a project drop removes it. Rows must not copy span payloads (spec 03 § Cascades keeps `alert_events` because they are aggregates). A notification body that quotes a trace id is acceptable; one that quotes span content is not.
- **H (console)**: `saved_views` already cascades from `projects`; H's MCP `list_projects` already skips inactive orgs and projects. Nothing to do.
- **J (datasets)**: dataset items copied from a deleted trace (`source_trace_id` in the scope) must be hard-deleted, all versions (D24). J adds that statement to `deletion::cascade::traces_deleted` (M8), which runs in the scope transaction.
- **P (plans and entitlements)**:
  - replace `None` in `retention::retention_once` with the org's retention cap (D175, D-M-15). `retention::apply_now` (the `PATCH` path) stays as is: the hourly run applies the cap. Once a cap exists, projects without their own `retention_days` are capped too: `retention_once` already lists every active project, not only those with retention;
  - replace the body of `routes/projects.rs::may_manage_retention` with the entitlement check, and the matching UI state: the settings page already shows `plan_required` (D177, D-M-16);
  - P's hidden data-access window is enforced in A2's query time window, not by M. It must **not** apply to M's scope reads (`filter_trace_ids`, `user_deletion_scope`, `eval_trace_ids`) or to the deletion preview: an erasure must also cover hidden-but-kept data. P adds a test for that;
  - ingest's D85 age check uses the project's `retention_days` today; P switches it to `retention::effective_days`, and drops spec 12's `orgs.limits.max_retention_days` in favour of the cap.
- **Q (cloud)**: add the control-plane notification to `deletion::on_dropped` (D170, D-M-17). `on_dropped` runs after the `meta.db` rows are deleted, at most once per drop; it receives the deletion id, the org id, the dropped project ids and whether the whole org went. For durability Q reconciles from `deletions` rows of kind `project`/`org` in status `applied` (kept forever), so a crash or a failed call is caught up.
- **N (packaging)**: the backup docs must say to keep the pre-restore data directory, or a copy of its `meta.db`, and to run `llmobs admin reapply-deletions --from <it>` after a restore (D-M-9).

## API contract gaps

Found while planning. M closes 1–6.

1. Spec 09 listed `POST /deletions` with two `kind: "traces"` shapes and statuses `pending`/`applied`. They become kinds `trace_ids`, `filter`, `user` and statuses `resolving`, `pending`, `applied`, `failed` (D-M-2, D-M-3).
2. No preview endpoint existed for the UI's "preview count". It is `POST …/deletions/preview` (D-M-5).
3. `DELETE /projects/{project}` and `DELETE /orgs/{org}` had no request or response shape. They take `?confirm=<slug>` and answer `202` with a `Deletion` (D-M-7).
4. There was no org-level history, so a dropped project's deletion record was unreachable. It is `GET /orgs/{org}/deletions` (D-M-8).
5. No problem code meant "too many traces". `too_many_traces` (422) is added (D-M-5).
6. Spec 12's `llmobs admin reapply-deletions --since <date>` read the deletions from the restored `meta.db`, which predates them. It becomes `--from <data_dir>` (D-M-9).
7. **Not closed**: `Idempotency-Key` (spec 09 § Conventions) is implemented by no route. A retried `POST /deletions` creates a second request. That is harmless, because deletions are idempotent in effect, but it counts toward the 1,000 open deletions.
8. **Not closed**: `UpdateProjectRequest.retention_days` is typed without `null` in `openapi.json` (L's contract gap CG5). M6 does not change it.
9. A new problem code `plan_required` (403) is added by M6 for the retention entitlement check (D-M-16). P reuses it for its other gates.
