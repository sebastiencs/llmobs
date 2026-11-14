# M6 — Retention

Read [README.md](README.md) first, especially § Design › Retention and § Plans and cloud.

**Goal:** per-project retention actually drops data:
- an hourly task applies every active project's **effective retention** (`Storage::apply_retention`);
- `PATCH /api/v1/projects/{project}` applies a new retention right away;
- setting retention goes through the entitlement check `may_manage_retention` (always allowed until track P, D177), which can refuse with `403 plan_required`;
- every retention change is an audit event.

Storage already does the work (spec 03 § 8): it obsoletes whole days before the watermark, hides memtable rows older than it at once (keyed by `start_time`'s day), and refuses late files.

**Edit only:**
- `crates/server/src/retention.rs` (new), `crates/server/src/lib.rs` (declare it);
- `crates/server/src/meta/projects.rs` (`projects_for_retention`, `decode_retention`);
- `crates/server/src/routes/projects.rs` (`create_project`, `update_project`, `may_manage_retention`);
- `crates/server/src/api/common.rs` (`ProblemCode::PlanRequired`), `crates/server/src/http/error.rs` (`ApiError::PlanRequired`);
- `crates/server/src/api/openapi.rs` (the 403 description), `crates/server/openapi.json` and `web/src/api/schema.gen.ts` (regenerated: the problem-code enum gains `plan_required`);
- `crates/server/src/serve.rs` (spawn the task).

**Commit:** `Apply project retention`

## Read first

- `docs/spec/03-storage.md` § 8.
- `llmobs_storage::Storage::apply_retention(project, keep_from: NaiveDate) -> Result<RetentionReport, StorageError>` and `RetentionReport { files, bytes, keep_from }`.
- `crates/server/src/routes/projects.rs`: `create_project`, `update_project`, `checked_retention`, `ProjectPatch`.
- `crates/server/src/meta/projects.rs`: `project_row` (which checks `1..=36_500` inline) and `encode_retention`.
- `crates/server/src/auth/principal.rs`: `session_user`.
- `crates/server/src/serve.rs`: the `tasks` list (after M3 it holds `spawn_deletions` before `spawn_usage`).
- `crates/server/src/test_support/mod.rs`: `add_project(seeded, slug, retention_days)`, `commit_spans`, `dataset_span`, `capture_logs`, the manual clock (`2026-01-01T00:00:00Z`).
- `chrono` 0.4 is already a dependency of `llmobs-server`.

## Code

`crates/server/src/retention.rs`:

```rust
//! Per-project retention (spec 03 § 8, D-M-10, D-M-15).

use std::time::Duration;

use chrono::{DateTime, Days, NaiveDate};
use llmobs_core::ids::{OrgId, ProjectId};

use crate::http::AppState;
use crate::meta::MetaError;

/// Time between two runs.
pub(crate) const EVERY: Duration = Duration::from_secs(3600);
/// Delay of the first run after startup.
pub(crate) const FIRST_RUN: Duration = Duration::from_secs(60);

/// The first UTC day to keep: the day of `now_ns`, minus `days`.
pub(crate) fn keep_from(now_ns: i64, days: u32) -> NaiveDate {
    let today = DateTime::from_timestamp_nanos(now_ns).date_naive();
    // i64 ns covers 1677–2262 and days ≤ 36_500, so this cannot leave chrono's range;
    // `NaiveDate::MIN` only keeps the function total.
    today.checked_sub_days(Days::new(u64::from(days))).unwrap_or(NaiveDate::MIN)
}

/// Effective retention: the project's own and the org's cap, whichever is shorter;
/// the one that is set when only one is; none when neither is (D-M-15).
pub(crate) fn effective_days(project: Option<u32>, cap: Option<u32>) -> Option<u32> {
    match (project, cap) {
        (Some(days), Some(cap)) => Some(days.min(cap)),
        (Some(days), None) | (None, Some(days)) => Some(days),
        (None, None) => None,
    }
}

/// Totals of one run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct RetentionRun {
    /// Projects whose call succeeded.
    pub(crate) projects: u64,
    /// Projects whose call failed (logged).
    pub(crate) failures: u64,
    /// Files obsoleted, all projects.
    pub(crate) files: u64,
}

/// Applies the effective retention of every active project of an active org.
/// A storage error on one project is logged and counted; the others still run.
///
/// # Errors
///
/// [`MetaError`] when the projects cannot be listed.
pub(crate) async fn retention_once(state: &AppState) -> Result<RetentionRun, MetaError> {
    let now = state.clock.now_ns();
    let mut run = RetentionRun::default();
    for (org, project, own) in crate::meta::projects::projects_for_retention(state.meta.reader()).await? {
        // D175, D-M-15: track P replaces `None` with the org's cap
        // (`state.entitlements.of(org).retention_cap_days()`: data-access window + 30 days).
        let cap: Option<u32> = None;
        let _ = org;
        let Some(days) = effective_days(own, cap) else {
            continue;
        };
        match state.storage.apply_retention(project, keep_from(now, days)).await {
            Ok(report) => {
                run.projects += 1;
                run.files += report.files;
            }
            Err(error) => {
                run.failures += 1;
                tracing::warn!(project = %project, error = %error, "retention failed");
            }
        }
    }
    Ok(run)
}

/// Applies `days` to one project now (a `PATCH`). Errors are logged; the hourly run retries.
pub(crate) async fn apply_now(state: &AppState, project: ProjectId, days: u32) {
    let keep = keep_from(state.clock.now_ns(), days);
    if let Err(error) = state.storage.apply_retention(project, keep).await {
        tracing::warn!(project = %project, error = %error, "retention failed");
    }
}
```

`let _ = org;` keeps `org` named at the call site P will use; if clippy flags it (`let_underscore_untyped` is not enabled in this workspace; check `[workspace.lints]`), bind the tuple as `(_org, project, own)` instead and keep the comment. `StorageError`'s `Display` names no payload, so logging `%error` is allowed. `PATCH`'s immediate apply uses the project's own days only: the hourly run applies the cap. In `lib.rs`, add `pub(crate) mod retention;`, alphabetical.

`meta/projects.rs`:

```rust
/// Decodes a stored `retention_days`: NULL is `None`, `1..=36_500` is `Some`, anything else is corrupt.
fn decode_retention(value: Option<i64>) -> Result<Option<u32>, MetaError>;

/// `(org, project, retention_days)` of every project with `state = 'active'` in an active
/// org, ordered by project id. Projects without retention are included: an org cap
/// (D-M-15) applies to them too.
///
/// # Errors
///
/// Returns [`MetaError`] when the query fails or a value does not decode.
pub(crate) async fn projects_for_retention(pool: &SqlitePool) -> Result<Vec<(OrgId, ProjectId, Option<u32>)>, MetaError>;
```

- Extract `decode_retention` from the inline check in `project_row` (same `Corrupt { table: "projects", column: "retention_days" }` error), and use it in both places.
- SQL: `SELECT projects.org_id, projects.id, projects.retention_days FROM projects JOIN orgs ON orgs.id = projects.org_id WHERE projects.state = 'active' AND orgs.state = 'active' ORDER BY projects.id`. Ids that do not parse are `Corrupt { table: "projects", column: "id" | "org_id" }`.

## Entitlement check and `plan_required` (D177, D-M-16)

`api/common.rs`, `ProblemCode`: add `PlanRequired` (`/// The organization's plan does not include this feature.`); `status` → 403 (join the `Forbidden | SsoRequired | EmailNotVerified` arm); `reason_phrase` → `"Forbidden"` (same arm).

`http/error.rs`, `ApiError`:

```rust
/// The org's plan does not include the feature (D177). `detail` names it.
#[error("plan required")]
PlanRequired {
    /// Static text shown as the problem `detail`.
    detail: &'static str,
},
```
and in `problem()`: `Self::PlanRequired { detail } => Problem::new(ProblemCode::PlanRequired, Some((*detail).to_owned())),`.

`api/openapi.rs` `problem(code)`: change the `403` text to `"The credentials do not allow this operation, or the organization's plan does not include it (code plan_required)."`.

`routes/projects.rs`:

```rust
/// Whether the org may set project retention (D177, D-M-16). Free when self-hosted, a Pro
/// feature on the cloud. Track P replaces the body with
/// `state.entitlements.of(org).allows(Feature::RetentionManagement)`; until then it allows.
fn may_manage_retention(_state: &AppState, _org: OrgId) -> bool {
    true
}

const RETENTION_PLAN: &str = "data retention management is part of the Pro plan";
```

- `create_project`: when `body.retention_days.is_some()` and `!may_manage_retention(&state, access.org.id)` → `Err(ApiError::PlanRequired { detail: RETENTION_PLAN })`, before the write.
- `update_project`: when `body.retention_days.is_some()` (a number or `null`) and `!may_manage_retention(&state, access.project.org_id)` → the same, before the write.

The `false` branch cannot be reached until P changes the body; it is tested through `ApiError::PlanRequired`'s mapping (test 9).

## PATCH

`routes/projects.rs`, `update_project`:
- Add `Extension(principal): Extension<Principal>` to its arguments, and at the top of the handler `let actor_id = principal::session_user(&principal)?.id;` (`ProjectPerm::ManageSettings` has no key scope, so the principal is a session or a PAT; doing this before the write means a failure cannot follow a committed change).
- Copy `let retention = retention_days;` (it is `Copy`) before it moves into `ProjectPatch`.
- After the write commits and before reloading the project:

```rust
if let Some(retention) = retention {
    let text = retention.map_or_else(|| "none".to_owned(), |days| days.to_string());
    tracing::info!(target: "audit", project_id = %id, actor_id = %actor_id, retention_days = %text, "retention changed");
    if let Some(days) = retention {
        crate::retention::apply_now(&state, id, days).await;
    }
}
```

`%text` prints the value unquoted (`retention_days=7`). The event is logged whenever the body has `retention_days`, even when the value did not change; that is simpler and harmless.

## Task

`serve.rs`: insert `spawn_retention(state.clone())` into `tasks` immediately before `spawn_usage(state)` (which takes the last move of `state`):

```rust
fn spawn_retention(state: AppState) -> JoinHandle<()> {
    tokio::spawn(async move {
        tokio::time::sleep(crate::retention::FIRST_RUN).await;
        loop {
            match crate::retention::retention_once(&state).await {
                Ok(run) if run.files > 0 || run.failures > 0 => {
                    tracing::info!(projects = run.projects, failures = run.failures, files = run.files, "retention run");
                }
                Ok(_) => {}
                Err(error) => tracing::error!(error = %error, "retention run failed"),
            }
            tokio::time::sleep(crate::retention::EVERY).await;
        }
    })
}
```

## Tests

Spans: `dataset_span(0)` with `trace_id = "<32 hex>".parse().unwrap()`, `span_id` edited, `parent_span_id = None`, `start_time` as given and `end_time = start_time + 1_000`. `commit_spans` does not run normalization, so an old `start_time` is stored even when the project has retention. A trace is read with `GET /api/v1/projects/{project.id}/traces/{trace_hex}` and the seeded owner's cookie (`seeded.cookie`), which also reaches projects from `add_project` (same org).

Times: `NOW` = `2026-01-01T00:00:00Z` (the manual clock), `DAY = 86_400 * 1_000_000_000`, `HOUR = 3_600 * 1_000_000_000`. Use midday times (`NOW - 3 * DAY + 12 * HOUR`, `NOW - 12 * HOUR`): spec 03 § 8 lets a row next to midnight land differently in the memtable and in a file.

1. `keep_from_counts_utc_days`: `keep_from(NOW, 30)` = 2025-12-02; `keep_from(NOW + DAY - 1, 1)` = 2025-12-31; `keep_from(NOW, 36500)` equals `NaiveDate::from_ymd_opt(2026, 1, 1).unwrap() - Days::new(36_500)` (compute it; do not hard-code); `keep_from(i64::MIN, 1)` does not panic.
2. `effective_days_takes_the_shorter`: `(Some(30), Some(7))` → 7; `(Some(7), Some(30))` → 7; `(Some(7), None)` → 7; `(None, Some(30))` → 30; `(None, None)` → `None`.
3. `hourly_run_drops_old_days`:
   - `let (short, _) = app.add_project(&seeded, "short", Some(1));` Commit trace A at `NOW - 3 * DAY + 12 * HOUR` and trace B at `NOW - 12 * HOUR`.
   - The seeded project has no retention. Commit the same two start times to it as traces C and D.
   - `retention_once` → `projects == 1`, `failures == 0`.
   - Trace A of `short` → `404`; B → `200`; C and D → `200`.
4. `inactive_projects_are_skipped`:
   - (a) `add_project(&seeded, "p-del", Some(1))`, then `UPDATE projects SET state = 'deleting' WHERE id = ?`; (b) `add_project(&seeded, "p-ok", Some(1))`.
   - `retention_once` → `projects == 1` (only `p-ok`).
   - Then `UPDATE orgs SET state = 'deleting' WHERE id = ?` on the seeded org → `retention_once` → `projects == 0`.
5. `projects_for_retention_lists_active_ones` (in `meta/projects.rs` tests, with `test_db()`): one org with projects at `None`, `Some(7)`, `Some(30)`, and one at `Some(1)` in state `deleting` → three rows (the deleting one is left out), with their org id and days, ordered by project id; `None` is included.
6. `patch_applies_retention_at_once` (in `routes/projects.rs` tests): seeded project, trace A at `NOW - 3 * DAY + 12 * HOUR`; `PATCH {retention_days: 1}` with the owner cookie → `200`; trace A → `404` with no hourly run.
7. `patch_to_forever_keeps_dropped_data_dropped`: after test 6's steps, `PATCH {retention_days: null}` → `200`; trace A is still `404` (the watermark never moves back).
8. `patch_logs_the_change`: `capture_logs` around `PATCH {retention_days: 7}` contains `retention changed`, `retention_days=7` and `audit`; around `PATCH {name: "x"}` it contains no `retention changed`.
9. `plan_required_is_a_403_problem` (in `http/error.rs` tests, next to the existing problem tests): `ApiError::PlanRequired { detail: "x" }.into_response()` has status `403`, and its body's `code` is `plan_required` and `detail` `x`.

Not tested: the `failures` counter and the `files` total (nothing is flushed in server tests, so `files` stays 0), `retention_once`'s `Err` path, and `may_manage_retention`'s `false` branch (unreachable until P).

## Done when

- [ ] `retention_once` runs hourly (first after 60 s) over every active project's effective retention, with the cap call site for P; `PATCH` applies at once and logs the audit event; `may_manage_retention` guards create and update.
- [ ] `plan_required` is in `openapi.json` and `schema.gen.ts`.
- [ ] The 9 tests pass; `just check` is green; one commit `Apply project retention`.
