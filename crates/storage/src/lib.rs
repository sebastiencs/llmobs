//! Columnar storage for spans and scores: a write-ahead log, an in-memory
//! memtable, and DataFusion table providers.
//!
//! See `docs/spec/03-storage.md`. The design and milestones are in
//! `docs/plans/a1-storage/00-design.md`.
//!
//! # M1 is not usable in production
//!
//! The server must not wire this crate in yet. There is no flush, so:
//!
//! - the WAL is never truncated and grows until the process restarts;
//! - the memtable only grows; once it reaches `storage.memtable.hard_limit`,
//!   every append answers [`error::AppendError::Backpressure`];
//! - shutdown does not flush: data stays in the WAL and is replayed at the next open.

#![warn(missing_docs)]

pub mod config;
pub mod error;

pub(crate) mod hooks;
pub(crate) mod memtable;
pub(crate) mod wal;
