//! OTLP decoding, convention detection and normalization, media extraction,
//! truncation, and cost computation.
//!
//! Decoding and the span skeleton are specified in `docs/spec/02-ingestion.md`.

mod bag;
mod context;
mod decode;
mod detect;
mod error;
mod fields;
mod kind;
mod normalize;
mod reject;
mod value;
mod warn;

pub use decode::{decode_json, decode_protobuf};
pub use error::{DecodeError, JsonErrorCategory};
pub use normalize::{IngestContext, IngestLimits, Normalized, normalize};
pub use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
pub use reject::{RejectReason, Rejections};
