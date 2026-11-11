//! Token columns and time to first token.
//!
//! Spec 02 § Model, provider, usage, § Token semantics, and rules 3 and 4.
//! Gemini `usage_metadata`, Langfuse `usage_details` and ADK-specific keys are
//! not in these tables; later convention phases read them. `gen_ai.usage.*`
//! stays inclusive and relies on the safety net.

use llmobs_core::enums::{Convention, SpanKind};
use llmobs_core::names::attr;
use llmobs_core::time_serde::parse_rfc3339;
use opentelemetry_proto::tonic::common::v1::AnyValue;

use crate::bag::AttrBag;
use crate::fields::{ResourceView, Source, first_i64, span};
use crate::value::{InvalidNumber, as_f64, as_i64, as_str};
use crate::warn::{Warning, Warnings};

const NO_CACHE_TOKENS: &str = "ai.usage.inputTokenDetails.noCacheTokens";
const INPUT_TOKEN_DETAILS: &str = "ai.usage.inputTokenDetails.";

/// Whether a usage count already contains the related tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Inclusive {
    /// The count already includes the related tokens.
    Yes,
    /// Add the related tokens back. Gemini output counts use this; no current
    /// table row does, and the add-back still handles it.
    #[allow(dead_code)]
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
    let cache_read = first_count(bag, &resource, CACHE_READ, warnings).map(|(value, _, _)| value);
    let cache_write = first_count(bag, &resource, CACHE_WRITE, warnings).map(|(value, _, _)| value);
    let reasoning = first_count(bag, &resource, REASONING, warnings).map(|(value, _, _)| value);
    let reported_total = first_count(bag, &resource, TOTAL, warnings).map(|(value, _, _)| value);

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
            Some(text) => match parse_rfc3339(text) {
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
