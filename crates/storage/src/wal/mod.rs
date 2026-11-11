//! Write-ahead log for one table.
//!
//! Spec 03 §1. Records, segment files, replay, and the writer task live here.
#![allow(dead_code)]

mod record;
mod replay;
mod segment;
pub(crate) mod writer;

pub(crate) use replay::replay;
pub(crate) use segment::SegmentWriter;

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use arrow::datatypes::SchemaRef;

use llmobs_core::schema::{scores_schema, spans_schema};

/// Which WAL a record belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Table {
    /// The `spans` table.
    Spans,
    /// The `scores` table.
    Scores,
}

impl Table {
    /// Directory and log name: `spans` or `scores`.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Spans => "spans",
            Self::Scores => "scores",
        }
    }

    /// Stored Arrow schema.
    pub(crate) fn schema(self) -> SchemaRef {
        match self {
            Self::Spans => spans_schema(),
            Self::Scores => scores_schema(),
        }
    }

    /// Column whose UTC day partitions the memtable.
    ///
    /// Spans use `start_time` until the trace anchor lands. Scores use `created_at`.
    pub(crate) fn time_column(self) -> &'static str {
        match self {
            Self::Spans => llmobs_core::schema::spans::START_TIME,
            Self::Scores => llmobs_core::schema::scores::CREATED_AT,
        }
    }
}

/// `00000000000000000042.wal` for seq 42. Lexical order matches numeric order.
pub(crate) fn segment_name(first_seq: u64) -> String {
    format!("{first_seq:020}.wal")
}

/// Parses a name from [`segment_name`].
///
/// Anything else — a short number, a non-numeric name, another extension — is
/// `None`. Callers ignore those files and do not delete them.
pub(crate) fn parse_segment_name(name: &str) -> Option<u64> {
    let digits = name.strip_suffix(".wal")?;
    if digits.len() != 20 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// Makes a new directory entry durable. `path` is the directory itself.
pub(crate) fn sync_dir(path: &Path) -> io::Result<()> {
    let dir = File::open(path)?;
    dir.sync_all()
}

/// How many segment files [`truncate_segments`] deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TruncateOutcome {
    /// Files removed. The newest segment is never one of them.
    pub deleted: u64,
}

/// Deletes segments whose every record is at or below `flushed_seq`.
///
/// Segment `k` goes only when a later segment exists and
/// `first_seq(k + 1) - 1 <= flushed_seq`. The newest segment stays, because
/// the writer may still be appending to it. Unknown files stay. The directory
/// is fsynced after a deletion.
pub(crate) fn truncate_segments(dir: &Path, flushed_seq: u64) -> io::Result<TruncateOutcome> {
    let segments = list_segment_files(dir)?;
    if segments.len() < 2 {
        return Ok(TruncateOutcome { deleted: 0 });
    }
    let mut deleted = 0;
    for index in 0..segments.len() - 1 {
        let covered = segments[index + 1].0.saturating_sub(1);
        if covered <= flushed_seq {
            std::fs::remove_file(&segments[index].1)?;
            deleted += 1;
        }
    }
    if deleted > 0 {
        sync_dir(dir)?;
    }
    Ok(TruncateOutcome { deleted })
}

fn list_segment_files(dir: &Path) -> io::Result<Vec<(u64, PathBuf)>> {
    let entries = std::fs::read_dir(dir)?;
    let mut segments = Vec::new();
    for entry in entries {
        let entry = entry?;
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        match parse_segment_name(&name) {
            Some(seq) => segments.push((seq, entry.path())),
            None => tracing::warn!(file = name.as_ref(), "ignoring unrecognized WAL file"),
        }
    }
    segments.sort_by_key(|(seq, _)| *seq);
    Ok(segments)
}

/// One span, for segment and replay tests.
#[cfg(test)]
pub(crate) fn sample_span_batch() -> arrow::array::RecordBatch {
    spans_at(&[1_700_000_000_000_000_000])
}

/// One span per start time, for memtable tests.
#[cfg(test)]
pub(crate) fn spans_at(starts: &[i64]) -> arrow::array::RecordBatch {
    spans_named(starts, "root")
}

/// [`spans_at`] with a chosen span name.
#[cfg(test)]
pub(crate) fn spans_named(starts: &[i64], name: &str) -> arrow::array::RecordBatch {
    use std::collections::BTreeMap;

    use llmobs_core::batch::spans_to_record_batch;
    use llmobs_core::enums::{SpanKind, SpanStatus};
    use llmobs_core::ids::{ProjectId, SpanId, TraceId};
    use llmobs_core::span::Span;

    let project_id = "018f6b4e-7c1a-7c1a-8c1a-000000000001"
        .parse::<ProjectId>()
        .unwrap();
    let trace_id = "4bf92f3577b34da6a3ce929d0e0e4736"
        .parse::<TraceId>()
        .unwrap();
    let spans: Vec<Span> = starts
        .iter()
        .enumerate()
        .map(|(index, start)| {
            let mut span_bytes = [0; 8];
            span_bytes[7] = u8::try_from(index + 1).unwrap_or(1);
            Span {
                project_id,
                trace_id,
                span_id: SpanId::from_bytes(span_bytes).unwrap(),
                parent_span_id: None,
                name: name.to_owned(),
                span_kind: SpanKind::Span,
                start_time: *start,
                end_time: start.saturating_add(100),
                status: SpanStatus::Ok,
                status_message: None,
                service_name: None,
                session_id: None,
                user_id: None,
                environment: None,
                release: None,
                tags: Vec::new(),
                metadata: BTreeMap::new(),
                provider: None,
                request_model: None,
                response_model: None,
                operation: None,
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
                reasoning_tokens: None,
                total_tokens: None,
                cost_input: None,
                cost_output: None,
                cost_total: None,
                cost_source: None,
                ttft_ns: None,
                request_params: None,
                input: None,
                input_format: None,
                output: None,
                output_format: None,
                input_preview: None,
                output_preview: None,
                system_instructions: None,
                tool_definitions: None,
                attributes: BTreeMap::new(),
                resource_attributes: BTreeMap::new(),
                scope_name: None,
                scope_version: None,
                events: Vec::new(),
                links: Vec::new(),
                media_refs: Vec::new(),
                truncated: Vec::new(),
                conventions: Vec::new(),
                ingest_time: start.saturating_add(200),
                schema_version: 1,
            }
        })
        .collect();
    spans_to_record_batch(&spans).unwrap()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{SegmentWriter, TruncateOutcome, segment_name, truncate_segments};
    use crate::hooks::Hooks;

    fn segments(dir: &std::path::Path, seqs: &[u64]) {
        for seq in seqs {
            drop(SegmentWriter::create(dir, *seq, 1024, Hooks::default()).unwrap());
        }
    }

    fn names(dir: &std::path::Path) -> Vec<String> {
        let mut found = Vec::new();
        for entry in fs::read_dir(dir).unwrap() {
            found.push(entry.unwrap().file_name().to_string_lossy().into_owned());
        }
        found.sort();
        found
    }

    #[test]
    fn truncation_keeps_the_newest_segment_and_unflushed_records() {
        let dir = tempfile::tempdir().unwrap();
        segments(dir.path(), &[1, 10, 20]);
        fs::write(dir.path().join("notes.txt"), b"keep").unwrap();

        let kept = truncate_segments(dir.path(), 8).unwrap();
        assert_eq!(kept, TruncateOutcome { deleted: 0 });
        assert_eq!(
            names(dir.path()),
            vec![
                segment_name(1),
                segment_name(10),
                segment_name(20),
                "notes.txt".to_owned(),
            ]
        );

        let covered = truncate_segments(dir.path(), 9).unwrap();
        assert_eq!(covered, TruncateOutcome { deleted: 1 });
        assert_eq!(
            names(dir.path()),
            vec![segment_name(10), segment_name(20), "notes.txt".to_owned(),]
        );

        let rest = truncate_segments(dir.path(), u64::MAX).unwrap();
        assert_eq!(rest, TruncateOutcome { deleted: 1 });
        assert_eq!(
            names(dir.path()),
            vec![segment_name(20), "notes.txt".to_owned()]
        );
        assert!(fs::read(dir.path().join("notes.txt")).unwrap() == b"keep");
    }

    #[test]
    fn truncation_of_one_segment_deletes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        segments(dir.path(), &[1]);
        let outcome = truncate_segments(dir.path(), u64::MAX).unwrap();
        assert_eq!(outcome, TruncateOutcome { deleted: 0 });
        assert_eq!(names(dir.path()), vec![segment_name(1)]);
    }
}
