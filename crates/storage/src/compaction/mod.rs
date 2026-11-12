//! Compaction: slicing a job's inputs and merging them into new files.
//!
//! The policy (which partitions to pick, when) is spec 03 §5; `job.rs` and
//! `task.rs` arrive in M4.6 and drive this module from `Storage`.
#![allow(dead_code)]
// Wired into Storage in M4.6.

pub(crate) mod merge;
pub(crate) mod plan;
pub(crate) mod slice;
