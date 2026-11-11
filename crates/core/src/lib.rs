//! Shared contracts used by more than one crate: identifiers, stored enums,
//! branded names, the canonical message format, span and score records, media blobs,
//! their Arrow schemas, and OpenAPI schemas.
//!
//! See `docs/spec/01-data-model.md` and the conventions in `docs/spec/README.md`.

#![warn(missing_docs)]

pub mod batch;
pub mod enums;
pub mod error;
pub mod ids;
pub mod media;
pub mod message;
pub mod names;
pub mod schema;
pub mod score;
pub mod span;
pub mod time_serde;

#[cfg(test)]
mod schema_drift;
