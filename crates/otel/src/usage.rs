//! Token columns and time to first token.
//!
//! Spec 02 § Model, provider, usage, § Token semantics, and rules 3 and 4.
//! Langfuse `usage_details` and Gemini `usage_metadata` inside
//! `gcp.vertex.agent.llm_response` are read after the attribute sources.
//! Anthropic
//! cache write before Vercel v6 is read from `ai.response.providerMetadata`.
//! `gen_ai.usage.*` stays inclusive and relies on the safety net.

use llmobs_core::enums::{Convention, SpanKind};
use llmobs_core::names::attr;
use llmobs_core::time_serde::parse_rfc3339;
use opentelemetry_proto::tonic::common::v1::AnyValue;
use opentelemetry_proto::tonic::common::v1::any_value::Value;
use serde_json::Value as Json;

use crate::bag::AttrBag;
use crate::fields::{ResourceView, Source, first_i64, span};
use crate::value::{InvalidNumber, as_f64, as_i64, as_str};
use crate::warn::{Warning, Warnings};

const USAGE_DETAILS: &str = "langfuse.observation.usage_details";
const NO_CACHE_TOKENS: &str = "ai.usage.inputTokenDetails.noCacheTokens";
const INPUT_TOKEN_DETAILS: &str = "ai.usage.inputTokenDetails.";

/// Whether a usage count already contains the related tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Inclusive {
    /// The count already includes the related tokens.
    Yes,
    /// Add the related tokens back. Langfuse `usage_details.input` uses this.
    No,
    /// Exclusive only for a Vercel span with no `inputTokenDetails` and provider `anthropic`.
    VercelLegacy,
}

/// One usage attribute and how its count is defined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UsageSource {
    /// The attribute to read.
    pub source: Source,
    /// Whether an input count from this source includes cache tokens.
    pub input_includes_cache: Inclusive,
    /// Whether an output count from this source includes reasoning tokens.
    pub output_includes_reasoning: Inclusive,
}

const fn count(
    key: &'static str,
    convention: Convention,
    input: Inclusive,
    output: Inclusive,
) -> UsageSource {
    UsageSource {
        source: span(key, convention),
        input_includes_cache: input,
        output_includes_reasoning: output,
    }
}

const INPUT: &[UsageSource] = &[
    count(
        "gen_ai.usage.input_tokens",
        Convention::Genai,
        Inclusive::VercelLegacy,
        Inclusive::Yes,
    ),
    count(
        "llm.token_count.prompt",
        Convention::Openinference,
        Inclusive::Yes,
        Inclusive::Yes,
    ),
    count(
        "gen_ai.usage.prompt_tokens",
        Convention::Genai,
        Inclusive::Yes,
        Inclusive::Yes,
    ),
    count(
        "ai.usage.inputTokens",
        Convention::Vercel,
        Inclusive::VercelLegacy,
        Inclusive::Yes,
    ),
    count(
        "ai.usage.promptTokens",
        Convention::Vercel,
        Inclusive::VercelLegacy,
        Inclusive::Yes,
    ),
];

const OUTPUT: &[UsageSource] = &[
    count(
        "gen_ai.usage.output_tokens",
        Convention::Genai,
        Inclusive::Yes,
        Inclusive::Yes,
    ),
    count(
        "llm.token_count.completion",
        Convention::Openinference,
        Inclusive::Yes,
        Inclusive::Yes,
    ),
    count(
        "gen_ai.usage.completion_tokens",
        Convention::Genai,
        Inclusive::Yes,
        Inclusive::Yes,
    ),
    count(
        "ai.usage.outputTokens",
        Convention::Vercel,
        Inclusive::Yes,
        Inclusive::Yes,
    ),
    count(
        "ai.usage.completionTokens",
        Convention::Vercel,
        Inclusive::Yes,
        Inclusive::Yes,
    ),
];

const CACHE_READ: &[UsageSource] = &[
    count(
        "gen_ai.usage.cache_read.input_tokens",
        Convention::Genai,
        Inclusive::Yes,
        Inclusive::Yes,
    ),
    count(
        "llm.token_count.prompt_details.cache_read",
        Convention::Openinference,
        Inclusive::Yes,
        Inclusive::Yes,
    ),
    count(
        "ai.usage.inputTokenDetails.cacheReadTokens",
        Convention::Vercel,
        Inclusive::Yes,
        Inclusive::Yes,
    ),
    count(
        "ai.usage.cachedInputTokens",
        Convention::Vercel,
        Inclusive::Yes,
        Inclusive::Yes,
    ),
    count(
        "gen_ai.usage.cache_read_input_tokens",
        Convention::Genai,
        Inclusive::Yes,
        Inclusive::Yes,
    ),
    count(
        "gen_ai.usage.details.cache_read_tokens",
        Convention::Genai,
        Inclusive::Yes,
        Inclusive::Yes,
    ),
    count(
        "gen_ai.usage.input_tokens.cached",
        Convention::Genai,
        Inclusive::Yes,
        Inclusive::Yes,
    ),
];

const CACHE_WRITE: &[UsageSource] = &[
    count(
        "gen_ai.usage.cache_write.input_tokens",
        Convention::Genai,
        Inclusive::Yes,
        Inclusive::Yes,
    ),
    count(
        "gen_ai.usage.cache_creation.input_tokens",
        Convention::Genai,
        Inclusive::Yes,
        Inclusive::Yes,
    ),
    count(
        "llm.token_count.prompt_details.cache_write",
        Convention::Openinference,
        Inclusive::Yes,
        Inclusive::Yes,
    ),
    count(
        "ai.usage.inputTokenDetails.cacheWriteTokens",
        Convention::Vercel,
        Inclusive::Yes,
        Inclusive::Yes,
    ),
    count(
        "gen_ai.usage.cache_creation_input_tokens",
        Convention::Genai,
        Inclusive::Yes,
        Inclusive::Yes,
    ),
    count(
        "gen_ai.usage.cache_write_input_tokens",
        Convention::Genai,
        Inclusive::Yes,
        Inclusive::Yes,
    ),
    count(
        "gen_ai.usage.details.cache_write_tokens",
        Convention::Genai,
        Inclusive::Yes,
        Inclusive::Yes,
    ),
];

const REASONING: &[UsageSource] = &[
    count(
        "gen_ai.usage.reasoning.output_tokens",
        Convention::Genai,
        Inclusive::Yes,
        Inclusive::Yes,
    ),
    count(
        "llm.token_count.completion_details.reasoning",
        Convention::Openinference,
        Inclusive::Yes,
        Inclusive::Yes,
    ),
    count(
        "ai.usage.outputTokenDetails.reasoningTokens",
        Convention::Vercel,
        Inclusive::Yes,
        Inclusive::Yes,
    ),
    count(
        "ai.usage.reasoningTokens",
        Convention::Vercel,
        Inclusive::Yes,
        Inclusive::Yes,
    ),
    count(
        "gen_ai.usage.reasoning_tokens",
        Convention::Genai,
        Inclusive::Yes,
        Inclusive::Yes,
    ),
    count(
        "gen_ai.usage.output_tokens.reasoning",
        Convention::Genai,
        Inclusive::Yes,
        Inclusive::Yes,
    ),
];

const TOTAL: &[UsageSource] = &[
    count(
        "llm.token_count.total",
        Convention::Openinference,
        Inclusive::Yes,
        Inclusive::Yes,
    ),
    count(
        "gen_ai.usage.total_tokens",
        Convention::Genai,
        Inclusive::Yes,
        Inclusive::Yes,
    ),
    count(
        "ai.usage.totalTokens",
        Convention::Vercel,
        Inclusive::Yes,
        Inclusive::Yes,
    ),
    count(
        "ai.usage.tokens",
        Convention::Vercel,
        Inclusive::Yes,
        Inclusive::Yes,
    ),
    count(
        "llm.usage.total_tokens",
        Convention::Openinference,
        Inclusive::Yes,
        Inclusive::Yes,
    ),
];

/// Token columns for one span. Cost is not computed here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Usage {
    /// Input tokens, after add-back and the safety net.
    pub input_tokens: Option<i64>,
    /// Output tokens, after reasoning add-back.
    pub output_tokens: Option<i64>,
    /// Cache-read tokens.
    pub cache_read_tokens: Option<i64>,
    /// Cache-write tokens.
    pub cache_write_tokens: Option<i64>,
    /// Reasoning tokens.
    pub reasoning_tokens: Option<i64>,
    /// Reported total, otherwise `input + output` when both are known.
    pub total_tokens: Option<i64>,
}

/// Resolve token columns. On any other kind the usage attributes stay unread.
pub(crate) fn usage(
    bag: &mut AttrBag<'_>,
    kind: SpanKind,
    provider: Option<&str>,
    warnings: &mut Warnings,
) -> Usage {
    if !matches!(kind, SpanKind::Llm | SpanKind::Embedding) {
        return Usage::default();
    }
    let resource = ResourceView::new(&[]);
    let legacy = vercel_legacy(bag, provider);
    let input = first_count(bag, &resource, INPUT, warnings);
    let output = first_count(bag, &resource, OUTPUT, warnings);
    let mut cache_read =
        first_count(bag, &resource, CACHE_READ, warnings).map(|(value, _, _)| value);
    let mut cache_write = match first_count(bag, &resource, CACHE_WRITE, warnings) {
        Some((value, _, _)) => Some(value),
        None => provider_cache_write(bag, warnings),
    };
    let mut reasoning = first_count(bag, &resource, REASONING, warnings).map(|(value, _, _)| value);
    let mut reported_total =
        first_count(bag, &resource, TOTAL, warnings).map(|(value, _, _)| value);
    let details = langfuse_usage(bag, warnings);
    let mut from_details = false;
    let input = match input {
        Some(found) => Some(found),
        None => details
            .as_ref()
            .and_then(|details| details.input)
            .map(|value| {
                from_details = true;
                (value, Inclusive::No, Inclusive::Yes)
            }),
    };
    let output = match output {
        Some(found) => Some(found),
        None => details
            .as_ref()
            .and_then(|details| details.output)
            .map(|value| {
                from_details = true;
                (value, Inclusive::Yes, Inclusive::Yes)
            }),
    };
    fill_detail(
        &mut cache_read,
        details.as_ref().and_then(|details| details.cache_read),
        &mut from_details,
    );
    fill_detail(
        &mut cache_write,
        details.as_ref().and_then(|details| details.cache_write),
        &mut from_details,
    );
    fill_detail(
        &mut reasoning,
        details.as_ref().and_then(|details| details.reasoning),
        &mut from_details,
    );
    fill_detail(
        &mut reported_total,
        details.as_ref().and_then(|details| details.total),
        &mut from_details,
    );
    if from_details {
        bag.take(USAGE_DETAILS);
    }
    let metadata = gemini_usage(bag, warnings);
    let input = match input {
        Some(found) => Some(found),
        None => metadata
            .as_ref()
            .and_then(|metadata| metadata.input)
            .map(|value| (value, Inclusive::Yes, Inclusive::Yes)),
    };
    let output = match output {
        Some(found) => Some(found),
        None => metadata
            .as_ref()
            .and_then(|metadata| metadata.output)
            .map(|value| (value, Inclusive::Yes, Inclusive::No)),
    };
    fill_if_empty(
        &mut cache_read,
        metadata.as_ref().and_then(|metadata| metadata.cache_read),
    );
    fill_if_empty(
        &mut reasoning,
        metadata.as_ref().and_then(|metadata| metadata.reasoning),
    );
    fill_if_empty(
        &mut reported_total,
        metadata.as_ref().and_then(|metadata| metadata.total),
    );

    let mut input_tokens = input.map(|(value, _, _)| value);
    let mut input_inclusive = input
        .map(|(_, semantics, _)| input_is_inclusive(semantics, legacy))
        .unwrap_or(true);
    if input_tokens.is_none()
        && let Some(no_cache) = no_cache_tokens(bag, warnings)
    {
        input_tokens = Some(no_cache.saturating_add(cache_sum(cache_read, cache_write)));
        input_inclusive = true;
        bag.take(NO_CACHE_TOKENS);
    }
    if let Some(value) = input_tokens
        && !input_inclusive
    {
        input_tokens = Some(value.saturating_add(cache_sum(cache_read, cache_write)));
    }
    if let Some(value) = input_tokens {
        let cache = cache_sum(cache_read, cache_write);
        if value < cache {
            input_tokens = Some(value.saturating_add(cache));
            warnings.add(Warning::InputExcludesCache);
        }
    }

    let mut output_tokens = output.map(|(value, _, _)| value);
    let output_inclusive = output
        .map(|(_, _, semantics)| output_is_inclusive(semantics))
        .unwrap_or(true);
    if let (Some(value), Some(reasoning)) = (output_tokens, reasoning)
        && !output_inclusive
    {
        output_tokens = Some(value.saturating_add(reasoning));
    }

    let total_tokens = reported_total.or(match (input_tokens, output_tokens) {
        (Some(input), Some(output)) => Some(input.saturating_add(output)),
        _ => None,
    });
    Usage {
        input_tokens,
        output_tokens,
        cache_read_tokens: cache_read,
        cache_write_tokens: cache_write,
        reasoning_tokens: reasoning,
        total_tokens,
    }
}

struct GeminiUsage {
    input: Option<i64>,
    output: Option<i64>,
    total: Option<i64>,
    cache_read: Option<i64>,
    reasoning: Option<i64>,
}

/// `usage_metadata` inside `gcp.vertex.agent.llm_response`.
///
/// `candidates_token_count` excludes thoughts. The attribute stays in place:
/// the ADK reader consumes it as the output payload.
fn gemini_usage(bag: &AttrBag<'_>, warnings: &mut Warnings) -> Option<GeminiUsage> {
    let value = bag.get("gcp.vertex.agent.llm_response")?;
    let json = crate::payload::decode_json_text(value, warnings)?;
    let object = json.get("usage_metadata")?.as_object()?;
    Some(GeminiUsage {
        input: detail_count(object, &["prompt_token_count"], warnings),
        output: detail_count(object, &["candidates_token_count"], warnings),
        total: detail_count(object, &["total_token_count"], warnings),
        cache_read: detail_count(object, &["cached_content_token_count"], warnings),
        reasoning: detail_count(object, &["thoughts_token_count"], warnings),
    })
}

/// Time to first token, in nanoseconds, for any span kind.
///
/// `warnings` is not in the plan's sketch; a non-numeric candidate records
/// `invalid_number` and is skipped, matching rule 20.
pub(crate) fn ttft(bag: &mut AttrBag<'_>, start_time: i64, warnings: &mut Warnings) -> Option<i64> {
    if let Some(nanos) = take(bag, attr::TTFT_NS, warnings, read_nanos) {
        return Some(nanos);
    }
    if let Some(nanos) = take(
        bag,
        "gen_ai.response.time_to_first_chunk",
        warnings,
        read_seconds,
    ) {
        return Some(nanos);
    }
    if let Some(nanos) = take(bag, "ai.response.msToFirstChunk", warnings, read_millis) {
        return Some(nanos);
    }
    if let Some(nanos) = take(bag, "ai.stream.msToFirstChunk", warnings, read_millis) {
        return Some(nanos);
    }
    take_instant(
        bag,
        "langfuse.observation.completion_start_time",
        start_time,
        warnings,
    )
}

struct LangfuseUsage {
    input: Option<i64>,
    output: Option<i64>,
    total: Option<i64>,
    cache_read: Option<i64>,
    cache_write: Option<i64>,
    reasoning: Option<i64>,
}

fn fill_if_empty(slot: &mut Option<i64>, value: Option<i64>) {
    if slot.is_none()
        && let Some(value) = value
    {
        *slot = Some(value);
    }
}

fn fill_detail(slot: &mut Option<i64>, value: Option<i64>, used: &mut bool) {
    if slot.is_none()
        && let Some(value) = value
    {
        *slot = Some(value);
        *used = true;
    }
}

/// Langfuse `usage_details`, after the attribute sources.
///
/// `input` excludes cache. The attribute is consumed only when a column is
/// filled from it.
fn langfuse_usage(bag: &AttrBag<'_>, warnings: &mut Warnings) -> Option<LangfuseUsage> {
    let value = bag.get(USAGE_DETAILS)?;
    let json = crate::payload::decode_json_text(value, warnings)?;
    let object = json.as_object()?;
    Some(LangfuseUsage {
        input: detail_count(object, &["input"], warnings),
        output: detail_count(object, &["output"], warnings),
        total: detail_count(object, &["total"], warnings),
        cache_read: detail_count(
            object,
            &["input_cached_tokens", "cache_read_input_tokens"],
            warnings,
        ),
        cache_write: detail_count(object, &["cache_creation_input_tokens"], warnings),
        reasoning: detail_count(
            object,
            &["output_reasoning_tokens", "reasoning_tokens"],
            warnings,
        ),
    })
}

fn detail_count(
    object: &serde_json::Map<String, Json>,
    keys: &[&str],
    warnings: &mut Warnings,
) -> Option<i64> {
    for key in keys {
        let Some(value) = object.get(*key) else {
            continue;
        };
        return match json_count(Some(value)) {
            Ok(Some(count)) if count >= 0 => Some(count),
            Ok(Some(_)) | Err(InvalidNumber) => {
                warnings.add(Warning::InvalidNumber);
                None
            }
            Ok(None) => None,
        };
    }
    None
}

fn first_count(
    bag: &mut AttrBag<'_>,
    resource: &ResourceView<'_>,
    table: &[UsageSource],
    warnings: &mut Warnings,
) -> Option<(i64, Inclusive, Inclusive)> {
    for item in table {
        if let Some(resolved) = first_i64(bag, resource, &[item.source], warnings) {
            return Some((
                resolved.value,
                item.input_includes_cache,
                item.output_includes_reasoning,
            ));
        }
    }
    None
}

/// Anthropic cache write recorded only inside `ai.response.providerMetadata`.
///
/// v5 uses `anthropic.cacheCreationInputTokens`. Later responses use
/// `anthropic.usage.cache_creation_input_tokens`. The attribute stays unread:
/// it also carries provider fields that are not token counts.
fn provider_cache_write(bag: &AttrBag<'_>, warnings: &mut Warnings) -> Option<i64> {
    let value = bag.get("ai.response.providerMetadata")?;
    let json = crate::payload::decode_json_text(value, warnings)?;
    match cache_write_count(&json) {
        Ok(count) => count,
        Err(InvalidNumber) => {
            warnings.add(Warning::InvalidNumber);
            None
        }
    }
}

fn cache_write_count(json: &Json) -> Result<Option<i64>, InvalidNumber> {
    let Some(anthropic) = json.get("anthropic") else {
        return Ok(None);
    };
    if let Some(count) = json_count(anthropic.get("cacheCreationInputTokens"))? {
        return Ok(Some(count));
    }
    json_count(
        anthropic
            .get("usage")
            .and_then(|usage| usage.get("cache_creation_input_tokens")),
    )
}

fn json_count(value: Option<&Json>) -> Result<Option<i64>, InvalidNumber> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value {
        Json::Null => Ok(None),
        Json::Number(number) => number_count(number),
        Json::String(text) => as_i64(&AnyValue {
            value: Some(Value::StringValue(text.clone())),
        }),
        _ => Err(InvalidNumber),
    }
}

fn number_count(number: &serde_json::Number) -> Result<Option<i64>, InvalidNumber> {
    if let Some(value) = number.as_i64() {
        return Ok(Some(value));
    }
    let Some(value) = number.as_f64() else {
        return Err(InvalidNumber);
    };
    if !value.is_finite()
        || value.fract() != 0.0
        || value < i64::MIN as f64
        || value >= i64::MAX as f64
    {
        return Err(InvalidNumber);
    }
    Ok(Some(value as i64))
}

fn vercel_legacy(bag: &AttrBag<'_>, provider: Option<&str>) -> bool {
    provider == Some("anthropic")
        && bag.keys_with_prefix("ai.").next().is_some()
        && bag.keys_with_prefix(INPUT_TOKEN_DETAILS).next().is_none()
}

fn input_is_inclusive(semantics: Inclusive, legacy: bool) -> bool {
    match semantics {
        Inclusive::Yes => true,
        Inclusive::No => false,
        Inclusive::VercelLegacy => !legacy,
    }
}

fn output_is_inclusive(semantics: Inclusive) -> bool {
    match semantics {
        Inclusive::Yes | Inclusive::VercelLegacy => true,
        Inclusive::No => false,
    }
}

fn cache_sum(read: Option<i64>, write: Option<i64>) -> i64 {
    read.unwrap_or(0).saturating_add(write.unwrap_or(0))
}

fn no_cache_tokens(bag: &AttrBag<'_>, warnings: &mut Warnings) -> Option<i64> {
    let value = bag.get(NO_CACHE_TOKENS)?;
    match as_i64(value) {
        Ok(Some(number)) => Some(number),
        Ok(None) => None,
        Err(InvalidNumber) => {
            warnings.add(Warning::InvalidNumber);
            None
        }
    }
}

enum Read {
    Value(i64),
    Skip,
    Invalid,
}

fn take(
    bag: &mut AttrBag<'_>,
    key: &str,
    warnings: &mut Warnings,
    read: impl Fn(&AnyValue) -> Read,
) -> Option<i64> {
    let value = bag.get(key)?;
    match read(value) {
        Read::Value(nanos) => {
            bag.take(key);
            Some(nanos)
        }
        Read::Skip => None,
        Read::Invalid => {
            warnings.add(Warning::InvalidNumber);
            None
        }
    }
}

fn take_instant(
    bag: &mut AttrBag<'_>,
    key: &str,
    start_time: i64,
    warnings: &mut Warnings,
) -> Option<i64> {
    let value = bag.get(key)?;
    let read = match as_i64(value) {
        Ok(Some(instant)) => duration_since(instant, start_time),
        Ok(None) => Read::Skip,
        Err(InvalidNumber) => match as_str(value) {
            Some(text) => match parse_rfc3339(&unquoted(text)) {
                Ok(instant) => duration_since(instant, start_time),
                Err(_) => Read::Skip,
            },
            None => Read::Invalid,
        },
    };
    match read {
        Read::Value(nanos) => {
            bag.take(key);
            Some(nanos)
        }
        Read::Skip => None,
        Read::Invalid => {
            warnings.add(Warning::InvalidNumber);
            None
        }
    }
}

/// The Langfuse SDK JSON-encodes its attribute values, so an instant may
/// arrive as a JSON string literal (`"\"2025-01-01T00:00:00Z\""`).
fn unquoted(text: &str) -> std::borrow::Cow<'_, str> {
    if text.starts_with('"')
        && let Ok(inner) = serde_json::from_str::<String>(text)
    {
        return std::borrow::Cow::Owned(inner);
    }
    std::borrow::Cow::Borrowed(text)
}

fn read_nanos(value: &AnyValue) -> Read {
    match as_i64(value) {
        Ok(Some(nanos)) if nanos >= 0 => Read::Value(nanos),
        Ok(_) => Read::Skip,
        Err(InvalidNumber) => Read::Invalid,
    }
}

fn read_seconds(value: &AnyValue) -> Read {
    match as_f64(value) {
        Ok(Some(seconds)) if seconds >= 0.0 => match scale(seconds, 1_000_000_000.0) {
            Some(nanos) => Read::Value(nanos),
            None => Read::Skip,
        },
        Ok(_) => Read::Skip,
        Err(InvalidNumber) => Read::Invalid,
    }
}

fn read_millis(value: &AnyValue) -> Read {
    match as_i64(value) {
        Ok(Some(millis)) if millis >= 0 => match millis.checked_mul(1_000_000) {
            Some(nanos) => Read::Value(nanos),
            None => Read::Skip,
        },
        Ok(_) => Read::Skip,
        Err(InvalidNumber) => match as_f64(value) {
            Ok(Some(millis)) if millis >= 0.0 => match scale(millis, 1_000_000.0) {
                Some(nanos) => Read::Value(nanos),
                None => Read::Skip,
            },
            Ok(_) => Read::Skip,
            Err(InvalidNumber) => Read::Invalid,
        },
    }
}

fn scale(amount: f64, factor: f64) -> Option<i64> {
    let nanos = amount * factor;
    if !nanos.is_finite() {
        return None;
    }
    let rounded = nanos.round();
    if !(0.0..=i64::MAX as f64).contains(&rounded) {
        return None;
    }
    Some(rounded as i64)
}

fn duration_since(instant: i64, start_time: i64) -> Read {
    match instant.checked_sub(start_time) {
        Some(delta) if delta >= 0 => Read::Value(delta),
        _ => Read::Skip,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opentelemetry_proto::tonic::common::v1::any_value::Value;
    use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue};

    use crate::bag::Use;

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

    fn counted<'a>(
        attrs: &'a [KeyValue],
        kind: SpanKind,
        provider: Option<&str>,
    ) -> (Usage, Warnings, AttrBag<'a>) {
        let mut warnings = Warnings::default();
        let mut bag = AttrBag::new(attrs, &mut warnings);
        let usage = usage(&mut bag, kind, provider, &mut warnings);
        (usage, warnings, bag)
    }

    #[test]
    fn inclusive_sources_keep_the_reported_counts() {
        let attrs = [
            int_attr("llm.token_count.prompt", 9),
            int_attr("gen_ai.usage.input_tokens", 50),
            int_attr("gen_ai.usage.output_tokens", 10),
            int_attr("gen_ai.usage.cache_read.input_tokens", 20),
            int_attr("gen_ai.usage.cache_write.input_tokens", 5),
            int_attr("gen_ai.usage.audio.input_tokens", 4),
        ];
        let (usage, warnings, bag) = counted(&attrs, SpanKind::Llm, Some("anthropic"));
        assert_eq!(usage.input_tokens, Some(50));
        assert_eq!(usage.output_tokens, Some(10));
        assert_eq!(usage.cache_read_tokens, Some(20));
        assert_eq!(usage.cache_write_tokens, Some(5));
        assert_eq!(usage.total_tokens, Some(60));
        assert_eq!(warnings.to_attribute(), None);
        assert_eq!(bag.state("gen_ai.usage.input_tokens"), Some(Use::Consumed));
        assert_eq!(bag.state("llm.token_count.prompt"), Some(Use::Unread));
        assert_eq!(
            bag.state("gen_ai.usage.audio.input_tokens"),
            Some(Use::Unread)
        );
    }

    #[test]
    fn langfuse_usage_details_excludes_cache_and_loses_to_an_attribute() {
        let attrs = [string_attr(
            "langfuse.observation.usage_details",
            r#"{"input":100,"output":20,"input_cached_tokens":40,"cache_creation_input_tokens":10,"output_reasoning_tokens":5,"total":160}"#,
        )];
        let (usage, warnings, bag) = counted(&attrs, SpanKind::Llm, None);
        assert_eq!(usage.input_tokens, Some(150));
        assert_eq!(usage.cache_read_tokens, Some(40));
        assert_eq!(usage.cache_write_tokens, Some(10));
        assert_eq!(usage.output_tokens, Some(20));
        assert_eq!(usage.reasoning_tokens, Some(5));
        assert_eq!(usage.total_tokens, Some(160));
        assert_eq!(warnings.to_attribute(), None);
        assert_eq!(
            bag.state("langfuse.observation.usage_details"),
            Some(Use::Consumed)
        );

        let attrs = [string_attr(
            "langfuse.observation.usage_details",
            r#"{"cache_read_input_tokens":7,"reasoning_tokens":3,"output":4}"#,
        )];
        let (usage, _, _) = counted(&attrs, SpanKind::Llm, None);
        assert_eq!(usage.cache_read_tokens, Some(7));
        assert_eq!(usage.reasoning_tokens, Some(3));
        assert_eq!(usage.output_tokens, Some(4));
        assert_eq!(usage.input_tokens, None);

        let attrs = [
            int_attr("gen_ai.usage.input_tokens", 12),
            string_attr(
                "langfuse.observation.usage_details",
                r#"{"input":100,"output":20}"#,
            ),
        ];
        let (usage, _, bag) = counted(&attrs, SpanKind::Llm, None);
        assert_eq!(usage.input_tokens, Some(12));
        assert_eq!(usage.output_tokens, Some(20));
        assert_eq!(
            bag.state("langfuse.observation.usage_details"),
            Some(Use::Consumed)
        );

        let attrs = [string_attr(
            "langfuse.observation.usage_details",
            r#"{"input":-3,"input_cached_tokens":null,"cache_read_input_tokens":9}"#,
        )];
        let (usage, warnings, bag) = counted(&attrs, SpanKind::Llm, None);
        assert_eq!(usage.input_tokens, None);
        assert_eq!(usage.cache_read_tokens, None);
        assert_eq!(
            warnings.to_attribute().as_deref(),
            Some("[\"invalid_number\"]")
        );
        assert_eq!(
            bag.state("langfuse.observation.usage_details"),
            Some(Use::Unread)
        );
    }

    #[test]
    fn gemini_usage_metadata_adds_thoughts_unless_gen_ai_already_counted() {
        let response = r#"{"usage_metadata":{"prompt_token_count":100,"candidates_token_count":20,"thoughts_token_count":8,"total_token_count":128}}"#;
        let attrs = [string_attr("gcp.vertex.agent.llm_response", response)];
        let (usage, warnings, bag) = counted(&attrs, SpanKind::Llm, None);
        assert_eq!(usage.input_tokens, Some(100));
        assert_eq!(usage.output_tokens, Some(28));
        assert_eq!(usage.reasoning_tokens, Some(8));
        assert_eq!(usage.total_tokens, Some(128));
        assert_eq!(warnings.to_attribute(), None);
        assert_eq!(
            bag.state("gcp.vertex.agent.llm_response"),
            Some(Use::Unread)
        );

        let attrs = [
            int_attr("gen_ai.usage.output_tokens", 30),
            int_attr("gen_ai.usage.reasoning.output_tokens", 8),
            string_attr("gcp.vertex.agent.llm_response", response),
        ];
        let (usage, _, _) = counted(&attrs, SpanKind::Llm, None);
        assert_eq!(usage.output_tokens, Some(30));
        assert_eq!(usage.reasoning_tokens, Some(8));
        assert_eq!(usage.input_tokens, Some(100));
    }

    #[test]
    fn openinference_counts_are_inclusive() {
        let attrs = [
            int_attr("llm.token_count.prompt", 30),
            int_attr("llm.token_count.completion", 8),
            int_attr("llm.token_count.prompt_details.cache_read", 12),
            int_attr("llm.token_count.prompt_details.audio", 3),
        ];
        let (usage, warnings, bag) = counted(&attrs, SpanKind::Embedding, None);
        assert_eq!(usage.input_tokens, Some(30));
        assert_eq!(usage.output_tokens, Some(8));
        assert_eq!(usage.cache_read_tokens, Some(12));
        assert_eq!(usage.total_tokens, Some(38));
        assert_eq!(warnings.to_attribute(), None);
        assert_eq!(
            bag.state("llm.token_count.prompt_details.audio"),
            Some(Use::Unread)
        );
    }

    #[test]
    fn vercel_legacy_input_is_exclusive_only_without_token_details() {
        let attrs = [
            int_attr("ai.usage.inputTokens", 100),
            int_attr("ai.usage.cachedInputTokens", 40),
            int_attr("ai.usage.outputTokens", 20),
        ];
        let (usage, warnings, _) = counted(&attrs, SpanKind::Llm, Some("anthropic"));
        assert_eq!(usage.input_tokens, Some(140));
        assert_eq!(usage.output_tokens, Some(20));
        assert_eq!(usage.cache_read_tokens, Some(40));
        assert_eq!(usage.total_tokens, Some(160));
        assert_eq!(warnings.to_attribute(), None);

        let (usage, warnings, _) = counted(&attrs, SpanKind::Llm, Some("openai"));
        assert_eq!(usage.input_tokens, Some(100));
        assert_eq!(warnings.to_attribute(), None);

        let mut detailed = attrs.to_vec();
        detailed.push(int_attr("ai.usage.inputTokenDetails.cacheReadTokens", 40));
        let (usage, _, _) = counted(&detailed, SpanKind::Llm, Some("anthropic"));
        assert_eq!(usage.input_tokens, Some(100));
        assert_eq!(usage.cache_read_tokens, Some(40));
    }

    #[test]
    fn anthropic_cache_write_comes_from_provider_metadata_when_no_attribute_does() {
        let metadata = r#"{"anthropic":{"cacheCreationInputTokens":20,"usage":{"cache_creation_input_tokens":99}}}"#;
        let attrs = [
            string_attr("ai.model.provider", "anthropic"),
            int_attr("ai.usage.inputTokens", 100),
            int_attr("ai.usage.outputTokens", 10),
            string_attr("ai.response.providerMetadata", metadata),
        ];
        let (usage, warnings, bag) = counted(&attrs, SpanKind::Llm, Some("anthropic"));
        assert_eq!(usage.cache_write_tokens, Some(20));
        assert_eq!(usage.input_tokens, Some(120));
        assert_eq!(warnings.to_attribute(), None);
        assert_eq!(bag.state("ai.response.providerMetadata"), Some(Use::Unread));

        let nested = r#"{"anthropic":{"usage":{"cache_creation_input_tokens":8}}}"#;
        let attrs = [
            int_attr("ai.usage.inputTokens", 100),
            string_attr("ai.response.providerMetadata", nested),
        ];
        let (usage, _, _) = counted(&attrs, SpanKind::Llm, Some("anthropic"));
        assert_eq!(usage.cache_write_tokens, Some(8));
        assert_eq!(usage.input_tokens, Some(108));

        let mut with_attribute = attrs.to_vec();
        with_attribute.push(int_attr("ai.usage.inputTokenDetails.cacheWriteTokens", 3));
        let (usage, _, bag) = counted(&with_attribute, SpanKind::Llm, Some("anthropic"));
        assert_eq!(usage.cache_write_tokens, Some(3));
        assert_eq!(bag.state("ai.response.providerMetadata"), Some(Use::Unread));
    }

    #[test]
    fn a_vercel_span_adds_cache_back_onto_mirrored_gen_ai_input() {
        let attrs = [
            string_attr("ai.model.provider", "anthropic"),
            int_attr("gen_ai.usage.input_tokens", 100),
            int_attr("ai.usage.inputTokens", 100),
            int_attr("ai.usage.cachedInputTokens", 40),
        ];
        let (usage, _, bag) = counted(&attrs, SpanKind::Llm, Some("anthropic"));
        assert_eq!(usage.input_tokens, Some(140));
        assert_eq!(bag.state("gen_ai.usage.input_tokens"), Some(Use::Consumed));
        assert_eq!(bag.state("ai.usage.inputTokens"), Some(Use::Unread));
    }

    #[test]
    fn no_cache_tokens_build_an_inclusive_input() {
        let attrs = [
            int_attr(NO_CACHE_TOKENS, 100),
            int_attr("ai.usage.inputTokenDetails.cacheReadTokens", 20),
            int_attr("ai.usage.inputTokenDetails.cacheWriteTokens", 10),
            int_attr("ai.usage.outputTokens", 40),
            int_attr("ai.usage.outputTokenDetails.reasoningTokens", 15),
        ];
        let (usage, warnings, bag) = counted(&attrs, SpanKind::Llm, Some("anthropic"));
        assert_eq!(usage.input_tokens, Some(130));
        assert_eq!(usage.output_tokens, Some(40));
        assert_eq!(usage.reasoning_tokens, Some(15));
        assert_eq!(usage.total_tokens, Some(170));
        assert_eq!(warnings.to_attribute(), None);
        assert_eq!(bag.state(NO_CACHE_TOKENS), Some(Use::Consumed));

        let attrs = [
            int_attr("ai.usage.inputTokens", 130),
            int_attr(NO_CACHE_TOKENS, 100),
            int_attr("ai.usage.inputTokenDetails.cacheReadTokens", 20),
        ];
        let (usage, _, bag) = counted(&attrs, SpanKind::Llm, Some("openai"));
        assert_eq!(usage.input_tokens, Some(130));
        assert_eq!(bag.state(NO_CACHE_TOKENS), Some(Use::Unread));
    }

    #[test]
    fn the_safety_net_adds_cache_back_and_keeps_a_reported_total() {
        let attrs = [
            int_attr("gen_ai.usage.input_tokens", 10),
            int_attr("gen_ai.usage.cache_read.input_tokens", 40),
            int_attr("gen_ai.usage.cache_write.input_tokens", 10),
            int_attr("gen_ai.usage.output_tokens", 5),
        ];
        let (usage, warnings, _) = counted(&attrs, SpanKind::Llm, None);
        assert_eq!(usage.input_tokens, Some(60));
        assert_eq!(usage.output_tokens, Some(5));
        assert_eq!(usage.total_tokens, Some(65));
        assert!(warnings.contains(Warning::InputExcludesCache));

        let mut with_total = attrs.to_vec();
        with_total.push(int_attr("gen_ai.usage.total_tokens", 99));
        let (usage, warnings, _) = counted(&with_total, SpanKind::Llm, None);
        assert_eq!(usage.input_tokens, Some(60));
        assert_eq!(usage.total_tokens, Some(99));
        assert!(warnings.contains(Warning::InputExcludesCache));
    }

    #[test]
    fn token_arithmetic_saturates() {
        let attrs = [
            int_attr("ai.usage.inputTokens", i64::MAX),
            int_attr("ai.usage.cachedInputTokens", 1),
            int_attr("ai.usage.outputTokens", 1),
        ];
        let (usage, _, _) = counted(&attrs, SpanKind::Llm, Some("anthropic"));
        assert_eq!(usage.input_tokens, Some(i64::MAX));
        assert_eq!(usage.total_tokens, Some(i64::MAX));

        let attrs = [
            int_attr("gen_ai.usage.input_tokens", 1),
            int_attr("gen_ai.usage.cache_read.input_tokens", i64::MAX),
            int_attr("gen_ai.usage.output_tokens", 1),
        ];
        let (usage, warnings, _) = counted(&attrs, SpanKind::Llm, None);
        assert_eq!(usage.input_tokens, Some(i64::MAX));
        assert_eq!(usage.total_tokens, Some(i64::MAX));
        assert!(warnings.contains(Warning::InputExcludesCache));
    }

    #[test]
    fn other_kinds_leave_usage_attributes_unread() {
        let attrs = [
            int_attr("gen_ai.usage.input_tokens", 99),
            int_attr("gen_ai.usage.output_tokens", 5),
        ];
        let (usage, _, bag) = counted(&attrs, SpanKind::Chain, None);
        assert_eq!(usage, Usage::default());
        assert_eq!(bag.state("gen_ai.usage.input_tokens"), Some(Use::Unread));
        assert_eq!(bag.state("gen_ai.usage.output_tokens"), Some(Use::Unread));
    }

    #[test]
    fn ttft_conversions_skip_negative_values() {
        let start = 1_767_225_600_000_000_000;
        let attrs = [double_attr("gen_ai.response.time_to_first_chunk", 0.25)];
        let mut warnings = Warnings::default();
        let mut bag = AttrBag::new(&attrs, &mut warnings);
        assert_eq!(ttft(&mut bag, start, &mut warnings), Some(250_000_000));
        assert_eq!(
            bag.state("gen_ai.response.time_to_first_chunk"),
            Some(Use::Consumed)
        );

        let attrs = [
            int_attr(attr::TTFT_NS, -5),
            double_attr("gen_ai.response.time_to_first_chunk", 1.5),
        ];
        let mut bag = AttrBag::new(&attrs, &mut warnings);
        assert_eq!(ttft(&mut bag, start, &mut warnings), Some(1_500_000_000));
        assert_eq!(bag.state(attr::TTFT_NS), Some(Use::Unread));

        let attrs = [int_attr("ai.response.msToFirstChunk", 2)];
        let mut bag = AttrBag::new(&attrs, &mut warnings);
        assert_eq!(ttft(&mut bag, start, &mut warnings), Some(2_000_000));

        let attrs = [double_attr("ai.stream.msToFirstChunk", 0.5)];
        let mut bag = AttrBag::new(&attrs, &mut warnings);
        assert_eq!(ttft(&mut bag, start, &mut warnings), Some(500_000));

        let attrs = [string_attr(
            "langfuse.observation.completion_start_time",
            "2026-01-01T00:00:00.250000000Z",
        )];
        let mut bag = AttrBag::new(&attrs, &mut warnings);
        assert_eq!(ttft(&mut bag, start, &mut warnings), Some(250_000_000));

        let attrs = [int_attr(
            "langfuse.observation.completion_start_time",
            start - 1,
        )];
        let mut bag = AttrBag::new(&attrs, &mut warnings);
        assert_eq!(ttft(&mut bag, start, &mut warnings), None);
        assert_eq!(
            bag.state("langfuse.observation.completion_start_time"),
            Some(Use::Unread)
        );
        assert_eq!(warnings.to_attribute(), None);
    }

    #[test]
    fn output_without_reasoning_can_be_exclusive() {
        let mut output: Option<i64> = Some(10);
        let reasoning: Option<i64> = Some(7);
        if let (Some(value), Some(extra)) = (output, reasoning)
            && !output_is_inclusive(Inclusive::No)
        {
            output = Some(value.saturating_add(extra));
        }
        assert_eq!(output, Some(17));
        assert!(output_is_inclusive(Inclusive::Yes));
        assert!(!input_is_inclusive(Inclusive::No, false));
        assert!(!input_is_inclusive(Inclusive::VercelLegacy, true));
        assert!(input_is_inclusive(Inclusive::VercelLegacy, false));
    }
}
