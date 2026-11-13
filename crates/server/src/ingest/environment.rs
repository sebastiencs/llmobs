//! The OTLP-only rewrite of the reserved `environment` value (D-G-7).

use llmobs_core::names::{environment, metadata_key};
use llmobs_core::span::Span;

/// Warning code added to a span whose external `environment` was rewritten.
const RESERVED_ENVIRONMENT: &str = "reserved_environment";

/// Rewrites `llmobs-eval` claims from external data to `llmobs-eval-external` and
/// records the `reserved_environment` warning. Returns how many spans were rewritten.
pub(crate) fn rewrite_reserved_environment(spans: &mut [Span]) -> usize {
    let mut rewritten = 0;
    for span in spans {
        if span.environment.as_deref() != Some(environment::EVAL) {
            continue;
        }
        span.environment = Some(environment::EVAL_EXTERNAL.to_owned());
        // The normalizer writes a valid JSON array, so anything else counts as empty.
        let mut codes: Vec<String> = span
            .attributes
            .get(metadata_key::NORMALIZE_WARNING)
            .and_then(|text| serde_json::from_str(text).ok())
            .unwrap_or_default();
        codes.push(RESERVED_ENVIRONMENT.to_owned());
        codes.sort();
        codes.dedup();
        if let Ok(text) = serde_json::to_string(&codes) {
            span.attributes
                .insert(metadata_key::NORMALIZE_WARNING.to_owned(), text);
        }
        rewritten += 1;
    }
    rewritten
}

#[cfg(test)]
mod tests {
    use super::*;

    fn span(environment: Option<&str>, warning: Option<&str>) -> Span {
        let mut span: Span = serde_json::from_str(
            r#"{
                "project_id": "01900000-0000-7000-8000-000000000001",
                "trace_id": "a1000000000000000000000000000001",
                "span_id": "b100000000000011",
                "name": "eval",
                "span_kind": "span",
                "start_time": "2026-01-01T10:00:00.000000000Z",
                "end_time": "2026-01-01T10:00:00.000000000Z",
                "status": "unset",
                "ingest_time": "2026-01-01T10:00:00.000000000Z",
                "schema_version": 1
            }"#,
        )
        .expect("span JSON");
        span.environment = environment.map(str::to_owned);
        if let Some(text) = warning {
            span.attributes
                .insert(metadata_key::NORMALIZE_WARNING.to_owned(), text.to_owned());
        }
        span
    }

    fn warning(span: &Span) -> Option<&str> {
        span.attributes
            .get(metadata_key::NORMALIZE_WARNING)
            .map(String::as_str)
    }

    #[test]
    fn eval_without_warning_gets_the_reserved_code() {
        let mut spans = [span(Some(environment::EVAL), None)];
        assert_eq!(rewrite_reserved_environment(&mut spans), 1);
        assert_eq!(
            spans[0].environment.as_deref(),
            Some(environment::EVAL_EXTERNAL)
        );
        assert_eq!(warning(&spans[0]), Some("[\"reserved_environment\"]"));
    }

    #[test]
    fn existing_warnings_are_kept_and_sorted() {
        let mut spans = [span(
            Some(environment::EVAL),
            Some("[\"end_before_start\"]"),
        )];
        assert_eq!(rewrite_reserved_environment(&mut spans), 1);
        assert_eq!(
            warning(&spans[0]),
            Some("[\"end_before_start\",\"reserved_environment\"]")
        );
    }

    #[test]
    fn an_existing_reserved_code_is_not_duplicated() {
        let mut spans = [span(
            Some(environment::EVAL),
            Some("[\"reserved_environment\"]"),
        )];
        assert_eq!(rewrite_reserved_environment(&mut spans), 1);
        assert_eq!(warning(&spans[0]), Some("[\"reserved_environment\"]"));
    }

    #[test]
    fn a_malformed_warning_is_replaced_by_the_reserved_code() {
        let mut spans = [span(Some(environment::EVAL), Some("not json"))];
        assert_eq!(rewrite_reserved_environment(&mut spans), 1);
        assert_eq!(warning(&spans[0]), Some("[\"reserved_environment\"]"));
    }

    #[test]
    fn other_environments_are_untouched_and_counted_correctly() {
        let mut spans = [
            span(Some("production"), None),
            span(None, None),
            span(Some(environment::EVAL_EXTERNAL), None),
            span(Some(environment::EVAL), None),
        ];
        assert_eq!(rewrite_reserved_environment(&mut spans), 1);
        assert_eq!(spans[0].environment.as_deref(), Some("production"));
        assert_eq!(spans[1].environment, None);
        assert_eq!(
            spans[1].attributes.get(metadata_key::NORMALIZE_WARNING),
            None
        );
        assert_eq!(
            spans[2].environment.as_deref(),
            Some(environment::EVAL_EXTERNAL)
        );
        assert_eq!(
            spans[2].attributes.get(metadata_key::NORMALIZE_WARNING),
            None
        );
        assert_eq!(
            spans[3].environment.as_deref(),
            Some(environment::EVAL_EXTERNAL)
        );
    }
}
