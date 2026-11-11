//! Precedence tables and the first-usable-value resolver.
//!
//! Spec 02 § Mapping tables and rule 2. The tables themselves arrive with the
//! fields that use them. Resource keys are never consumed: `resource_attributes`
//! keeps every key except `service.name`.
#![allow(dead_code)]

use std::collections::HashMap;

use llmobs_core::enums::Convention;
use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue};

use crate::bag::AttrBag;
use crate::value::{InvalidNumber, as_f64, as_i64, as_str, as_string_list};
use crate::warn::{Warning, Warnings};

const EMPTY: AnyValue = AnyValue { value: None };

/// Where a [`Source`] key is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    /// A span attribute. The winning key is marked consumed.
    Span,
    /// A resource attribute. It is looked up and left untouched.
    Resource,
}

/// One candidate in a precedence table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Source {
    /// Attribute key, including its convention prefix.
    pub key: &'static str,
    /// Convention that owns the key. Recorded with the value that wins.
    pub convention: Convention,
    /// Span or resource.
    pub scope: Scope,
}

/// A value taken from the first usable [`Source`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Resolved<T> {
    /// The converted value.
    pub value: T,
    /// The source that produced it.
    pub source: Source,
}

/// Last-duplicate-wins lookup over one resource's attributes.
///
/// Built once per `ResourceSpans`. Duplicate-key warnings stay with the
/// resource bag that encodes `resource_attributes`.
#[derive(Debug)]
pub(crate) struct ResourceView<'a> {
    values: HashMap<&'a str, &'a AnyValue>,
}

impl<'a> ResourceView<'a> {
    pub(crate) fn new(attributes: &'a [KeyValue]) -> Self {
        let mut values = HashMap::with_capacity(attributes.len());
        for attr in attributes {
            values.insert(attr.key.as_str(), attr.value.as_ref().unwrap_or(&EMPTY));
        }
        Self { values }
    }

    pub(crate) fn get(&self, key: &str) -> Option<&'a AnyValue> {
        self.values.get(key).copied()
    }
}

/// A span attribute source.
pub(crate) const fn span(key: &'static str, convention: Convention) -> Source {
    Source {
        key,
        convention,
        scope: Scope::Span,
    }
}

/// A resource attribute source.
pub(crate) const fn resource(key: &'static str, convention: Convention) -> Source {
    Source {
        key,
        convention,
        scope: Scope::Resource,
    }
}

/// First non-empty string. A non-string is skipped with no warning.
pub(crate) fn first_string(
    bag: &mut AttrBag<'_>,
    resource: &ResourceView<'_>,
    sources: &[Source],
) -> Option<Resolved<String>> {
    resolve(bag, resource, sources, None, read_string)
}

/// First integer. An unusable number is skipped, left unread, and warned.
pub(crate) fn first_i64(
    bag: &mut AttrBag<'_>,
    resource: &ResourceView<'_>,
    sources: &[Source],
    warnings: &mut Warnings,
) -> Option<Resolved<i64>> {
    resolve(bag, resource, sources, Some(warnings), read_i64)
}

/// First float. An unusable number is skipped, left unread, and warned.
pub(crate) fn first_f64(
    bag: &mut AttrBag<'_>,
    resource: &ResourceView<'_>,
    sources: &[Source],
    warnings: &mut Warnings,
) -> Option<Resolved<f64>> {
    resolve(bag, resource, sources, Some(warnings), read_f64)
}

/// First string list. A value that is not a list of strings is skipped.
pub(crate) fn first_string_list(
    bag: &mut AttrBag<'_>,
    resource: &ResourceView<'_>,
    sources: &[Source],
) -> Option<Resolved<Vec<String>>> {
    resolve(bag, resource, sources, None, read_string_list)
}

enum Candidate<T> {
    Absent,
    Invalid,
    Value(T),
}

fn resolve<T>(
    bag: &mut AttrBag<'_>,
    resource: &ResourceView<'_>,
    sources: &[Source],
    mut warnings: Option<&mut Warnings>,
    read: impl Fn(&AnyValue) -> Candidate<T>,
) -> Option<Resolved<T>> {
    for source in sources {
        let found = match source.scope {
            Scope::Span => bag.get(source.key).map(&read),
            Scope::Resource => resource.get(source.key).map(&read),
        };
        match found {
            Some(Candidate::Value(value)) => {
                if source.scope == Scope::Span {
                    bag.take(source.key);
                }
                return Some(Resolved {
                    value,
                    source: *source,
                });
            }
            Some(Candidate::Invalid) => {
                if let Some(warnings) = &mut warnings {
                    warnings.add(Warning::InvalidNumber);
                }
            }
            Some(Candidate::Absent) | None => {}
        }
    }
    None
}

fn read_string(value: &AnyValue) -> Candidate<String> {
    match as_str(value) {
        Some(text) if !text.is_empty() => Candidate::Value(text.to_owned()),
        _ => Candidate::Absent,
    }
}

fn read_i64(value: &AnyValue) -> Candidate<i64> {
    if as_str(value).is_some_and(str::is_empty) {
        return Candidate::Absent;
    }
    match as_i64(value) {
        Ok(Some(number)) => Candidate::Value(number),
        Ok(None) => Candidate::Absent,
        Err(InvalidNumber) => Candidate::Invalid,
    }
}

fn read_f64(value: &AnyValue) -> Candidate<f64> {
    if as_str(value).is_some_and(str::is_empty) {
        return Candidate::Absent;
    }
    match as_f64(value) {
        Ok(Some(number)) => Candidate::Value(number),
        Ok(None) => Candidate::Absent,
        Err(InvalidNumber) => Candidate::Invalid,
    }
}

fn read_string_list(value: &AnyValue) -> Candidate<Vec<String>> {
    match as_string_list(value) {
        Some(items) => Candidate::Value(items),
        None => Candidate::Absent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bag::Use;
    use opentelemetry_proto::tonic::common::v1::any_value::Value;

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

    fn bool_attr(key: &str, flag: bool) -> KeyValue {
        KeyValue {
            key: key.to_owned(),
            value: Some(AnyValue {
                value: Some(Value::BoolValue(flag)),
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

    struct Fixture {
        span_attrs: Vec<KeyValue>,
        resource_attrs: Vec<KeyValue>,
    }

    impl Fixture {
        fn span(attrs: Vec<KeyValue>) -> Self {
            Self {
                span_attrs: attrs,
                resource_attrs: Vec::new(),
            }
        }
    }

    fn resolve_string<'a>(
        fixture: &'a Fixture,
        sources: &[Source],
    ) -> (Option<Resolved<String>>, AttrBag<'a>) {
        let mut warnings = Warnings::default();
        let mut bag = AttrBag::new(&fixture.span_attrs, &mut warnings);
        let view = ResourceView::new(&fixture.resource_attrs);
        let resolved = first_string(&mut bag, &view, sources);
        (resolved, bag)
    }

    #[test]
    fn the_first_usable_source_wins() {
        let fixture = Fixture::span(vec![
            string_attr("llm.model_name", "openinference"),
            string_attr("ai.model.id", "vercel"),
            string_attr("gen_ai.request.model", "genai"),
        ]);
        let sources = [
            span("gen_ai.request.model", Convention::Genai),
            span("llm.model_name", Convention::Openinference),
            span("ai.model.id", Convention::Vercel),
        ];
        let (resolved, bag) = resolve_string(&fixture, &sources);
        let resolved = resolved.expect("genai value");
        assert_eq!(resolved.value, "genai");
        assert_eq!(resolved.source, sources[0]);
        assert_eq!(bag.state("gen_ai.request.model"), Some(Use::Consumed));
        assert_eq!(bag.state("llm.model_name"), Some(Use::Unread));
        assert_eq!(bag.state("ai.model.id"), Some(Use::Unread));

        let missing_first = Fixture::span(vec![
            string_attr("llm.model_name", "openinference"),
            string_attr("ai.model.id", "vercel"),
        ]);
        let (resolved, bag) = resolve_string(&missing_first, &sources);
        let resolved = resolved.expect("openinference value");
        assert_eq!(resolved.value, "openinference");
        assert_eq!(resolved.source, sources[1]);
        assert_eq!(bag.state("llm.model_name"), Some(Use::Consumed));
        assert_eq!(bag.state("ai.model.id"), Some(Use::Unread));
    }

    #[test]
    fn an_empty_string_is_skipped_and_left_unread() {
        let attrs = vec![
            string_attr("gen_ai.request.model", ""),
            string_attr("llm.model_name", "kept"),
            string_attr("empty_count", ""),
            int_attr("count", 8),
        ];
        let mut warnings = Warnings::default();
        let mut bag = AttrBag::new(&attrs, &mut warnings);
        let view = ResourceView::new(&[]);
        let text = first_string(
            &mut bag,
            &view,
            &[
                span("gen_ai.request.model", Convention::Genai),
                span("llm.model_name", Convention::Openinference),
            ],
        );
        let text = text.expect("second string");
        assert_eq!(text.value, "kept");
        assert_eq!(bag.state("gen_ai.request.model"), Some(Use::Unread));
        assert_eq!(bag.state("llm.model_name"), Some(Use::Consumed));

        let number = first_i64(
            &mut bag,
            &view,
            &[
                span("empty_count", Convention::Genai),
                span("count", Convention::Openinference),
            ],
            &mut warnings,
        );
        let number = number.expect("integer");
        assert_eq!(number.value, 8);
        assert_eq!(warnings.to_attribute(), None);
        assert_eq!(bag.state("empty_count"), Some(Use::Unread));
        assert_eq!(bag.state("count"), Some(Use::Consumed));
    }

    #[test]
    fn an_invalid_number_is_skipped_and_kept() {
        let attrs = vec![
            bool_attr("gen_ai.usage.input_tokens", true),
            int_attr("llm.token_count.prompt", 4),
            string_attr("gen_ai.usage.output_tokens", "nope"),
            double_attr("ai.usage.outputTokens", 1.5),
            string_attr("blank", ""),
        ];
        let mut warnings = Warnings::default();
        let mut bag = AttrBag::new(&attrs, &mut warnings);
        let view = ResourceView::new(&[]);
        let input = first_i64(
            &mut bag,
            &view,
            &[
                span("gen_ai.usage.input_tokens", Convention::Genai),
                span("llm.token_count.prompt", Convention::Openinference),
            ],
            &mut warnings,
        );
        let input = input.expect("prompt tokens");
        assert_eq!(input.value, 4);
        assert_eq!(input.source.convention, Convention::Openinference);
        assert_eq!(bag.state("gen_ai.usage.input_tokens"), Some(Use::Unread));
        assert_eq!(bag.state("llm.token_count.prompt"), Some(Use::Consumed));

        let output = first_f64(
            &mut bag,
            &view,
            &[
                span("blank", Convention::Genai),
                span("gen_ai.usage.output_tokens", Convention::Genai),
                span("ai.usage.outputTokens", Convention::Vercel),
            ],
            &mut warnings,
        );
        let output = output.expect("output tokens");
        assert_eq!(output.value, 1.5);
        assert_eq!(output.source.convention, Convention::Vercel);
        assert_eq!(bag.state("blank"), Some(Use::Unread));
        assert_eq!(bag.state("gen_ai.usage.output_tokens"), Some(Use::Unread));
        assert_eq!(bag.state("ai.usage.outputTokens"), Some(Use::Consumed));
        assert!(warnings.contains(Warning::InvalidNumber));

        let mut truncated = Vec::new();
        let stored = bag.into_attributes(&mut warnings, &mut truncated);
        assert_eq!(
            stored.get("gen_ai.usage.input_tokens").map(String::as_str),
            Some("true")
        );
        assert_eq!(
            stored.get("gen_ai.usage.output_tokens").map(String::as_str),
            Some("nope")
        );
    }

    #[test]
    fn a_resource_source_is_read_and_not_consumed() {
        let span_attrs = vec![string_attr("langfuse.environment", "dev")];
        let resource_attrs = vec![
            string_attr("deployment.environment.name", ""),
            string_attr("deployment.environment.name", "prod"),
        ];
        let mut warnings = Warnings::default();
        let mut bag = AttrBag::new(&span_attrs, &mut warnings);
        let view = ResourceView::new(&resource_attrs);
        let resolved = first_string(
            &mut bag,
            &view,
            &[
                resource("deployment.environment.name", Convention::Genai),
                span("langfuse.environment", Convention::Langfuse),
            ],
        );
        let resolved = resolved.expect("resource environment");
        assert_eq!(resolved.value, "prod");
        assert_eq!(resolved.source.scope, Scope::Resource);
        assert_eq!(bag.state("langfuse.environment"), Some(Use::Unread));

        let mut bag = AttrBag::new(&span_attrs, &mut warnings);
        let resolved = first_string(
            &mut bag,
            &view,
            &[
                span("langfuse.environment", Convention::Langfuse),
                resource("deployment.environment.name", Convention::Genai),
            ],
        );
        let resolved = resolved.expect("span environment");
        assert_eq!(resolved.value, "dev");
        assert_eq!(bag.state("langfuse.environment"), Some(Use::Consumed));

        let empty_resource = vec![string_attr("deployment.environment.name", "")];
        let view = ResourceView::new(&empty_resource);
        let mut bag = AttrBag::new(&span_attrs, &mut warnings);
        let resolved = first_string(
            &mut bag,
            &view,
            &[
                resource("deployment.environment.name", Convention::Genai),
                span("langfuse.environment", Convention::Langfuse),
            ],
        );
        let resolved = resolved.expect("span fills an empty resource value");
        assert_eq!(resolved.value, "dev");
        assert_eq!(bag.state("langfuse.environment"), Some(Use::Consumed));
    }

    #[test]
    fn the_winning_key_is_consumed_and_losers_stay_unread() {
        let attrs = vec![
            string_attr("first", ""),
            string_attr("second", "kept"),
            string_attr("third", "later"),
        ];
        let mut warnings = Warnings::default();
        let mut bag = AttrBag::new(&attrs, &mut warnings);
        let view = ResourceView::new(&[]);
        let resolved = first_string(
            &mut bag,
            &view,
            &[
                span("first", Convention::Llmobs),
                span("second", Convention::Genai),
                span("third", Convention::Vercel),
            ],
        );
        assert_eq!(resolved.expect("second").value, "kept");
        assert_eq!(bag.state("first"), Some(Use::Unread));
        assert_eq!(bag.state("second"), Some(Use::Consumed));
        assert_eq!(bag.state("third"), Some(Use::Unread));
    }

    #[test]
    fn a_string_list_uses_the_first_list() {
        let attrs = vec![
            int_attr("llmobs.tags", 1),
            string_attr("tag.tags", "[]"),
            string_attr("langfuse.trace.tags", "[\"later\"]"),
        ];
        let mut warnings = Warnings::default();
        let mut bag = AttrBag::new(&attrs, &mut warnings);
        let view = ResourceView::new(&[]);
        let resolved = first_string_list(
            &mut bag,
            &view,
            &[
                span("llmobs.tags", Convention::Llmobs),
                span("tag.tags", Convention::Openinference),
                span("langfuse.trace.tags", Convention::Langfuse),
            ],
        );
        let resolved = resolved.expect("empty list");
        assert!(resolved.value.is_empty());
        assert_eq!(resolved.source.convention, Convention::Openinference);
        assert_eq!(bag.state("llmobs.tags"), Some(Use::Unread));
        assert_eq!(bag.state("tag.tags"), Some(Use::Consumed));
        assert_eq!(bag.state("langfuse.trace.tags"), Some(Use::Unread));
    }
}
