//! OTLP `ExportTraceServiceRequest` decoding.
//!
//! Protobuf uses `prost`. JSON uses `opentelemetry-proto`'s `with-serde`
//! support for the standard OTLP/JSON encoding. Spec 02 § Endpoints.

use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use prost::Message;

use crate::error::DecodeError;

/// Decode a protobuf `ExportTraceServiceRequest`.
pub fn decode_protobuf(body: &[u8]) -> Result<ExportTraceServiceRequest, DecodeError> {
    ExportTraceServiceRequest::decode(body).map_err(DecodeError::Protobuf)
}

/// Decode an OTLP/JSON `ExportTraceServiceRequest`.
pub fn decode_json(body: &[u8]) -> Result<ExportTraceServiceRequest, DecodeError> {
    serde_json::from_slice(body).map_err(|err| DecodeError::from_json(&err))
}
