//! Model, provider, operation, request params, and context fields.
//!
//! Spec 02 § Model, provider, usage, § Context fields, and rules 3, 8, 11 and 12.
//! The precedence tables live here, next to the functions that read them.
//! `service_name` is filled while the resource is encoded. Provider inference
//! from the price table waits for N16.

use std::collections::BTreeMap;

use llmobs_core::enums::Convention;
use llmobs_core::names::{attr, metadata_key};
use serde_json::Value as Json;

use crate::bag::{AttrBag, Use};
use crate::fields::{ResourceView, Source, first_string, resource, span};
use crate::value::{as_str, as_string_list, to_json, to_map_string};
use crate::warn::Warnings;

const REQUEST_MODEL: &[Source] = &[
    span("gen_ai.request.model", Convention::Genai),
    span("llm.model_name", Convention::Openinference),
    span("embedding.model_name", Convention::Openinference),
    span("ai.model.id", Convention::Vercel),
    span("langfuse.observation.model.name", Convention::Langfuse),
];

const RESPONSE_MODEL: &[Source] = &[
    span("gen_ai.response.model", Convention::Genai),
    span("ai.response.model", Convention::Vercel),
];

const PROVIDER: &[Source] = &[
    span("gen_ai.provider.name", Convention::Genai),
    span("gen_ai.system", Convention::Genai),
    span("llm.provider", Convention::Openinference),
    span("llm.system", Convention::Openinference),
    span("ai.model.provider", Convention::Vercel),
];

const OPERATION: &[Source] = &[span("gen_ai.operation.name", Convention::Genai)];

const SESSION_ID: &[Source] = &[
    span(attr::SESSION_ID, Convention::Llmobs),
    span("session.id", Convention::Openinference),
    span("gen_ai.conversation.id", Convention::Genai),
    span(
        "traceloop.association.properties.session_id",
        Convention::Openllmetry,
    ),
    span("ai.telemetry.metadata.sessionId", Convention::Vercel),
    span("langfuse.session.id", Convention::Langfuse),
    span(
        "lmnr.association.properties.session_id",
        Convention::Laminar,
    ),
];

const USER_ID: &[Source] = &[
    span(attr::USER_ID, Convention::Llmobs),
    span("user.id", Convention::Openinference),
    span("enduser.id", Convention::Openinference),
    span(
        "traceloop.association.properties.user_id",
        Convention::Openllmetry,
    ),
    span("ai.telemetry.metadata.userId", Convention::Vercel),
    span("langfuse.user.id", Convention::Langfuse),
    span("lmnr.association.properties.user_id", Convention::Laminar),
];

const TAGS: &[Source] = &[
    span(attr::TAGS, Convention::Llmobs),
    span("tag.tags", Convention::Openinference),
    span("ai.telemetry.metadata.tags", Convention::Vercel),
    span("langfuse.trace.tags", Convention::Langfuse),
    span("lmnr.association.properties.tags", Convention::Laminar),
];

const ENVIRONMENT: &[Source] = &[
    span(attr::ENVIRONMENT, Convention::Llmobs),
    resource("deployment.environment.name", Convention::Openinference),
    resource("deployment.environment", Convention::Openinference),
    span("langfuse.environment", Convention::Langfuse),
];

const RELEASE: &[Source] = &[
    span(attr::RELEASE, Convention::Llmobs),
    resource("service.version", Convention::Openinference),
    span("langfuse.release", Convention::Langfuse),
    span("lmnr.span.release", Convention::Laminar),
];

/// Sorted so [`canonical_provider`] can binary-search it.
const WELL_KNOWN_PROVIDERS: &[&str] = &[
    "anthropic",
    "aws.bedrock",
    "azure.ai.inference",
    "azure.ai.openai",
    "cohere",
    "deepseek",
    "gcp.gemini",
    "gcp.gen_ai",
    "gcp.vertex_ai",
    "groq",
    "ibm.watsonx.ai",
    "mistral_ai",
    "openai",
    "perplexity",
    "x_ai",
];

/// Context keys that share a metadata prefix. They are not copied into `metadata`.
fn is_metadata_context_key(key: &str) -> bool {
    matches!(
        key,
        "ai.telemetry.metadata.sessionId"
            | "ai.telemetry.metadata.userId"
            | "ai.telemetry.metadata.tags"
            | "traceloop.association.properties.session_id"
            | "traceloop.association.properties.user_id"
    )
}

/// Scalar columns and context maps for one span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ContextFields {
    /// Model named on the request.
    pub request_model: Option<String>,
    /// Model named on the response.
    pub response_model: Option<String>,
    /// Provider after rule 12. `None` until price-table inference in N16.
    pub provider: Option<String>,
    /// `gen_ai.operation.name`, when it is a non-empty string.
    pub operation: Option<String>,
    /// Compact JSON object, keys sorted. `None` when there are no parameters.
    pub request_params: Option<String>,
    /// Session id.
    pub session_id: Option<String>,
    /// End-user id.
    pub user_id: Option<String>,
    /// Deployment environment.
    pub environment: Option<String>,
    /// Release or version.
    pub release: Option<String>,
    /// Tags, first-occurrence order, without duplicates.
    pub tags: Vec<String>,
    /// User metadata. Values follow rule 1.
    pub metadata: BTreeMap<String, String>,
}

/// Fill the scalar and context columns. Rule 3: these columns are filled for every kind.
pub(crate) fn fill(
    bag: &mut AttrBag<'_>,
    resource: &ResourceView<'_>,
    warnings: &mut Warnings,
) -> ContextFields {
    let request_model = string_field(bag, resource, REQUEST_MODEL);
    let response_model = string_field(bag, resource, RESPONSE_MODEL);
    let provider = first_string(bag, resource, PROVIDER).map(|resolved| {
        canonical_provider(&resolved.value, resolved.source.key == "ai.model.provider")
    });
    let operation = string_field(bag, resource, OPERATION);
    let request_params = request_params(bag, warnings);
    let session_id = string_field(bag, resource, SESSION_ID);
    let user_id = string_field(bag, resource, USER_ID);
    let environment = string_field(bag, resource, ENVIRONMENT);
    let release = string_field(bag, resource, RELEASE);
    let tags = tags(bag);
    let metadata = metadata(bag, warnings);
    ContextFields {
        request_model,
        response_model,
        provider,
        operation,
        request_params,
        session_id,
        user_id,
        environment,
        release,
        tags,
        metadata,
    }
}

fn string_field(
    bag: &mut AttrBag<'_>,
    resource: &ResourceView<'_>,
    sources: &[Source],
) -> Option<String> {
    first_string(bag, resource, sources).map(|resolved| resolved.value)
}

fn canonical_provider(value: &str, from_vercel: bool) -> String {
    let lower = value.to_lowercase();
    if let Some(alias) = exact_alias(&lower) {
        return alias.to_owned();
    }
    if WELL_KNOWN_PROVIDERS.binary_search(&lower.as_str()).is_ok() {
        return lower;
    }
    if from_vercel {
        return vercel_provider(&lower);
    }
    lower
}

fn exact_alias(value: &str) -> Option<&'static str> {
    Some(match value {
        "openai.chat" | "openai.responses" => "openai",
        "anthropic.messages" => "anthropic",
        "xai" => "x_ai",
        _ => return None,
    })
}

fn vercel_provider(value: &str) -> String {
    if value == "google.generative-ai" {
        return "gcp.gemini".to_owned();
    }
    if value.starts_with("google.vertex.") {
        return "gcp.vertex_ai".to_owned();
    }
    if value == "amazon-bedrock" {
        return "aws.bedrock".to_owned();
    }
    if value.starts_with("azure.") {
        return "azure.ai.openai".to_owned();
    }
    if value.starts_with("xai.") {
        return "x_ai".to_owned();
    }
    if value.starts_with("mistral.") {
        return "mistral_ai".to_owned();
    }
    match value.split_once('.') {
        Some((name, _)) if !name.is_empty() => name.to_owned(),
        _ => value.to_owned(),
    }
}

fn request_params(bag: &mut AttrBag<'_>, warnings: &mut Warnings) -> Option<String> {
    let mut params = BTreeMap::new();
    for key in prefixed(bag, "gen_ai.request.") {
        if key == "gen_ai.request.model" {
            continue;
        }
        let Some(name) = key.strip_prefix("gen_ai.request.") else {
            continue;
        };
        insert_param(bag, &key, name, &mut params, warnings);
    }
    expand_param_object(bag, "llm.invocation_parameters", &mut params);
    for key in prefixed(bag, "ai.settings.") {
        let Some(name) = key.strip_prefix("ai.settings.") else {
            continue;
        };
        insert_param(bag, &key, name, &mut params, warnings);
    }
    expand_param_object(bag, "langfuse.observation.model.parameters", &mut params);
    if params.is_empty() {
        None
    } else {
        Some(serde_json::to_string(&params).unwrap_or_else(|_| "{}".to_owned()))
    }
}

fn insert_param(
    bag: &mut AttrBag<'_>,
    key: &str,
    name: &str,
    params: &mut BTreeMap<String, Json>,
    warnings: &mut Warnings,
) {
    if name.is_empty() {
        return;
    }
    let Some(value) = bag.get(key) else {
        return;
    };
    if !params.contains_key(name) {
        params.insert(name.to_owned(), to_json(value, warnings));
    }
    bag.take(key);
}

/// A JSON object string is expanded and consumed. Any other value stays unread.
fn expand_param_object(bag: &mut AttrBag<'_>, key: &str, params: &mut BTreeMap<String, Json>) {
    let Some(text) = bag.get(key).and_then(as_str) else {
        return;
    };
    let Ok(Json::Object(entries)) = serde_json::from_str::<Json>(text) else {
        return;
    };
    for (name, value) in entries {
        if !name.is_empty() && !params.contains_key(&name) {
            params.insert(name, value);
        }
    }
    bag.take(key);
}

fn tags(bag: &mut AttrBag<'_>) -> Vec<String> {
    for source in TAGS {
        let Some(value) = bag.get(source.key) else {
            continue;
        };
        let Some(items) = as_string_list(value) else {
            bag.take(source.key);
            continue;
        };
        let tags = dedup(items);
        bag.take(source.key);
        if !tags.is_empty() {
            return tags;
        }
    }
    Vec::new()
}

fn dedup(items: Vec<String>) -> Vec<String> {
    let mut tags = Vec::new();
    for item in items {
        if item.is_empty() || tags.iter().any(|existing| existing == &item) {
            continue;
        }
        tags.push(item);
    }
    tags
}

fn metadata(bag: &mut AttrBag<'_>, warnings: &mut Warnings) -> BTreeMap<String, String> {
    let mut metadata = BTreeMap::new();
    sweep_prefix(bag, attr::METADATA_PREFIX, &mut metadata, warnings);
    expand_metadata_object(bag, "metadata", &mut metadata);
    sweep_prefix(bag, "ai.telemetry.metadata.", &mut metadata, warnings);
    sweep_prefix(
        bag,
        "traceloop.association.properties.",
        &mut metadata,
        warnings,
    );
    sweep_prefix(bag, "langfuse.trace.metadata.", &mut metadata, warnings);
    sweep_prefix(
        bag,
        "lmnr.association.properties.metadata.",
        &mut metadata,
        warnings,
    );
    link(
        bag,
        attr::EXPERIMENT_ID,
        metadata_key::EXPERIMENT_ID,
        &mut metadata,
        warnings,
    );
    link(
        bag,
        attr::DATASET_ITEM_ID,
        metadata_key::DATASET_ITEM_ID,
        &mut metadata,
        warnings,
    );
    metadata
}

fn sweep_prefix(
    bag: &mut AttrBag<'_>,
    prefix: &str,
    metadata: &mut BTreeMap<String, String>,
    warnings: &mut Warnings,
) {
    for key in prefixed(bag, prefix) {
        if skipped(bag, &key) {
            continue;
        }
        let Some(name) = key.strip_prefix(prefix) else {
            continue;
        };
        if name.is_empty() || metadata.contains_key(name) {
            continue;
        }
        let Some(value) = bag.get(&key) else {
            continue;
        };
        if as_str(value).is_some_and(str::is_empty) {
            continue;
        }
        let text = to_map_string(value, warnings);
        bag.take(&key);
        metadata.insert(name.to_owned(), text);
    }
}

fn expand_metadata_object(
    bag: &mut AttrBag<'_>,
    key: &str,
    metadata: &mut BTreeMap<String, String>,
) {
    if skipped(bag, key) {
        return;
    }
    let Some(text) = bag.get(key).and_then(as_str) else {
        return;
    };
    if text.is_empty() {
        return;
    }
    let Ok(Json::Object(entries)) = serde_json::from_str::<Json>(text) else {
        return;
    };
    for (name, value) in entries {
        if name.is_empty() || metadata.contains_key(&name) || value.is_null() {
            continue;
        }
        let text = match value {
            Json::String(text) => text,
            other => serde_json::to_string(&other).unwrap_or_else(|_| "null".to_owned()),
        };
        metadata.insert(name, text);
    }
    bag.take(key);
}

fn link(
    bag: &mut AttrBag<'_>,
    source: &str,
    name: &str,
    metadata: &mut BTreeMap<String, String>,
    warnings: &mut Warnings,
) {
    let Some(value) = bag.get(source) else {
        return;
    };
    if as_str(value).is_some_and(str::is_empty) {
        return;
    }
    let text = to_map_string(value, warnings);
    bag.take(source);
    metadata.insert(name.to_owned(), text);
}

fn skipped(bag: &AttrBag<'_>, key: &str) -> bool {
    matches!(bag.state(key), Some(Use::Consumed | Use::Payload)) || is_metadata_context_key(key)
}

fn prefixed(bag: &AttrBag<'_>, prefix: &str) -> Vec<String> {
    bag.keys_with_prefix(prefix).map(str::to_owned).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_proto::tonic::common::v1::any_value::Value;
    use opentelemetry_proto::tonic::common::v1::{AnyValue, ArrayValue, KeyValue};

    fn string_attr(key: &str, text: &str) -> KeyValue {
        KeyValue {
            key: key.to_owned(),
            value: Some(AnyValue {
                value: Some(Value::StringValue(text.to_owned())),
            }),
            ..KeyValue::default()
        }
    }

    fn int_attr(key: &str, number: i64) -> KeyValue {
        KeyValue {
            key: key.to_owned(),
            value: Some(AnyValue {
                value: Some(Value::IntValue(number)),
            }),
            ..KeyValue::default()
        }
    }

    fn double_attr(key: &str, number: f64) -> KeyValue {
        KeyValue {
            key: key.to_owned(),
            value: Some(AnyValue {
                value: Some(Value::DoubleValue(number)),
            }),
            ..KeyValue::default()
        }
    }

    fn list_attr(key: &str, items: &[&str]) -> KeyValue {
        KeyValue {
            key: key.to_owned(),
            value: Some(AnyValue {
                value: Some(Value::ArrayValue(ArrayValue {
                    values: items
                        .iter()
                        .map(|item| AnyValue {
                            value: Some(Value::StringValue((*item).to_owned())),
                        })
                        .collect(),
                })),
            }),
            ..KeyValue::default()
        }
    }

    fn filled<'a>(
        attrs: &'a [KeyValue],
        resource_attrs: &[KeyValue],
    ) -> (ContextFields, AttrBag<'a>) {
        let mut warnings = Warnings::default();
        let mut bag = AttrBag::new(attrs, &mut warnings);
        let view = ResourceView::new(resource_attrs);
        let fields = fill(&mut bag, &view, &mut warnings);
        (fields, bag)
    }

    #[test]
    fn well_known_providers_are_sorted() {
        assert!(WELL_KNOWN_PROVIDERS.is_sorted());
    }

    #[test]
    fn request_model_uses_precedence() {
        let attrs = [
            string_attr("ai.model.id", "vercel"),
            string_attr("llm.model_name", "openinference"),
            string_attr("gen_ai.request.model", "genai"),
        ];
        let (fields, bag) = filled(&attrs, &[]);
        assert_eq!(fields.request_model.as_deref(), Some("genai"));
        assert_eq!(bag.state("gen_ai.request.model"), Some(Use::Consumed));
        assert_eq!(bag.state("llm.model_name"), Some(Use::Unread));
        assert_eq!(bag.state("ai.model.id"), Some(Use::Unread));

        let attrs = [
            string_attr("gen_ai.request.model", ""),
            string_attr("embedding.model_name", "embed"),
        ];
        let (fields, bag) = filled(&attrs, &[]);
        assert_eq!(fields.request_model.as_deref(), Some("embed"));
        assert_eq!(bag.state("gen_ai.request.model"), Some(Use::Unread));
        assert_eq!(bag.state("embedding.model_name"), Some(Use::Consumed));
    }

    #[test]
    fn response_model_uses_precedence() {
        let attrs = [
            string_attr("ai.response.model", "vercel"),
            string_attr("gen_ai.response.model", "genai"),
        ];
        let (fields, _) = filled(&attrs, &[]);
        assert_eq!(fields.response_model.as_deref(), Some("genai"));
        let (fields, _) = filled(&[string_attr("ai.response.model", "vercel")], &[]);
        assert_eq!(fields.response_model.as_deref(), Some("vercel"));
    }

    #[test]
    fn operation_is_the_gen_ai_name() {
        let attrs = [string_attr("gen_ai.operation.name", "retrieval")];
        let (fields, bag) = filled(&attrs, &[]);
        assert_eq!(fields.operation.as_deref(), Some("retrieval"));
        assert_eq!(bag.state("gen_ai.operation.name"), Some(Use::Consumed));
        let (fields, _) = filled(&[], &[]);
        assert_eq!(fields.operation, None);
    }

    #[test]
    fn provider_uses_precedence_and_the_alias_table() {
        let attrs = [
            string_attr("ai.model.provider", "anthropic.messages"),
            string_attr("gen_ai.provider.name", "OpenAI"),
        ];
        let (fields, bag) = filled(&attrs, &[]);
        assert_eq!(fields.provider.as_deref(), Some("openai"));
        assert_eq!(bag.state("gen_ai.provider.name"), Some(Use::Consumed));
        assert_eq!(bag.state("ai.model.provider"), Some(Use::Unread));

        let cases = [
            ("gen_ai.provider.name", "openai.chat", "openai"),
            ("gen_ai.provider.name", "openai.responses", "openai"),
            ("gen_ai.provider.name", "anthropic.messages", "anthropic"),
            ("gen_ai.provider.name", "xai", "x_ai"),
            ("gen_ai.provider.name", "XAI", "x_ai"),
            ("gen_ai.provider.name", "aws.bedrock", "aws.bedrock"),
            ("gen_ai.provider.name", "AWS.Bedrock", "aws.bedrock"),
            ("gen_ai.provider.name", "custom.vendor", "custom.vendor"),
            ("ai.model.provider", "openai.chat", "openai"),
            ("ai.model.provider", "aws.bedrock", "aws.bedrock"),
            ("ai.model.provider", "custom.vendor", "custom"),
            ("ai.model.provider", "google.generative-ai", "gcp.gemini"),
            (
                "ai.model.provider",
                "google.vertex.anthropic",
                "gcp.vertex_ai",
            ),
            ("ai.model.provider", "amazon-bedrock", "aws.bedrock"),
            ("ai.model.provider", "azure.openai", "azure.ai.openai"),
            ("ai.model.provider", "xai.chat", "x_ai"),
            ("ai.model.provider", "mistral.small", "mistral_ai"),
            ("ai.model.provider", "groq", "groq"),
            ("gen_ai.system", "OpenAI", "openai"),
        ];
        for (key, raw, expected) in cases {
            let (fields, _) = filled(&[string_attr(key, raw)], &[]);
            assert_eq!(fields.provider.as_deref(), Some(expected), "{key}={raw}");
        }
    }

    #[test]
    fn request_params_keep_json_types_and_precedence() {
        let attrs = [
            double_attr("gen_ai.request.temperature", 0.2),
            int_attr("gen_ai.request.max_tokens", 128),
            string_attr("gen_ai.request.stop", "stop"),
            string_attr("gen_ai.request.model", "demo"),
            int_attr("ai.settings.temperature", 9),
            int_attr("ai.settings.maxRetries", 2),
            string_attr(
                "llm.invocation_parameters",
                r#"{"top_p":0.5,"stop":"later"}"#,
            ),
            string_attr("langfuse.observation.model.parameters", "not-json"),
            string_attr("llm.invocation_parameters.ignored", "{}"),
        ];
        let (fields, bag) = filled(&attrs, &[]);
        assert_eq!(
            fields.request_params.as_deref(),
            Some(
                r#"{"maxRetries":2,"max_tokens":128,"stop":"stop","temperature":0.2,"top_p":0.5}"#
            )
        );
        assert_eq!(fields.request_model.as_deref(), Some("demo"));
        assert_eq!(bag.state("gen_ai.request.temperature"), Some(Use::Consumed));
        assert_eq!(bag.state("ai.settings.temperature"), Some(Use::Consumed));
        assert_eq!(bag.state("ai.settings.maxRetries"), Some(Use::Consumed));
        assert_eq!(bag.state("llm.invocation_parameters"), Some(Use::Consumed));
        assert_eq!(
            bag.state("langfuse.observation.model.parameters"),
            Some(Use::Unread)
        );
        assert_eq!(bag.state("gen_ai.request.model"), Some(Use::Consumed));
    }

    #[test]
    fn session_id_and_user_id_use_precedence() {
        let attrs = [
            string_attr("langfuse.session.id", "langfuse"),
            string_attr("session.id", "otel"),
            string_attr(attr::SESSION_ID, "ours"),
            string_attr("enduser.id", "end"),
            string_attr("user.id", "user"),
        ];
        let (fields, bag) = filled(&attrs, &[]);
        assert_eq!(fields.session_id.as_deref(), Some("ours"));
        assert_eq!(fields.user_id.as_deref(), Some("user"));
        assert_eq!(bag.state("session.id"), Some(Use::Unread));
        assert_eq!(bag.state("enduser.id"), Some(Use::Unread));

        let (fields, _) = filled(&[string_attr("gen_ai.conversation.id", "conv")], &[]);
        assert_eq!(fields.session_id.as_deref(), Some("conv"));
    }

    #[test]
    fn tags_use_the_first_list_and_consume_other_forms() {
        let attrs = [
            string_attr("tag.tags", "red,blue"),
            string_attr(attr::TAGS, r#"["alpha","beta","alpha",""]"#),
        ];
        let (fields, bag) = filled(&attrs, &[]);
        assert_eq!(fields.tags, vec!["alpha".to_owned(), "beta".to_owned()]);
        assert_eq!(bag.state(attr::TAGS), Some(Use::Consumed));
        assert_eq!(bag.state("tag.tags"), Some(Use::Unread));

        let attrs = [string_attr("tag.tags", "red,blue")];
        let (fields, bag) = filled(&attrs, &[]);
        assert!(fields.tags.is_empty());
        assert_eq!(bag.state("tag.tags"), Some(Use::Consumed));

        let attrs = [
            string_attr("tag.tags", "red,blue"),
            list_attr("langfuse.trace.tags", &["red", "green"]),
        ];
        let (fields, bag) = filled(&attrs, &[]);
        assert_eq!(fields.tags, vec!["red".to_owned(), "green".to_owned()]);
        assert_eq!(bag.state("tag.tags"), Some(Use::Consumed));
        assert_eq!(bag.state("langfuse.trace.tags"), Some(Use::Consumed));
    }

    #[test]
    fn metadata_strips_prefixes_and_skips_context_keys() {
        let attrs = [
            string_attr("ai.telemetry.metadata.feature", "later"),
            string_attr("ai.telemetry.metadata.sessionId", "session"),
            string_attr("ai.telemetry.metadata.plan", "pro"),
            string_attr(attr::SESSION_ID, "ours"),
            string_attr("llmobs.metadata.feature", "search"),
            int_attr("llmobs.metadata.n", 2),
            string_attr(
                "metadata",
                r#"{"feature":"openinference","region":"eu","n":null}"#,
            ),
            string_attr("traceloop.association.properties.user_id", "traceloop-user"),
            string_attr("traceloop.association.properties.topic", "billing"),
            string_attr(attr::EXPERIMENT_ID, "exp-1"),
            string_attr(attr::DATASET_ITEM_ID, "item-1"),
            string_attr("llmobs.metadata.llmobs.experiment_id", "from-metadata"),
        ];
        let (fields, bag) = filled(&attrs, &[]);
        assert_eq!(fields.session_id.as_deref(), Some("ours"));
        assert_eq!(fields.user_id.as_deref(), Some("traceloop-user"));
        assert_eq!(
            fields.metadata.get("feature").map(String::as_str),
            Some("search")
        );
        assert_eq!(fields.metadata.get("n").map(String::as_str), Some("2"));
        assert_eq!(
            fields.metadata.get("region").map(String::as_str),
            Some("eu")
        );
        assert_eq!(fields.metadata.get("plan").map(String::as_str), Some("pro"));
        assert_eq!(
            fields.metadata.get("topic").map(String::as_str),
            Some("billing")
        );
        assert_eq!(
            fields
                .metadata
                .get(metadata_key::EXPERIMENT_ID)
                .map(String::as_str),
            Some("exp-1")
        );
        assert_eq!(
            fields
                .metadata
                .get(metadata_key::DATASET_ITEM_ID)
                .map(String::as_str),
            Some("item-1")
        );
        assert!(!fields.metadata.contains_key("sessionId"));
        assert!(!fields.metadata.contains_key("user_id"));
        assert_eq!(
            bag.state("ai.telemetry.metadata.sessionId"),
            Some(Use::Unread)
        );
        assert_eq!(
            bag.state("ai.telemetry.metadata.feature"),
            Some(Use::Unread)
        );
        assert_eq!(bag.state("llmobs.metadata.feature"), Some(Use::Consumed));
        assert_eq!(bag.state("metadata"), Some(Use::Consumed));
        assert_eq!(
            bag.state("traceloop.association.properties.user_id"),
            Some(Use::Consumed)
        );
        assert_eq!(bag.state(attr::EXPERIMENT_ID), Some(Use::Consumed));
    }

    #[test]
    fn environment_and_release_read_resource_keys() {
        let resource_attrs = [
            string_attr("deployment.environment", "legacy"),
            string_attr("deployment.environment.name", "prod"),
            string_attr("service.version", "1.2.3"),
        ];
        let (fields, _) = filled(&[], &resource_attrs);
        assert_eq!(fields.environment.as_deref(), Some("prod"));
        assert_eq!(fields.release.as_deref(), Some("1.2.3"));

        let (fields, _) = filled(
            &[string_attr(attr::ENVIRONMENT, "staging")],
            &resource_attrs,
        );
        assert_eq!(fields.environment.as_deref(), Some("staging"));

        let (fields, _) = filled(&[], &[string_attr("deployment.environment", "legacy")]);
        assert_eq!(fields.environment.as_deref(), Some("legacy"));

        let attrs = [string_attr("lmnr.span.release", "sha")];
        let (fields, bag) = filled(&attrs, &[]);
        assert_eq!(fields.release.as_deref(), Some("sha"));
        assert_eq!(bag.state("lmnr.span.release"), Some(Use::Consumed));
    }
}
