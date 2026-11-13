//! The verify pass: reads each suspect file whole, straight from the object
//! store, and quarantines the corrupt ones (spec 03 §10, D-A1-10).

use std::sync::atomic::Ordering;
use std::sync::{Arc, PoisonError};

use crate::catalog::{CatalogEdit, FileId, FileMeta, FileTable};
use crate::compaction::job::{ClaimGuard, Compactor};
use crate::compaction::plan::{Job, JobKind, Policy};
use crate::hooks::HookPoint;
use crate::read_state::publish_catalog;
use crate::verify::{Verdict, verify_file};

/// What one verify pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct VerifyReport {
    /// Files read whole and found sound.
    pub(crate) healthy: u64,
    /// Corrupt files quarantined, with their `traces` files.
    pub(crate) quarantined: u64,
    /// Corrupt `traces` files rebuilt from their `spans` sources.
    pub(crate) repaired: u64,
    /// Files whose read the object store failed; queued again.
    pub(crate) unavailable: u64,
    /// Files whose handling failed or lost a conflict; queued again.
    pub(crate) failed: u64,
}

/// Files verified per pass.
const VERIFY_PER_PASS: usize = 16;

impl Compactor {
    /// One verify pass at `now` (ns): up to [`VERIFY_PER_PASS`] suspects, one
    /// at a time.
    pub(crate) async fn verify_pass(self: &Arc<Self>, now: i64) -> VerifyReport {
        let policy = *self.policy.lock().unwrap_or_else(PoisonError::into_inner);
        let mut report = VerifyReport::default();
        let ids = self.verify.take(VERIFY_PER_PASS);
        for (index, id) in ids.iter().enumerate() {
            if self.stopping.load(Ordering::Acquire) {
                for rest in &ids[index..] {
                    self.verify.requeue(*rest);
                }
                break;
            }
            self.verify_one(*id, now, policy, &mut report).await;
        }
        report
    }

    /// Verifies one suspect and acts on the verdict. Every path counts once
    /// in `report`, or leaves the file in the queue when it cannot be decided.
    async fn verify_one(
        self: &Arc<Self>,
        id: FileId,
        now: i64,
        policy: Policy,
        report: &mut VerifyReport,
    ) {
        let catalog = self.committer.catalog().await;
        let Some(file) = catalog
            .files_all(FileTable::Spans)
            .chain(catalog.files_all(FileTable::Traces))
            .chain(catalog.files_all(FileTable::Scores))
            .find(|file| file.id == id)
            .cloned()
        else {
            // Replaced or quarantined since it was queued.
            return;
        };

        // The file and, for `spans`, the `traces` file derived from it: a job
        // on either must not run meanwhile.
        let mut claimed = vec![id];
        if file.table == FileTable::Spans {
            match self.committer.derived_traces(&[id]).await {
                Ok(pairs) => claimed.extend(
                    pairs
                        .into_iter()
                        .filter(|(_, spans)| *spans == id)
                        .map(|(traces, _)| traces),
                ),
                Err(_) => {
                    report.failed += 1;
                    self.verify.requeue(id);
                    return;
                }
            }
        }
        {
            let mut claims = self.claims.lock().unwrap_or_else(PoisonError::into_inner);
            if claimed.iter().any(|claim| claims.contains(claim)) {
                drop(claims);
                // A running job holds it; a later pass looks again.
                self.verify.requeue(id);
                return;
            }
            for claim in &claimed {
                claims.insert(*claim);
            }
        }
        let guard = ClaimGuard {
            compactor: Arc::clone(self),
            ids: claimed.clone(),
        };

        // Straight from the object store: a corrupt cached block must not
        // make a healthy file look corrupt (spec 03 §10).
        let store = self.objects.raw_store();
        match verify_file(&store, &file).await {
            Verdict::Healthy => {
                self.verify.healthy(id, now);
                report.healthy += 1;
            }
            Verdict::Unavailable => {
                self.verify.requeue(id);
                report.unavailable += 1;
            }
            Verdict::Corrupt { reason } => {
                tracing::error!(
                    file_id = %id,
                    table = file.table.as_str(),
                    project = %file.project,
                    day = %file.day,
                    reason,
                    "corrupt data file"
                );
                match file.table {
                    FileTable::Spans | FileTable::Scores => {
                        self.quarantine(&file, &claimed, report).await;
                    }
                    FileTable::Traces => {
                        // Its rows are the spans' rows: rebuild it from them.
                        drop(guard);
                        self.rebuild_traces(&file, policy, report).await;
                    }
                }
                return;
            }
        }
        drop(guard);
    }

    /// Quarantines a corrupt `spans` or `scores` file with the claimed
    /// `traces` file, if any (`claimed` holds the file first).
    async fn quarantine(
        &self,
        file: &Arc<FileMeta>,
        claimed: &[FileId],
        report: &mut VerifyReport,
    ) {
        self.hooks.pause(HookPoint::QuarantineBeforeCommit);
        if self.hooks.fail(HookPoint::QuarantineBeforeCommit).is_some() {
            report.failed += 1;
            self.verify.requeue(file.id);
            return;
        }
        let edit = CatalogEdit {
            quarantine: claimed.to_vec(),
            ..Default::default()
        };
        let published = Arc::clone(&self.published);
        match self
            .committer
            .commit(&edit, move |catalog| publish_catalog(&published, catalog))
            .await
        {
            Ok(_) => {
                tracing::error!(
                    file_id = %file.id,
                    table = file.table.as_str(),
                    project = %file.project,
                    day = %file.day,
                    rows = file.rows,
                    "file quarantined; its rows are lost"
                );
                report.quarantined += 1;
            }
            Err(_) => {
                report.failed += 1;
                self.verify.requeue(file.id);
                tracing::warn!(file_id = %file.id, "quarantining a corrupt file failed");
            }
        }
    }

    /// Rebuilds a corrupt `traces` file by rewriting each of its `spans`
    /// sources. The rewrite recomputes the `traces` file and obsoletes this one.
    async fn rebuild_traces(
        self: &Arc<Self>,
        file: &Arc<FileMeta>,
        policy: Policy,
        report: &mut VerifyReport,
    ) {
        let id = file.id;
        let sources = match self.committer.traces_sources(id).await {
            Ok(sources) if !sources.is_empty() => sources,
            _ => {
                report.failed += 1;
                self.verify.requeue(id);
                return;
            }
        };
        let catalog = self.committer.catalog().await;
        let mut inputs: Vec<Arc<FileMeta>> = Vec::new();
        for source in &sources {
            let found = catalog
                .files(FileTable::Spans, file.project, file.day..=file.day)
                .find(|candidate| candidate.id == *source)
                .cloned();
            match found {
                Some(found) => inputs.push(found),
                None => {
                    // The catalog moved on; a later pass looks again.
                    self.verify.requeue(id);
                    return;
                }
            }
        }
        {
            let mut claims = self.claims.lock().unwrap_or_else(PoisonError::into_inner);
            if inputs.iter().any(|input| claims.contains(&input.id)) {
                drop(claims);
                self.verify.requeue(id);
                return;
            }
            for input in &inputs {
                claims.insert(input.id);
            }
        }
        let _guard = ClaimGuard {
            compactor: Arc::clone(self),
            ids: inputs.iter().map(|input| input.id).collect(),
        };
        for input in inputs {
            let job = Job {
                table: FileTable::Spans,
                project: file.project,
                day: file.day,
                kind: JobKind::Rewrite,
                inputs: vec![input],
                whole_partition: false,
                drop_deleted: false,
            };
            if let Err(error) = self.run(job, policy).await {
                report.failed += 1;
                tracing::warn!(
                    file_id = %id,
                    error = error.label(),
                    "rebuilding a corrupt traces file failed"
                );
                self.verify.requeue(id);
                return;
            }
        }
        report.repaired += 1;
        tracing::warn!(
            file_id = %id,
            project = %file.project,
            day = %file.day,
            "corrupt traces file rebuilt from its spans"
        );
    }
}
