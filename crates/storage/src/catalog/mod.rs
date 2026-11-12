//! In-memory catalog. `catalog.db` is loaded in [`db`] and published by [`committer`].
//!
//! `Storage::open` loads this from `catalog.db` and publishes it with the
//! replayed memtables. A flush replaces it in the same publish that drops
//! `frozen`.
#![allow(dead_code)]

mod committer;
mod db;

pub(crate) use committer::{CatalogEdit, Committer, ObsoleteRemoval, StagedRemoval, now_ns};

use std::collections::BTreeMap;
use std::ops::RangeInclusive;
use std::sync::Arc;

use chrono::NaiveDate;
use llmobs_core::ids::ProjectId;
use uuid::Uuid;

use crate::error::CommitError;
use crate::wal::Table;

/// A table that has Parquet files.
///
/// Unlike [`Table`], this has a slot for `traces`, which has files but no WAL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileTable {
    Spans,
    Traces,
    Scores,
}

impl FileTable {
    /// The `data_files.table_name` value and the `tables/<table>/` path segment.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Spans => "spans",
            Self::Traces => "traces",
            Self::Scores => "scores",
        }
    }
}

impl From<Table> for FileTable {
    fn from(table: Table) -> Self {
        match table {
            Table::Spans => Self::Spans,
            Table::Scores => Self::Scores,
        }
    }
}

/// One Parquet object. The inner UUID is the `data_files.id` primary key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct FileId(pub(crate) Uuid);

impl FileId {
    /// A new UUIDv7.
    pub(crate) fn new_v7() -> Self {
        Self(Uuid::now_v7())
    }
}

impl std::fmt::Display for FileId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0.as_hyphenated(), formatter)
    }
}

/// Where a catalogued file is allowed to be.
///
/// A remove names the state the row must have. The update then marks it
/// `obsolete` and sets `obsoleted_at`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FileState {
    /// Readable. These rows are the in-memory catalog.
    Active,
    /// Replaced. GC deletes it after `gc.grace`.
    Obsolete,
    /// Unreadable. Only accepted as an expected state.
    Quarantined,
}

impl FileState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Obsolete => "obsolete",
            Self::Quarantined => "quarantined",
        }
    }
}

/// One active file. Immutable once it is inside a published [`Catalog`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileMeta {
    /// Primary key.
    pub(crate) id: FileId,
    /// The table this file belongs to.
    pub(crate) table: FileTable,
    /// Owning project.
    pub(crate) project: ProjectId,
    /// Partition day.
    pub(crate) day: NaiveDate,
    /// `0` or `1`.
    pub(crate) level: u8,
    /// Path relative to the object-store root.
    pub(crate) path: String,
    /// Object size in bytes.
    pub(crate) size: u64,
    /// Footer size in bytes.
    pub(crate) footer_size: u64,
    /// Row count.
    pub(crate) rows: u64,
    /// Smallest event time, nanoseconds.
    pub(crate) min_time: i64,
    /// Largest event time, nanoseconds.
    pub(crate) max_time: i64,
    /// Smallest ingest time, nanoseconds.
    pub(crate) min_ingest_time: i64,
    /// Largest ingest time, nanoseconds.
    pub(crate) max_ingest_time: i64,
    /// Every row in the file is a merged version.
    pub(crate) fully_merged: bool,
    /// No row of a deletion with `deletion_seq` at or below this remains in the
    /// file (spec 03 §7).
    pub(crate) deletions_applied_seq: u64,
}

/// `spans`, `traces`, then `scores`, indexed by [`FileTable`] via [`file_index`].
pub(super) type ProjectDays = Arc<BTreeMap<NaiveDate, Vec<Arc<FileMeta>>>>;
pub(super) type Tables = [BTreeMap<ProjectId, ProjectDays>; 3];

/// Active files. Immutable once published: a commit builds a new value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Catalog {
    /// Table, then project, then day. Each day's files are ordered by `(min_time, id)`.
    tables: Tables,
}

impl Catalog {
    /// No files.
    pub(crate) fn empty() -> Self {
        Self {
            tables: [BTreeMap::new(), BTreeMap::new(), BTreeMap::new()],
        }
    }

    /// Every active file of `table`, in project then day order.
    #[cfg(test)]
    pub(crate) fn test_files(&self, table: FileTable) -> Vec<Arc<FileMeta>> {
        self.tables[file_index(table)]
            .values()
            .flat_map(|days| days.values())
            .flatten()
            .map(Arc::clone)
            .collect()
    }

    /// Every active file of `table`, across projects and days, in key order.
    pub(crate) fn files_all(&self, table: FileTable) -> impl Iterator<Item = &Arc<FileMeta>> {
        self.tables[file_index(table)]
            .values()
            .flat_map(|days| days.values())
            .flatten()
    }

    /// Files for `project` on the inclusive day range, in `(min_time, id)` order.
    ///
    /// A reversed range yields nothing.
    pub(crate) fn files(
        &self,
        table: FileTable,
        project: ProjectId,
        days: RangeInclusive<NaiveDate>,
    ) -> impl Iterator<Item = &Arc<FileMeta>> {
        let project_days = (days.start() <= days.end())
            .then(|| self.tables[file_index(table)].get(&project))
            .flatten();
        project_days.into_iter().flat_map(move |days_map| {
            days_map
                .range(days.clone())
                .flat_map(|(_, files)| files.iter())
        })
    }

    /// Clone the maps a remove or an add touches. Other projects stay the same `Arc`.
    ///
    /// Returns the next catalog and the [`FileMeta`] of every file an `Active`
    /// remove took out. A remove with another expected state changes nothing in
    /// memory and returns nothing for it.
    pub(super) fn apply(
        &self,
        edit: &CatalogEdit,
    ) -> Result<(Self, Vec<Arc<FileMeta>>), CommitError> {
        let mut tables = self.tables.clone();
        let mut removed = Vec::new();
        for (id, state) in &edit.remove {
            if *state != FileState::Active {
                continue;
            }
            match remove_active(&mut tables, *id) {
                Some(file) => removed.push(file),
                None => {
                    return Err(CommitError::conflict("file is not active in the catalog"));
                }
            }
        }
        for (id, seq) in &edit.raise {
            // Before `add`: the SQL check rejects a raise of an added file
            // before its row is inserted, and this keeps the two in step.
            let replaced = replace_active(&mut tables, *id, |file| FileMeta {
                deletions_applied_seq: *seq,
                ..file.clone()
            });
            if !replaced {
                return Err(CommitError::conflict("raised file is not active"));
            }
        }
        for file in &edit.add {
            insert_sorted(&mut tables, file.clone());
        }
        Ok((Self { tables }, removed))
    }
}

/// Index into the WAL-sized arrays (`flushed`, `replayed`): one slot per WAL.
///
/// Do not use this for [`Tables`], which has a slot per file table.
pub(super) fn table_index(table: Table) -> usize {
    match table {
        Table::Spans => 0,
        Table::Scores => 1,
    }
}

/// Index into [`Tables`]: one slot per file table, `traces` included.
///
/// Do not use this for the WAL-sized arrays; those use [`table_index`].
pub(super) fn file_index(table: FileTable) -> usize {
    match table {
        FileTable::Spans => 0,
        FileTable::Traces => 1,
        FileTable::Scores => 2,
    }
}

pub(super) fn insert_sorted(tables: &mut Tables, file: FileMeta) {
    let days = tables[file_index(file.table)]
        .entry(file.project)
        .or_insert_with(|| Arc::new(BTreeMap::new()));
    let map = Arc::make_mut(days);
    let files = map.entry(file.day).or_default();
    let key = (file.min_time, file.id);
    let position = files.partition_point(|existing| (existing.min_time, existing.id) < key);
    files.insert(position, Arc::new(file));
}

/// Replace the `Arc<FileMeta>` of the active file `id` in place, keeping its
/// day's order. `false` when the id is not an active file.
fn replace_active(tables: &mut Tables, id: FileId, f: impl FnOnce(&FileMeta) -> FileMeta) -> bool {
    for table in tables.iter_mut() {
        let Some((project, day)) = find_file(table, id) else {
            continue;
        };
        let Some(days) = table.get_mut(&project) else {
            continue;
        };
        let map = Arc::make_mut(days);
        let Some(files) = map.get_mut(&day) else {
            continue;
        };
        let Some(position) = files.iter().position(|file| file.id == id) else {
            return false;
        };
        files[position] = Arc::new(f(&files[position]));
        return true;
    }
    false
}

fn remove_active(tables: &mut Tables, id: FileId) -> Option<Arc<FileMeta>> {
    for table in tables.iter_mut() {
        let Some((project, day)) = find_file(table, id) else {
            continue;
        };
        let Some(days) = table.get_mut(&project) else {
            continue;
        };
        let map = Arc::make_mut(days);
        let Some(files) = map.get_mut(&day) else {
            continue;
        };
        let position = files.iter().position(|file| file.id == id)?;
        let file = Arc::clone(&files[position]);
        files.remove(position);
        if files.is_empty() {
            map.remove(&day);
        }
        if map.is_empty() {
            table.remove(&project);
        }
        return Some(file);
    }
    None
}

fn find_file(
    table: &BTreeMap<ProjectId, ProjectDays>,
    id: FileId,
) -> Option<(ProjectId, NaiveDate)> {
    for (project, days) in table {
        for (day, files) in days.iter() {
            if files.iter().any(|file| file.id == id) {
                return Some((*project, *day));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> ProjectId {
        "018f6b4e-7c1a-7c1a-8c1a-000000000001".parse().unwrap()
    }

    fn day(value: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2024, 1, value).unwrap()
    }

    fn file(min_time: i64, day: NaiveDate) -> FileMeta {
        FileMeta {
            id: FileId::new_v7(),
            table: FileTable::Spans,
            project: project(),
            day,
            level: 0,
            path: format!("objects/{min_time}"),
            size: 1,
            footer_size: 1,
            rows: 1,
            min_time,
            max_time: min_time,
            min_ingest_time: min_time,
            max_ingest_time: min_time,
            fully_merged: false,
            deletions_applied_seq: 0,
        }
    }

    #[test]
    fn files_are_ordered_and_a_reversed_range_is_empty() {
        let edit = CatalogEdit {
            add: vec![file(20, day(2)), file(10, day(2)), file(5, day(3))],
            remove: Vec::new(),
            flushed_seq: None,
            sources: Vec::new(),
            raise: Vec::new(),
        };
        let (catalog, removed) = Catalog::empty().apply(&edit).unwrap();
        assert!(removed.is_empty());
        let ids: Vec<_> = catalog
            .files(FileTable::Spans, project(), day(2)..=day(2))
            .map(|file| file.min_time)
            .collect();
        assert_eq!(ids, vec![10, 20]);
        let across: Vec<_> = catalog
            .files(FileTable::Spans, project(), day(2)..=day(3))
            .map(|file| file.min_time)
            .collect();
        assert_eq!(across, vec![10, 20, 5]);
        assert_eq!(
            catalog
                .files(FileTable::Spans, project(), day(3)..=day(2))
                .count(),
            0
        );
    }
}
