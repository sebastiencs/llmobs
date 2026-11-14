//! Success body of `POST /v1/traces` (spec 02 § Endpoints, D-G-6).

use axum::http::HeaderValue;
use llmobs_otel::Rejections;
use opentelemetry_proto::tonic::collector::trace::v1::{
    ExportTracePartialSuccess, ExportTraceServiceResponse,
};
use prost::Message;
use serde_json::json;

use super::body::BodyFormat;

/// Body and content type of a `200` for `/v1/traces` (spec 02 § Endpoints).
pub(crate) fn export_response(
    format: BodyFormat,
    rejections: &Rejections,
) -> (HeaderValue, Vec<u8>) {
    let partial = partial_success(rejections);
    match format {
        BodyFormat::Protobuf => {
            let partial_success =
                partial.map(
                    |(rejected_spans, error_message)| ExportTracePartialSuccess {
                        rejected_spans,
                        error_message,
                    },
                );
            let body = ExportTraceServiceResponse { partial_success }.encode_to_vec();
            (HeaderValue::from_static("application/x-protobuf"), body)
        }
        BodyFormat::Json => {
            let body = match partial {
                None => "{}".to_owned(),
                // `int64` is a decimal string in OTLP/JSON.
                Some((rejected_spans, error_message)) => json!({
                    "partialSuccess": {
                        "rejectedSpans": rejected_spans.to_string(),
                        "errorMessage": error_message,
                    }
                })
                .to_string(),
            };
            (
                HeaderValue::from_static("application/json"),
                body.into_bytes(),
            )
        }
    }
}

/// `None` when nothing was rejected; otherwise the count and the message.
pub(crate) fn partial_success(rejections: &Rejections) -> Option<(i64, String)> {
    let total = rejections.total();
    if total == 0 {
        return None;
    }
    let rejected_spans = i64::try_from(total).unwrap_or(i64::MAX);
    // `message` is `Some` whenever `total > 0`.
    Some((rejected_spans, rejections.message().unwrap_or_default()))
}

#[cfg(test)]
mod tests {
    use llmobs_core::ids::ProjectId;
    use llmobs_otel::{IngestContext, IngestLimits, PriceTable, decode_json, normalize};

    use super::*;

    /// Rejections from `edge/rejected-ids`: four spans with bad ids, one kept.
    fn rejections() -> Rejections {
        let request = decode_json(include_bytes!(
            "../../../../fixtures/otlp/edge/rejected-ids/input.json"
        ))
        .expect("fixture decodes");
        let ctx = IngestContext {
            project_id: "01900000-0000-7000-8000-000000000001"
                .parse::<ProjectId>()
                .expect("project id"),
            ingest_time: 1_767_225_600_000_000_000,
            retention_days: None,
            limits: IngestLimits::default(),
        };
        let prices = PriceTable::new(Vec::new()).expect("empty price table");
        let normalized = normalize(&request, &ctx, &prices);
        assert_eq!(normalized.rejections.total(), 4);
        normalized.rejections
    }

    #[test]
    fn protobuf_without_rejections_is_empty() {
        let (content_type, body) = export_response(BodyFormat::Protobuf, &Rejections::default());
        assert_eq!(content_type, "application/x-protobuf");
        assert!(body.is_empty());
        let decoded = ExportTraceServiceResponse::decode(body.as_slice()).expect("decodes");
        assert_eq!(decoded.partial_success, None);
    }

    #[test]
    fn json_without_rejections_is_an_empty_object() {
        let (content_type, body) = export_response(BodyFormat::Json, &Rejections::default());
        assert_eq!(content_type, "application/json");
        assert_eq!(body, b"{}");
    }

    #[test]
    fn protobuf_with_rejections_carries_count_and_message() {
        let rejections = rejections();
        let (content_type, body) = export_response(BodyFormat::Protobuf, &rejections);
        assert_eq!(content_type, "application/x-protobuf");
        let decoded = ExportTraceServiceResponse::decode(body.as_slice()).expect("decodes");
        let partial = decoded.partial_success.expect("partial success");
        assert_eq!(partial.rejected_spans, 4);
        assert_eq!(Some(partial.error_message), rejections.message());
    }

    #[test]
    fn json_with_rejections_carries_count_as_a_string() {
        let rejections = rejections();
        let (content_type, body) = export_response(BodyFormat::Json, &rejections);
        assert_eq!(content_type, "application/json");
        let value: serde_json::Value = serde_json::from_slice(&body).expect("valid JSON");
        assert_eq!(value["partialSuccess"]["rejectedSpans"], "4");
        assert_eq!(
            value["partialSuccess"]["errorMessage"].as_str(),
            rejections.message().as_deref()
        );
        assert_eq!(value.as_object().map(serde_json::Map::len), Some(1));
    }
}
