//! Limits, tokenization and positioned errors for caller-written filter and SQL text.
//!
//! The filter compiler and the SQL console call into this module. Until they
//! land, the non-test crate does not reference these items.

#![allow(dead_code)]

use datafusion::error::DataFusionError;
use datafusion::sql::sqlparser::dialect::GenericDialect;
use datafusion::sql::sqlparser::keywords::Keyword;
use datafusion::sql::sqlparser::parser::{Parser, ParserError};
use datafusion::sql::sqlparser::tokenizer::{Token, TokenWithSpan, Tokenizer, TokenizerError};

use crate::query_error::{InvalidQuery, Position};
use crate::views::PARSER_RECURSION_LIMIT;

static DIALECT: GenericDialect = GenericDialect {};

const MESSAGE_CAP: usize = 2048;

/// Byte and operator-token caps for one kind of caller text.
pub(crate) struct TextLimits {
    pub(crate) max_bytes: usize,
    pub(crate) max_operators: usize,
}

pub(crate) const FILTER_LIMITS: TextLimits = TextLimits {
    max_bytes: 4 * 1024,
    max_operators: 128,
};

pub(crate) const SQL_LIMITS: TextLimits = TextLimits {
    max_bytes: 64 * 1024,
    max_operators: 512,
};

/// Which caller text a message is talking about.
#[derive(Clone, Copy)]
pub(crate) enum TextKind {
    /// A filter expression.
    Filter,
    /// A SQL console statement.
    Sql,
}

impl TextKind {
    fn noun(self) -> &'static str {
        match self {
            Self::Filter => "filter",
            Self::Sql => "query",
        }
    }
}

/// Cuts `message` to at most 2,048 bytes on a character boundary.
pub(crate) fn cap_message(message: &str) -> String {
    if message.len() <= MESSAGE_CAP {
        return message.to_owned();
    }
    let mut end = MESSAGE_CAP;
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    message[..end].to_owned()
}

/// Tokenize caller text, rejecting oversized text and operator chains.
pub(crate) fn tokenize(
    text: &str,
    limits: &TextLimits,
    kind: TextKind,
) -> Result<Vec<TokenWithSpan>, InvalidQuery> {
    if text.len() > limits.max_bytes {
        return Err(invalid(
            format!(
                "the {} is longer than {} bytes",
                kind.noun(),
                limits.max_bytes
            ),
            None,
        ));
    }
    let mut tokenizer = Tokenizer::new(&DIALECT, text);
    let tokens = tokenizer
        .tokenize_with_location()
        .map_err(tokenizer_failure)?;
    let mut operators = 0;
    for token in &tokens {
        if !is_operator(&token.token) {
            continue;
        }
        operators += 1;
        if operators > limits.max_operators {
            return Err(invalid(
                format!(
                    "the {} has more than {} operators",
                    kind.noun(),
                    limits.max_operators
                ),
                Some(position_of(token.span.start.line, token.span.start.column)),
            ));
        }
    }
    Ok(tokens)
}

/// A parser over tokens already checked by [`tokenize`].
pub(crate) fn parser(tokens: Vec<TokenWithSpan>) -> Parser<'static> {
    Parser::new(&DIALECT)
        .with_tokens_with_locations(tokens)
        .with_recursion_limit(PARSER_RECURSION_LIMIT)
}

/// Turn a parse error into a positioned [`InvalidQuery`].
pub(crate) fn parse_failure(error: ParserError, parser: &Parser<'_>, text: &str) -> InvalidQuery {
    let (message, located) = match error {
        ParserError::RecursionLimitExceeded => {
            ("the expression is nested too deeply".to_owned(), None)
        }
        ParserError::ParserError(message) | ParserError::TokenizerError(message) => {
            split_appended_location(&message)
        }
    };
    let position = located.or_else(|| {
        let token = parser.peek_token_ref();
        if matches!(token.token, Token::EOF) || token.span == sqlparser_empty_span() {
            Some(end_of(text))
        } else if token.span.start.line == 0 {
            None
        } else {
            Some(position_of(token.span.start.line, token.span.start.column))
        }
    });
    invalid(message, position)
}

/// Turn a planning error into a positioned [`InvalidQuery`].
pub(crate) fn planning_failure(error: &DataFusionError) -> InvalidQuery {
    if let Some(diagnostic) = error.diagnostic() {
        let position = diagnostic.span.and_then(|span| {
            if span.start.line == 0 {
                None
            } else {
                Some(position_of(span.start.line, span.start.column))
            }
        });
        return invalid(diagnostic.message.clone(), position);
    }
    invalid(error.strip_backtrace(), None)
}

/// Fail when the parser has not consumed the expression.
pub(crate) fn expect_end(parser: &mut Parser<'_>, _text: &str) -> Result<(), InvalidQuery> {
    let token = parser.peek_token_ref();
    if matches!(token.token, Token::EOF) {
        Ok(())
    } else {
        Err(invalid(
            "unexpected text after the expression",
            Some(position_of(token.span.start.line, token.span.start.column)),
        ))
    }
}

fn invalid(message: impl AsRef<str>, position: Option<Position>) -> InvalidQuery {
    InvalidQuery {
        message: cap_message(message.as_ref()),
        position,
    }
}

fn tokenizer_failure(error: TokenizerError) -> InvalidQuery {
    invalid(
        error.message,
        Some(position_of(error.location.line, error.location.column)),
    )
}

fn position_of(line: u64, column: u64) -> Position {
    Position {
        line: u32::try_from(line).unwrap_or(u32::MAX),
        column: u32::try_from(column).unwrap_or(u32::MAX),
    }
}

fn sqlparser_empty_span() -> datafusion::sql::sqlparser::tokenizer::Span {
    datafusion::sql::sqlparser::tokenizer::Span::empty()
}

fn end_of(text: &str) -> Position {
    if text.is_empty() {
        return Position { line: 1, column: 1 };
    }
    let mut line = 1u32;
    let mut column = 1u32;
    let mut last_line = 1u32;
    let mut last_column = 1u32;
    for ch in text.chars() {
        last_line = line;
        last_column = column;
        if ch == '\n' {
            line = line.saturating_add(1);
            column = 1;
        } else {
            column = column.saturating_add(1);
        }
    }
    Position {
        line: last_line,
        column: last_column.saturating_add(1),
    }
}

fn split_appended_location(message: &str) -> (String, Option<Position>) {
    let marker = " at Line: ";
    let Some(index) = message.rfind(marker) else {
        return (message.to_owned(), None);
    };
    let tail = &message[index + marker.len()..];
    let Some((line, column)) = tail.split_once(", Column: ") else {
        return (message.to_owned(), None);
    };
    if line.is_empty()
        || column.is_empty()
        || !line.bytes().all(|byte| byte.is_ascii_digit())
        || !column.bytes().all(|byte| byte.is_ascii_digit())
    {
        return (message.to_owned(), None);
    }
    let line = line.parse::<u64>().unwrap_or(u64::from(u32::MAX));
    let column = column.parse::<u64>().unwrap_or(u64::from(u32::MAX));
    (
        message[..index].trim_end().to_owned(),
        Some(position_of(line, column)),
    )
}

fn is_operator(token: &Token) -> bool {
    match token {
        Token::Eq
        | Token::DoubleEq
        | Token::Neq
        | Token::Lt
        | Token::Gt
        | Token::LtEq
        | Token::GtEq
        | Token::Spaceship
        | Token::Plus
        | Token::Minus
        | Token::Mul
        | Token::Div
        | Token::Mod
        | Token::DuckIntDiv
        | Token::StringConcat
        | Token::AtArrow
        | Token::ArrowAt
        | Token::Arrow
        | Token::LongArrow
        | Token::HashArrow
        | Token::HashLongArrow
        | Token::AtAt
        | Token::Question
        | Token::QuestionAnd
        | Token::QuestionPipe
        | Token::Ampersand
        | Token::Pipe
        | Token::Caret
        | Token::ShiftLeft
        | Token::ShiftRight
        | Token::Tilde
        | Token::TildeAsterisk
        | Token::ExclamationMarkTilde
        | Token::ExclamationMarkTildeAsterisk
        | Token::DoubleColon
        | Token::LBracket
        | Token::Colon
        | Token::Sharp
        | Token::DoubleTilde
        | Token::DoubleTildeAsterisk
        | Token::ExclamationMarkDoubleTilde
        | Token::ExclamationMarkDoubleTildeAsterisk
        | Token::HashMinus
        | Token::AtQuestion
        | Token::CaretAt => true,
        Token::Word(word) => matches!(
            word.keyword,
            Keyword::AND
                | Keyword::OR
                | Keyword::NOT
                | Keyword::IS
                | Keyword::IN
                | Keyword::BETWEEN
                | Keyword::LIKE
                | Keyword::ILIKE
                | Keyword::SIMILAR
                | Keyword::XOR
                | Keyword::DIV
                | Keyword::UNION
                | Keyword::INTERSECT
                | Keyword::EXCEPT
                | Keyword::MINUS
                | Keyword::AT
                | Keyword::COLLATE
                | Keyword::RLIKE
                | Keyword::REGEXP
                | Keyword::MATCH
                | Keyword::MEMBER
                | Keyword::OVERLAPS
                | Keyword::OPERATOR
        ),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use datafusion::common::DFSchema;

    use super::*;
    use crate::test_support::example_query;
    use crate::views::QueryContext;

    fn filter_tokens(text: &str) -> Result<Vec<TokenWithSpan>, InvalidQuery> {
        tokenize(text, &FILTER_LIMITS, TextKind::Filter)
    }

    #[test]
    fn byte_limit_has_no_position() {
        let over = "a".repeat(4097);
        let error = filter_tokens(&over).expect_err("too long");
        assert_eq!(error.message, "the filter is longer than 4096 bytes");
        assert_eq!(error.position, None);
        assert!(filter_tokens(&"a".repeat(4096)).is_ok());
    }

    #[test]
    fn operator_limit_points_at_the_extra_or() {
        let ok = format!("{}a", "a OR ".repeat(128));
        assert!(filter_tokens(&ok).is_ok(), "{ok}");
        let over = format!("{}a", "a OR ".repeat(129));
        let error = filter_tokens(&over).expect_err("too many operators");
        assert_eq!(error.message, "the filter has more than 128 operators");
        // "a OR " is 5 characters, and each OR starts at column 3 of its group.
        assert_eq!(
            error.position,
            Some(Position {
                line: 1,
                column: 643
            })
        );
    }

    #[test]
    fn unterminated_string_counts_characters() {
        let text = "status = 'é' AND name = 'oops";
        let error = filter_tokens(text).expect_err("unterminated");
        assert_eq!(error.message, "Unterminated string literal");
        assert_eq!(
            error.position,
            Some(Position {
                line: 1,
                column: 25
            })
        );
    }

    #[test]
    fn parse_failures_keep_a_position() {
        let trailing = "status = 'error' AND";
        let error = parse(trailing);
        assert_eq!(error.message, "Expected: an expression, found: EOF");
        assert_eq!(error.position, Some(end_of(trailing)));

        let doubled = "a = = 1";
        let error = parse(doubled);
        assert_eq!(error.message, "Expected: an expression, found: =");
        assert_eq!(error.position, Some(Position { line: 1, column: 5 }));

        let lines = "status = 'ok'\n=";
        let error = parse(lines);
        assert_eq!(error.message, "Expected: an expression, found: EOF");
        assert_eq!(error.position, Some(Position { line: 2, column: 2 }));
    }

    #[test]
    fn trailing_text_is_rejected() {
        let text = "1 2";
        let tokens = tokenize(text, &SQL_LIMITS, TextKind::Sql).expect("tokens");
        let mut parsed = parser(tokens);
        parsed.parse_expr().expect("one");
        let error = expect_end(&mut parsed, text).expect_err("trailing");
        assert_eq!(error.message, "unexpected text after the expression");
        assert_eq!(error.position, Some(Position { line: 1, column: 3 }));
    }

    #[test]
    fn deep_nesting_is_rejected() {
        let text = format!("{}true{}", "(".repeat(60), ")".repeat(60));
        let error = parse(&text);
        assert_eq!(error.message, "the expression is nested too deeply");
    }

    #[tokio::test]
    async fn planning_failure_uses_the_diagnostic() {
        let ctx = QueryContext::new(example_query(), None).expect("context");
        let frame = ctx.session().table("spans").await.expect("spans");
        let schema = DFSchema::clone(frame.schema());
        let error = ctx
            .session()
            .state()
            .create_logical_expr("nme = 'a'", &schema)
            .expect_err("unknown column");
        let invalid = planning_failure(&error);
        let diagnostic = error.diagnostic().expect("diagnostic");
        assert_eq!(invalid.message, diagnostic.message);
        assert_eq!(invalid.position, Some(Position { line: 1, column: 1 }));
    }

    #[test]
    fn cap_message_stops_on_a_character() {
        let text = "€".repeat(1000);
        assert_eq!(text.len(), 3000);
        let capped = cap_message(&text);
        assert!(capped.len() <= 2048, "{}", capped.len());
        assert!(text.starts_with(&capped));
        assert_eq!(capped.len() % "€".len(), 0);
        assert!(capped.len() > 2048 - "€".len());
    }

    #[test]
    fn chained_forms_count_as_operators() {
        let cases = [
            ("SELECT 1 UNION SELECT 1", 1),
            ("SELECT 1 UNION ALL SELECT 1", 1),
            ("SELECT 1 INTERSECT SELECT 1", 1),
            ("SELECT 1 EXCEPT SELECT 1", 1),
            ("SELECT 1 MINUS SELECT 1", 1),
            ("SELECT now() AT TIME ZONE 'UTC'", 1),
            ("SELECT 'a' COLLATE x", 1),
            ("SELECT metadata['k'] FROM spans", 1),
            ("SELECT 'a' ~~ 'a'", 1),
            ("SELECT 'a' RLIKE 'a'", 1),
            ("SELECT {n:Int}", 1),
            ("SELECT \"union\" FROM spans", 0),
        ];
        for (text, expected) in cases {
            let tokens = Tokenizer::new(&DIALECT, text)
                .tokenize_with_location()
                .expect("tokens");
            let counted = tokens
                .iter()
                .filter(|token| is_operator(&token.token))
                .count();
            assert_eq!(counted, expected, "{text}");
        }
    }

    fn parse(text: &str) -> InvalidQuery {
        let tokens = filter_tokens(text).expect("tokens");
        let mut parser = parser(tokens);
        let error = parser.parse_expr().expect_err(text);
        parse_failure(error, &parser, text)
    }
}
