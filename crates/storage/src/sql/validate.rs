//! Which statements, tables and functions the SQL console accepts.

#![allow(dead_code)]

use std::collections::HashSet;
use std::ops::ControlFlow;

use datafusion::sql::sqlparser::ast::{
    DescribeAlias, Expr, Ident, ObjectName, ObjectNamePart, Query, Select, Spanned, Statement,
    TableFactor, Visit, Visitor,
};
use datafusion::sql::sqlparser::tokenizer::Span;

use crate::query_error::{InvalidQuery, Position};
use crate::query_text::cap_message;

/// Reject anything that is not one read-only query over the registered views.
pub(crate) fn validate_statement(
    statement: &Statement,
    registered: &HashSet<String>,
    text: &str,
    statement_start: Option<Position>,
) -> Result<(), InvalidQuery> {
    let query = match statement {
        Statement::Query(query) => query,
        Statement::Explain {
            describe_alias,
            analyze,
            verbose,
            query_plan,
            estimate,
            statement,
            format,
            options,
        } if *describe_alias == DescribeAlias::Explain
            && !analyze
            && !verbose
            && !query_plan
            && !estimate
            && format.is_none()
            && options.is_none() =>
        {
            let Statement::Query(query) = statement.as_ref() else {
                return Err(invalid(
                    "only plain EXPLAIN of a query is allowed",
                    statement_start,
                ));
            };
            query
        }
        Statement::Explain { .. } => {
            return Err(invalid(
                "only plain EXPLAIN of a query is allowed",
                statement_start,
            ));
        }
        _ => {
            return Err(invalid("only SELECT queries are allowed", statement_start));
        }
    };
    let mut ctes = CteNames::default();
    let _ = query.visit(&mut CollectCtes { names: &mut ctes });
    let mut walk = Walk {
        registered,
        ctes,
        text,
    };
    match query.visit(&mut walk) {
        ControlFlow::Break(error) => Err(error),
        ControlFlow::Continue(()) => Ok(()),
    }
}

#[derive(Default)]
struct CteNames {
    /// Unquoted CTE names, already lowercased.
    folded: HashSet<String>,
    /// Quoted CTE names, compared exactly.
    exact: HashSet<String>,
}

impl CteNames {
    fn insert(&mut self, ident: &Ident) {
        if ident.quote_style.is_some() {
            self.exact.insert(ident.value.clone());
        } else {
            self.folded.insert(ident.value.to_ascii_lowercase());
        }
    }

    fn contains(&self, ident: &Ident) -> bool {
        if ident.quote_style.is_some() {
            self.exact.contains(&ident.value)
        } else {
            self.folded.contains(&ident.value.to_ascii_lowercase())
        }
    }
}

struct CollectCtes<'a> {
    names: &'a mut CteNames,
}

impl Visitor for CollectCtes<'_> {
    type Break = ();

    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<Self::Break> {
        if let Some(with) = &query.with {
            for cte in &with.cte_tables {
                self.names.insert(&cte.alias.name);
            }
        }
        ControlFlow::Continue(())
    }
}

struct Walk<'a> {
    registered: &'a HashSet<String>,
    ctes: CteNames,
    text: &'a str,
}

impl Visitor for Walk<'_> {
    type Break = InvalidQuery;

    fn pre_visit_query(&mut self, query: &Query) -> ControlFlow<Self::Break> {
        if query.with.as_ref().is_some_and(|with| with.recursive) {
            return ControlFlow::Break(invalid(
                "recursive queries are not allowed",
                at(query.span()),
            ));
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_select(&mut self, select: &Select) -> ControlFlow<Self::Break> {
        if let Some(into) = &select.into {
            return ControlFlow::Break(invalid("SELECT INTO is not allowed", at(into.name.span())));
        }
        ControlFlow::Continue(())
    }

    fn pre_visit_table_factor(&mut self, factor: &TableFactor) -> ControlFlow<Self::Break> {
        match factor {
            TableFactor::Derived { .. }
            | TableFactor::NestedJoin { .. }
            | TableFactor::UNNEST { .. } => ControlFlow::Continue(()),
            TableFactor::Table { name, args, .. } => {
                if args.is_some() {
                    return ControlFlow::Break(invalid(
                        "table functions are not allowed",
                        at(factor.span()),
                    ));
                }
                self.relation(name)
            }
            TableFactor::TableFunction { .. } => {
                ControlFlow::Break(invalid("`TABLE` is not allowed in FROM", at(factor.span())))
            }
            TableFactor::Function { .. } => ControlFlow::Break(invalid(
                "`FUNCTION` is not allowed in FROM",
                at(factor.span()),
            )),
            TableFactor::JsonTable { .. } => ControlFlow::Break(invalid(
                "`JSON_TABLE` is not allowed in FROM",
                at(factor.span()),
            )),
            TableFactor::OpenJsonTable { .. } => ControlFlow::Break(invalid(
                "`OPENJSON` is not allowed in FROM",
                at(factor.span()),
            )),
            TableFactor::Pivot { .. } => {
                ControlFlow::Break(invalid("`PIVOT` is not allowed in FROM", at(factor.span())))
            }
            TableFactor::Unpivot { .. } => ControlFlow::Break(invalid(
                "`UNPIVOT` is not allowed in FROM",
                at(factor.span()),
            )),
            TableFactor::MatchRecognize { .. } => ControlFlow::Break(invalid(
                "`MATCH_RECOGNIZE` is not allowed in FROM",
                at(factor.span()),
            )),
            TableFactor::XmlTable { .. } => ControlFlow::Break(invalid(
                "`XMLTABLE` is not allowed in FROM",
                at(factor.span()),
            )),
            TableFactor::SemanticView { .. } => ControlFlow::Break(invalid(
                "`SEMANTIC_VIEW` is not allowed in FROM",
                at(factor.span()),
            )),
        }
    }

    fn pre_visit_expr(&mut self, expr: &Expr) -> ControlFlow<Self::Break> {
        let Expr::Function(function) = expr else {
            return ControlFlow::Continue(());
        };
        let Some(name) = bare_name(&function.name) else {
            return ControlFlow::Continue(());
        };
        if matches!(name.as_str(), "score" | "score_label" | "has_score") {
            return ControlFlow::Break(invalid(
                "score functions are only available in filters; join the scores view instead",
                at(expr.span()),
            ));
        }
        ControlFlow::Continue(())
    }
}

impl Walk<'_> {
    fn relation(&self, name: &ObjectName) -> ControlFlow<InvalidQuery> {
        let [part] = name.0.as_slice() else {
            return ControlFlow::Break(invalid(
                "use the view name alone, without catalog or schema",
                at(name.span()),
            ));
        };
        let ObjectNamePart::Identifier(ident) = part else {
            return ControlFlow::Break(invalid("table functions are not allowed", at(name.span())));
        };
        let known = if ident.quote_style.is_some() {
            self.registered.contains(&ident.value) || self.ctes.contains(ident)
        } else {
            let folded = ident.value.to_ascii_lowercase();
            self.registered.contains(&folded) || self.ctes.contains(ident)
        };
        if known {
            ControlFlow::Continue(())
        } else {
            ControlFlow::Break(invalid(
                format!("unknown table `{}`", ident.value),
                self.located(ident),
            ))
        }
    }

    /// Identifier span, or the quoted token in the source when sqlparser left it empty.
    fn located(&self, ident: &Ident) -> Option<Position> {
        if ident.span.start.line != 0 {
            return at(ident.span);
        }
        let needle = match ident.quote_style {
            Some(quote) => format!("{quote}{}{quote}", ident.value),
            None => ident.value.clone(),
        };
        self.text
            .find(&needle)
            .map(|index| position_at(self.text, index))
    }
}

fn bare_name(name: &ObjectName) -> Option<String> {
    match name.0.as_slice() {
        [ObjectNamePart::Identifier(ident)] => Some(ident.value.to_ascii_lowercase()),
        _ => None,
    }
}

fn position_at(text: &str, byte: usize) -> Position {
    let mut line = 1u32;
    let mut column = 1u32;
    for character in text[..byte].chars() {
        if character == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    Position { line, column }
}

fn at(span: Span) -> Option<Position> {
    if span.start.line == 0 {
        None
    } else {
        Some(Position {
            line: u32::try_from(span.start.line).unwrap_or(u32::MAX),
            column: u32::try_from(span.start.column).unwrap_or(u32::MAX),
        })
    }
}

fn invalid(message: impl AsRef<str>, position: Option<Position>) -> InvalidQuery {
    InvalidQuery {
        message: cap_message(message.as_ref()),
        position,
    }
}

#[cfg(test)]
mod tests {
    use super::super::plan_sql;
    use crate::query_error::Position;
    use crate::test_support::example_query;
    use crate::views::QueryContext;

    fn context() -> QueryContext {
        QueryContext::new(example_query(), None).expect("example project")
    }

    fn at(line: u32, column: u32) -> Option<Position> {
        Some(Position { line, column })
    }

    #[tokio::test]
    async fn statements_outside_a_query_are_rejected() {
        let cases = [
            (
                "INSERT INTO spans VALUES (1)",
                "only SELECT queries are allowed",
                at(1, 1),
            ),
            (
                "CREATE TABLE t (a INT)",
                "only SELECT queries are allowed",
                at(1, 1),
            ),
            (
                "DROP TABLE spans",
                "only SELECT queries are allowed",
                at(1, 1),
            ),
            (
                "SET datafusion.execution.batch_size = 1",
                "only SELECT queries are allowed",
                at(1, 1),
            ),
            (
                "COPY (SELECT 1) TO '/tmp/x.csv'",
                "only SELECT queries are allowed",
                at(1, 1),
            ),
            ("SHOW TABLES", "only SELECT queries are allowed", at(1, 1)),
            (
                "EXPLAIN ANALYZE SELECT 1",
                "only plain EXPLAIN of a query is allowed",
                at(1, 1),
            ),
            (
                "EXPLAIN VERBOSE SELECT 1",
                "only plain EXPLAIN of a query is allowed",
                at(1, 1),
            ),
            (
                "SELECT 1; SELECT 2",
                "only one statement is allowed",
                at(1, 11),
            ),
            (
                "SELECT * FROM raw_spans",
                "unknown table `raw_spans`",
                at(1, 15),
            ),
            (
                "SELECT * FROM datafusion.public.spans",
                "use the view name alone, without catalog or schema",
                at(1, 15),
            ),
            (
                "SELECT * FROM information_schema.tables",
                "use the view name alone, without catalog or schema",
                at(1, 15),
            ),
            (
                "SELECT * FROM generate_series(1, 3)",
                "table functions are not allowed",
                at(1, 15),
            ),
            (
                "SELECT * FROM read_parquet('/etc/passwd')",
                "table functions are not allowed",
                at(1, 15),
            ),
            (
                "SELECT * FROM '/etc/hostname'",
                "unknown table `/etc/hostname`",
                at(1, 15),
            ),
            (
                "WITH RECURSIVE r AS (SELECT 1) SELECT * FROM r",
                "recursive queries are not allowed",
                at(1, 1),
            ),
            (
                "SELECT score('x') FROM traces",
                "score functions are only available in filters; join the scores view instead",
                at(1, 8),
            ),
            ("SELECT 1 INTO t", "SELECT INTO is not allowed", at(1, 15)),
        ];
        let ctx = context();
        let mut mismatches = String::new();
        for (text, message, position) in cases {
            let error = plan_sql(&ctx, text).await.expect_err(text);
            if error.message != message || error.position != position {
                mismatches.push_str(&format!(
                    "{text}\n  got ({:?}, {:?})\n  want ({message}, {position:?})\n",
                    error.message, error.position
                ));
            }
        }
        assert!(mismatches.is_empty(), "{mismatches}");
    }
}
