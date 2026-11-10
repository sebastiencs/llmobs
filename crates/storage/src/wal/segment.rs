//! One open WAL segment.
//!
//! The writer task rotates between groups. [`SegmentWriter::needs_rotation`]
//! is therefore checked after a group is durable, never in the middle of one,
//! and a single group may leave the file larger than `segment_bytes`.
//!
//! Called from tests until the writer task lands.
#![allow(dead_code)]

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::error::WalError;
use crate::hooks::{HookPoint, Hooks};
use crate::wal::{segment_name, sync_dir};

/// An append-only segment file.
#[derive(Debug)]
pub(crate) struct SegmentWriter {
    dir: PathBuf,
    file: std::fs::File,
    name: String,
    len: u64,
    segment_bytes: u64,
    hooks: Hooks,
}

impl SegmentWriter {
    /// Creates `dir/<first_seq>.wal` and fsyncs `dir`.
    ///
    /// An empty file of that name is reused. A crash can create the segment
    /// and exit before the first record; `create_new` would then fail on the
    /// next open. A non-empty file is left alone and this returns an error.
    pub(crate) fn create(
        dir: &Path,
        first_seq: u64,
        segment_bytes: u64,
        hooks: Hooks,
    ) -> Result<Self, WalError> {
        let name = segment_name(first_seq);
        let path = dir.join(&name);
        let file = open_segment(&path)?;
        sync_dir(dir).map_err(|source| WalError::io("syncing WAL directory", dir, source))?;
        let len = file
            .metadata()
            .map_err(|source| WalError::io("reading WAL segment", &path, source))?
            .len();
        tracing::info!(segment = name.as_str(), bytes = len, "created WAL segment");
        Ok(Self {
            dir: dir.to_path_buf(),
            file,
            name,
            len,
            segment_bytes,
            hooks,
        })
    }

    /// Segment file name, for example `00000000000000000042.wal`.
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    /// Bytes written through this writer, including a reused empty file's length.
    pub(crate) fn len(&self) -> u64 {
        self.len
    }

    /// Absolute path of the segment file.
    pub(crate) fn path(&self) -> PathBuf {
        self.dir.join(&self.name)
    }

    /// Appends `bytes` and advances [`Self::len`].
    ///
    /// The `WalWrite` hook runs first. An injected fault writes nothing.
    pub(crate) fn write(&mut self, bytes: &[u8]) -> Result<(), WalError> {
        self.hooks.pause(HookPoint::WalWrite);
        if let Some(source) = self.hooks.fail(HookPoint::WalWrite) {
            return Err(self.io("writing WAL segment", source));
        }
        self.file
            .write_all(bytes)
            .map_err(|source| self.io("writing WAL segment", source))?;
        self.len += u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        Ok(())
    }

    /// `fdatasync` on the segment file. The `WalSync` hook runs first.
    pub(crate) fn sync(&mut self) -> Result<(), WalError> {
        self.hooks.pause(HookPoint::WalSync);
        if let Some(source) = self.hooks.fail(HookPoint::WalSync) {
            return Err(self.io("syncing WAL segment", source));
        }
        self.file
            .sync_data()
            .map_err(|source| self.io("syncing WAL segment", source))?;
        Ok(())
    }

    /// Drops any bytes past `len` and fsyncs the file.
    ///
    /// A group that was written but not acknowledged is removed, so the next
    /// replay does not treat it as a committed record.
    pub(crate) fn rewind(&mut self, len: u64) -> Result<(), WalError> {
        self.file
            .set_len(len)
            .map_err(|source| self.io("truncating WAL segment", source))?;
        self.file
            .sync_all()
            .map_err(|source| self.io("syncing WAL segment", source))?;
        self.len = len;
        Ok(())
    }

    /// The file is at least `segment_bytes`.
    ///
    /// Rotation happens after the group that crossed the limit, so the file
    /// can be larger than the limit by that one group.
    pub(crate) fn needs_rotation(&self) -> bool {
        self.len >= self.segment_bytes
    }

    fn io(&self, action: &'static str, source: io::Error) -> WalError {
        WalError::io(action, &self.dir.join(&self.name), source)
    }
}

fn open_segment(path: &Path) -> Result<std::fs::File, WalError> {
    let mut options = OpenOptions::new();
    options.append(true);
    match options.create_new(true).open(path) {
        Ok(file) => Ok(file),
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
            let meta = std::fs::metadata(path)
                .map_err(|source| WalError::io("creating WAL segment", path, source))?;
            if meta.len() != 0 {
                return Err(WalError::io("creating WAL segment", path, source));
            }
            OpenOptions::new()
                .append(true)
                .open(path)
                .map_err(|source| WalError::io("creating WAL segment", path, source))
        }
        Err(source) => Err(WalError::io("creating WAL segment", path, source)),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::hooks::HookPoint;
    use crate::wal::record::encode;
    use crate::wal::{parse_segment_name, sample_span_batch, segment_name};

    fn assert_io(err: WalError, action: &str) {
        match err {
            WalError::Io {
                action: got,
                source,
                ..
            } => {
                assert_eq!(got, action);
                assert_eq!(source.to_string(), "injected fault");
            }
            other => panic!("expected an I/O error, got {other}"),
        }
    }

    #[test]
    fn segment_names_round_trip_and_reject_the_rest() {
        for seq in [0, 42, u64::MAX] {
            assert_eq!(parse_segment_name(&segment_name(seq)), Some(seq));
        }
        assert!(parse_segment_name("42.wal").is_none());
        assert!(parse_segment_name("000000000000000000042.wal").is_none());
        assert!(parse_segment_name("0000000000000000004g.wal").is_none());
        assert!(parse_segment_name("00000000000000000042.WAL").is_none());
        assert!(parse_segment_name("00000000000000000042.wal.bak").is_none());
        assert!(parse_segment_name("99999999999999999999.wal").is_none());
        assert!(parse_segment_name("").is_none());
    }

    #[test]
    fn a_group_may_pass_the_segment_limit() {
        let dir = tempfile::tempdir().unwrap();
        let mut writer = SegmentWriter::create(dir.path(), 1, 64, Hooks::default()).unwrap();
        assert!(!writer.needs_rotation());
        writer.write(&[0; 80]).unwrap();
        assert!(writer.needs_rotation());
        assert_eq!(writer.len(), 80);
        assert_eq!(
            fs::metadata(dir.path().join(writer.name())).unwrap().len(),
            80
        );
    }

    #[test]
    fn an_empty_segment_left_behind_is_reused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(segment_name(7));
        fs::write(&path, b"").unwrap();
        let mut writer = SegmentWriter::create(dir.path(), 7, 1024, Hooks::default()).unwrap();
        writer.write(b"abc").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"abc");
    }

    #[test]
    fn a_non_empty_segment_is_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(segment_name(7));
        fs::write(&path, b"already").unwrap();
        let err = SegmentWriter::create(dir.path(), 7, 1024, Hooks::default()).unwrap_err();
        match err {
            WalError::Io { action, source, .. } => {
                assert_eq!(action, "creating WAL segment");
                assert_eq!(source.kind(), io::ErrorKind::AlreadyExists);
            }
            other => panic!("expected AlreadyExists, got {other}"),
        }
        assert_eq!(fs::read(&path).unwrap(), b"already");
    }

    #[test]
    fn injected_write_and_sync_faults_surface() {
        let dir = tempfile::tempdir().unwrap();
        let hooks = Hooks::default();
        hooks.arm_fail(HookPoint::WalWrite);
        let mut writer = SegmentWriter::create(dir.path(), 1, 1024, hooks.clone()).unwrap();
        assert_io(writer.write(b"nope").unwrap_err(), "writing WAL segment");
        assert_eq!(writer.len(), 0);
        assert_eq!(
            fs::metadata(dir.path().join(writer.name())).unwrap().len(),
            0
        );

        writer.write(b"kept").unwrap();
        hooks.arm_fail(HookPoint::WalSync);
        assert_io(writer.sync().unwrap_err(), "syncing WAL segment");
        assert_eq!(fs::read(dir.path().join(writer.name())).unwrap(), b"kept");
        writer.sync().unwrap();
    }

    #[test]
    fn records_land_in_a_new_segment_after_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let batch = sample_span_batch();
        let hooks = Hooks::default();
        let mut writer = SegmentWriter::create(dir.path(), 1, 1, hooks.clone()).unwrap();
        for seq in 1..=3 {
            if writer.needs_rotation() {
                writer = SegmentWriter::create(dir.path(), seq, 1, hooks.clone()).unwrap();
            }
            let mut buf = Vec::new();
            encode(seq, &batch, &mut buf).unwrap();
            writer.write(&buf).unwrap();
            writer.sync().unwrap();
        }
        for seq in 1..=3 {
            assert!(dir.path().join(segment_name(seq)).is_file());
        }
    }
}
