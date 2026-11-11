//! OTLP decoding, convention detection and normalization, media extraction,
//! truncation, and cost computation.
//!
//! Decoding is specified in `docs/spec/02-ingestion.md` § Endpoints.

mod bag;
mod decode;
mod error;
mod value;
mod warn;

pub use decode::{decode_json, decode_protobuf};
pub use error::{DecodeError, JsonErrorCategory};
pub use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
