//! `{name:Type}` parameters for the SQL console.

#![allow(dead_code)]

use std::collections::{HashMap, VecDeque};

use datafusion::common::{DataFusionError, ScalarValue};
use datafusion::logical_expr::LogicalPlan;
use datafusion::sql::sqlparser::tokenizer::{Token, TokenWithSpan, Word};
use llmobs_core::error::TimeError;
use llmobs_core::time_serde::parse_rfc3339;
use serde_json::{Map, Number, Value};

use crate::query_error::{InvalidQuery, Position, RunError};
use crate::query_text::cap_message;

/// One distinct `{name:Type}` in a query, in the order it first appears.
pub(crate) struct Placeholder {
    name: String,
    kind: ParamKind,
    position: Position,
}

/// The five parameter types the console accepts.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ParamKind {
    String,
    Int,
    Float,
    Bool,
    Timestamp,
}

impl ParamKind {
    fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "string" => Some(Self::String),
            "int" => Some(Self::Int),
            "float" => Some(Self::Float),
            "bool" => Some(Self::Bool),
            "timestamp" => Some(Self::Timestamp),
            _ => None,
        }
    }

    fn expectation(self) -> &'static str {
        match self {
            Self::String => "a string",
            Self::Int => "an integer",
            Self::Float => "a number",
            Self::Bool => "a boolean",
            Self::Timestamp => "an RFC 3339 string or an integer",
        }
    }
}

/// Replace each `{name:Type}` with `$k` and record the distinct parameters.
///
/// A `{` that is not the start of that exact five-token pattern is left for
/// the parser. `$1` and `?` are rejected here.
pub(crate) fn bind_placeholders(
    tokens: Vec<TokenWithSpan>,
) -> Result<(Vec<TokenWithSpan>, Vec<Placeholder>), InvalidQuery> {
    let mut incoming: VecDeque<TokenWithSpan> = tokens.into_iter().collect();
    let mut rewritten = Vec::with_capacity(incoming.len());
    let mut placeholders = Vec::new();
    let mut seen: HashMap<String, Seen> = HashMap::new();
    while let Some(open) = incoming.pop_front() {
        if !matches!(open.token, Token::LBrace) {
            reject_raw_placeholder(&open)?;
            rewritten.push(open);
            continue;
        }
        let Some(name_token) = incoming.pop_front() else {
            rewritten.push(open);
            break;
        };
        let Some(colon) = incoming.pop_front() else {
            rewritten.extend([open, name_token]);
            break;
        };
        let Some(type_token) = incoming.pop_front() else {
            rewritten.extend([open, name_token, colon]);
            break;
        };
        let Some(close) = incoming.pop_front() else {
            rewritten.extend([open, name_token, colon, type_token]);
            break;
        };
        let Some(found) = parameter(&open, &name_token, &colon, &type_token, &close) else {
            incoming.push_front(close);
            incoming.push_front(type_token);
            incoming.push_front(colon);
            incoming.push_front(name_token);
            rewritten.push(open);
            continue;
        };
        let kind = ParamKind::parse(&found.type_name).ok_or_else(|| {
            invalid(
                &format!(
                    "unknown parameter type `{}`; use String, Int, Float, Bool or Timestamp",
                    found.type_name
                ),
                found.type_position,
            )
        })?;
        let k = if let Some(previous) = seen.get(&found.name) {
            if previous.kind != kind {
                return Err(invalid(
                    &format!("parameter `{}` is declared with two types", found.name),
                    found.type_position,
                ));
            }
            previous.k
        } else {
            let k = placeholders.len() + 1;
            seen.insert(found.name.clone(), Seen { k, kind });
            placeholders.push(Placeholder {
                name: found.name,
                kind,
                position: found.position,
            });
            k
        };
        rewritten.push(TokenWithSpan::new(
            Token::Placeholder(format!("${k}")),
            found.span,
        ));
    }
    Ok((rewritten, placeholders))
}

/// JSON values for `placeholders`, in `$k` order.
pub(crate) fn param_values(
    placeholders: &[Placeholder],
    params: &Map<String, Value>,
) -> Result<Vec<ScalarValue>, RunError> {
    let mut values = Vec::with_capacity(placeholders.len());
    for placeholder in placeholders {
        let Some(value) = params.get(&placeholder.name) else {
            return Err(invalid_request(format!(
                "missing value for parameter {}",
                placeholder.name
            )));
        };
        values.push(convert(placeholder, value)?);
    }
    for name in params.keys() {
        if !placeholders
            .iter()
            .any(|placeholder| placeholder.name == *name)
        {
            return Err(invalid_request(format!("unknown parameter {name}")));
        }
    }
    Ok(values)
}

/// Cast each value to the type DataFusion inferred, then substitute `$k`.
pub(crate) fn bind_parameters(
    plan: LogicalPlan,
    placeholders: &[Placeholder],
    mut values: Vec<ScalarValue>,
) -> Result<LogicalPlan, RunError> {
    if placeholders.is_empty() {
        return Ok(plan);
    }
    let inferred = plan.get_parameter_types().map_err(RunError::Internal)?;
    for (index, placeholder) in placeholders.iter().enumerate() {
        let Some(target) = inferred.get(&format!("${}", index + 1)).cloned().flatten() else {
            continue;
        };
        let Some(value) = values.get_mut(index) else {
            return Err(RunError::Internal(DataFusionError::Internal(
                "parameter value is missing".into(),
            )));
        };
        if value.data_type() == target {
            continue;
        }
        *value = value.cast_to(&target).map_err(|_| {
            invalid_request(format!(
                "parameter {} does not fit where it is used ({target})",
                placeholder.name
            ))
        })?;
    }
    plan.with_param_values(values).map_err(RunError::Internal)
}

struct Seen {
    k: usize,
    kind: ParamKind,
}

struct Found {
    name: String,
    type_name: String,
    type_position: Position,
    position: Position,
    span: datafusion::sql::sqlparser::tokenizer::Span,
}

/// `{` + unquoted word + `:` + unquoted word + `}`, with nothing between them.
fn parameter(
    open: &TokenWithSpan,
    name: &TokenWithSpan,
    colon: &TokenWithSpan,
    ty: &TokenWithSpan,
    close: &TokenWithSpan,
) -> Option<Found> {
    if !matches!(colon.token, Token::Colon) || !matches!(close.token, Token::RBrace) {
        return None;
    }
    let name_word = unquoted_word(&name.token)?;
    let type_word = unquoted_word(&ty.token)?;
    Some(Found {
        name: name_word.value.clone(),
        type_name: type_word.value.clone(),
        type_position: position_of(ty),
        position: position_of(open),
        span: open.span.union(&close.span),
    })
}

fn reject_raw_placeholder(token: &TokenWithSpan) -> Result<(), InvalidQuery> {
    if matches!(token.token, Token::Placeholder(_)) {
        return Err(invalid(
            "write parameters as {name:Type}",
            position_of(token),
        ));
    }
    Ok(())
}

fn unquoted_word(token: &Token) -> Option<&Word> {
    match token {
        Token::Word(word) if word.quote_style.is_none() => Some(word),
        _ => None,
    }
}

fn convert(placeholder: &Placeholder, value: &Value) -> Result<ScalarValue, RunError> {
    match placeholder.kind {
        ParamKind::String => match value {
            Value::String(text) => Ok(ScalarValue::Utf8(Some(text.clone()))),
            _ => Err(must_be(placeholder)),
        },
        ParamKind::Int => match value {
            Value::Number(number) => Ok(ScalarValue::Int64(Some(integer(
                number,
                &placeholder.name,
            )?))),
            _ => Err(must_be(placeholder)),
        },
        ParamKind::Float => match value {
            Value::Number(number) => match number.as_f64() {
                Some(number) => Ok(ScalarValue::Float64(Some(number))),
                None => Err(invalid_request(format!(
                    "parameter {} is out of range",
                    placeholder.name
                ))),
            },
            _ => Err(must_be(placeholder)),
        },
        ParamKind::Bool => match value {
            Value::Bool(value) => Ok(ScalarValue::Boolean(Some(*value))),
            _ => Err(must_be(placeholder)),
        },
        ParamKind::Timestamp => timestamp(placeholder, value),
    }
}

fn integer(number: &Number, name: &str) -> Result<i64, RunError> {
    if let Some(value) = number.as_i64() {
        return Ok(value);
    }
    if number.as_u64().is_some() {
        return Err(invalid_request(format!("parameter {name} is out of range")));
    }
    Err(invalid_request(format!(
        "parameter {name} must be an integer"
    )))
}

fn timestamp(placeholder: &Placeholder, value: &Value) -> Result<ScalarValue, RunError> {
    let nanos = match value {
        Value::String(text) => match parse_rfc3339(text) {
            Ok(nanos) => nanos,
            Err(TimeError::OutOfRange) => {
                return Err(invalid_request(format!(
                    "parameter {} is out of range",
                    placeholder.name
                )));
            }
            Err(TimeError::Invalid) => return Err(must_be(placeholder)),
        },
        Value::Number(number) => {
            let Some(millis) = number.as_i64() else {
                if number.as_u64().is_some() {
                    return Err(invalid_request(format!(
                        "parameter {} is out of range",
                        placeholder.name
                    )));
                }
                return Err(must_be(placeholder));
            };
            millis.checked_mul(1_000_000).ok_or_else(|| {
                invalid_request(format!("parameter {} is out of range", placeholder.name))
            })?
        }
        _ => return Err(must_be(placeholder)),
    };
    Ok(ScalarValue::TimestampNanosecond(
        Some(nanos),
        Some("UTC".into()),
    ))
}

fn must_be(placeholder: &Placeholder) -> RunError {
    invalid_request(format!(
        "parameter {} must be {}",
        placeholder.name,
        placeholder.kind.expectation()
    ))
}

fn invalid_request(message: String) -> RunError {
    RunError::InvalidRequest(message)
}

fn invalid(message: &str, position: Position) -> InvalidQuery {
    InvalidQuery {
        message: cap_message(message),
        position: Some(position),
    }
}

fn position_of(token: &TokenWithSpan) -> Position {
    Position {
        line: u32::try_from(token.span.start.line).unwrap_or(u32::MAX),
        column: u32::try_from(token.span.start.column).unwrap_or(u32::MAX),
    }
}
