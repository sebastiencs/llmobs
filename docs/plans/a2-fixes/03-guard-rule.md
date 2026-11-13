# P3 — Guard planner-built functions and `||` (B-2, part 2)

Read [README.md](README.md) and [02-guarded-functions.md](02-guarded-functions.md) first. P2 must have landed.

**Goal:** every scalar function call in an optimized plan goes through the guard, including the ones that bypass the session registry:
- **Functions the planner builds from global instances.** For example, `tags || tags` on lists becomes `array_concat`, and `tags[1]` becomes `array_element`. `metadata['k']` becomes `get_field`, which stays unguarded (P2). A2's own read queries build functions such as `date_bin` the same way.
- **String concatenation `a || b`.** It stays a `BinaryExpr` with `Operator::StringConcat`, which no function wrapper sees. Chained through CTEs, it doubles memory at every step (`WITH a AS (SELECT input || input AS x FROM spans), b AS (SELECT x || x AS x FROM a), …`).

One logical optimizer rule fixes both. It runs after DataFusion's own rules and does two things:
1. it swaps every unguarded scalar function for its guarded twin, by name;
2. it rewrites `a || b` on strings into `guarded_string_concat(a, b)`, a function that reserves its arguments like P2's wrapper and then evaluates DataFusion's own `||` kernel.

**Edit only:**
- `crates/storage/src/views/guard.rs`;
- `crates/storage/src/views/hex.rs` (make one helper `pub(super)`);
- `crates/storage/src/views/session.rs`;
- `docs/spec/05-query.md`;
- `docs/mvp-decisions.md`.

**Commit:** `Guard planner-built functions and string concatenation`

## Background (checked on DataFusion 55.2)

Optimized logical plans in this session:

| SQL | Optimized projection |
|---|---|
| `SELECT name \|\| 'x' AS a FROM spans` | `spans.name \|\| Utf8("x") AS a` (`BinaryExpr`, type `Utf8`, non-null) |
| `SELECT tags \|\| tags AS a FROM spans` | `array_concat(spans.tags, spans.tags) AS a` |
| `SELECT 'a' \|\| 'b' AS a` | `Utf8("ab") AS a` (folded before any rule of ours runs) |
| `SELECT metadata['k'] AS a FROM spans` | `get_field(raw_spans.metadata, Utf8("k"))` |
| `SELECT tags[1] AS a FROM spans` | `array_element(spans.tags, Int64(1)) AS a` |

- `concat(a, b)` is **not** a substitute for `a || b`. `concat` skips nulls (`concat('a', NULL) = 'a'`) and returns a nullable result; `'a' || NULL` is `NULL`. Rewriting into `concat` would change results and the plan schema.
- DataFusion's optimizer runs every rule in order, in several passes, and stops once a pass changes nothing. A rule appended with `with_optimizer_rule` runs last in each pass. A rule with `apply_order() = Some(BottomUp)` is applied to subquery plans too (`Optimizer::optimize` uses `rewrite_with_subqueries`).
- The optimizer requires a rule to keep the plan's schema: same names, same types. `SimplifyExpressions` keeps names with `NamePreserver` (`datafusion-optimizer-55.2.0/src/simplify_expressions/simplify_exprs.rs`, around line 139). This rule does the same. `NamePreserver` is `datafusion::logical_expr::expr_rewriter::NamePreserver`.
- `HexIdRule` (`views/hex.rs`) is the model for a rule here: `name`, `apply_order`, `supports_rewrite` (with `#[allow(deprecated)]`), and `rewrite` with `plan.map_expressions`. Its private helper `expression_schema(plan)` gives the schema the node's expressions are typed against.

## Changes

### 1. `views/hex.rs`

Change `fn expression_schema` to `pub(super) fn expression_schema`. No other change.

### 2. `views/guard.rs`: `GuardedStringConcat`

```rust
/// `a || b` on strings, run through DataFusion's own kernel after the same
/// reservation as [`GuardedScalar`].
#[derive(Debug, PartialEq, Eq, Hash)]
pub(crate) struct GuardedStringConcat {
    return_type: DataType,
    signature: Signature,
    pool: PtrEq<Arc<dyn MemoryPool>>,
}

impl GuardedStringConcat {
    pub(crate) fn new(return_type: DataType, pool: Arc<dyn MemoryPool>) -> Self {
        Self {
            return_type,
            signature: Signature::any(2, Volatility::Immutable),
            pool: pool.into(),
        }
    }
}
```

The struct holds `PtrEq` exactly as P2's `GuardedScalar` does. If P2 had to implement `PartialEq`/`Hash` by hand, do the same here: compare `return_type`, `signature` and the pool pointer, and hash `return_type` and `signature`.

`impl ScalarUDFImpl for GuardedStringConcat`. Only these methods; everything else keeps the trait default:

```rust
    fn name(&self) -> &str {
        "guarded_string_concat"
    }

    fn signature(&self) -> &Signature {
        &self.signature
    }

    fn return_type(&self, _arg_types: &[DataType]) -> Result<DataType> {
        Ok(self.return_type.clone())
    }

    fn return_field_from_args(&self, args: ReturnFieldArgs) -> Result<FieldRef> {
        let nullable = args.arg_fields.iter().any(|field| field.is_nullable());
        Ok(Arc::new(Field::new(self.name(), self.return_type.clone(), nullable)))
    }

    fn invoke_with_args(&self, args: ScalarFunctionArgs) -> Result<ColumnarValue> {
        let bytes = argument_bytes(&args.args, args.number_rows);
        // Arrow's Utf8 concatenation unwraps an i32 offset and panics past 2 GiB
        // (arrow-string concat_elements.rs); fail instead.
        if self.return_type == DataType::Utf8 && bytes > i32::MAX as usize {
            return exec_err!("string concatenation result is larger than 2 GiB");
        }
        let reservation = MemoryConsumer::new("string concatenation").register(&self.pool);
        reservation.try_grow(bytes)?;
        let result = concat_strings(&args.args, args.number_rows);
        drop(reservation);
        result
    }
```

- The nullability matches DataFusion's `BinaryExpr`, whose result is nullable when either side is.
- `exec_err!` is `datafusion::common::exec_err`.
- The 2 GiB check matters once the pool is larger than 2 GiB (`query` memory is configurable from M6). Without it, a call that passes the reservation could still hit arrow's panic.

```rust
/// `left || right` with DataFusion's `BinaryExpr` kernel. All-scalar arguments
/// give a scalar, as the operator would.
fn concat_strings(args: &[ColumnarValue], rows: usize) -> Result<ColumnarValue> {
    let [left, right] = args else {
        return internal_err!("guarded_string_concat takes two arguments");
    };
    let scalar = matches!(left, ColumnarValue::Scalar(_)) && matches!(right, ColumnarValue::Scalar(_));
    let rows = if scalar { 1 } else { rows };
    let left = left.to_array(rows)?;
    let right = right.to_array(rows)?;
    let schema = Arc::new(Schema::new(vec![
        Field::new("left", left.data_type().clone(), true),
        Field::new("right", right.data_type().clone(), true),
    ]));
    let batch = RecordBatch::try_new(schema, vec![left, right])?;
    let expr = BinaryExpr::new(
        Arc::new(Column::new("left", 0)),
        Operator::StringConcat,
        Arc::new(Column::new("right", 1)),
    );
    let value = expr.evaluate(&batch)?;
    if scalar {
        let array = value.into_array(1)?;
        return Ok(ColumnarValue::Scalar(ScalarValue::try_from_array(&array, 0)?));
    }
    Ok(value)
}
```

Import paths:
- `BinaryExpr` and `Column` are `datafusion::physical_expr::expressions::{BinaryExpr, Column}`;
- `PhysicalExpr` (for `evaluate`) is `datafusion::physical_expr::PhysicalExpr`;
- `internal_err!` is `datafusion::common::internal_err`;
- `Operator` is `datafusion::logical_expr::Operator`.

### 3. `views/guard.rs`: `GuardRule`

```rust
/// Routes every scalar function call through the guard (D-A2-2): swaps unguarded
/// functions for their guarded twins by name, and turns `a || b` on strings into
/// `guarded_string_concat`.
#[derive(Debug)]
pub(crate) struct GuardRule {
    /// Guarded functions by name and alias.
    functions: HashMap<String, Arc<ScalarUDF>>,
    pool: Arc<dyn MemoryPool>,
}

impl GuardRule {
    /// `functions` is P2's `guarded_functions` list; the unguarded ones in it are skipped.
    pub(crate) fn new(functions: &[Arc<ScalarUDF>], pool: Arc<dyn MemoryPool>) -> Self {
        let mut by_name = HashMap::new();
        for udf in functions.iter().filter(|udf| udf.inner().is::<GuardedScalar>()) {
            by_name.insert(udf.name().to_owned(), Arc::clone(udf));
            for alias in udf.aliases() {
                by_name.insert(alias.clone(), Arc::clone(udf));
            }
        }
        Self { functions: by_name, pool }
    }
}
```

`impl OptimizerRule for GuardRule`:
- `name()` returns `"guard_scalar_functions"`;
- `apply_order()` returns `Some(ApplyOrder::BottomUp)`;
- `supports_rewrite()` returns `true`, with `#[allow(deprecated)]` exactly as in `HexIdRule`;
- `rewrite`:

```rust
    fn rewrite(
        &self,
        plan: LogicalPlan,
        _config: &dyn OptimizerConfig,
    ) -> Result<Transformed<LogicalPlan>, DataFusionError> {
        let schema = expression_schema(&plan)?;
        let names = NamePreserver::new(&plan);
        plan.map_expressions(|expr| {
            // A grouping set keeps its own children's names; aliasing the set
            // itself would hide it from the Aggregate (as SimplifyExpressions does).
            if let Expr::GroupingSet(_) = &expr {
                expr.map_children(|child| self.guard_named(child, &names, &schema))
            } else {
                self.guard_named(expr, &names, &schema)
            }
        })
    }
```

with, in `impl GuardRule`:

```rust
    fn guard_named(
        &self,
        expr: Expr,
        names: &NamePreserver,
        schema: &DFSchema,
    ) -> Result<Transformed<Expr>, DataFusionError> {
        let saved = names.save(&expr);
        let rewritten = expr.transform_up(|expr| self.guard_one(expr, schema))?;
        Ok(Transformed::new_transformed(
            saved.restore(rewritten.data),
            rewritten.transformed,
        ))
    }
```

This mirrors `SimplifyExpressions::optimize_internal` (`simplify_exprs.rs:139-156`).

`expression_schema` is `super::hex::expression_schema`.

```rust
impl GuardRule {
    fn guard_one(&self, expr: Expr, schema: &DFSchema) -> Result<Transformed<Expr>, DataFusionError> {
        match expr {
            Expr::ScalarFunction(call) => {
                let inner = call.func.inner();
                let name = call.func.name();
                if inner.is::<GuardedScalar>()
                    || inner.is::<GuardedStringConcat>()
                    || UNGUARDED_FUNCTIONS.contains(&name)
                {
                    return Ok(Transformed::no(Expr::ScalarFunction(call)));
                }
                if let Some(guarded) = self.functions.get(name) {
                    return Ok(Transformed::yes(Expr::ScalarFunction(ScalarFunction::new_udf(
                        Arc::clone(guarded),
                        call.args,
                    ))));
                }
                if DENIED_FUNCTIONS.contains(&name) {
                    return plan_err!("function `{name}` is not available");
                }
                Ok(Transformed::no(Expr::ScalarFunction(call)))
            }
            Expr::BinaryExpr(binary) if binary.op == Operator::StringConcat => {
                let original = Expr::BinaryExpr(binary);
                let return_type = original.get_type(schema)?;
                let Expr::BinaryExpr(binary) = original else {
                    return internal_err!("string concatenation changed shape");
                };
                let udf = ScalarUDF::new_from_impl(GuardedStringConcat::new(
                    return_type,
                    Arc::clone(&self.pool),
                ));
                Ok(Transformed::yes(Expr::ScalarFunction(ScalarFunction::new_udf(
                    Arc::new(udf),
                    vec![*binary.left, *binary.right],
                ))))
            }
            other => Ok(Transformed::no(other)),
        }
    }
}
```

- `ScalarFunction` is `datafusion::logical_expr::expr::ScalarFunction`.
- `get_type` comes from `datafusion::logical_expr::ExprSchemable`.
- `plan_err!` is `datafusion::common::plan_err`.
- `transform_up` visits children first. When the outer `||` of `(a || b) || c` is typed, its left child is already a `guarded_string_concat` call, and `get_type` gets the stored type from `return_type`.
- A function name that is neither guarded nor denied (none is known today) is left as is.
- The `plan_err!` branch is defensive. No planner path is known to build a denied function, so there is no test for it.

### 4. `views/session.rs`

Build the functions once and give the rule the same list:

```rust
    let pool = Arc::clone(&runtime.memory_pool);
    let functions = guarded_functions(&pool);
    let mut builder = SessionStateBuilder::new()
        // … unchanged …
        .with_scalar_functions(functions.clone())
        // … unchanged …
        .with_optimizer_rule(Arc::new(HexIdRule))
        .with_optimizer_rule(Arc::new(GuardRule::new(&functions, Arc::clone(&pool))));
```

- `GuardRule` comes after `HexIdRule`. The hex rule matches `encode` by name and works on guarded and unguarded calls alike.
- `SessionStateBuilder::build` registers each function's `with_updated_config` result. For this session's config that equals the instance passed in, and `GuardedScalar::with_updated_config` re-wraps anyway, so building the rule's map from the pre-`build` list is equivalent.

## Docs

### `docs/spec/05-query.md`, § SQL console › Sandbox, item 4 (as P2 left it)

After the sentence ending `…fails with `query_too_large` (D-A2-2).`, insert:

```
Functions the planner inserts itself (`array_concat` for `||` on lists, `array_element` for subscripts, …) and string concatenation with `||` go through the same reservation.
```

### `docs/mvp-decisions.md`, row D-A2-2

Append to the end of the decision cell, before the closing `|`:

```
 A last optimizer rule swaps planner-built functions for their guarded twins and turns `a || b` on strings into a guarded call of DataFusion's own `||` kernel, so concatenation chained through CTEs cannot double memory past the pool either.
```

## Tests

In `views/guard.rs` `mod tests`. Reuse P2's `big_input_context`.

**T1 `every_call_in_an_optimized_plan_is_guarded`.** Build `let ctx = QueryContext::new(example_query(), None)`. For each query below:
- `let frame = ctx.session().sql(query).await.expect(query)`;
- `let plan = ctx.session().state().optimize(frame.logical_plan()).expect("optimize")`;
- walk the plan with subqueries: `plan.apply_with_subqueries(|node| { node.apply_expressions(|expr| expr.apply(|e| { … })) })`;
- assert every `Expr::ScalarFunction(call)` has `call.func.inner().is::<GuardedScalar>() || call.func.inner().is::<GuardedStringConcat>()`;
- assert no `Expr::BinaryExpr` has `op == Operator::StringConcat`.

Report the query and the offending expression in the failure message. Queries:
1. `SELECT tags || tags AS a, name || 'x' AS b, metadata['k'] AS c, tags[1] AS d, upper(name) AS e FROM spans WHERE name || '!' <> ''`
   - `get_field` (from `metadata['k']`) is exempt: the walk accepts a call whose `call.func.name()` is in `UNGUARDED_FUNCTIONS`.
2. `SELECT span_id FROM spans WHERE trace_id IN (SELECT trace_id FROM traces WHERE name || 'x' <> '')`
3. `WITH a AS (SELECT name || name AS x FROM spans) SELECT x || x AS y FROM a`
4. `SELECT date_bin(INTERVAL '1 hour', start_time, TIMESTAMP '1970-01-01T00:00:00Z') AS b, count(*) AS n FROM spans GROUP BY 1`
5. `SELECT name || 'x' AS a, count(*) AS n FROM spans GROUP BY ROLLUP(name || 'x')`

**T2 `string_concatenation_keeps_its_semantics`.** Through `sql_json` on `example_query()`, run `SELECT name, name || '!' AS a, name || CASE WHEN name = 'no such name' THEN 'x' END AS b FROM spans ORDER BY span_id LIMIT 3`. For each of the 3 rows:
- `a` equals `format!("{name}!")`, with `name` the row's `name` string;
- `b` is JSON `null`.

The `CASE` is a null the simplifier cannot fold. `name || CAST(NULL AS VARCHAR)` is folded to a null literal before any rule of ours runs, so it would not reach the guarded kernel.

Also run `SELECT name || 'x' AS a, count(*) AS n FROM spans GROUP BY ROLLUP(name || 'x') ORDER BY a NULLS FIRST`. Assert it succeeds, and that the row with `a` null has `n` equal to the dataset's span count (14).

**T3 `the_rule_keeps_the_plan_schema`.** For `SELECT name || 'x' FROM spans` (no alias), `SELECT name || 'x' AS a, count(*) AS n FROM spans GROUP BY name || 'x'` and `SELECT name || 'x' AS a, count(*) AS n FROM spans GROUP BY ROLLUP(name || 'x')`:
- `let unoptimized = ctx.session().sql(q).await?.logical_plan().clone()`;
- `let optimized = ctx.session().state().optimize(&unoptimized)?`;
- assert the two schemas have the same field names, data types and nullability, in order.

**T4 `cte_doubling_hits_the_memory_limit`.**
- Use `big_input_context(64 * 1024 * 1024, 100_000)` (P2's helper already takes the input length).
- Run through `sql_json`:

  ```
  WITH a0 AS (SELECT input AS x FROM spans),
       a1 AS (SELECT x || x AS x FROM a0),
       …
       a12 AS (SELECT x || x AS x FROM a11)
  SELECT length(x) AS n FROM a12
  ```

  Build the text in a loop. It returns `Err(RunError::TooLarge)`.
- The same shape with 3 levels (`a1`–`a3`, `SELECT length(x) AS n FROM a3`) returns `Ok` with 14 rows of `{"n": 800000}`.
- After the `Ok` run, `runtime.memory_pool.reserved() == 0`.
- On the unchanged code, the 12-level query does not return: it grows to several GB and panics in arrow's `concat_elements` (offset overflow). Do not run it without the change.
- After the change it must return `TooLarge` at about level 6 (1.4 MB × 2^k passes 64 MiB). If it fails with another error, or returns `Ok`, **stop and report** the error and the optimized plan (`EXPLAIN`). Do not change the size accounting.

**Existing tests** must pass unchanged, in particular:
- the hex rule tests, whose filter `Display` checks contain `encode(`;
- every `reads::` test;
- `sql::` tests that check `EXPLAIN` output, if any. If an `EXPLAIN` expectation changes only because `||` now prints as `guarded_string_concat(…)`, update that expectation and list the test in your report.

## Done when

- `cargo test -p llmobs-storage --lib` passes.
- `just check` passes.
