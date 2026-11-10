//! Walk a filter AST and reject everything the DSL does not allow.
//!
//! The parent module allows `dead_code` for the same reason: the callers arrive
//! with the score functions and the read queries.

#![allow(dead_code)]
//!
//! Casts that pass are rewritten to `TRY_CAST` (D100). The walk is safe because
//! Q6 already bounded the tree.

use datafusion::arrow::datatypes::DataType as ArrowType;
use datafusion::common::DFSchema;
use datafusion::sql::sqlparser::ast::{
    AccessExpr, BinaryOperator, CastKind, DataType, ExactNumberInfo, Expr, Function, FunctionArg,
    FunctionArgExpr, FunctionArguments, Ident, Interval, ObjectNamePart, Spanned, Subscript,
    TimezoneInfo, UnaryOperator, Value,
};

use super::FilterView;
use super::scores::MAX_SCORE_NAMES;
use crate::query_error::{InvalidQuery, Position};
use crate::query_text::cap_message;

/// What [`validate`] rewrote while accepting an expression.
pub(crate) struct Validated {
    /// True when a resolved column is `environment`.
    pub(crate) references_environment: bool,
    /// Distinct score names, in order of first appearance.
    pub(crate) scores: Vec<String>,
}

/// Accept `expr` for `schema`, rewriting casts and score calls in place.
pub(crate) fn validate(
    view: FilterView,
    schema: &DFSchema,
    expr: &mut Expr,
) -> Result<Validated, InvalidQuery> {
    let mut walk = Walk {
        view,
        schema,
        references_environment: false,
        scores: Vec::new(),
    };
    walk.expr(expr)?;
    Ok(Validated {
        references_environment: walk.references_environment,
        scores: walk.scores,
    })
}

struct Walk<'a> {
    view: FilterView,
    schema: &'a DFSchema,
    references_environment: bool,
    scores: Vec<String>,
}

impl Walk<'_> {
    fn expr(&mut self, expr: &mut Expr) -> Result<(), InvalidQuery> {
        if matches!(expr, Expr::Cast { .. }) {
            return self.cast(expr);
        }
        if matches!(expr, Expr::Function(_)) {
            return self.function(expr);
        }
        let position = at(expr);
        match expr {
            Expr::Identifier(ident) => self.column(ident),
            Expr::CompoundIdentifier(_) => Err(forbid(position, "a qualified name")),
            Expr::CompoundFieldAccess { root, access_chain } => {
                self.map_access(position, root.as_ref(), access_chain.as_slice())
            }
            Expr::BinaryOp { left, op, right } => {
                if !binary_allowed(op) {
                    return Err(forbid(position, &format!("operator {op}")));
                }
                self.expr(left)?;
                self.expr(right)
            }
            Expr::UnaryOp { op, expr } => {
                if !unary_allowed(op) {
                    return Err(forbid(position, &format!("operator {op}")));
                }
                self.expr(expr)
            }
            Expr::IsNull(inner) | Expr::IsNotNull(inner) => self.expr(inner),
            Expr::IsDistinctFrom(left, right) | Expr::IsNotDistinctFrom(left, right) => {
                self.expr(left)?;
                self.expr(right)
            }
            Expr::InList { expr, list, .. } => {
                self.expr(expr)?;
                for item in list {
                    self.expr(item)?;
                }
                Ok(())
            }
            Expr::Between {
                expr, low, high, ..
            } => {
                self.expr(expr)?;
                self.expr(low)?;
                self.expr(high)
            }
            Expr::Like {
                any, expr, pattern, ..
            } => {
                if *any {
                    return Err(forbid(position, "LIKE ANY"));
                }
                self.expr(expr)?;
                self.expr(pattern)
            }
            Expr::ILike {
                any, expr, pattern, ..
            } => {
                if *any {
                    return Err(forbid(position, "ILIKE ANY"));
                }
                self.expr(expr)?;
                self.expr(pattern)
            }
            Expr::Nested(inner) => self.expr(inner),
            Expr::Value(value) => literal(position, &value.value),
            Expr::Interval(interval) => interval_value(position, interval),
            Expr::Array(array) => {
                if array.named {
                    return Err(forbid(position, "a named array"));
                }
                for element in &mut array.elem {
                    self.expr(element)?;
                }
                Ok(())
            }
            Expr::InSubquery { .. } | Expr::Exists { .. } | Expr::Subquery(_) => {
                Err(forbid(position, "a subquery"))
            }
            Expr::Case { .. } => Err(forbid(position, "CASE")),
            _ => Err(forbid(position, "this expression")),
        }
    }

    fn column(&mut self, ident: &Ident) -> Result<(), InvalidQuery> {
        let name = column_name(ident);
        if self.schema.field_with_unqualified_name(&name).is_err() {
            return Err(invalid(format!("unknown column `{name}`"), at_ident(ident)));
        }
        if name == "environment" {
            self.references_environment = true;
        }
        Ok(())
    }

    fn map_access(
        &mut self,
        position: Option<Position>,
        root: &Expr,
        access_chain: &[AccessExpr],
    ) -> Result<(), InvalidQuery> {
        let Expr::Identifier(ident) = root else {
            return Err(forbid(position, "map access"));
        };
        let name = column_name(ident);
        let is_map = match self.schema.field_with_unqualified_name(&name) {
            Ok(field) => matches!(field.data_type(), ArrowType::Map(_, _)),
            Err(_) => {
                return Err(invalid(format!("unknown column `{name}`"), at_ident(ident)));
            }
        };
        if !is_map {
            return Err(invalid(format!("`{name}` is not a map column"), position));
        }
        if name == "environment" {
            self.references_environment = true;
        }
        let [AccessExpr::Subscript(Subscript::Index { index })] = access_chain else {
            return Err(invalid("map keys must be string literals", position));
        };
        if !is_single_quoted(index) {
            return Err(invalid("map keys must be string literals", at(index)));
        }
        Ok(())
    }

    fn cast(&mut self, expr: &mut Expr) -> Result<(), InvalidQuery> {
        let position = at(expr);
        let error = match &*expr {
            Expr::Cast {
                kind,
                data_type,
                array,
                format,
                ..
            } => {
                if *array {
                    Some(forbid(position, "an array cast"))
                } else if format.is_some() {
                    Some(forbid(position, "a cast format"))
                } else {
                    match kind {
                        CastKind::TryCast => Some(forbid(position, "TRY_CAST")),
                        CastKind::SafeCast => Some(forbid(position, "SAFE_CAST")),
                        CastKind::Cast | CastKind::DoubleColon if !cast_allowed(data_type) => Some(
                            invalid(format!("casts to `{data_type}` are not allowed"), position),
                        ),
                        CastKind::Cast | CastKind::DoubleColon => None,
                    }
                }
            }
            _ => None,
        };
        if let Some(error) = error {
            return Err(error);
        }
        if let Expr::Cast {
            kind, expr: inner, ..
        } = expr
        {
            *kind = CastKind::TryCast;
            self.expr(inner)?;
        }
        Ok(())
    }

    fn function(&mut self, expr: &mut Expr) -> Result<(), InvalidQuery> {
        let position = at(expr);
        if self.rewrite_score(position, expr)? {
            return Ok(());
        }
        let Expr::Function(function) = expr else {
            return Ok(());
        };
        function_shape(position, function)?;
        let FunctionArguments::List(list) = &mut function.args else {
            return Ok(());
        };
        for arg in &mut list.args {
            let FunctionArg::Unnamed(FunctionArgExpr::Expr(inner)) = arg else {
                continue;
            };
            self.expr(inner)?;
        }
        Ok(())
    }

    /// Replace a score call with its internal column. User identifiers are
    /// checked against the view before this runs, so `__score_0_num` typed by
    /// a caller is still an unknown column.
    fn rewrite_score(
        &mut self,
        position: Option<Position>,
        expr: &mut Expr,
    ) -> Result<bool, InvalidQuery> {
        let rewritten = {
            let Expr::Function(function) = &*expr else {
                return Ok(false);
            };
            let Some(name) = bare_name(function) else {
                return Ok(false);
            };
            if !matches!(name.as_str(), "score" | "score_label" | "has_score") {
                return Ok(false);
            }
            ensure_plain_call(position, function)?;
            let Some(score_name) = one_string_literal(function) else {
                return Err(invalid(
                    format!("{name}() takes one string literal, the score name"),
                    position,
                ));
            };
            if self.view == FilterView::Scores {
                return Err(invalid(
                    "score functions are not available on the scores view",
                    position,
                ));
            }
            let index = self.score_index(position, score_name)?;
            let suffix = match name.as_str() {
                "score" => "num",
                "score_label" => "str",
                _ => "present",
            };
            let column = Expr::Identifier(Ident::new(format!("__score_{index}_{suffix}")));
            if name == "has_score" {
                Expr::IsTrue(Box::new(column))
            } else {
                column
            }
        };
        *expr = rewritten;
        Ok(true)
    }

    fn score_index(
        &mut self,
        position: Option<Position>,
        name: String,
    ) -> Result<usize, InvalidQuery> {
        if let Some(index) = self.scores.iter().position(|existing| existing == &name) {
            return Ok(index);
        }
        if self.scores.len() == MAX_SCORE_NAMES {
            return Err(invalid("a filter can use at most 8 score names", position));
        }
        self.scores.push(name);
        Ok(self.scores.len() - 1)
    }
}

fn ensure_plain_call(position: Option<Position>, function: &Function) -> Result<(), InvalidQuery> {
    if function.uses_odbc_syntax {
        return Err(forbid(position, "an ODBC function"));
    }
    if function.over.is_some() {
        return Err(forbid(position, "a window function"));
    }
    if function.filter.is_some() {
        return Err(forbid(position, "FILTER"));
    }
    if !function.within_group.is_empty() {
        return Err(forbid(position, "WITHIN GROUP"));
    }
    if function.null_treatment.is_some() {
        return Err(forbid(position, "null treatment"));
    }
    if !arguments_empty(&function.parameters) {
        return Err(forbid(position, "function parameters"));
    }
    let list = match &function.args {
        FunctionArguments::Subquery(_) => return Err(forbid(position, "a subquery")),
        FunctionArguments::None => {
            return Err(forbid(position, "a function call without parentheses"));
        }
        FunctionArguments::List(list) => list,
    };
    if list.duplicate_treatment.is_some() {
        return Err(forbid(position, "DISTINCT"));
    }
    if !list.clauses.is_empty() {
        return Err(forbid(position, "a function clause"));
    }
    Ok(())
}

fn bare_name(function: &Function) -> Option<String> {
    match function.name.0.as_slice() {
        [ObjectNamePart::Identifier(ident)] => Some(ident.value.to_ascii_lowercase()),
        _ => None,
    }
}

fn one_string_literal(function: &Function) -> Option<String> {
    let FunctionArguments::List(list) = &function.args else {
        return None;
    };
    let [FunctionArg::Unnamed(FunctionArgExpr::Expr(expr))] = list.args.as_slice() else {
        return None;
    };
    match expr {
        Expr::Value(value) => match &value.value {
            Value::SingleQuotedString(text) => Some(text.clone()),
            _ => None,
        },
        _ => None,
    }
}

fn function_shape(position: Option<Position>, function: &Function) -> Result<(), InvalidQuery> {
    ensure_plain_call(position, function)?;
    let FunctionArguments::List(list) = &function.args else {
        return Err(forbid(position, "a function call without parentheses"));
    };
    let name = match function.name.0.as_slice() {
        [ObjectNamePart::Identifier(ident)] => ident.value.to_ascii_lowercase(),
        _ => return Err(forbid(position, "a qualified name")),
    };
    if !allowed_function(&name) {
        return Err(invalid(format!("unknown function `{name}`"), position));
    }
    if name == "now" && !list.args.is_empty() {
        return Err(invalid("now() takes no arguments", position));
    }
    for arg in &list.args {
        match arg {
            FunctionArg::Unnamed(FunctionArgExpr::Expr(_)) => {}
            FunctionArg::Unnamed(_) => return Err(forbid(position, "a wildcard")),
            FunctionArg::Named { .. } | FunctionArg::ExprNamed { .. } => {
                return Err(forbid(position, "a named argument"));
            }
        }
    }
    Ok(())
}

fn arguments_empty(args: &FunctionArguments) -> bool {
    match args {
        FunctionArguments::None => true,
        FunctionArguments::List(list) => {
            list.args.is_empty() && list.duplicate_treatment.is_none() && list.clauses.is_empty()
        }
        FunctionArguments::Subquery(_) => false,
    }
}

fn allowed_function(name: &str) -> bool {
    matches!(
        name,
        "now"
            | "lower"
            | "upper"
            | "length"
            | "coalesce"
            | "array_has"
            | "array_length"
            | "date_trunc"
            | "abs"
    )
}

fn binary_allowed(op: &BinaryOperator) -> bool {
    matches!(
        op,
        BinaryOperator::And
            | BinaryOperator::Or
            | BinaryOperator::Eq
            | BinaryOperator::NotEq
            | BinaryOperator::Lt
            | BinaryOperator::LtEq
            | BinaryOperator::Gt
            | BinaryOperator::GtEq
            | BinaryOperator::Plus
            | BinaryOperator::Minus
            | BinaryOperator::Multiply
            | BinaryOperator::Divide
            | BinaryOperator::Modulo
            | BinaryOperator::AtArrow
    )
}

fn unary_allowed(op: &UnaryOperator) -> bool {
    matches!(
        op,
        UnaryOperator::Not | UnaryOperator::Minus | UnaryOperator::Plus
    )
}

/// `BIGINT`, `DOUBLE` / `DOUBLE PRECISION`, `VARCHAR` / `TEXT` / `STRING`,
/// `BOOLEAN` / `BOOL`, and `TIMESTAMP` with neither precision nor time zone.
fn cast_allowed(data_type: &DataType) -> bool {
    matches!(
        data_type,
        DataType::BigInt(None)
            | DataType::Double(ExactNumberInfo::None)
            | DataType::DoublePrecision
            | DataType::Varchar(None)
            | DataType::Text
            | DataType::String(None)
            | DataType::Boolean
            | DataType::Bool
            | DataType::Timestamp(None, TimezoneInfo::None | TimezoneInfo::WithoutTimeZone)
    )
}

fn literal(position: Option<Position>, value: &Value) -> Result<(), InvalidQuery> {
    match value {
        Value::Number(_, _) | Value::SingleQuotedString(_) | Value::Boolean(_) | Value::Null => {
            Ok(())
        }
        Value::HexStringLiteral(_) => Err(forbid(position, "a hex literal")),
        Value::DoubleQuotedString(_) => Err(forbid(position, "a double-quoted string")),
        Value::Placeholder(_) => Err(forbid(position, "a placeholder")),
        Value::DollarQuotedString(_) => Err(forbid(position, "a dollar-quoted string")),
        _ => Err(forbid(position, "this literal")),
    }
}

fn interval_value(position: Option<Position>, interval: &Interval) -> Result<(), InvalidQuery> {
    if is_single_quoted(&interval.value) {
        Ok(())
    } else {
        Err(invalid("interval values must be string literals", position))
    }
}

fn is_single_quoted(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::Value(value) if matches!(value.value, Value::SingleQuotedString(_))
    )
}

/// Unquoted names compare like DataFusion: lowercased. Quoted names stay as written.
fn column_name(ident: &Ident) -> String {
    if ident.quote_style.is_some() {
        ident.value.clone()
    } else {
        ident.value.to_ascii_lowercase()
    }
}

fn forbid(position: Option<Position>, what: &str) -> InvalidQuery {
    invalid(format!("{what} is not allowed in a filter"), position)
}

fn invalid(message: impl AsRef<str>, position: Option<Position>) -> InvalidQuery {
    InvalidQuery {
        message: cap_message(message.as_ref()),
        position,
    }
}

fn at(spanned: &impl Spanned) -> Option<Position> {
    let start = spanned.span().start;
    at_location(start.line, start.column)
}

/// `Ident` carries a span but does not implement [`Spanned`] in this sqlparser.
fn at_ident(ident: &Ident) -> Option<Position> {
    at_location(ident.span.start.line, ident.span.start.column)
}

fn at_location(line: u64, column: u64) -> Option<Position> {
    if line == 0 {
        return None;
    }
    Some(Position {
        line: u32::try_from(line).unwrap_or(u32::MAX),
        column: u32::try_from(column).unwrap_or(u32::MAX),
    })
}
