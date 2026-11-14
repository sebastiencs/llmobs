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
    fn lock(&self) -> std::sync::MutexGuard<'_, GuardState> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records `traces` of `project` from a deletion created at `created_at`. An existing entry
    /// keeps the later `created_at`.
    pub(crate) fn add_traces(&self, project: ProjectId, traces: &[TraceId], created_at: i64) {
        let mut state = self.lock();
        let map = state.traces.entry(project).or_default();
        for trace in traces {
            let slot = map.entry(*trace).or_insert(created_at);
            *slot = (*slot).max(created_at);
        }
    }

    /// Records an end user of `project`.
    pub(crate) fn add_user(&self, project: ProjectId, user_id: &str, created_at: i64) {
        let mut state = self.lock();
        let map = state.users.entry(project).or_default();
        let slot = map.entry(user_id.to_owned()).or_insert(created_at);
        *slot = (*slot).max(created_at);
    }

    /// Whether a judge run about `trace` must not go on (D-M-6). Only entries with
    /// `created_at > now - GUARD_NS` count:
    /// - a trace-id entry for `trace` blocks;
    /// - a user entry for one of `users` blocks only when `newest_ingest_ns <= entry.created_at`
    ///   (the trace's data predates that user's deletion; newer data is not covered, spec S5).
    ///
    /// `users` are the distinct user ids of the trace's loaded spans; `newest_ingest_ns` their
    /// largest `ingest_time`. Callers that know neither pass `&[]` and `0`.
    pub(crate) fn blocks(
        &self,
        project: ProjectId,
        trace: TraceId,
        users: &[&str],
        newest_ingest_ns: i64,
        now: i64,
    ) -> bool {
        let floor = now.saturating_sub(GUARD_NS);
        let state = self.lock();
        let trace_hit = state
            .traces
            .get(&project)
            .and_then(|map| map.get(&trace))
            .is_some_and(|created| *created > floor);
        if trace_hit {
            return true;
        }
        let Some(map) = state.users.get(&project) else {
            return false;
        };
        users.iter().any(|user| {
            map.get(*user)
                .is_some_and(|created| *created > floor && newest_ingest_ns <= *created)
        })
    }

    /// Removes entries with `created_at <= now - GUARD_NS`, and empty project maps.
    pub(crate) fn prune(&self, now: i64) {
        let floor = now.saturating_sub(GUARD_NS);
        let mut state = self.lock();
        for map in state.traces.values_mut() {
            map.retain(|_, created| *created > floor);
        }
        state.traces.retain(|_, map| !map.is_empty());
        for map in state.users.values_mut() {
            map.retain(|_, created| *created > floor);
        }
        state.users.retain(|_, map| !map.is_empty());
    }

    /// Adds `meta::deletions::guard_entries(now - GUARD_NS)`.
    ///
    /// # Errors
    ///
    /// Returns [`MetaError`] when the entries cannot be read.
    pub(crate) async fn load(&self, meta: &MetaDb, now: i64) -> Result<(), MetaError> {
        let entries =
            crate::meta::deletions::guard_entries(meta.reader(), now.saturating_sub(GUARD_NS))
                .await?;
        for (project, trace, created_at) in entries.traces {
            self.add_traces(project, &[trace], created_at);
        }
        for (project, user, created_at) in entries.users {
            self.add_user(project, &user, created_at);
        }
        Ok(())
    }

    /// Total entries held, for tests.
    #[cfg(test)]
    fn len(&self) -> usize {
        let state = self.lock();
        let traces: usize = state.traces.values().map(HashMap::len).sum();
        let users: usize = state.users.values().map(HashMap::len).sum();
        traces + users
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::meta::deletions::{
        ActorKind, DeletionKind, DeletionState, NewDeletion, RequestDetails, insert_deletion,
        insert_traces,
    };
    use crate::test_support::TestApp;
    use llmobs_core::ids::OrgId;
    use uuid::Uuid;

    fn trace(n: u128) -> TraceId {
        TraceId::from_bytes((n + 1).to_be_bytes()).unwrap()
    }

    #[test]
    fn blocks_known_traces_and_users_for_a_day() {
        let guard = DeletionGuard::default();
        let p = ProjectId::new_v7();
        let p2 = ProjectId::new_v7();
        guard.add_traces(p, &[trace(1)], 100);
        guard.add_user(p, "u", 100);
        let (t, t2) = (trace(1), trace(2));

        assert!(guard.blocks(p, t, &[], 0, 101));
        // Data ingested at the cut-off is covered; newer data is not.
        assert!(guard.blocks(p, t2, &["u"], 100, 101));
        assert!(!guard.blocks(p, t2, &["u"], 101, 102));
        assert!(!guard.blocks(p2, t, &[], 0, 101));
        assert!(!guard.blocks(p, t2, &[], 0, 101));
        // Expired after a day.
        assert!(!guard.blocks(p, t, &[], 0, 100 + GUARD_NS));
    }

    #[test]
    fn prune_drops_expired_entries() {
        let guard = DeletionGuard::default();
        let p = ProjectId::new_v7();
        guard.add_traces(p, &[trace(1), trace(2)], 100);
        guard.add_user(p, "u", 100);
        assert_eq!(guard.len(), 3);

        guard.prune(100 + GUARD_NS);
        assert_eq!(guard.len(), 0);
        assert!(!guard.blocks(p, trace(1), &["u"], 0, 100 + GUARD_NS));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn load_reads_recent_deletions() {
        let app = TestApp::new().await;
        let now = app.state.clock.now_ns();
        let p = ProjectId::new_v7();
        let org: OrgId = OrgId::new_v7();
        let (t, t2, x) = (trace(1), trace(2), trace(3));

        let user_row = NewDeletion {
            id: Uuid::now_v7(),
            org_id: org,
            project_id: Some(p),
            kind: DeletionKind::User,
            user_id: Some("user-1".to_owned()),
            request: RequestDetails::default(),
            actor_kind: ActorKind::User,
            actor_id: Uuid::now_v7(),
            state: DeletionState::Resolving,
            created_at: now,
        };
        let old_row = NewDeletion {
            id: Uuid::now_v7(),
            kind: DeletionKind::TraceIds,
            user_id: None,
            created_at: now - GUARD_NS - 1,
            ..user_row.clone()
        };
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    insert_deletion(conn, &user_row).await?;
                    insert_traces(conn, user_row.id, &[t]).await?;
                    insert_deletion(conn, &old_row).await?;
                    insert_traces(conn, old_row.id, &[t2]).await?;
                    Ok(())
                })
            })
            .await
            .unwrap();

        let guard = DeletionGuard::default();
        guard.load(&app.state.meta, now).await.unwrap();
        assert!(guard.blocks(p, t, &[], 0, now));
        assert!(guard.blocks(p, x, &["user-1"], now - 1, now));
        assert!(!guard.blocks(p, t2, &[], 0, now));
    }
}
