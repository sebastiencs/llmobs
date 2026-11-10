# A2 · 02 — Filter DSL (Q6–Q9)

Read [README.md](README.md) first: decisions D91–D101, boundaries, verified DataFusion facts, crate layout, public API.

---

## Q6 — Query text: limits, tokens, positions

**Goal:** one place that turns caller text (filter or SQL) into checked tokens and a parser, and turns any parse or planning failure into an `InvalidQuery` with a 1-based character position.

**Edit only:** new `crates/storage/src/query_text.rs`, `crates/storage/src/query_error.rs`, `crates/storage/src/lib.rs`.

**Commit:** `Add query text limits and error positions`

### Types

```rust
/// 1-based line and column, counted in characters (spec 09 § Conventions).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position { pub line: u32, pub column: u32 }

/// Filter or SQL text the caller sent is invalid. Maps to 400 `invalid_filter`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct InvalidQuery { pub message: String, pub position: Option<Position> }
```

`RunError` gains `#[error(transparent)] Invalid(#[from] InvalidQuery)`, whose `kind()` is `"invalid"`.

Messages are capped at 2,048 bytes, cut on a `char` boundary (one helper, `cap_message`). They may quote the caller's own text (identifiers, literals): that goes back to the caller, never to logs (D94).

### Limits

```rust
pub(crate) struct TextLimits { pub(crate) max_bytes: usize, pub(crate) max_operators: usize }
pub(crate) const FILTER_LIMITS: TextLimits = TextLimits { max_bytes: 4 * 1024, max_operators: 128 };
pub(crate) const SQL_LIMITS: TextLimits = TextLimits { max_bytes: 64 * 1024, max_operators: 512 };
```

**Operator tokens** are these `sqlparser::tokenizer::Token` variants:
- `Eq`, `DoubleEq`, `Neq`, `Lt`, `Gt`, `LtEq`, `GtEq`, `Spaceship`;
- `Plus`, `Minus`, `Mul`, `Div`, `Mod`, `DuckIntDiv`;
- `StringConcat`;
- `AtArrow`, `ArrowAt`, `Arrow`, `LongArrow`, `HashArrow`, `HashLongArrow`, `AtAt`, `Question`, `QuestionAnd`, `QuestionPipe`;
- `Ampersand`, `Pipe`, `Caret`, `ShiftLeft`, `ShiftRight`, `Tilde`, `TildeAsterisk`, `ExclamationMarkTilde`, `ExclamationMarkTildeAsterisk`;
- `DoubleColon`.

They also include a `Token::Word` whose keyword is one of `AND`, `OR`, `NOT`, `IS`, `IN`, `BETWEEN`, `LIKE`, `ILIKE`, `SIMILAR`, `XOR`, `DIV`.

If a variant name above does not exist in sqlparser 0.62, drop it and say so in the report. Do not invent a wider rule. Spec 05 (Q1) lists the same set; keep them equal.

**Why:** a left-deep chain of operators is not bounded by the parser's recursion limit, and planning about 3,000 of them overflows a 2 MiB stack (README § Verified facts). Capping operators bounds the depth of every expression tree, and the recursion limit bounds nesting. After these checks, recursive walks over the AST are safe.

### Functions

`pub(crate) fn tokenize(text: &str, limits: &TextLimits, what: TextKind) -> Result<Vec<TokenWithSpan>, InvalidQuery>`, with `TextKind::{Filter, Sql}` used only in messages ("the filter …" / "the query …"). Steps:
1. If `text.len() > limits.max_bytes`, return "the filter is longer than 4096 bytes" (or the SQL equivalent), without position.
2. Tokenize with `Tokenizer::new(&DIALECT, text).tokenize_with_location()`, where `static DIALECT: GenericDialect = GenericDialect {};`. A `TokenizerError` becomes its `message` with `position = location`. Convert `u64` to `u32` saturating.
3. Count operator tokens. At the first one beyond the limit, return "the filter has more than 128 operators", positioned at that token.

`pub(crate) fn parser(tokens: Vec<TokenWithSpan>) -> Parser<'static>` returns `Parser::new(&DIALECT).with_tokens_with_locations(tokens).with_recursion_limit(PARSER_RECURSION_LIMIT)`, with the constant from Q2.

`pub(crate) fn parse_failure(error: ParserError, parser: &Parser<'_>, text: &str) -> InvalidQuery`:
- `RecursionLimitExceeded`: "the expression is nested too deeply".
- `ParserError(m)` and `TokenizerError(m)`: `m`, with a trailing ` at Line: <n>, Column: <n>` removed if present (sqlparser appends it to some messages).
- **Position:** the start of `parser.peek_token_ref().span`. If that token is EOF or its span is empty, use the end of input: the line of the last character and one past its column, or `1:1` for empty text.

`pub(crate) fn planning_failure(error: &DataFusionError) -> InvalidQuery`:
- If `error.diagnostic()` is `Some(d)`: the message is `d.message`, and the position is `d.span`'s start when the span is present and non-empty.
- Otherwise: `error.strip_backtrace()`, without position.

Both cap the message.

`pub(crate) fn expect_end(parser: &mut Parser<'_>, text: &str) -> Result<(), InvalidQuery>`: if the next non-whitespace token is not EOF, return "unexpected text after the expression", positioned at that token.

### Tests (`query_text.rs`)

1. A 4,097-byte filter fails with no position. A 4,096-byte one tokenizes.
2. `a OR a OR … OR a` with 128 `OR`s passes. With 129 it fails, positioned at the 129th `OR`; assert its exact column.
3. `status = 'é' AND name = 'oops` fails with "Unterminated string literal" at `1:25`: the quote is the 25th character, and `é` counts once.
4. Through `parser` and `parse_failure`:
   - `status = 'error' AND` points at the end of input;
   - `a = = 1` points at the second `=`;
   - a filter on two lines with an error on line 2 reports line 2.
5. Sixty nested parentheses give "nested too deeply".
6. `planning_failure` on a DataFusion error with a diagnostic: plan `nme = 'a'` with `SessionState::create_logical_expr` against the `spans` view schema. It returns the diagnostic's message and `1:1`.
7. `cap_message` cuts a 3,000-byte string of multi-byte characters on a character boundary, at ≤ 2,048 bytes.

### Report

Operator token variants that did not exist. The exact messages sqlparser produced in test 4.

---

## Q7 — Filter validation and compilation

**Goal:** `QueryContext` turns filter text into a checked DataFusion `Expr` over one view, with positioned errors.

**Edit only:** new `crates/storage/src/filter/{mod.rs,validate.rs}`, `crates/storage/src/views/mod.rs` (view schemas kept on `QueryContext`), `crates/storage/src/lib.rs`.

**Commit:** `Compile filter DSL expressions`

### Types

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FilterView { Traces, Spans, Sessions, Scores }

pub(crate) struct Filter {
    pub(crate) view: FilterView,
    pub(crate) expr: Expr,                 // over the view's columns
    pub(crate) references_environment: bool,
}
```

Both stay crate-private. The read functions (Q14+) choose the view themselves, and other callers of the filter (online rules, alerts, the SSE filter) will widen the API when they need it.

`QueryContext::new` keeps each registered view's `DFSchemaRef` (from its `LogicalPlan`) in a private `HashMap<ViewName, DFSchemaRef>`. Then `parse_filter` is synchronous:

`pub(crate) fn parse_filter(&self, view: FilterView, text: &str) -> Result<Filter, InvalidQuery>`:
1. `tokenize(text, &FILTER_LIMITS, TextKind::Filter)`. Text that is empty or only whitespace fails with "the filter is empty". Callers that treat an empty filter as no filter check before calling (Q14).
2. `parser(tokens)`, `parse_expr()` (failure → `parse_failure`), then `expect_end`.
3. `validate(view, &mut ast)` (`validate.rs`, below). It may rewrite the AST in place, for casts.
4. Plan with `self.session().state().create_logical_expr_from_sql_expr(SQLExprWithAlias { expr: ast, alias: None }, &view_schema)`. Failure → `planning_failure`.
5. The result's type, `expr.get_type(&view_schema)`, must be `Boolean` or `Null`. Otherwise fail with "the filter must be a true/false expression", positioned at `1:1`.

### Validation (`validate.rs`)

A recursive walk over `sqlparser::ast::Expr`, safe because Q6 bounds the tree. It accepts exactly these forms; anything else fails with "`<what>` is not allowed in a filter".
- `<what>` is a short name: "subquery", "CASE", "a qualified name", "a window function", …
- The position is the node's span start: `sqlparser::ast::Spanned::span()`, when non-empty.

| Form | Rule |
|---|---|
| `Identifier(ident)` | A column of the view, compared like DataFusion: the unquoted name lowercased, the quoted name as is. Unknown: "unknown column `<name>`" at the identifier. A name equal to `environment` sets `references_environment`. |
| `CompoundIdentifier` | Rejected ("a qualified name"). |
| `CompoundFieldAccess { root: Identifier(m), access_chain: [Subscript(Index { index })] }` | `m` is a `Map` column of the view (`metadata`; `attributes`, `resource_attributes` on `spans`), and `index` is a single-quoted string literal. Otherwise: "`<m>` is not a map column" or "map keys must be string literals". |
| `BinaryOp` | `op` ∈ `And, Or, Eq, NotEq, Lt, LtEq, Gt, GtEq, Plus, Minus, Multiply, Divide, Modulo, AtArrow`. |
| `UnaryOp` | `op` ∈ `Not, Minus, Plus`. |
| `IsNull`, `IsNotNull`, `IsDistinctFrom`, `IsNotDistinctFrom` | |
| `InList { list, .. }` | Every element is validated. |
| `Between`, `Like`, `ILike` | `any == false`. An escape character is allowed. |
| `Nested` | |
| `Value` | Single-quoted string, number, boolean, `NULL`. |
| `Interval` | Its value is a single-quoted string literal. |
| `Array` | Unnamed, elements validated. |
| `Cast { kind: Cast \| DoubleColon, format: None, data_type }` | `data_type` is `BIGINT`, `DOUBLE` / `DOUBLE PRECISION`, `VARCHAR` / `TEXT` / `STRING`, `BOOLEAN` / `BOOL`, or `TIMESTAMP` without time zone or precision (check the sqlparser 0.62 `DataType` variants). The walk sets `kind = CastKind::TryCast` (D100). Other types: "casts to `<type>` are not allowed". |
| `Function` | See below. |

**Functions:**
- One unqualified name, case-insensitive, in the allow-list of spec 05 (`now`, `lower`, `upper`, `length`, `coalesce`, `array_has`, `array_length`, `date_trunc`, `abs`).
- `parameters` empty. Arguments a plain `FunctionArguments::List` of unnamed `FunctionArgExpr::Expr`, validated.
- No `duplicate_treatment`, no clauses, `filter`, `over`, `within_group` or `null_treatment`.
- `now` with an empty list (`now()`) only.

Unknown names fail with "unknown function `<name>`". The score functions arrive in Q8.

### Applying a filter

`impl Filter { pub(crate) fn apply(self, frame: DataFrame) -> Result<DataFrame, DataFusionError> }` returns `frame.filter(self.expr)`. The frame must have the view's columns (unqualified names). Q8 extends this.

### Tests (`filter/mod.rs`; the example project via `test_support`)

**Accepted.** Each case asserts the exact set of matching IDs from the dataset:
1. On `spans`:
   - `status = 'error'`;
   - `model LIKE 'claude%'` (or a prefix that exists in the dataset);
   - `input ILIKE '%<a word present in one span's input>%'`;
   - `span_kind IN ('llm', 'tool')`;
   - `duration_ns BETWEEN 1e8 AND 5e9`.
2. On `traces`:
   - `cost_total > 0.001 AND tags @> ['<a tag present>']`;
   - `metadata['<key present>'] = '<value>'`;
   - `NOT has_root` (a boolean column);
   - `start_time >= '2026-01-01T10:05:00Z'`;
   - `start_time > now() - INTERVAL '3650 days'` (all traces).
3. On `sessions`: `trace_count >= 2`.
4. On `scores`: `name = '<a score name>' AND value_num > 0.5`.
5. Casts: `metadata['<key with a non-numeric value>']::DOUBLE > 0` returns no rows and no error. `CAST(duration_ns AS DOUBLE) > 0` returns all rows.
6. `references_environment` is true for `environment = 'production' OR status = 'ok'`, false for `status = 'ok'`.
7. Identifiers: `"name" = 'x'` and `NAME = 'x'` resolve to `name`.

**Rejected.** Each case asserts the message and the exact position:
- `nme = 'a'` (unknown column, `1:1`);
- `spans.name = 'a'`;
- `trace_id IN (SELECT trace_id FROM spans)`;
- `EXISTS (SELECT 1)`;
- `CASE WHEN true THEN true END`;
- `sleep(1) > 0`;
- `count(*) > 1`;
- `lower(DISTINCT name) = 'a'`;
- `row_number() OVER () > 1`;
- `name = 'a' extra`;
- `cost_total + 1` (not boolean);
- `name['k'] = 'a'`;
- `metadata[1] = 'a'`;
- `CAST(name AS INT) = 1`;
- `x'00' = name` (hex literal);
- `score('x') > 0` (unknown function until Q8);
- an empty filter.

**Stack.** On a `std::thread::Builder::new().stack_size(2 * 1024 * 1024)` thread, with a current-thread Tokio runtime, a filter with exactly 128 operators and 40 levels of parentheses parses, plans, applies to `spans` and runs. This runs in the debug test build: it is the guard for D95.

### Report

The sqlparser `DataType` variants matched for each cast type. Any DSL form you had to treat differently from the table.

---

## Q8 — Score functions

**Goal:** `score(name)`, `score_label(name)` and `has_score(name)` in filters on `traces`, `spans` and `sessions` (D97).

**Edit only:** new `crates/storage/src/filter/scores.rs`, `crates/storage/src/filter/{mod.rs,validate.rs}`.

**Commit:** `Add score functions to the filter DSL`

### Behaviour

**Validation.** The function rule gains `score`, `score_label`, `has_score`:
- Exactly one argument, a single-quoted string literal. Otherwise: "`score()` takes one string literal, the score name".
- On the `scores` view: "score functions are not available on the scores view".

**Rewrite.** Each distinct name (exact string compare) gets an index `i` in order of first appearance, at most 8. The 9th distinct name fails with "a filter can use at most 8 score names", positioned at its call. The walk replaces calls in the AST:
- `score('x')` → `Identifier("__score_<i>_num")`;
- `score_label('x')` → `Identifier("__score_<i>_str")`;
- `has_score('x')` → `IsTrue(Identifier("__score_<i>_present"))`.

These identifiers are internal. A user typing `__score_0_num` still fails as an unknown column, because user identifiers are checked before the rewrite and only against the view.

**Planning.** The rewritten AST is planned against the view's schema extended, for each `i`, with nullable fields `__score_<i>_num: Float64`, `__score_<i>_str: Utf8` and `__score_<i>_present: Boolean`.

`Filter` gains `pub(crate) scores: Vec<String>`, the names in index order.

**`Filter::apply(frame, ctx)`** now also takes `&QueryContext`. For each name `i`:
1. `latest` = `ctx.session().table("scores")`.
   - Filter `target = '<trace|span|session>' AND name = '<x>'`.
   - Add `row_number() OVER (PARTITION BY <keys> ORDER BY updated_at DESC, score_id DESC) AS __rn`, filter `__rn = 1`.
   - Select the keys renamed `__score_<i>_<key>`, plus `value_num AS __score_<i>_num`, `value_str AS __score_<i>_str`, `lit(true) AS __score_<i>_present`.

   The target string comes from `llmobs_core::enums::ScoreTargetKind` (its serialized name), not a literal in this module.

   | View | Keys |
   |---|---|
   | `traces` | `trace_id` |
   | `spans` | `trace_id`, `span_id` |
   | `sessions` | `session_id` |
2. `frame = frame.join(latest, JoinType::Left, &[keys], &[__score_<i>_keys], None)`.

Then filter with the expression, and select the original frame's columns in their original order, which drops every `__score_` column.

The join keys are hex strings (both sides are views), as Q4 documents.

### Tests

The dataset scores, plus a hand-built scores table where noted.
1. `score('<numeric name on trace targets>') >= <v>` on `traces` returns exactly the expected traces. `score('<that name>') IS NULL` returns the others.
2. `has_score('<name>')` and `NOT has_score('<name>')` partition the traces.
3. `score_label('<categorical name>') = '<label>'` on the target kind that has it.
4. Span targets: `score('<span score name>') > …` on `spans`. Session targets: the same on `sessions`.
5. "Latest" (hand-built table, one trace):
   - two scores named `q` with `updated_at` 1 and 2 → `score('q')` is the second one's value;
   - equal `updated_at` → the greater `score_id` wins;
   - the latest one categorical → `score('q') IS NULL` and `score_label('q')` is its label.
6. Two names in one filter, with `OR` and `AND`, and the same name used twice (one join).
7. Output columns are exactly the view's columns.
8. Errors:
   - `score(name) > 1`;
   - `score('a', 'b') > 1`;
   - `score('x') > 1` on `scores`;
   - nine distinct names (position at the 9th);
   - `__score_0_num > 1` (unknown column).

### Report

The plan shape DataFusion produced for one score join (`EXPLAIN` output in the report, not in the code).

---

## Q9 — Exclusions, time window, search

**Goal:** the clauses every list adds next to the user's filter: default exclusions, the time window and full-text search.

**Edit only:** new `crates/storage/src/filter/scope.rs`, `crates/storage/src/filter/mod.rs`.

**Commit:** `Add default exclusions, time windows and search`

### Behaviour

`TimeWindow` and `RunError::InvalidRequest` exist since Q2. The read queries build their `QueryContext` without a window (D101) and add their own with `time_window`.

The `pub(crate)` functions in `scope.rs` build `Expr`s over view columns:
- `time_window(window) -> Expr`: `window.predicate("start_time")`.
- `exclusions(include_eval, include_experiments, filter_references_environment) -> Option<Expr>`. It ANDs:
  - unless `include_eval || filter_references_environment`: `environment IS DISTINCT FROM '<names::environment::EVAL>'`;
  - unless `include_experiments`: `metadata['<names::metadata_key::EXPERIMENT_ID>'] IS NULL`.

  Build the map access by planning the fixed text through `create_logical_expr` against the view schema (trusted text, with the names from `llmobs_core::names`). It returns `None` when both are off. It is valid on `traces` and `spans`.
- `search(view, query: &str, window: Option<TimeWindow>, ctx) -> Result<Option<Expr>, RunError>`:
  - Trim the query; an empty one returns `Ok(None)`.
  - Escape `\` → `\\`, `%` → `\%`, `_` → `\_`, and make the pattern `%<escaped>%`.
  - The span predicate is `input ILIKE p OR output ILIKE p OR name ILIKE p OR system_instructions ILIKE p`, each with escape `'\'` (`Expr::Like` with `case_insensitive: true` and `escape_char: Some('\\')`).
  - On `Spans`: that predicate.
  - On `Traces`: `trace_id IN (SELECT trace_id FROM spans WHERE <span predicate> AND <time_window>)`, built with `in_subquery` over a plan from `ctx.session().table("spans")`.
  - `window` is required on both. Without one: `InvalidRequest("search needs a time range")`.
  - Other views: `InvalidRequest("search is not available on this list")`.
  - The 30-day default and the "search all" confirmation are UI behaviour (spec 05). The API accepts any window.

### Tests

1. `exclusions(false, false, false)` on `traces` drops trace 5 (environment `llmobs-eval`, fixture README) and any trace with the experiment metadata key. The visible set equals `fixtures/api/responses/traces.json`'s IDs.
2. `include_eval` brings trace 5 back. `filter_references_environment` does too, while the experiment exclusion stays.
3. `time_window` with `[10:05, 10:20)` on 2026-01-01 returns exactly the traces starting in it (the boundary trace at 10:20 is excluded).
4. Search on `spans`:
   - a word from one span's input finds that span, case-insensitively;
   - `%` and `_` in the query match literally: `100%` does not match `1000`;
   - a backslash query matches a literal backslash (use a hand-built spans table);
   - an empty query adds nothing.
5. Search on `traces` returns the traces owning the matching spans. Without a window it fails with `InvalidRequest`.

### Report

Whether `in_subquery` planned as a semi-join (`EXPLAIN` in the report).
