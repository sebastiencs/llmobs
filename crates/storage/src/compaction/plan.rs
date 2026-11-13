//! The compaction policy: which job, on which files (spec 03 §5).
//!
//! [`plan`] is pure: it turns the catalog, the claimed files and `now` into
//! the list of jobs, with no I/O, so every rule is unit-tested here.

use std::collections::HashSet;
use std::sync::Arc;

use chrono::NaiveDate;
use llmobs_core::ids::ProjectId;

use crate::catalog::{Catalog, FileId, FileMeta, FileTable};
use crate::config::CompactionConfig;

/// The policy values, from `[storage.compaction]` (M4.6) or set directly by tests.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Policy {
    /// Output files are cut at this many bytes.
    pub(crate) target_file_size: u64,
    /// An L0 merge includes an L1 file only if the output so far is at least
    /// this multiple of the L1 file's size.
    pub(crate) size_ratio: u64,
    /// L0 merges are due at this many L0 files.
    pub(crate) l0_files: usize,
    /// L0 merges are due at this many bytes of L0.
    pub(crate) l0_bytes: u64,
    /// L0 merges are due at this age of the newest L0 file, ns.
    pub(crate) l0_age: i64,
    /// A day close runs this long after the partition day ended, ns.
    pub(crate) close_delay: i64,
}

impl From<&CompactionConfig> for Policy {
    fn from(config: &CompactionConfig) -> Self {
        Self {
            target_file_size: config.target_file_size.0,
            size_ratio: config.size_ratio,
            l0_files: config.l0_files,
            l0_bytes: config.l0_bytes.0,
            l0_age: i64::try_from(config.l0_age.as_nanos()).unwrap_or(i64::MAX),
            close_delay: i64::try_from(config.close_delay.as_nanos()).unwrap_or(i64::MAX),
        }
    }
}

/// The kind of one compaction job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JobKind {
    /// Merge the L0 files (plus L1 files by size ratio) of a partition.
    L0Merge,
    /// Close the partition day: everything (or, for a big `spans` day, the L0
    /// files) into level-1 files.
    DayClose,
    /// Rewrite one file without its covered rows (spec 03 §7). Built by the
    /// deletion pass, never by [`plan`].
    Rewrite,
}

/// One compaction job: some files of one `(table, project, day)` partition.
#[derive(Debug, Clone)]
pub(crate) struct Job {
    /// `Spans` or `Scores`, never `Traces`.
    pub(crate) table: FileTable,
    /// Owning project.
    pub(crate) project: ProjectId,
    /// Partition day.
    pub(crate) day: NaiveDate,
    /// Which rule picked the job.
    pub(crate) kind: JobKind,
    /// Input files, ascending `FileId` (oldest first).
    pub(crate) inputs: Vec<Arc<FileMeta>>,
    /// The job took every file of the partition.
    pub(crate) whole_partition: bool,
    /// Scores only: drop rows whose latest version is deleted (day close).
    pub(crate) drop_deleted: bool,
}

/// The jobs a pass should run now, in table, project and day order.
pub(crate) fn plan(
    catalog: &Catalog,
    claimed: &HashSet<FileId>,
    policy: &Policy,
    now: i64,
) -> Vec<Job> {
    let mut jobs = Vec::new();
    for table in [FileTable::Spans, FileTable::Scores] {
        // `files_all` walks projects then days in `BTreeMap` order; the files
        // of one partition are contiguous in it.
        let mut current: Option<(ProjectId, NaiveDate)> = None;
        let mut files: Vec<Arc<FileMeta>> = Vec::new();
        let flush_partition = |current: &mut Option<(ProjectId, NaiveDate)>,
                               files: &mut Vec<Arc<FileMeta>>,
                               jobs: &mut Vec<Job>| {
            if let Some((project, day)) = *current
                && let Some(job) = partition_job(table, project, day, files, claimed, policy, now)
            {
                jobs.push(job);
            }
            files.clear();
        };
        for file in catalog.files_all(table) {
            let key = (file.project, file.day);
            if current != Some(key) {
                flush_partition(&mut current, &mut files, &mut jobs);
                current = Some(key);
            }
            files.push(Arc::clone(file));
        }
        flush_partition(&mut current, &mut files, &mut jobs);
    }
    jobs
}

/// The measures of one partition (rules 2–4).
struct Partition<'a> {
    table: FileTable,
    project: ProjectId,
    day: NaiveDate,
    /// `(day + 1 day)` at 00:00:00 UTC, ns.
    day_end: i64,
    files: &'a [Arc<FileMeta>],
    l0: Vec<&'a Arc<FileMeta>>,
    /// Sum of `size` over `files`.
    total: u64,
    /// Greatest `max_ingest_time` over `l0`, if any.
    newest_l0: Option<i64>,
    closed: bool,
}

/// The job for one partition, if any (spec 03 §5 "Policy, per partition").
fn partition_job(
    table: FileTable,
    project: ProjectId,
    day: NaiveDate,
    files: &[Arc<FileMeta>],
    claimed: &HashSet<FileId>,
    policy: &Policy,
    now: i64,
) -> Option<Job> {
    if files.is_empty() || files.iter().any(|file| claimed.contains(&file.id)) {
        return None;
    }
    // Rule 3: a partition whose day end cannot be computed is skipped.
    let next = day.succ_opt()?;
    let day_end = next
        .and_hms_opt(0, 0, 0)
        .and_then(|time| time.and_utc().timestamp_nanos_opt())?;
    let l0: Vec<&Arc<FileMeta>> = files.iter().filter(|file| file.level == 0).collect();
    let total = files
        .iter()
        .fold(0_u64, |sum, file| sum.saturating_add(file.size));
    let newest_l0 = l0.iter().map(|file| file.max_ingest_time).max();

    let closed = match table {
        FileTable::Spans => l0.is_empty() && (files.len() <= 1 || total > policy.target_file_size),
        FileTable::Scores => files.iter().all(|file| file.fully_merged),
        FileTable::Traces => return None,
    };
    let partition = Partition {
        table,
        project,
        day,
        day_end,
        files,
        l0,
        total,
        newest_l0,
        closed,
    };
    day_close_job(&partition, policy, now).or_else(|| l0_merge_job(&partition, policy, now))
}

/// Rule 5: the day close, due once the day plus the delay has passed.
fn day_close_job(part: &Partition<'_>, policy: &Policy, now: i64) -> Option<Job> {
    if now < part.day_end.saturating_add(policy.close_delay) || part.closed {
        return None;
    }
    if !part
        .newest_l0
        .is_none_or(|time| now.saturating_sub(time) >= policy.close_delay)
    {
        return None;
    }
    let whole = part.table == FileTable::Scores || part.total <= policy.target_file_size;
    let mut inputs: Vec<Arc<FileMeta>> = if whole {
        part.files.iter().map(Arc::clone).collect()
    } else {
        part.l0.iter().map(|file| Arc::clone(*file)).collect()
    };
    inputs.sort_unstable_by_key(|file| file.id);
    Some(Job {
        table: part.table,
        project: part.project,
        day: part.day,
        kind: JobKind::DayClose,
        inputs,
        whole_partition: whole,
        drop_deleted: part.table == FileTable::Scores,
    })
}

/// Rule 6: the L0 merge, due on count, bytes or age.
fn l0_merge_job(part: &Partition<'_>, policy: &Policy, now: i64) -> Option<Job> {
    let l0_total = part
        .l0
        .iter()
        .fold(0_u64, |sum, file| sum.saturating_add(file.size));
    let due = part.l0.len() >= policy.l0_files
        || l0_total >= policy.l0_bytes
        || (part.l0.len() >= 2
            && part
                .newest_l0
                .is_some_and(|time| now.saturating_sub(time) >= policy.l0_age));
    if !due {
        return None;
    }
    // The size ratio (G12): include an L1 file only while the output so far is
    // at least `size_ratio` times its size, smallest first.
    let mut l1: Vec<&Arc<FileMeta>> = part.files.iter().filter(|file| file.level == 1).collect();
    l1.sort_unstable_by(|left, right| {
        left.size
            .cmp(&right.size)
            .then_with(|| left.id.cmp(&right.id))
    });
    let mut inputs: Vec<Arc<FileMeta>> = part.l0.iter().map(|file| Arc::clone(*file)).collect();
    let mut out = l0_total;
    for file in l1 {
        if out >= policy.size_ratio.saturating_mul(file.size) {
            out = out.saturating_add(file.size);
            inputs.push(Arc::clone(file));
        } else {
            break;
        }
    }
    if inputs.len() < 2 {
        return None;
    }
    let whole_partition = inputs.len() == part.files.len();
    inputs.sort_unstable_by_key(|file| file.id);
    Some(Job {
        table: part.table,
        project: part.project,
        day: part.day,
        kind: JobKind::L0Merge,
        inputs,
        whole_partition,
        drop_deleted: false,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{JobKind, Policy, plan};
    use crate::catalog::{Catalog, CatalogEdit, FileId, FileMeta, FileTable};

    fn policy() -> Policy {
        Policy {
            target_file_size: 100,
            size_ratio: 4,
            l0_files: 8,
            l0_bytes: 1000,
            l0_age: 10 * 60 * 1_000_000_000,
            close_delay: 2 * 3_600 * 1_000_000_000,
        }
    }

    /// `now` = D 12:00, in ns.
    fn noon() -> i64 {
        day()
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_nanos_opt()
            .unwrap()
    }

    fn day() -> chrono::NaiveDate {
        chrono::NaiveDate::from_ymd_opt(2026, 3, 5).unwrap()
    }

    fn project() -> llmobs_core::ids::ProjectId {
        "018f6b4e-7c1a-7c1a-8c1a-000000000001".parse().unwrap()
    }

    fn meta(
        table: FileTable,
        day: chrono::NaiveDate,
        level: u8,
        size: u64,
        max_ingest_time: i64,
        fully_merged: bool,
    ) -> FileMeta {
        FileMeta {
            id: FileId::new_v7(),
            table,
            project: project(),
            day,
            level,
            path: format!("tables/{size}-{}", FileId::new_v7()),
            size,
            footer_size: 1,
            rows: 1,
            min_time: 0,
            max_time: 0,
            min_ingest_time: 0,
            max_ingest_time,
            fully_merged,
            deletions_applied_seq: 0,
        }
    }

    fn catalog(files: &[FileMeta]) -> Catalog {
        Catalog::empty()
            .apply(&CatalogEdit {
                add: files.to_vec(),
                remove: Vec::new(),
                flushed_seq: None,
                sources: Vec::new(),
                raise: Vec::new(),
            })
            .unwrap()
            .0
    }

    /// Fresh L0 spans files of `size` each: `max_ingest_time` is `now`.
    fn fresh_l0(count: usize, size: u64) -> Vec<FileMeta> {
        (0..count)
            .map(|_| meta(FileTable::Spans, day(), 0, size, noon(), false))
            .collect()
    }

    /// `n` minutes before `noon`, in ns.
    fn minutes_ago(n: i64) -> i64 {
        noon() - n * 60 * 1_000_000_000
    }

    /// D+1 02:00:00, in ns: the day plus the close delay.
    fn after_delay() -> i64 {
        day()
            .succ_opt()
            .unwrap()
            .and_hms_opt(2, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_nanos_opt()
            .unwrap()
    }

    /// Three hours before `after_delay`, in ns.
    fn three_hours_before_after_delay() -> i64 {
        after_delay() - 3 * 3_600 * 1_000_000_000
    }

    fn kinds(jobs: &[super::Job]) -> Vec<JobKind> {
        jobs.iter().map(|job| job.kind).collect()
    }

    #[test]
    fn an_l0_merge_needs_the_count_or_more() {
        assert!(
            plan(
                &catalog(&fresh_l0(7, 1)),
                &HashSet::new(),
                &policy(),
                noon()
            )
            .is_empty()
        );
        let files = fresh_l0(8, 1);
        let jobs = plan(&catalog(&files), &HashSet::new(), &policy(), noon());
        assert_eq!(kinds(&jobs), vec![JobKind::L0Merge]);
        assert_eq!(jobs[0].inputs.len(), 8);
        assert!(jobs[0].whole_partition);
    }

    #[test]
    fn l0_bytes_trigger_a_merge() {
        let files: Vec<_> = (0..2)
            .map(|_| meta(FileTable::Spans, day(), 0, 500, noon(), false))
            .collect();
        let jobs = plan(&catalog(&files), &HashSet::new(), &policy(), noon());
        assert_eq!(kinds(&jobs), vec![JobKind::L0Merge]);
    }

    #[test]
    fn l0_age_triggers_only_with_two_files() {
        let aged = |count: usize, newest: i64| -> Vec<FileMeta> {
            (0..count)
                .map(|_| meta(FileTable::Spans, day(), 0, 1, newest, false))
                .collect()
        };
        let ten_minutes = 10 * 60 * 1_000_000_000_i64;
        // The newest L0 exactly 10 min old: due.
        assert_eq!(
            kinds(&plan(
                &catalog(&aged(2, noon() - ten_minutes)),
                &HashSet::new(),
                &policy(),
                noon()
            )),
            vec![JobKind::L0Merge]
        );
        // 9 min 59 s: one second short.
        assert!(
            plan(
                &catalog(&aged(2, noon() - ten_minutes + 1_000_000_000)),
                &HashSet::new(),
                &policy(),
                noon()
            )
            .is_empty()
        );
        // One file, however old: not due.
        assert!(
            plan(
                &catalog(&aged(1, noon() - 3_600 * 1_000_000_000)),
                &HashSet::new(),
                &policy(),
                noon()
            )
            .is_empty()
        );
    }

    #[test]
    fn the_size_ratio_keeps_big_l1_files_out() {
        // Two aged L0 files summing 40, plus L1 files.
        let mut files: Vec<FileMeta> = (0..2)
            .map(|_| meta(FileTable::Spans, day(), 0, 20, minutes_ago(30), false))
            .collect();
        files.push(meta(FileTable::Spans, day(), 1, 5, 0, false));
        files.push(meta(FileTable::Spans, day(), 1, 10, 0, false));
        files.push(meta(FileTable::Spans, day(), 1, 20, 0, false));
        let l1_of = |size: u64| {
            files
                .iter()
                .find(|file| file.level == 1 && file.size == size)
                .unwrap()
                .id
        };

        let jobs = plan(&catalog(&files), &HashSet::new(), &policy(), noon());
        assert_eq!(kinds(&jobs), vec![JobKind::L0Merge]);
        let input_ids: HashSet<_> = jobs[0].inputs.iter().map(|file| file.id).collect();
        assert_eq!(input_ids.len(), 4, "L0 + L1 of 5 + L1 of 10");
        assert!(input_ids.contains(&l1_of(5)), "the L1 of 5 is an input");
        assert!(input_ids.contains(&l1_of(10)), "the L1 of 10 is an input");
        assert!(!input_ids.contains(&l1_of(20)), "the L1 of 20 stays out");

        // The catalog walks a partition in `(min_time, id)` order, which with
        // fresh v7 ids is creation order: the L1 of 20 sorts before the L1 of 5.
        // The policy must still check the 5 first: out = 40 >= 4 x 5, so it is
        // added (out = 45); then 45 < 4 x 20 = 80, so the 20 stays out.
        let mut files: Vec<FileMeta> = (0..2)
            .map(|_| meta(FileTable::Spans, day(), 0, 20, minutes_ago(30), false))
            .collect();
        let big = meta(FileTable::Spans, day(), 1, 20, 0, false);
        let small = meta(FileTable::Spans, day(), 1, 5, 0, false);
        let (big_id, small_id) = (big.id, small.id);
        files.push(big);
        files.push(small);
        let jobs = plan(&catalog(&files), &HashSet::new(), &policy(), noon());
        assert_eq!(kinds(&jobs), vec![JobKind::L0Merge]);
        let input_ids: HashSet<_> = jobs[0].inputs.iter().map(|file| file.id).collect();
        assert_eq!(input_ids.len(), 3, "L0 + the L1 of 5 only");
        assert!(input_ids.contains(&small_id), "the L1 of 5 is an input");
        assert!(!input_ids.contains(&big_id), "the L1 of 20 stays out");
    }

    #[test]
    fn a_claimed_file_hides_its_whole_partition() {
        let files = fresh_l0(8, 1);
        let claimed: HashSet<_> = files.iter().map(|file| file.id).take(1).collect();
        assert!(plan(&catalog(&files), &claimed, &policy(), noon()).is_empty());
    }

    #[test]
    fn a_day_close_waits_for_the_day_and_the_delay() {
        // D+1 01:59:59: not yet due, and the files are fresh, so no L0 rule
        // fires either.
        let before = day()
            .succ_opt()
            .unwrap()
            .and_hms_opt(1, 59, 59)
            .unwrap()
            .and_utc()
            .timestamp_nanos_opt()
            .unwrap();
        let fresh: Vec<_> = (0..3)
            .map(|_| {
                meta(
                    FileTable::Spans,
                    day(),
                    0,
                    10,
                    before - 1_000_000_000,
                    false,
                )
            })
            .collect();
        assert!(plan(&catalog(&fresh), &HashSet::new(), &policy(), before).is_empty());

        // D+1 02:00:00 with the newest L0 three hours old.
        let due = after_delay();
        let files: Vec<_> = (0..3)
            .map(|_| {
                meta(
                    FileTable::Spans,
                    day(),
                    0,
                    10,
                    three_hours_before_after_delay(),
                    false,
                )
            })
            .collect();
        let jobs = plan(&catalog(&files), &HashSet::new(), &policy(), due);
        assert_eq!(kinds(&jobs), vec![JobKind::DayClose]);
        assert_eq!(jobs[0].inputs.len(), 3);
        assert!(jobs[0].whole_partition);
        assert!(!jobs[0].drop_deleted);
    }

    #[test]
    fn late_data_delays_the_day_close_and_merges_instead() {
        let due = after_delay();
        // The newest L0 is one hour old, younger than the close delay.
        let files: Vec<_> = (0..3)
            .map(|_| {
                meta(
                    FileTable::Spans,
                    day(),
                    0,
                    10,
                    due - 3_600 * 1_000_000_000,
                    false,
                )
            })
            .collect();
        let jobs = plan(&catalog(&files), &HashSet::new(), &policy(), due);
        assert_eq!(kinds(&jobs), vec![JobKind::L0Merge]);
        assert_eq!(jobs[0].inputs.len(), 3);
        // The merge takes every file (there is no L1), so it is whole.
        assert!(jobs[0].whole_partition);
    }

    #[test]
    fn a_big_day_closes_only_its_l0_files() {
        let due = after_delay();
        let mut files: Vec<FileMeta> = (0..2)
            .map(|_| meta(FileTable::Spans, day(), 1, 80, 0, false))
            .collect();
        files.extend((0..2).map(|_| {
            meta(
                FileTable::Spans,
                day(),
                0,
                10,
                three_hours_before_after_delay(),
                false,
            )
        }));
        let jobs = plan(&catalog(&files), &HashSet::new(), &policy(), due);
        assert_eq!(kinds(&jobs), vec![JobKind::DayClose]);
        assert_eq!(jobs[0].inputs.len(), 2, "the L0 files only");
        assert!(jobs[0].inputs.iter().all(|file| file.level == 0));
        assert!(!jobs[0].whole_partition);
    }

    #[test]
    fn a_closed_spans_day_needs_nothing() {
        let due = after_delay();
        // One L1 file: closed.
        let one = vec![meta(FileTable::Spans, day(), 1, 80, 0, false)];
        assert!(plan(&catalog(&one), &HashSet::new(), &policy(), due).is_empty());
        // Two L1 files over the target: closed.
        let big = vec![
            meta(FileTable::Spans, day(), 1, 80, 0, false),
            meta(FileTable::Spans, day(), 1, 70, 0, false),
        ];
        assert!(plan(&catalog(&big), &HashSet::new(), &policy(), due).is_empty());
        // Two L1 files under the target: one whole-partition day close.
        let small = vec![
            meta(FileTable::Spans, day(), 1, 30, 0, false),
            meta(FileTable::Spans, day(), 1, 30, 0, false),
        ];
        let jobs = plan(&catalog(&small), &HashSet::new(), &policy(), due);
        assert_eq!(kinds(&jobs), vec![JobKind::DayClose]);
        assert_eq!(jobs[0].inputs.len(), 2);
        assert!(jobs[0].whole_partition);
    }

    #[test]
    fn a_scores_day_close_takes_every_file() {
        let due = after_delay();
        let files = vec![
            meta(
                FileTable::Scores,
                day(),
                0,
                80,
                three_hours_before_after_delay(),
                false,
            ),
            meta(FileTable::Scores, day(), 1, 80, 0, true),
        ];
        let jobs = plan(&catalog(&files), &HashSet::new(), &policy(), due);
        assert_eq!(kinds(&jobs), vec![JobKind::DayClose]);
        assert_eq!(jobs[0].inputs.len(), 2);
        assert!(jobs[0].whole_partition);
        assert!(jobs[0].drop_deleted);

        let merged = vec![
            meta(FileTable::Scores, day(), 1, 80, 0, true),
            meta(FileTable::Scores, day(), 1, 80, 0, true),
        ];
        assert!(plan(&catalog(&merged), &HashSet::new(), &policy(), due).is_empty());
    }

    #[test]
    fn traces_and_the_last_day_are_ignored() {
        let mut files = fresh_l0(8, 1);
        files.push(meta(FileTable::Traces, day(), 0, 1, noon(), false));
        let jobs = plan(&catalog(&files), &HashSet::new(), &policy(), noon());
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].table, FileTable::Spans);

        // A partition at `NaiveDate::MAX` has no day end; it is skipped.
        let last = vec![
            meta(
                FileTable::Spans,
                chrono::NaiveDate::MAX,
                0,
                1,
                noon(),
                false
            );
            8
        ];
        assert!(plan(&catalog(&last), &HashSet::new(), &policy(), noon()).is_empty());
    }
}
