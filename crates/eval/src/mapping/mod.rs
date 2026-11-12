//! Variable mapping expressions (D162).
//!
//! The parser is hand-written. A trailing `?` is the optional marker and is
//! stripped before the source and the JSONPath are read, so a filter's `?`
//! (which is never the last character of a path) stays part of the path.
//! `expr_offset` is a byte index into the original expression. One kibibyte
//! is 1024 bytes.

pub(crate) mod resolve;
mod transcript;

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

use llmobs_core::enums::{PayloadFormat, SpanKind};
use serde_json::Value;
use serde_json_path::JsonPath;

use crate::error::MappingError;

/// One parsed mapping expression, including the text it was parsed from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expr {
    source: Source,
    path: Option<JsonPath>,
    optional: bool,
    text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Source {
    Trace(TraceField),
    Span(SpanSelector, SpanField),
    Expected,
    Item(ItemField),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TraceField {
    Input,
    Output,
    Metadata(String),
    Transcript,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SpanSelector {
    Target,
    Name(String),
    Kind(SpanKind),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SpanField {
    Input,
    Output,
    Metadata(String),
    Attributes(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ItemField {
    Input,
    Metadata(String),
}

/// Named expressions from an evaluator's `variables` object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariableMapping(BTreeMap<String, Expr>);

impl Expr {
    /// Parses one mapping expression.
    ///
    /// # Errors
    ///
    /// Returns [`MappingError::Syntax`] when the expression is longer than 1 KiB
    /// or does not match the grammar. The reason is a fixed string.
    pub fn parse(text: &str) -> Result<Self, MappingError> {
        if text.len() > 1024 {
            return Err(syntax(1024, "expression is longer than 1 KiB"));
        }
        // The suffix is removed from the end, so indexes into `body` are indexes
        // into `text`.
        let (body, optional) = match text.strip_suffix('?') {
            Some(body) => (body, true),
            None => (text, false),
        };
        let (source, consumed) = parse_source(body)?;
        let rest = &body[consumed..];
        let path = if rest.is_empty() {
            None
        } else if rest.starts_with('$') {
            if matches!(source, Source::Trace(TraceField::Transcript)) {
                return Err(syntax(consumed, "transcript has no JSONPath"));
            }
            match JsonPath::parse(rest) {
                Ok(path) => Some(path),
                Err(_) => return Err(syntax(consumed, "invalid JSONPath")),
            }
        } else {
            return Err(syntax(consumed, "trailing characters"));
        };
        Ok(Self {
            source,
            path,
            optional,
            text: text.to_owned(),
        })
    }
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.text)
    }
}

impl VariableMapping {
    /// Parses a JSON object of `{variable: expression}`.
    ///
    /// # Errors
    ///
    /// Returns [`MappingError`] when the value is not an object, a name is not
    /// `^[A-Za-z_][A-Za-z0-9_]{0,63}$`, there are more than 32 entries, a value
    /// is not a string, or an expression does not parse. Expression failures
    /// name the variable.
    pub fn from_json(value: &Value) -> Result<Self, MappingError> {
        let Value::Object(map) = value else {
            return Err(MappingError::NotAnObject);
        };
        if map.len() > 32 {
            return Err(MappingError::TooMany);
        }
        let mut variables = BTreeMap::new();
        for (name, expr_value) in map {
            if !valid_variable_name(name) {
                return Err(MappingError::Variable {
                    name: name.clone(),
                    reason: "invalid variable name",
                });
            }
            let Value::String(text) = expr_value else {
                return Err(MappingError::Variable {
                    name: name.clone(),
                    reason: "expression must be a string",
                });
            };
            let expr = Expr::parse(text).map_err(|source| MappingError::InVariable {
                name: name.clone(),
                source: Box::new(source),
            })?;
            variables.insert(name.clone(), expr);
        }
        Ok(Self(variables))
    }

    /// True when any expression reads `expected` or `item.*`.
    #[must_use]
    pub fn needs_item(&self) -> bool {
        self.0
            .values()
            .any(|expr| matches!(expr.source, Source::Expected | Source::Item(_)))
    }

    /// True when any expression reads the target span (`span.*` without a selector).
    #[must_use]
    pub fn needs_target_span(&self) -> bool {
        self.0
            .values()
            .any(|expr| matches!(expr.source, Source::Span(SpanSelector::Target, _)))
    }
}

fn syntax(expr_offset: usize, reason: &'static str) -> MappingError {
    MappingError::Syntax {
        expr_offset,
        reason,
    }
}

fn parse_source(body: &str) -> Result<(Source, usize), MappingError> {
    if let Some(rest) = body.strip_prefix("trace.") {
        return parse_trace(rest);
    }
    if let Some(rest) = body.strip_prefix("span") {
        return parse_span(body, rest);
    }
    if body.starts_with("expected") {
        return Ok((Source::Expected, "expected".len()));
    }
    if let Some(rest) = body.strip_prefix("item.") {
        return parse_item(rest);
    }
    Err(syntax(0, "unknown source"))
}

fn parse_trace(rest: &str) -> Result<(Source, usize), MappingError> {
    let base = "trace.".len();
    if rest.starts_with("input") {
        return Ok((Source::Trace(TraceField::Input), base + "input".len()));
    }
    if rest.starts_with("output") {
        return Ok((Source::Trace(TraceField::Output), base + "output".len()));
    }
    if rest.starts_with("transcript") {
        return Ok((
            Source::Trace(TraceField::Transcript),
            base + "transcript".len(),
        ));
    }
    if let Some(after) = rest.strip_prefix("metadata.") {
        let key_at = base + "metadata.".len();
        let (key, end) = parse_key(after, key_at)?;
        return Ok((Source::Trace(TraceField::Metadata(key)), end));
    }
    Err(syntax(0, "unknown source"))
}

fn parse_item(rest: &str) -> Result<(Source, usize), MappingError> {
    let base = "item.".len();
    if rest.starts_with("input") {
        return Ok((Source::Item(ItemField::Input), base + "input".len()));
    }
    if let Some(after) = rest.strip_prefix("metadata.") {
        let key_at = base + "metadata.".len();
        let (key, end) = parse_key(after, key_at)?;
        return Ok((Source::Item(ItemField::Metadata(key)), end));
    }
    Err(syntax(0, "unknown source"))
}

fn parse_span(body: &str, rest: &str) -> Result<(Source, usize), MappingError> {
    let span_len = "span".len();
    let (selector, field_at) = if rest.starts_with("[name=\"") {
        let content_at = span_len + "[name=\"".len();
        let (name, after_quote) = parse_quoted(body, content_at)?;
        let after_bracket = expect_bracket(body, after_quote)?;
        (SpanSelector::Name(name), after_bracket)
    } else if rest.starts_with("[kind=\"") {
        let content_at = span_len + "[kind=\"".len();
        let (kind_text, after_quote) = parse_quoted(body, content_at)?;
        let kind =
            SpanKind::from_str(&kind_text).map_err(|_| syntax(content_at, "unknown span kind"))?;
        let after_bracket = expect_bracket(body, after_quote)?;
        (SpanSelector::Kind(kind), after_bracket)
    } else if rest.starts_with('[') {
        return Err(syntax(span_len, "expected a name or kind selector"));
    } else {
        (SpanSelector::Target, span_len)
    };
    parse_span_field(body, field_at, selector)
}

fn expect_bracket(body: &str, at: usize) -> Result<usize, MappingError> {
    if body[at..].starts_with(']') {
        Ok(at + 1)
    } else {
        Err(syntax(at, "expected closing bracket"))
    }
}

fn parse_span_field(
    body: &str,
    at: usize,
    selector: SpanSelector,
) -> Result<(Source, usize), MappingError> {
    let Some(rest) = body[at..].strip_prefix('.') else {
        return Err(syntax(at, "expected a span field"));
    };
    let field_at = at + 1;
    let (field, end) = if rest.starts_with("input") {
        (SpanField::Input, field_at + "input".len())
    } else if rest.starts_with("output") {
        (SpanField::Output, field_at + "output".len())
    } else if let Some(after) = rest.strip_prefix("metadata.") {
        let key_at = field_at + "metadata.".len();
        let (key, end) = parse_key(after, key_at)?;
        (SpanField::Metadata(key), end)
    } else if let Some(after) = rest.strip_prefix("attributes.") {
        let key_at = field_at + "attributes.".len();
        let (key, end) = parse_key(after, key_at)?;
        (SpanField::Attributes(key), end)
    } else {
        return Err(syntax(field_at, "expected a span field"));
    };
    Ok((Source::Span(selector, field), end))
}

/// The key runs to the first `$` or to the end. A final `?` is already gone.
fn parse_key(rest: &str, key_at: usize) -> Result<(String, usize), MappingError> {
    let len = rest.find('$').unwrap_or(rest.len());
    if len == 0 {
        return Err(syntax(key_at, "empty key"));
    }
    Ok((rest[..len].to_owned(), key_at + len))
}

/// Reads a double-quoted string that has already opened. `content_at` is the
/// first byte after the opening quote. Returns the unescaped text and the
/// index just past the closing quote. Only `\"` and `\\` are escapes.
fn parse_quoted(body: &str, content_at: usize) -> Result<(String, usize), MappingError> {
    let mut out = String::new();
    let mut chars = body[content_at..].char_indices();
    while let Some((rel, ch)) = chars.next() {
        let abs = content_at + rel;
        match ch {
            '"' => return Ok((out, abs + 1)),
            '\\' => match chars.next() {
                Some((_, '\\')) => out.push('\\'),
                Some((_, '"')) => out.push('"'),
                Some((_, _)) => return Err(syntax(abs, "unknown escape")),
                None => return Err(syntax(abs, "unterminated string")),
            },
            other => out.push(other),
        }
    }
    Err(syntax(content_at.saturating_sub(1), "unterminated string"))
}

/// Decodes one stored `input` or `output`.
///
/// `messages` and `json` parse as JSON. Storage guarantees that text is valid
/// JSON; a parse failure returns the raw string so a bad row still renders.
/// `text` becomes a JSON string. A missing payload or format is absent.
pub(crate) fn decode_payload(
    payload: Option<&str>,
    format: Option<PayloadFormat>,
) -> Option<Value> {
    let (payload, format) = (payload?, format?);
    Some(match format {
        PayloadFormat::Text => Value::String(payload.to_owned()),
        PayloadFormat::Messages | PayloadFormat::Json => {
            serde_json::from_str(payload).unwrap_or_else(|_| Value::String(payload.to_owned()))
        }
    })
}

fn valid_variable_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(ch) if ch.is_ascii_alphabetic() || ch == '_' => {}
        _ => return false,
    }
    for (index, ch) in chars.enumerate() {
        if index >= 63 || !(ch.is_ascii_alphanumeric() || ch == '_') {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::{Expr, ItemField, Source, SpanField, SpanSelector, TraceField, VariableMapping};
    use crate::error::{MappingError, assert_no_payload};
    use llmobs_core::enums::SpanKind;
    use serde_json::{Value, json};
    use serde_json_path::JsonPath;

    const KINDS: [SpanKind; 10] = [
        SpanKind::Llm,
        SpanKind::Embedding,
        SpanKind::Tool,
        SpanKind::Agent,
        SpanKind::Chain,
        SpanKind::Retriever,
        SpanKind::Reranker,
        SpanKind::Guardrail,
        SpanKind::Evaluator,
        SpanKind::Span,
    ];

    fn path(text: &str) -> JsonPath {
        JsonPath::parse(text).unwrap_or_else(|_| panic!("fixture path {text}"))
    }

    #[test]
    fn parses_every_valid_form() {
        let cases = [
            ("trace.input", Source::Trace(TraceField::Input), false, None),
            (
                "trace.output",
                Source::Trace(TraceField::Output),
                false,
                None,
            ),
            (
                "trace.metadata.llmobs.eval.rule_id",
                Source::Trace(TraceField::Metadata("llmobs.eval.rule_id".to_owned())),
                false,
                None,
            ),
            (
                "trace.transcript",
                Source::Trace(TraceField::Transcript),
                false,
                None,
            ),
            (
                "span.input",
                Source::Span(SpanSelector::Target, SpanField::Input),
                false,
                None,
            ),
            (
                "span.output",
                Source::Span(SpanSelector::Target, SpanField::Output),
                false,
                None,
            ),
            (
                "span.metadata.llmobs.eval.rule_id",
                Source::Span(
                    SpanSelector::Target,
                    SpanField::Metadata("llmobs.eval.rule_id".to_owned()),
                ),
                false,
                None,
            ),
            (
                "span.attributes.llmobs.eval.rule_id",
                Source::Span(
                    SpanSelector::Target,
                    SpanField::Attributes("llmobs.eval.rule_id".to_owned()),
                ),
                false,
                None,
            ),
            (
                "span[name=\"retrieve\"].output",
                Source::Span(SpanSelector::Name("retrieve".to_owned()), SpanField::Output),
                false,
                None,
            ),
            (
                "span[name=\"say \\\"hi\\\"\"].input",
                Source::Span(
                    SpanSelector::Name("say \"hi\"".to_owned()),
                    SpanField::Input,
                ),
                false,
                None,
            ),
            (
                "span[name=\"a\\\\b\"].output",
                Source::Span(SpanSelector::Name("a\\b".to_owned()), SpanField::Output),
                false,
                None,
            ),
            (
                "span[name=\"café\"].input",
                Source::Span(SpanSelector::Name("café".to_owned()), SpanField::Input),
                false,
                None,
            ),
            (
                "span[name=\"\"].output",
                Source::Span(SpanSelector::Name(String::new()), SpanField::Output),
                false,
                None,
            ),
            ("expected", Source::Expected, false, None),
            ("item.input", Source::Item(ItemField::Input), false, None),
            (
                "item.metadata.llmobs.eval.rule_id",
                Source::Item(ItemField::Metadata("llmobs.eval.rule_id".to_owned())),
                false,
                None,
            ),
            (
                "trace.output$.parts[0][?@ > 2]",
                Source::Trace(TraceField::Output),
                false,
                Some("$.parts[0][?@ > 2]"),
            ),
            (
                "trace.input$.foo?",
                Source::Trace(TraceField::Input),
                true,
                Some("$.foo"),
            ),
            ("trace.input?", Source::Trace(TraceField::Input), true, None),
            ("expected?", Source::Expected, true, None),
            (
                "span[kind=\"retriever\"].output?",
                Source::Span(SpanSelector::Kind(SpanKind::Retriever), SpanField::Output),
                true,
                None,
            ),
            (
                "item.metadata.tag$?",
                Source::Item(ItemField::Metadata("tag".to_owned())),
                true,
                Some("$"),
            ),
        ];

        for (text, source, optional, path_text) in cases {
            let expr = Expr::parse(text).unwrap_or_else(|err| panic!("{text}: {err}"));
            assert_eq!(expr.source, source, "{text}");
            assert_eq!(expr.optional, optional, "{text}");
            assert_eq!(expr.to_string(), text);
            match path_text {
                Some(path_text) => assert_eq!(expr.path.as_ref(), Some(&path(path_text)), "{text}"),
                None => assert!(expr.path.is_none(), "{text}"),
            }
        }

        for kind in KINDS {
            let text = format!("span[kind=\"{}\"].output", kind.as_str());
            let expr = Expr::parse(&text).unwrap_or_else(|err| panic!("{text}: {err}"));
            assert_eq!(
                expr.source,
                Source::Span(SpanSelector::Kind(kind), SpanField::Output),
                "{text}"
            );
            assert!(!expr.optional);
            assert!(expr.path.is_none());
        }
    }

    #[test]
    fn rejects_invalid_expressions() {
        let cases = [
            ("", 0, "unknown source"),
            ("nope", 0, "unknown source"),
            ("trace.foo", 0, "unknown source"),
            ("trace.transcript$.answer", 16, "transcript has no JSONPath"),
            ("trace.metadata.", 15, "empty key"),
            ("trace.metadata.$", 15, "empty key"),
            ("span.attributes.", 16, "empty key"),
            ("item.metadata.", 14, "empty key"),
            ("span[name=\"abc].input", 10, "unterminated string"),
            ("span[name=\"\\n\"].input", 11, "unknown escape"),
            ("span[kind=\"http\"].input", 11, "unknown span kind"),
            ("span[kind=\"LLM\"].input", 11, "unknown span kind"),
            ("trace.input.foo", 11, "trailing characters"),
            ("expected.extra", 8, "trailing characters"),
            (
                "span[name=retrieve].input",
                4,
                "expected a name or kind selector",
            ),
            ("span.metadata", 5, "expected a span field"),
            ("span[name=\"x\"]", 14, "expected a span field"),
            ("span[name=\"x\".input", 13, "expected closing bracket"),
            ("trace.input$.", 11, "invalid JSONPath"),
        ];
        for (text, offset, reason) in cases {
            let err = Expr::parse(text).expect_err(text);
            assert_eq!(
                err,
                MappingError::Syntax {
                    expr_offset: offset,
                    reason,
                },
                "{text}"
            );
        }

        let long = format!(
            "trace.metadata.{}",
            "k".repeat(1024 - "trace.metadata.".len())
        );
        assert_eq!(long.len(), 1024);
        assert!(Expr::parse(&long).is_ok());
        let longer = format!("{long}k");
        assert_eq!(
            Expr::parse(&longer).expect_err("too long"),
            MappingError::Syntax {
                expr_offset: 1024,
                reason: "expression is longer than 1 KiB",
            }
        );

        let planted = "SECRET-PAYLOAD";
        let err = Expr::parse(planted).expect_err("planted");
        assert_no_payload(&err.to_string(), planted);
        let with_path = format!("trace.input${planted}");
        let err = Expr::parse(&with_path).expect_err("planted path");
        assert_eq!(
            err,
            MappingError::Syntax {
                expr_offset: 11,
                reason: "invalid JSONPath",
            }
        );
        assert_no_payload(&err.to_string(), planted);
    }

    #[test]
    fn mapping_limits_and_names() {
        let mut entries = serde_json::Map::new();
        for index in 0..32 {
            entries.insert(format!("v{index}"), json!("trace.input"));
        }
        let mapping = VariableMapping::from_json(&Value::Object(entries.clone())).unwrap();
        assert_eq!(mapping.0.len(), 32);

        entries.insert("v32".to_owned(), json!("trace.input"));
        assert_eq!(
            VariableMapping::from_json(&Value::Object(entries)).unwrap_err(),
            MappingError::TooMany
        );

        let long_name = "a".repeat(64);
        let ok = json!({ &long_name: "expected", "_also": "item.input" });
        assert!(VariableMapping::from_json(&ok).is_ok());
        let too_long = "a".repeat(65);
        assert_eq!(
            VariableMapping::from_json(&json!({ too_long.clone(): "trace.input" })).unwrap_err(),
            MappingError::Variable {
                name: too_long,
                reason: "invalid variable name",
            }
        );
        for name in ["", "1output", "has-dash", "has.dot"] {
            let err = VariableMapping::from_json(&json!({ name: "trace.input" })).unwrap_err();
            assert!(
                matches!(
                    err,
                    MappingError::Variable {
                        reason: "invalid variable name",
                        ..
                    }
                ),
                "{name}: {err}"
            );
            assert!(err.to_string().contains(name), "{name}");
        }

        assert_eq!(
            VariableMapping::from_json(&json!([])).unwrap_err(),
            MappingError::NotAnObject
        );
        assert_eq!(
            VariableMapping::from_json(&json!({ "output": 1 })).unwrap_err(),
            MappingError::Variable {
                name: "output".to_owned(),
                reason: "expression must be a string",
            }
        );

        let planted = "SECRET-PAYLOAD";
        let err = VariableMapping::from_json(&json!({ "output": planted })).unwrap_err();
        assert!(matches!(
            err,
            MappingError::InVariable { ref name, .. } if name == "output"
        ));
        assert!(err.to_string().contains("output"));
        assert_no_payload(&err.to_string(), planted);
    }

    #[test]
    fn needs_item_and_target_span() {
        let both = VariableMapping::from_json(&json!({
            "output": "span.output",
            "gold": "expected",
            "note": "item.metadata.tag",
            "ctx": "span[name=\"retrieve\"].output",
        }))
        .unwrap();
        assert!(both.needs_item());
        assert!(both.needs_target_span());

        let trace_only = VariableMapping::from_json(&json!({
            "t": "trace.input?",
            "s": "span[kind=\"llm\"].output$.text",
        }))
        .unwrap();
        assert!(!trace_only.needs_item());
        assert!(!trace_only.needs_target_span());

        let empty = VariableMapping::from_json(&json!({})).unwrap();
        assert!(!empty.needs_item());
        assert!(!empty.needs_target_span());
    }
}
