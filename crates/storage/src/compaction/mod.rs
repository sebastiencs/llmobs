//! Compaction: picking jobs, merging their inputs slice by slice, and running
//! them from a background task. The policy is spec 03 §5.

pub(crate) mod job;
pub(crate) mod merge;
pub(crate) mod plan;
pub(crate) mod slice;
pub(crate) mod task;
