//! Span kind from the mapping table.
//!
//! Spec 02 § Span kind. The first row that decides wins, and only that
//! attribute is consumed. Fallback keys are peeked and left unread for later
//! phases.

use llmobs_core::enums::SpanKind;

use crate::bag::AttrBag;
use crate::value::as_str;

/// Kind of one span. Missing and unrecognized values fall through to `span`.
pub(crate) fn span_kind(bag: &mut AttrBag<'_>) -> SpanKind {
    if let Some(kind) = decide(bag, "llmobs.span.kind", |text| SpanKind::parse(text).ok()) {
        return kind;
    }
    if let Some(kind) = decide(bag, "gen_ai.operation.name", genai_operation) {
        return kind;
    }
    if let Some(kind) = decide(bag, "openinference.span.kind", openinference) {
        return kind;
    }
    if let Some(kind) = decide(bag, "ai.operationId", vercel) {
        return kind;
    }
    if let Some(kind) = decide(bag, "traceloop.span.kind", traceloop) {
        return kind;
    }
    if let Some(kind) = decide(bag, "llm.request.type", openllmetry) {
        return kind;
    }
    if let Some(kind) = decide(bag, "langfuse.observation.type", langfuse) {
        return kind;
    }
    if let Some(kind) = decide(bag, "lmnr.span.type", laminar) {
        return kind;
    }
    if present(bag, "gen_ai.tool.name") || present(bag, "gen_ai.tool.call.id") {
        return SpanKind::Tool;
    }
    if present(bag, "gen_ai.request.model") || present(bag, "llm.model_name") {
        return SpanKind::Llm;
    }
    SpanKind::Span
}

fn decide(
    bag: &mut AttrBag<'_>,
    key: &str,
    map: impl Fn(&str) -> Option<SpanKind>,
) -> Option<SpanKind> {
    let kind = map(bag.get(key).and_then(as_str)?)?;
    bag.take(key);
    Some(kind)
}

fn present(bag: &AttrBag<'_>, key: &str) -> bool {
    bag.get(key).is_some()
}

fn genai_operation(text: &str) -> Option<SpanKind> {
    Some(match text {
        "chat" | "text_completion" | "generate_content" | "response" => SpanKind::Llm,
        "embeddings" => SpanKind::Embedding,
        "execute_tool" => SpanKind::Tool,
        "invoke_agent" | "create_agent" => SpanKind::Agent,
        "retrieval" => SpanKind::Retriever,
        "invoke_workflow" | "invoke_runner" | "plan" => SpanKind::Chain,
        _ => return None,
    })
}

fn openinference(text: &str) -> Option<SpanKind> {
    Some(match text {
        "LLM" => SpanKind::Llm,
        "EMBEDDING" => SpanKind::Embedding,
        "TOOL" => SpanKind::Tool,
        "AGENT" => SpanKind::Agent,
        "CHAIN" => SpanKind::Chain,
        "RETRIEVER" => SpanKind::Retriever,
        "RERANKER" => SpanKind::Reranker,
        "GUARDRAIL" => SpanKind::Guardrail,
        "EVALUATOR" => SpanKind::Evaluator,
        _ => return None,
    })
}

fn vercel(text: &str) -> Option<SpanKind> {
    if text.ends_with(".doGenerate") || text.ends_with(".doStream") {
        return Some(SpanKind::Llm);
    }
    if matches!(
        text,
        "ai.generateText" | "ai.streamText" | "ai.generateObject" | "ai.streamObject"
    ) {
        return Some(SpanKind::Chain);
    }
    if text == "ai.toolCall" {
        return Some(SpanKind::Tool);
    }
    if text.starts_with("ai.embed") {
        return Some(SpanKind::Embedding);
    }
    None
}

fn traceloop(text: &str) -> Option<SpanKind> {
    Some(match text {
        "workflow" | "task" => SpanKind::Chain,
        "agent" => SpanKind::Agent,
        "tool" => SpanKind::Tool,
        _ => return None,
    })
}

fn openllmetry(text: &str) -> Option<SpanKind> {
    Some(match text {
        "chat" | "completion" => SpanKind::Llm,
        "embedding" => SpanKind::Embedding,
        _ => return None,
    })
}

fn langfuse(text: &str) -> Option<SpanKind> {
    Some(match text {
        "generation" => SpanKind::Llm,
        "embedding" => SpanKind::Embedding,
        "tool" => SpanKind::Tool,
        "retriever" => SpanKind::Retriever,
        "agent" => SpanKind::Agent,
        "chain" => SpanKind::Chain,
        "evaluator" => SpanKind::Evaluator,
        "guardrail" => SpanKind::Guardrail,
        "span" | "event" => SpanKind::Span,
        _ => return None,
    })
}

/// An empty string decides nothing. Any other string does, and unknown
/// spellings are `chain`.
fn laminar(text: &str) -> Option<SpanKind> {
    if text.is_empty() {
        return None;
    }
    Some(match text {
        "LLM" | "CACHED" => SpanKind::Llm,
        "TOOL" => SpanKind::Tool,
        "EXECUTOR" => SpanKind::Agent,
        "EVALUATOR" | "EVALUATION" | "HUMAN_EVALUATOR" => SpanKind::Evaluator,
        _ => SpanKind::Chain,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_proto::tonic::common::v1::any_value::Value;
    use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue};

    use crate::bag::Use;
    use crate::warn::Warnings;

    fn string_attr(key: &str, text: &str) -> KeyValue {
        KeyValue {
            key: key.to_owned(),
            value: Some(AnyValue {
                value: Some(Value::StringValue(text.to_owned())),
            }),
            ..KeyValue::default()
        }
    }

    fn bool_attr(key: &str) -> KeyValue {
        KeyValue {
            key: key.to_owned(),
            value: Some(AnyValue {
                value: Some(Value::BoolValue(true)),
            }),
            ..KeyValue::default()
        }
    }

    fn kind_of(attrs: &[KeyValue]) -> SpanKind {
        let mut bag = AttrBag::new(attrs, &mut Warnings::default());
        span_kind(&mut bag)
    }

    #[test]
    fn llmobs_span_kind_is_the_parsed_kind() {
        for (text, kind) in [
            ("llm", SpanKind::Llm),
            ("embedding", SpanKind::Embedding),
            ("tool", SpanKind::Tool),
            ("agent", SpanKind::Agent),
            ("chain", SpanKind::Chain),
            ("retriever", SpanKind::Retriever),
            ("reranker", SpanKind::Reranker),
            ("guardrail", SpanKind::Guardrail),
            ("evaluator", SpanKind::Evaluator),
            ("span", SpanKind::Span),
        ] {
            let attrs = [
                string_attr("llmobs.span.kind", text),
                string_attr("gen_ai.operation.name", "chat"),
            ];
            let mut bag = AttrBag::new(&attrs, &mut Warnings::default());
            assert_eq!(span_kind(&mut bag), kind, "{text}");
            assert_eq!(bag.state("llmobs.span.kind"), Some(Use::Consumed));
            assert_eq!(bag.state("gen_ai.operation.name"), Some(Use::Unread));
        }
        assert_eq!(
            kind_of(&[string_attr("llmobs.span.kind", "LLM")]),
            SpanKind::Span
        );
    }

    #[test]
    fn an_invalid_llmobs_span_kind_falls_through() {
        let attrs = [
            string_attr("llmobs.span.kind", "nope"),
            string_attr("gen_ai.operation.name", "chat"),
        ];
        let mut bag = AttrBag::new(&attrs, &mut Warnings::default());
        assert_eq!(span_kind(&mut bag), SpanKind::Llm);
        assert_eq!(bag.state("llmobs.span.kind"), Some(Use::Unread));
        assert_eq!(bag.state("gen_ai.operation.name"), Some(Use::Consumed));
    }

    #[test]
    fn gen_ai_operation_name_uses_the_semconv_table() {
        for (text, kind) in [
            ("chat", SpanKind::Llm),
            ("text_completion", SpanKind::Llm),
            ("generate_content", SpanKind::Llm),
            ("response", SpanKind::Llm),
            ("embeddings", SpanKind::Embedding),
            ("execute_tool", SpanKind::Tool),
            ("invoke_agent", SpanKind::Agent),
            ("create_agent", SpanKind::Agent),
            ("retrieval", SpanKind::Retriever),
            ("invoke_workflow", SpanKind::Chain),
            ("invoke_runner", SpanKind::Chain),
            ("plan", SpanKind::Chain),
        ] {
            assert_eq!(
                kind_of(&[string_attr("gen_ai.operation.name", text)]),
                kind,
                "{text}"
            );
        }
        let attrs = [
            string_attr("gen_ai.operation.name", "invoke"),
            string_attr("openinference.span.kind", "TOOL"),
        ];
        let mut bag = AttrBag::new(&attrs, &mut Warnings::default());
        assert_eq!(span_kind(&mut bag), SpanKind::Tool);
        assert_eq!(bag.state("gen_ai.operation.name"), Some(Use::Unread));
        assert_eq!(bag.state("openinference.span.kind"), Some(Use::Consumed));
    }

    #[test]
    fn openinference_span_kind_matches_the_uppercase_spellings() {
        for (text, kind) in [
            ("LLM", SpanKind::Llm),
            ("EMBEDDING", SpanKind::Embedding),
            ("TOOL", SpanKind::Tool),
            ("AGENT", SpanKind::Agent),
            ("CHAIN", SpanKind::Chain),
            ("RETRIEVER", SpanKind::Retriever),
            ("RERANKER", SpanKind::Reranker),
            ("GUARDRAIL", SpanKind::Guardrail),
            ("EVALUATOR", SpanKind::Evaluator),
        ] {
            assert_eq!(
                kind_of(&[string_attr("openinference.span.kind", text)]),
                kind,
                "{text}"
            );
        }
        assert_eq!(
            kind_of(&[string_attr("openinference.span.kind", "llm")]),
            SpanKind::Span
        );
    }

    #[test]
    fn ai_operation_id_matches_suffixes_then_exact_names_then_embed() {
        for (text, kind) in [
            ("ai.generateText.doGenerate", SpanKind::Llm),
            ("ai.streamText.doStream", SpanKind::Llm),
            ("custom.doGenerate", SpanKind::Llm),
            ("ai.embed.doGenerate", SpanKind::Llm),
            ("ai.generateText", SpanKind::Chain),
            ("ai.streamText", SpanKind::Chain),
            ("ai.generateObject", SpanKind::Chain),
            ("ai.streamObject", SpanKind::Chain),
            ("ai.toolCall", SpanKind::Tool),
            ("ai.embed", SpanKind::Embedding),
            ("ai.embedMany", SpanKind::Embedding),
        ] {
            assert_eq!(
                kind_of(&[string_attr("ai.operationId", text)]),
                kind,
                "{text}"
            );
        }
        assert_eq!(
            kind_of(&[string_attr("ai.operationId", "doGenerate")]),
            SpanKind::Span
        );
        assert_eq!(
            kind_of(&[string_attr(
                "ai.operationId",
                "ai.generateText.doGenerateExtra"
            )]),
            SpanKind::Span
        );
    }

    #[test]
    fn traceloop_span_kind_maps_workflow_and_task_to_chain() {
        for (text, kind) in [
            ("workflow", SpanKind::Chain),
            ("task", SpanKind::Chain),
            ("agent", SpanKind::Agent),
            ("tool", SpanKind::Tool),
        ] {
            assert_eq!(
                kind_of(&[string_attr("traceloop.span.kind", text)]),
                kind,
                "{text}"
            );
        }
        assert_eq!(
            kind_of(&[string_attr("traceloop.span.kind", "workflow ")]),
            SpanKind::Span
        );
    }

    #[test]
    fn llm_request_type_maps_chat_completion_and_embedding() {
        for (text, kind) in [
            ("chat", SpanKind::Llm),
            ("completion", SpanKind::Llm),
            ("embedding", SpanKind::Embedding),
        ] {
            assert_eq!(
                kind_of(&[string_attr("llm.request.type", text)]),
                kind,
                "{text}"
            );
        }
        assert_eq!(
            kind_of(&[string_attr("llm.request.type", "Chat")]),
            SpanKind::Span
        );
    }

    #[test]
    fn langfuse_observation_type_maps_generation_and_keeps_the_other_kinds() {
        for (text, kind) in [
            ("generation", SpanKind::Llm),
            ("embedding", SpanKind::Embedding),
            ("tool", SpanKind::Tool),
            ("retriever", SpanKind::Retriever),
            ("agent", SpanKind::Agent),
            ("chain", SpanKind::Chain),
            ("evaluator", SpanKind::Evaluator),
            ("guardrail", SpanKind::Guardrail),
            ("span", SpanKind::Span),
            ("event", SpanKind::Span),
        ] {
            assert_eq!(
                kind_of(&[string_attr("langfuse.observation.type", text)]),
                kind,
                "{text}"
            );
        }
        assert_eq!(
            kind_of(&[string_attr("langfuse.observation.type", "GENERATION")]),
            SpanKind::Span
        );
    }

    #[test]
    fn lmnr_span_type_maps_known_spellings_and_other_strings_to_chain() {
        for (text, kind) in [
            ("LLM", SpanKind::Llm),
            ("CACHED", SpanKind::Llm),
            ("TOOL", SpanKind::Tool),
            ("EXECUTOR", SpanKind::Agent),
            ("EVALUATOR", SpanKind::Evaluator),
            ("EVALUATION", SpanKind::Evaluator),
            ("HUMAN_EVALUATOR", SpanKind::Evaluator),
            ("SPAN", SpanKind::Chain),
            ("llm", SpanKind::Chain),
        ] {
            let attrs = [string_attr("lmnr.span.type", text)];
            let mut bag = AttrBag::new(&attrs, &mut Warnings::default());
            assert_eq!(span_kind(&mut bag), kind, "{text}");
            assert_eq!(bag.state("lmnr.span.type"), Some(Use::Consumed));
        }
        let attrs = [string_attr("lmnr.span.type", "")];
        let mut bag = AttrBag::new(&attrs, &mut Warnings::default());
        assert_eq!(span_kind(&mut bag), SpanKind::Span);
        assert_eq!(bag.state("lmnr.span.type"), Some(Use::Unread));
        assert_eq!(kind_of(&[bool_attr("lmnr.span.type")]), SpanKind::Span);
    }

    #[test]
    fn a_tool_key_wins_over_a_model_key_and_neither_is_consumed() {
        let attrs = [
            string_attr("gen_ai.tool.name", ""),
            string_attr("gen_ai.request.model", "gpt"),
        ];
        let mut bag = AttrBag::new(&attrs, &mut Warnings::default());
        assert_eq!(span_kind(&mut bag), SpanKind::Tool);
        assert_eq!(bag.state("gen_ai.tool.name"), Some(Use::Unread));
        assert_eq!(bag.state("gen_ai.request.model"), Some(Use::Unread));

        let attrs = [string_attr("gen_ai.tool.call.id", "call-1")];
        assert_eq!(kind_of(&attrs), SpanKind::Tool);

        let attrs = [string_attr("llm.model_name", "gpt")];
        let mut bag = AttrBag::new(&attrs, &mut Warnings::default());
        assert_eq!(span_kind(&mut bag), SpanKind::Llm);
        assert_eq!(bag.state("llm.model_name"), Some(Use::Unread));

        assert_eq!(kind_of(&[]), SpanKind::Span);
    }
}
