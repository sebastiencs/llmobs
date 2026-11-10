//! Replay a table's WAL directory.
//!
//! Spec 03 §1 recovery. A short read or a bad checksum at the tail of the last
//! segment is a torn write and is truncated. The same failure in an earlier
//! segment, a sequence that does not increase, or a checksum-valid payload
//! that is not one batch of this table, is corruption.
//!
//! Called from tests until `Storage::open` lands.
#![allow(dead_code)]

use std::path::Path;

use arrow::array::RecordBatch;

use crate::error::WalError;
use crate::wal::record::{Decoded, decode_next, read_batch};
use crate::wal::{Table, segment_name};

/// What [`replay`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReplayOutcome {
    /// `max(last valid seq, after_seq) + 1`. An empty directory is `after_seq + 1`.
    pub next_seq: u64,
    /// Records passed to `on_batch`. Records at or below `after_seq` are not counted.
    pub records: u64,
    /// Bytes removed from a torn tail.
    pub truncated_bytes: u64,
    /// Bytes left unread after a corrupt record when `skip_corrupt` is set.
    pub skipped_bytes: u64,
}

/// Reads every segment in `dir`, in seq order, and calls `on_batch` for each
/// record newer than `after_seq`.
///
/// `on_batch` is not called for skipped seqs, and those records are not decoded
/// as Arrow. `read_batch` failing is corruption wherever the record sits: the
/// checksum matched, so the bytes are what was written. With `skip_corrupt`,
/// that one record is skipped. A torn tail is truncated even when `skip_corrupt`
/// is set.
pub(crate) fn replay(
    dir: &Path,
    table: Table,
    after_seq: u64,
    skip_corrupt: bool,
    on_batch: &mut dyn FnMut(u64, RecordBatch) -> Result<(), WalError>,
) -> Result<ReplayOutcome, WalError> {
    let segments = list_segments(dir)?;
    let mut progress = Progress {
        previous: None,
        last_valid: None,
        records: 0,
        truncated_bytes: 0,
        skipped_bytes: 0,
    };

    for (index, (first_seq, path)) in segments.iter().enumerate() {
        let name = segment_name(*first_seq);
        let bytes = std::fs::read(path)
            .map_err(|source| WalError::io("reading WAL segment", path, source))?;
        let is_last = index + 1 == segments.len();
        let mut scan = Scan {
            table,
            name: &name,
            after_seq,
            skip_corrupt,
            is_last,
            progress: &mut progress,
        };
        if !scan_segment(&bytes, path, &mut scan, on_batch)? {
            break;
        }
    }

    let base = progress
        .last_valid
        .map_or(after_seq, |seq| seq.max(after_seq));
    let Some(next_seq) = base.checked_add(1) else {
        let segment = segments
            .last()
            .map(|(seq, _)| segment_name(*seq))
            .unwrap_or_default();
        return Err(WalError::Corrupt {
            table: table.name(),
            segment,
            offset: 0,
        });
    };
    let outcome = ReplayOutcome {
        next_seq,
        records: progress.records,
        truncated_bytes: progress.truncated_bytes,
        skipped_bytes: progress.skipped_bytes,
    };
    tracing::info!(
        table = table.name(),
        records = outcome.records,
        next_seq = outcome.next_seq,
        truncated_bytes = outcome.truncated_bytes,
        skipped_bytes = outcome.skipped_bytes,
        "replayed WAL"
    );
    Ok(outcome)
}

struct Progress {
    previous: Option<u64>,
    last_valid: Option<u64>,
    records: u64,
    truncated_bytes: u64,
    skipped_bytes: u64,
}

/// One pass over a segment's bytes.
struct Scan<'a> {
    table: Table,
    name: &'a str,
    after_seq: u64,
    skip_corrupt: bool,
    is_last: bool,
    progress: &'a mut Progress,
}

/// Returns `true` when the caller should read the next segment.
fn scan_segment(
    bytes: &[u8],
    path: &Path,
    scan: &mut Scan<'_>,
    on_batch: &mut dyn FnMut(u64, RecordBatch) -> Result<(), WalError>,
) -> Result<bool, WalError> {
    let mut offset = 0;
    while offset < bytes.len() {
        match decode_next(bytes, offset) {
            Decoded::End => break,
            Decoded::Record { seq, payload, next } => {
                if scan.progress.previous.is_some_and(|prev| seq <= prev) {
                    skip_rest(scan, offset, bytes.len() - offset)?;
                    break;
                }
                scan.progress.previous = Some(seq);
                if seq > scan.after_seq {
                    match read_batch(&bytes[payload], scan.table) {
                        Ok(batch) => {
                            on_batch(seq, batch)?;
                            scan.progress.records += 1;
                            scan.progress.last_valid = Some(seq);
                        }
                        Err(_) => {
                            if !scan.skip_corrupt {
                                return Err(corrupt(scan, offset));
                            }
                            tracing::error!(
                                table = scan.table.name(),
                                segment = scan.name,
                                offset = as_u64(offset),
                                "skipping corrupt WAL record"
                            );
                            scan.progress.skipped_bytes += as_u64(next - offset);
                        }
                    }
                } else {
                    scan.progress.last_valid = Some(seq);
                }
                offset = next;
            }
            Decoded::Short { offset: at } | Decoded::Invalid { offset: at, .. } => {
                if scan.is_last {
                    truncate_tail(path, at, bytes.len() - at, scan)?;
                    return Ok(false);
                }
                skip_rest(scan, at, bytes.len() - at)?;
                break;
            }
        }
    }
    Ok(true)
}

fn truncate_tail(
    path: &Path,
    at: usize,
    dropped: usize,
    scan: &mut Scan<'_>,
) -> Result<(), WalError> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .map_err(|source| WalError::io("truncating WAL segment", path, source))?;
    let len = as_u64(at);
    file.set_len(len)
        .map_err(|source| WalError::io("truncating WAL segment", path, source))?;
    file.sync_all()
        .map_err(|source| WalError::io("syncing WAL segment", path, source))?;
    let dropped = as_u64(dropped);
    scan.progress.truncated_bytes += dropped;
    tracing::warn!(
        table = scan.table.name(),
        segment = scan.name,
        offset = len,
        bytes = dropped,
        "truncated torn WAL tail"
    );
    Ok(())
}

fn skip_rest(scan: &mut Scan<'_>, offset: usize, rest: usize) -> Result<(), WalError> {
    if !scan.skip_corrupt {
        return Err(corrupt(scan, offset));
    }
    let bytes = as_u64(rest);
    tracing::error!(
        table = scan.table.name(),
        segment = scan.name,
        offset = as_u64(offset),
        bytes,
        "skipping corrupt WAL data"
    );
    scan.progress.skipped_bytes += bytes;
    Ok(())
}

fn corrupt(scan: &Scan<'_>, offset: usize) -> WalError {
    WalError::Corrupt {
        table: scan.table.name(),
        segment: scan.name.to_owned(),
        offset: as_u64(offset),
    }
}

fn as_u64(len: usize) -> u64 {
    u64::try_from(len).unwrap_or(u64::MAX)
}

fn list_segments(dir: &Path) -> Result<Vec<(u64, std::path::PathBuf)>, WalError> {
    let entries = std::fs::read_dir(dir)
        .map_err(|source| WalError::io("reading WAL directory", dir, source))?;
    let mut segments = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| WalError::io("reading WAL directory", dir, source))?;
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        match crate::wal::parse_segment_name(&name) {
            Some(seq) => segments.push((seq, entry.path())),
            None => {
                tracing::warn!(file = name.as_ref(), "ignoring unrecognized WAL file");
            }
        }
    }
    segments.sort_by_key(|(seq, _)| *seq);
    Ok(segments)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;
    use crate::hooks::Hooks;
    use crate::wal::record::{encode, push_framed};
    use crate::wal::segment::SegmentWriter;
    use crate::wal::{sample_span_batch, segment_name};

    fn write_segments(dir: &Path, segment_bytes: u64, seqs: &[u64]) {
        let batch = sample_span_batch();
        let hooks = Hooks::default();
        let mut writer = SegmentWriter::create(dir, seqs[0], segment_bytes, hooks.clone()).unwrap();
        for (index, seq) in seqs.iter().copied().enumerate() {
            if index > 0 && writer.needs_rotation() {
                writer = SegmentWriter::create(dir, seq, segment_bytes, hooks.clone()).unwrap();
            }
            let mut buf = Vec::new();
            encode(seq, &batch, &mut buf).unwrap();
            writer.write(&buf).unwrap();
            writer.sync().unwrap();
        }
    }

    fn write_bytes(dir: &Path, first_seq: u64, bytes: &[u8]) {
        let mut writer = SegmentWriter::create(dir, first_seq, u64::MAX, Hooks::default()).unwrap();
        writer.write(bytes).unwrap();
        writer.sync().unwrap();
    }

    fn collect(dir: &Path, after_seq: u64, skip_corrupt: bool) -> (ReplayOutcome, Vec<u64>) {
        let mut seqs = Vec::new();
        let outcome = replay(
            dir,
            Table::Spans,
            after_seq,
            skip_corrupt,
            &mut |seq, batch| {
                assert_eq!(batch.num_rows(), 1);
                seqs.push(seq);
                Ok(())
            },
        )
        .unwrap();
        (outcome, seqs)
    }

    fn encoded(seq: u64) -> Vec<u8> {
        let mut buf = Vec::new();
        encode(seq, &sample_span_batch(), &mut buf).unwrap();
        buf
    }

    #[test]
    fn an_empty_directory_starts_at_after_seq_plus_one() {
        let dir = tempfile::tempdir().unwrap();
        let (outcome, seqs) = collect(dir.path(), 0, false);
        assert_eq!(outcome.next_seq, 1);
        assert_eq!(outcome.records, 0);
        assert!(seqs.is_empty());
        let (outcome, _) = collect(dir.path(), 8, false);
        assert_eq!(outcome.next_seq, 9);
    }

    #[test]
    fn records_replay_across_segments_in_order() {
        let dir = tempfile::tempdir().unwrap();
        write_segments(dir.path(), 1, &[1, 2, 3]);
        for seq in 1..=3 {
            assert!(dir.path().join(segment_name(seq)).is_file());
        }
        let (outcome, seqs) = collect(dir.path(), 0, false);
        assert_eq!(seqs, [1, 2, 3]);
        assert_eq!(outcome.records, 3);
        assert_eq!(outcome.next_seq, 4);
        assert_eq!(outcome.truncated_bytes, 0);
        assert_eq!(outcome.skipped_bytes, 0);
    }

    #[test]
    fn after_seq_skips_records_without_reading_the_batch() {
        let dir = tempfile::tempdir().unwrap();
        let mut buf = Vec::new();
        push_framed(&mut buf, 1, b"not ipc");
        let good = encoded(2);
        buf.extend_from_slice(&good);
        write_bytes(dir.path(), 1, &buf);
        let (outcome, seqs) = collect(dir.path(), 1, false);
        assert_eq!(seqs, [2]);
        assert_eq!(outcome.records, 1);
        assert_eq!(outcome.next_seq, 3);
    }

    #[test]
    fn a_torn_tail_truncates_to_the_last_full_record() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(segment_name(1));
        let prefix = encoded(1);
        let mut tail = Vec::new();
        push_framed(&mut tail, 2, &[0; 32]);
        for cut in 0..tail.len() {
            let mut file = prefix.clone();
            file.extend_from_slice(&tail[..cut]);
            fs::write(&path, &file).unwrap();
            let (first, seqs) = collect(dir.path(), 0, false);
            assert_eq!(seqs, [1]);
            if cut == 0 {
                assert_eq!(first.truncated_bytes, 0);
            } else {
                assert_eq!(first.truncated_bytes, cut as u64);
                assert_eq!(fs::metadata(&path).unwrap().len(), prefix.len() as u64);
            }
            let (second, seqs) = collect(dir.path(), 0, false);
            assert_eq!(seqs, [1]);
            assert_eq!(second.truncated_bytes, 0);
            assert_eq!(second.next_seq, 2);
        }
    }

    #[test]
    fn a_checksum_error_in_the_last_segment_is_a_torn_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(segment_name(1));
        let prefix = encoded(1);
        let mut tail = encoded(2);
        let flip_at = 20;
        tail[flip_at] ^= 0x01;
        let mut file = prefix.clone();
        file.extend_from_slice(&tail);
        fs::write(&path, &file).unwrap();
        let (first, seqs) = collect(dir.path(), 0, false);
        assert_eq!(seqs, [1]);
        assert_eq!(first.truncated_bytes, tail.len() as u64);
        assert_eq!(fs::metadata(&path).unwrap().len(), prefix.len() as u64);
        let (second, seqs) = collect(dir.path(), 0, false);
        assert_eq!(seqs, [1]);
        assert_eq!(second.truncated_bytes, 0);
        assert_eq!(second.records, 1);
    }

    #[test]
    fn a_real_record_cut_in_the_last_segment_is_a_torn_tail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(segment_name(1));
        let prefix = encoded(1);
        let tail = encoded(2);
        let cuts = [1, 16, tail.len() / 2, tail.len() - 1];
        for cut in cuts {
            let mut file = prefix.clone();
            file.extend_from_slice(&tail[..cut]);
            fs::write(&path, &file).unwrap();
            let (first, seqs) = collect(dir.path(), 0, false);
            assert_eq!(seqs, [1], "cut {cut}");
            assert_eq!(first.truncated_bytes, cut as u64, "cut {cut}");
            let (second, _) = collect(dir.path(), 0, false);
            assert_eq!(second.truncated_bytes, 0, "cut {cut}");
        }
    }

    #[test]
    fn corruption_in_a_middle_segment_stops_or_skips_the_rest_of_it() {
        let dir = tempfile::tempdir().unwrap();
        write_segments(dir.path(), 1, &[1, 2, 3]);
        let middle = dir.path().join(segment_name(2));
        let mut bytes = fs::read(&middle).unwrap();
        let flipped_len = bytes.len() as u64;
        bytes[30] ^= 0x01;
        fs::write(&middle, &bytes).unwrap();

        let err = replay(dir.path(), Table::Spans, 0, false, &mut |_, _| Ok(())).unwrap_err();
        match err {
            WalError::Corrupt {
                table,
                segment,
                offset,
            } => {
                assert_eq!(table, "spans");
                assert_eq!(segment, segment_name(2));
                assert_eq!(offset, 0);
            }
            other => panic!("expected corruption, got {other}"),
        }
        assert_eq!(fs::metadata(&middle).unwrap().len(), flipped_len);

        let (outcome, seqs) = collect(dir.path(), 0, true);
        assert_eq!(seqs, [1, 3]);
        assert_eq!(outcome.records, 2);
        assert_eq!(outcome.skipped_bytes, flipped_len);
        assert_eq!(outcome.next_seq, 4);
        assert_eq!(outcome.truncated_bytes, 0);
    }

    #[test]
    fn a_seq_that_does_not_increase_is_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let first = encoded(4);
        let mut buf = first.clone();
        buf.extend_from_slice(&encoded(4));
        write_bytes(dir.path(), 4, &buf);
        let err = replay(dir.path(), Table::Spans, 0, false, &mut |_, _| Ok(())).unwrap_err();
        match err {
            WalError::Corrupt {
                offset, segment, ..
            } => {
                assert_eq!(segment, segment_name(4));
                assert_eq!(offset, first.len() as u64);
            }
            other => panic!("expected corruption, got {other}"),
        }

        let other = tempfile::tempdir().unwrap();
        write_bytes(other.path(), 5, &encoded(5));
        write_bytes(other.path(), 6, &encoded(5));
        let err = replay(other.path(), Table::Spans, 0, false, &mut |_, _| Ok(())).unwrap_err();
        match err {
            WalError::Corrupt {
                offset, segment, ..
            } => {
                assert_eq!(segment, segment_name(6));
                assert_eq!(offset, 0);
            }
            other => panic!("expected corruption, got {other}"),
        }
    }

    #[test]
    fn a_checksum_valid_payload_that_is_not_a_batch_is_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let mut bad = Vec::new();
        push_framed(&mut bad, 1, b"not ipc");
        let bad_len = bad.len() as u64;
        let mut buf = bad.clone();
        buf.extend_from_slice(&encoded(2));
        write_bytes(dir.path(), 1, &buf);
        let path = dir.path().join(segment_name(1));
        let before = fs::metadata(&path).unwrap().len();

        let err = replay(dir.path(), Table::Spans, 0, false, &mut |_, _| Ok(())).unwrap_err();
        match err {
            WalError::Corrupt { offset, .. } => assert_eq!(offset, 0),
            other => panic!("expected corruption, got {other}"),
        }
        assert_eq!(fs::metadata(&path).unwrap().len(), before);

        let (outcome, seqs) = collect(dir.path(), 0, true);
        assert_eq!(seqs, [2]);
        assert_eq!(outcome.skipped_bytes, bad_len);
        assert_eq!(outcome.next_seq, 3);
        assert_eq!(outcome.truncated_bytes, 0);
    }

    #[test]
    fn unknown_files_are_ignored_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let notes = dir.path().join("notes.txt");
        fs::write(&notes, b"leave me").unwrap();
        let partial = dir.path().join("00000000000000000001.wal.tmp");
        fs::write(&partial, b"tmp").unwrap();
        write_bytes(dir.path(), 3, &encoded(3));
        let (outcome, seqs) = collect(dir.path(), 0, false);
        assert_eq!(seqs, [3]);
        assert_eq!(outcome.next_seq, 4);
        assert_eq!(fs::read(&notes).unwrap(), b"leave me");
        assert_eq!(fs::read(&partial).unwrap(), b"tmp");
    }
}
