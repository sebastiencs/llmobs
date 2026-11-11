//! Which conventions a span carries.
//!
//! Spec 02 § Convention detection and rule 13. The result is precedence order,
//! not the order the keys were seen.

use llmobs_core::enums::Convention;
use opentelemetry_proto::tonic::trace::v1::span::Event;

use crate::bag::{AttrBag, is_decimal_index};

const PRECEDENCE: [Convention; 8] = [
    Convention::Llmobs,
    Convention::Genai,
    Convention::Openinference,
    Convention::Vercel,
    Convention::Openllmetry,
    Convention::Langfuse,
    Convention::Laminar,
    Convention::GoogleAdk,
];

/// Conventions present on this span, in precedence order, without duplicates.
pub(crate) fn detect(bag: &AttrBag<'_>, events: &[Event]) -> Vec<Convention> {
    let mut present = [false; PRECEDENCE.len()];
    for key in bag.keys() {
        if key.starts_with("llmobs.") {
            mark(&mut present, Convention::Llmobs);
        }
        if key.starts_with("gen_ai.") && !legacy_numbered(key) {
            mark(&mut present, Convention::Genai);
        }
        if openinference(key) {
            mark(&mut present, Convention::Openinference);
        }
        if key.starts_with("ai.") {
            mark(&mut present, Convention::Vercel);
        }
        if openllmetry(key) {
            mark(&mut present, Convention::Openllmetry);
        }
        if key.starts_with("langfuse.") {
            mark(&mut present, Convention::Langfuse);
        }
        if key.starts_with("lmnr.") {
            mark(&mut present, Convention::Laminar);
        }
        if key.starts_with("gcp.vertex.agent.") {
            mark(&mut present, Convention::GoogleAdk);
        }
    }
    if events.iter().any(|event| event.name.starts_with("gen_ai.")) {
        mark(&mut present, Convention::Genai);
    }
    PRECEDENCE
        .into_iter()
        .filter(|convention| present[slot(*convention)])
        .collect()
}

fn mark(present: &mut [bool; PRECEDENCE.len()], convention: Convention) {
    present[slot(convention)] = true;
}

fn slot(convention: Convention) -> usize {
    match convention {
        Convention::Llmobs => 0,
        Convention::Genai => 1,
        Convention::Openinference => 2,
        Convention::Vercel => 3,
        Convention::Openllmetry => 4,
        Convention::Langfuse => 5,
        Convention::Laminar => 6,
        Convention::GoogleAdk => 7,
    }
}

fn openinference(key: &str) -> bool {
    key == "openinference.span.kind"
        || key.starts_with("llm.")
        || key == "input.value"
        || key == "output.value"
        || key.starts_with("embedding.")
        || key.starts_with("retrieval.")
        || key.starts_with("tool.")
}

fn openllmetry(key: &str) -> bool {
    key.starts_with("traceloop.")
        || legacy_numbered(key)
        || key == "llm.request.type"
        || numbered(key, "llm.request.functions.")
}

/// `gen_ai.prompt.N.*` and `gen_ai.completion.N.*`, with `N` a decimal index.
fn legacy_numbered(key: &str) -> bool {
    numbered(key, "gen_ai.prompt.") || numbered(key, "gen_ai.completion.")
}

fn numbered(key: &str, prefix: &str) -> bool {
    let Some(rest) = key.strip_prefix(prefix) else {
        return false;
    };
    let Some((index, _)) = rest.split_once('.') else {
        return false;
    };
    is_decimal_index(index)
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value::Value};

    use crate::warn::Warnings;

    fn detect_keys(keys: &[&str]) -> Vec<Convention> {
        let attrs: Vec<KeyValue> = keys
            .iter()
            .map(|key| KeyValue {
                key: (*key).to_owned(),
                value: Some(AnyValue {
                    value: Some(Value::StringValue("x".to_owned())),
                }),
                ..KeyValue::default()
            })
            .collect();
        let bag = AttrBag::new(&attrs, &mut Warnings::default());
        detect(&bag, &[])
    }

    fn detect_event(name: &str) -> Vec<Convention> {
        let bag = AttrBag::new(&[], &mut Warnings::default());
        let events = [Event {
            name: name.to_owned(),
            ..Event::default()
        }];
        detect(&bag, &events)
    }

    #[test]
    fn each_convention_has_a_matching_key() {
        assert_eq!(detect_keys(&["llmobs.span.kind"]), vec![Convention::Llmobs]);
        assert_eq!(detect_keys(&["llmobs"]), Vec::new());
        assert_eq!(
            detect_keys(&["gen_ai.request.model"]),
            vec![Convention::Genai]
        );
        assert_eq!(detect_event("gen_ai.choice"), vec![Convention::Genai]);
        assert_eq!(
            detect_event("gen_ai.prompt.0.role"),
            vec![Convention::Genai]
        );
        assert_eq!(
            detect_keys(&["input.value"]),
            vec![Convention::Openinference]
        );
        assert_eq!(
            detect_keys(&["output.value"]),
            vec![Convention::Openinference]
        );
        assert_eq!(
            detect_keys(&["embedding.model_name"]),
            vec![Convention::Openinference]
        );
        assert_eq!(
            detect_keys(&["retrieval.documents"]),
            vec![Convention::Openinference]
        );
        assert_eq!(detect_keys(&["tool.name"]), vec![Convention::Openinference]);
        assert_eq!(
            detect_keys(&["openinference.span.kind"]),
            vec![Convention::Openinference]
        );
        assert_eq!(detect_keys(&["input.value.extra"]), Vec::new());
        assert_eq!(detect_keys(&["ai.operationId"]), vec![Convention::Vercel]);
        assert_eq!(detect_keys(&["air.quality"]), Vec::new());
        assert_eq!(
            detect_keys(&["traceloop.span.kind"]),
            vec![Convention::Openllmetry]
        );
        assert_eq!(
            detect_keys(&["langfuse.observation.type"]),
            vec![Convention::Langfuse]
        );
        assert_eq!(detect_keys(&["lmnr.span.type"]), vec![Convention::Laminar]);
        assert_eq!(
            detect_keys(&["gcp.vertex.agent.llm_request"]),
            vec![Convention::GoogleAdk]
        );
        assert_eq!(detect_keys(&["gcp.vertex.agent"]), Vec::new());
    }

    #[test]
    fn a_numbered_prompt_key_is_openllmetry_only() {
        assert_eq!(
            detect_keys(&["gen_ai.prompt.0.role"]),
            vec![Convention::Openllmetry]
        );
        assert_eq!(
            detect_keys(&["gen_ai.completion.2.content"]),
            vec![Convention::Openllmetry]
        );
        assert_eq!(
            detect_keys(&["gen_ai.prompt.name"]),
            vec![Convention::Genai]
        );
        assert_eq!(
            detect_keys(&["gen_ai.prompt.01.role"]),
            vec![Convention::Genai]
        );
    }

    #[test]
    fn llm_request_keys_count_for_openinference_and_openllmetry() {
        assert_eq!(
            detect_keys(&["llm.request.type"]),
            vec![Convention::Openinference, Convention::Openllmetry]
        );
        assert_eq!(
            detect_keys(&["llm.request.functions.0.name"]),
            vec![Convention::Openinference, Convention::Openllmetry]
        );
    }

    #[test]
    fn a_mixed_vercel_span_lists_genai_before_vercel() {
        assert_eq!(
            detect_keys(&["ai.model.id", "gen_ai.request.model"]),
            vec![Convention::Genai, Convention::Vercel]
        );
    }

    #[test]
    fn an_empty_span_has_no_convention() {
        assert_eq!(detect_keys(&[]), Vec::new());
        assert_eq!(detect_event("exception"), Vec::new());
    }
}
