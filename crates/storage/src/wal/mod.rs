//! Write-ahead log for one table.
//!
//! Spec 03 §1. M1 has the record codec; segments, replay, and the writer task follow.
//!
//! The codec is only called from tests until the segment writer lands.
#![allow(dead_code)]

mod record;

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
}
