# P2 — Guarded scalar functions (B-2, part 1)

Read [README.md](README.md) first. P1 must have landed.

**Goal:** a scalar function can no longer allocate memory the query memory pool never sees. Two changes:
1. **Deny-list.** Functions whose result can be far larger than their arguments, or that read storage internals, are removed from the session.
2. **Guard.** Every other scalar function, except `get_field`, is wrapped. Before each call, the wrapper reserves the bytes the call will read in the query memory pool. When the pool cannot grant them, the query fails with `RunError::TooLarge` (`query_too_large`, 422).

P3 then makes sure functions the planner inserts on its own, and the `||` operator, also go through the guard.

**The guard only takes effect once the query pool is bounded.** Today `Storage::open` builds the process-wide query runtime with `RuntimeEnvBuilder::new().with_metadata_cache_limit(..)` (`crates/storage/src/lib.rs`, `build_runtime`), whose pool is DataFusion's `UnboundedMemoryPool`. Spec 03 § 6 says "the memory pool stays the default until M6".

The user decided that this plan depends on A1 M6 bounding that pool. Until then only the deny-list protects production. The tests below use bounded pools, so the guard is fully tested now.

**Edit only:**
- new `crates/storage/src/views/guard.rs`;
- `crates/storage/src/views/mod.rs` (one `pub(crate) mod guard;` line);
- `crates/storage/src/views/session.rs`;
- `crates/storage/src/sql/validate.rs`;
- `docs/spec/05-query.md`;
- `docs/mvp-decisions.md`.

**Commit:** `Guard scalar functions with the query memory pool`

## Background

- `session_state` (`views/session.rs`) registers `SessionStateDefaults::default_scalar_functions()` as is.
- DataFusion's `MemoryPool` only sees operators that register a `MemoryReservation` (sorts, joins, aggregates). Scalar function kernels allocate their result arrays directly. Measured under a 64 MiB pool:
  - `SELECT array_length(range(0, 100000000))`: peak RSS from 48 to 825 MiB;
  - `SELECT length(repeat('x', 400000000))`: up to 1,598 MiB.

  Both succeeded.
- `query_error::classify` already maps `DataFusionError::ResourcesExhausted` to `RunError::TooLarge`. `ScalarFunctionExpr::evaluate` passes a function's error through unwrapped.
- The pool is `ProjectQuery::runtime.memory_pool` (`Arc<dyn MemoryPool>`, a public field of `RuntimeEnv`). `QueryContext::new` passes `source.runtime` to `session_state(runtime, …)`. In production that runtime is **process-wide**: every query shares one pool. A reservation here therefore competes with all running queries, which is the intended meaning of "the pool has room".
- Other code matches functions **by name**:
  - the hex rule (`func.name() != "encode"`);
  - A1's `query/trace_list.rs` (`function.fun().name() != "encode"`);
  - `query/mod.rs` (`func.name() == "arrow_cast"`).

  The wrapper forwards `name()`, so they keep working.
- Some DataFusion code matches functions by **concrete type**, and a wrapper fails those checks. The cost is accepted:
  - `get_field` simplifications and struct projection pushdown (`datafusion-functions-55.2.0/src/core/getfield.rs`, `datafusion-datasource-parquet` projection, `physical-expr-adapter` schema rewriting);
  - `log`/`power` simplifications (`math/log.rs`, `math/power.rs`).

  `get_field` only extracts part of its input, so it can never be larger than it. It therefore stays **unwrapped**, and its pushdowns and schema handling keep working. Losing the `log`/`power` simplifications is a minor planning cost.

## The deny-list

Canonical names and their aliases, as registered by DataFusion 55.2 (`SessionStateDefaults::default_scalar_functions()`):

| Name | Aliases | Why |
|---|---|---|
| `range` | — | builds a list of any length from two numbers |
| `generate_series` | — | same |
| `array_repeat` | `list_repeat` | list of any length from one value |
| `repeat` | — | string of any length |
| `lpad` | — | pads to any length (up to 2³¹ bytes per row) |
| `rpad` | — | same |
| `array_resize` | `list_resize` | list of any length |
| `replace` | — | each match becomes the replacement: output ≈ input length × replacement length |
| `regexp_replace` | — | same, with an empty pattern matching between every character |
| `array_replace` | `list_replace` | each matching element becomes the replacement |
| `array_replace_n` | `list_replace_n` | same |
| `array_replace_all` | `list_replace_all` | same |
| `levenshtein` | — | O(n·m) CPU in one uninterruptible kernel call |
| `input_file_name` | — | storage internals (file paths), not part of the views |
| `file_row_index` | — | storage internals, not part of the views |

- `replace` and `regexp_replace` are useful, but their output cannot be bounded by their arguments' size. Users can still write `split_part`, `substr`, `translate`, `regexp_match` and the like.
- `concat_ws` and `array_to_string` (aliases `list_to_string`, `array_join`, `list_join`) also repeat an argument, their separator. They stay in: the guard counts the separator once per place it is written (below).

## Changes

### 1. New `views/guard.rs`

Module doc: `//! Scalar functions that reserve the bytes they read in the query memory pool before each call (D-A2-2).`

**Constants:**

```rust
/// Scalar functions the session does not register, with their aliases (D-A2-2).
pub(crate) const DENIED_FUNCTIONS: &[&str] = &[
    "range",
    "generate_series",
    "array_repeat",
    "list_repeat",
    "repeat",
    "lpad",
    "rpad",
    "array_resize",
    "list_resize",
    "replace",
    "regexp_replace",
    "array_replace",
    "list_replace",
    "array_replace_n",
    "list_replace_n",
    "array_replace_all",
    "list_replace_all",
    "levenshtein",
    "input_file_name",
    "file_row_index",
];

/// Scalar functions registered without the guard: their result is part of
/// their input, and DataFusion recognises them by type (D-A2-2).
pub(crate) const UNGUARDED_FUNCTIONS: &[&str] = &["get_field"];
```

**The wrapper:**

```rust
/// A scalar function that reserves the bytes it reads in the query memory
/// pool for the duration of each call.
#[derive(Debug, PartialEq, Eq, Hash)]
pub(crate) struct GuardedScalar {
    inner: UdfEq<Arc<dyn ScalarUDFImpl>>,
    pool: PtrEq<Arc<dyn MemoryPool>>,
    /// Memory consumer name, built once: `scalar function <name>`.
    consumer: String,
}

impl GuardedScalar {
    pub(crate) fn new(inner: Arc<dyn ScalarUDFImpl>, pool: Arc<dyn MemoryPool>) -> Self {
        let consumer = format!("scalar function {}", inner.name());
        Self { inner: inner.into(), pool: pool.into(), consumer }
    }
}
```

- `UdfEq` is `datafusion::logical_expr::udf_eq::UdfEq`, and `PtrEq` is `datafusion::logical_expr::ptr_eq::PtrEq`.
- `derive(Debug, PartialEq, Eq, Hash)` compiles for both: `PtrEq<Arc<T: ?Sized>>` compares and hashes by pointer, and `MemoryPool: Debug`.

**`impl ScalarUDFImpl for GuardedScalar`** mirrors DataFusion's `AliasedScalarUDFImpl` (`datafusion-expr-55.2.0/src/udf.rs`, the `impl ScalarUDFImpl for AliasedScalarUDFImpl` block, lines 1110–1226) method by method:
- Put `#[warn(clippy::missing_trait_methods)]` on the impl, as DataFusion does.
- Every method forwards to `self.inner` exactly as `AliasedScalarUDFImpl` does, including the two `#[expect(deprecated)]` calls (`display_name`, `is_nullable`).
- Import each argument type from the same path that file's `use` lines show, through the `datafusion::logical_expr` / `datafusion::common` re-exports.

Three methods differ:

```rust
    fn aliases(&self) -> &[String] {
        self.inner.aliases()
    }

    fn with_updated_config(&self, config: &ConfigOptions) -> Option<ScalarUDF> {
        let updated = self.inner.with_updated_config(config)?;
        Some(ScalarUDF::new_from_impl(GuardedScalar::new(
            Arc::clone(updated.inner()),
            Arc::clone(&self.pool),
        )))
    }

    fn invoke_with_args(&self, args: ScalarFunctionArgs) -> Result<ColumnarValue> {
        let bytes = call_bytes(self.inner.name(), &args.args, args.number_rows)?;
        let reservation = MemoryConsumer::new(self.consumer.clone()).register(&self.pool);
        reservation.try_grow(bytes)?;
        let result = self.inner.invoke_with_args(args);
        drop(reservation);
        result
    }
```

- `MemoryReservation::try_grow` takes `&self`, so the binding is not `mut`.
- `&self.pool` dereferences through `PtrEq`'s `Deref` to `&Arc<dyn MemoryPool>`. Write `&*self.pool` if the compiler needs it.
- `try_grow` returns `DataFusionError::ResourcesExhausted` when the pool is full; `?` passes it up, and `classify` turns it into `TooLarge`.
- The reservation is dropped right after the call, so it is a **peak check**: the call may run only if the pool has room for what it reads at that moment. The result array is not tracked afterwards; the next call that reads it reserves it again.
- The consumer name contains the function name only (no values).

**The size helpers:**

```rust
/// Bytes a call reads: arrays by their buffer size, scalars once per row.
/// Conservative: shared buffers and the scalar struct size are counted whole.
pub(crate) fn argument_bytes(args: &[ColumnarValue], rows: usize) -> usize {
    args.iter()
        .map(|arg| match arg {
            ColumnarValue::Array(array) => array.get_array_memory_size(),
            ColumnarValue::Scalar(scalar) => scalar.size().saturating_mul(rows),
        })
        .fold(0, usize::saturating_add)
}

/// Bytes a call of `name` reads, plus the separators it writes more than once.
fn call_bytes(name: &str, args: &[ColumnarValue], rows: usize) -> Result<usize> {
    let mut bytes = argument_bytes(args, rows);
    match name {
        // concat_ws(sep, a1, …, an) writes sep n − 1 times per row; argument_bytes
        // counted it once.
        "concat_ws" if args.len() > 2 => {
            let extra = longest_string(&args[0])?
                .saturating_mul(args.len() - 2)
                .saturating_mul(rows);
            bytes = bytes.saturating_add(extra);
        }
        // array_to_string(list, delimiter[, null_string]) writes the delimiter
        // (or the null string) once per leaf element.
        "array_to_string" if args.len() >= 2 => {
            let mut per_element = longest_string(&args[1])?;
            if let Some(null_string) = args.get(2) {
                per_element = per_element.saturating_add(longest_string(null_string)?);
            }
            let extra = per_element.saturating_mul(leaf_count(&args[0], rows)?);
            bytes = bytes.saturating_add(extra);
        }
        _ => {}
    }
    Ok(bytes)
}
```

- `name` is the canonical name (`inner.name()`), so the aliases `list_to_string`, `array_join` and `list_join` land on `"array_to_string"`.
- Indexing `args[0]` and `args[1]` is guarded by the `args.len()` conditions in the match arms. Clippy's `indexing_slicing` is not denied in this crate; if it is, use `args.first()` and `args.get(1)`.

```rust
/// Longest string in `arg`, in bytes: a scalar's length, or the longest value
/// of an array. Non-string arguments count 0.
fn longest_string(arg: &ColumnarValue) -> Result<usize> {
    match arg {
        ColumnarValue::Scalar(
            ScalarValue::Utf8(Some(text))
            | ScalarValue::LargeUtf8(Some(text))
            | ScalarValue::Utf8View(Some(text)),
        ) => Ok(text.len()),
        ColumnarValue::Scalar(_) => Ok(0),
        ColumnarValue::Array(array) => {
            if !matches!(
                array.data_type(),
                DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View
            ) {
                return Ok(0);
            }
            let lengths = arrow::compute::kernels::length::length(array)?;
            let lengths = arrow::compute::cast(&lengths, &DataType::Int64)?;
            let longest = arrow::compute::max(lengths.as_primitive::<Int64Type>()).unwrap_or(0);
            Ok(usize::try_from(longest).unwrap_or(0))
        }
    }
}

/// Number of leaf values under a list argument, through nested lists.
/// A scalar list is broadcast to `rows` rows.
fn leaf_count(arg: &ColumnarValue, rows: usize) -> Result<usize> {
    let (mut array, broadcast) = match arg {
        ColumnarValue::Array(array) => (Arc::clone(array), 1),
        ColumnarValue::Scalar(scalar) => (scalar.to_array_of_size(1)?, rows),
    };
    loop {
        let values = if let Some(list) = array.as_list_opt::<i32>() {
            Arc::clone(list.values())
        } else if let Some(list) = array.as_list_opt::<i64>() {
            Arc::clone(list.values())
        } else if let Some(list) = array.as_fixed_size_list_opt() {
            Arc::clone(list.values())
        } else {
            return Ok(array.len().saturating_mul(broadcast));
        };
        array = values;
    }
}
```

- The loop walks list nesting levels. Their number is bounded by the type, not by data, so it is not recursion over user input.
- For a sliced list array, `values()` covers the whole child buffer, which over-counts. That is acceptable and conservative.
- `as_list_opt` and `as_fixed_size_list_opt` come from `arrow::array::AsArray`. `as_primitive` and `Int64Type` come from `arrow::array::AsArray` and `arrow::datatypes::Int64Type`.

**The registry:**

```rust
/// DataFusion's scalar functions minus [`DENIED_FUNCTIONS`], each wrapped in
/// [`GuardedScalar`] over `pool` except [`UNGUARDED_FUNCTIONS`].
pub(crate) fn guarded_functions(pool: &Arc<dyn MemoryPool>) -> Vec<Arc<ScalarUDF>> {
    SessionStateDefaults::default_scalar_functions()
        .into_iter()
        .filter(|udf| !DENIED_FUNCTIONS.contains(&udf.name()))
        .map(|udf| {
            if UNGUARDED_FUNCTIONS.contains(&udf.name()) {
                return udf;
            }
            Arc::new(ScalarUDF::new_from_impl(GuardedScalar::new(
                Arc::clone(udf.inner()),
                Arc::clone(pool),
            )))
        })
        .collect()
}
```

`ScalarUDF::inner()` returns `&Arc<dyn ScalarUDFImpl>` (`udf.rs:147`).

### 2. `views/mod.rs`

Add `pub(crate) mod guard;` next to the other `mod` lines. It must be `pub(crate)`: `sql/validate.rs` reads `DENIED_FUNCTIONS`.

### 3. `views/session.rs`

1. In `session_state`, before `SessionStateBuilder::new()`, take the pool:

   ```rust
   let pool = Arc::clone(&runtime.memory_pool);
   ```

2. Replace `.with_scalar_functions(SessionStateDefaults::default_scalar_functions())` with `.with_scalar_functions(guarded_functions(&pool))`, importing `super::guard::guarded_functions`.

3. Extend the module doc's "Left out" list with one bullet:

   ```
   //! - the scalar functions in `guard::DENIED_FUNCTIONS`; every other scalar
   //!   function except `get_field` is wrapped so it reserves what it reads in the
   //!   memory pool (D-A2-2).
   ```

### 4. `sql/validate.rs`: a clear error for denied functions

In `Walk::pre_visit_expr`, after `let Some(name) = bare_name(&function.name) …` and before the score-function check, add:

```rust
if DENIED_FUNCTIONS.contains(&name.as_str()) {
    return ControlFlow::Break(invalid(
        format!("function `{name}` is not available in the SQL console"),
        at(expr.span()),
    ));
}
```

- Import `crate::views::guard::DENIED_FUNCTIONS`.
- `bare_name` lowercases, so `RANGE(3)` reports `range`.
- The filter DSL needs no change: its allow-list (`filter/validate.rs`) contains none of these functions.

## Docs

### `docs/spec/05-query.md`, § SQL console › Sandbox, item 4

Replace item 4 with:

```
4. Functions: DataFusion's built-in scalar, nested, aggregate and window functions, except those whose result can be far larger than their arguments or that read storage internals: `range`, `generate_series`, `repeat`, `lpad`, `rpad`, `replace`, `regexp_replace`, `array_repeat`, `array_resize`, `array_replace`, `array_replace_n`, `array_replace_all` (and their `list_*` aliases), `levenshtein` (quadratic CPU), `input_file_name` and `file_row_index`. Using one is a 400 naming it. Every other scalar function call except `get_field` first reserves the bytes it reads in the query memory pool (for `concat_ws` and `array_to_string`, the separator once per place it is written); when the pool is full the query fails with `query_too_large`. The reservation takes effect once the query pool is bounded (A1 M6) (D-A2-2). No table or higher-order functions. Score functions are DSL-only (D97).
```

### `docs/mvp-decisions.md`, section `## Track A2 — Query layer`

Add after D-A2-1 (create the section as P1 describes if it is missing):

```
| D-A2-2 | Guarded scalar functions | **The SQL console's session (D96) leaves out the scalar functions whose result can be far larger than their arguments or that read storage internals (`range`, `generate_series`, `repeat`, `lpad`, `rpad`, `replace`, `regexp_replace`, `array_repeat`, `array_resize`, `array_replace*`, their `list_*` aliases, `levenshtein`, `input_file_name`, `file_row_index`), and wraps every other scalar function except `get_field` so each call reserves the bytes it reads in the query memory pool, failing with `query_too_large` when the pool is full.** DataFusion's pool only sees operators that reserve memory; the arrays a scalar function returns are invisible to it, so `repeat('x', 4e8)` used 1.6 GB under a 64 MiB pool. The reservation lasts for the call only (a peak check). For every function left in, the output is at most a small multiple of what is counted; `concat_ws` and `array_to_string` count their separator once per place it is written. `get_field` stays unwrapped: it only extracts from its input, and DataFusion recognises it by type for pushdowns. Literal broadcast and join row duplication stay outside the pool, bounded by the 64 KiB statement and the 8,192-row batch. ClickHouse-based competitors get the equivalent from ClickHouse's allocator-level `max_memory_usage`; DataFusion has none. The query pool is unbounded until A1 M6 bounds it; until then only the deny-list has effect (spec 05 § Sandbox). |
```

## Tests

### In `views/guard.rs` `#[cfg(test)] mod tests`

**T1 `the_registry_holds_only_guarded_functions`.** Build `let ctx = QueryContext::new(example_query(), None).expect("context")`. For every `(name, udf)` in `ctx.session().state().scalar_functions()` (a `&HashMap<String, Arc<ScalarUDF>>` keyed by name and alias):
- if `UNGUARDED_FUNCTIONS.contains(&udf.name())`, assert `!udf.inner().is::<GuardedScalar>()`; otherwise assert `udf.inner().is::<GuardedScalar>()`, with `name` in the failure message;
- assert `!DENIED_FUNCTIONS.contains(&name.as_str())`.

Also assert that the map contains `concat`, `encode`, `date_bin` and `get_field`.

**T2 `the_deny_list_names_every_alias`.** Loop over `SessionStateDefaults::default_scalar_functions()`:
- for each `udf` whose `name()` is in `DENIED_FUNCTIONS`, assert every entry of `udf.aliases()` is in `DENIED_FUNCTIONS`;
- collect every name and alias of every default function into a set, and assert each `DENIED_FUNCTIONS` and `UNGUARDED_FUNCTIONS` entry is in that set.

This catches renames when DataFusion is bumped.

**T3 `a_call_over_the_pool_fails_and_releases_everything`.** Build a context with a bounded pool and large inputs:

```rust
fn big_input_context(pool_bytes: usize, input_len: usize) -> (QueryContext, Arc<RuntimeEnv>) {
    let mut spans = dataset_spans();
    for span in &mut spans {
        span.input = Some("x".repeat(input_len));
    }
    let runtime = Arc::new(
        RuntimeEnvBuilder::new()
            .with_memory_limit(pool_bytes, 1.0)
            .build()
            .expect("runtime"),
    );
    let mut source = example_query();
    source.spans = spans_table(&spans);
    source.traces = traces_table(&spans);
    source.runtime = Arc::clone(&runtime);
    (QueryContext::new(source, None).expect("context"), runtime)
}
```

The dataset has 14 spans, so with `input_len = 200_000` the `input` column holds about 2.8 MB in one batch.
1. With `big_input_context(1024 * 1024, 200_000)`: `ctx.sql_json(SqlRequest { query: "SELECT length(upper(input)) AS n FROM spans", params: &Map::new(), limit: 100, timeout: Duration::from_secs(30) })` returns `Err(RunError::TooLarge)`.
2. With `big_input_context(256 * 1024 * 1024, 200_000)`: the same request returns `Ok`, with 14 rows, each `{"n": 200000}`.
3. After step 2, assert `runtime.memory_pool.reserved() == 0`: every reservation was released.

Without the guard, step 1 returns `Ok` (checked on today's code), so this test fails before the change.

**T4 `argument_bytes_counts_scalars_per_row`.**
- `argument_bytes(&[ColumnarValue::Scalar(ScalarValue::Utf8(Some("abc".into())))], 10)` equals `ScalarValue::Utf8(Some("abc".into())).size() * 10`.
- For an `Int64Array` of 3 values `a` (as `ArrayRef`): `argument_bytes(&[ColumnarValue::Array(Arc::clone(&a)), ColumnarValue::Array(Arc::clone(&a))], 3)` equals `2 * a.get_array_memory_size()`.

**T5 `separators_count_once_per_place_written`.**
- `concat_ws` with 4 arguments: separator scalar `"--"` (2 bytes), then three `StringArray`s of 5 rows. `call_bytes("concat_ws", &args, 5)` equals `argument_bytes(&args, 5) + 2 * 2 * 5`.
- `array_to_string` with a `ListArray` of 2 rows holding 3 and 4 strings (7 leaves), delimiter scalar `", "` (2 bytes), and no null string. `call_bytes("array_to_string", &args, 2)` equals `argument_bytes(&args, 2) + 2 * 7`.
- The same call with a third argument, the scalar null string `"NULL"` (4 bytes): `argument_bytes(&args, 2) + (2 + 4) * 7`.
- `array_to_string` whose delimiter is a `StringArray` `[",", ";;;"]`: the extra is `3 * 7`, the longest delimiter.
- A nested list (`List(List(Utf8))`, 1 row, 2 inner lists of 2 strings each): the leaf count is 4.

**T6 `a_long_separator_hits_the_pool`.** With `big_input_context(1024 * 1024, 10)`:
- query `SELECT length(concat_ws('<sep>', name, name, …)) AS n FROM spans`, with `<sep>` 20,000 `x` characters and `name` repeated 100 times;
- it returns `Err(RunError::TooLarge)`: about 20,000 × 99 × 14 ≈ 27.7 MB counted;
- `SELECT length(concat_ws('-', name, name)) AS n FROM spans` on the same context returns `Ok`.

### In `sql/validate.rs` `mod tests`

**T7 `denied_functions_are_rejected_with_a_position`.** Table-driven like `statements_outside_a_query_are_rejected`, through `plan_sql`:

| Text | Message | Position |
|---|---|---|
| `SELECT repeat('x', 3) AS a` | ``function `repeat` is not available in the SQL console`` | `at(1, 8)` |
| `SELECT list_repeat(1, 3) AS a` | ``function `list_repeat` is not available in the SQL console`` | `at(1, 8)` |
| `SELECT RANGE(3) AS a` | ``function `range` is not available in the SQL console`` | `at(1, 8)` |
| `SELECT 1 AS a WHERE levenshtein('a', 'b') > 0` | ``function `levenshtein` is not available in the SQL console`` | `at(1, 21)` |
| `SELECT replace(name, 'a', 'b') AS a FROM spans` | ``function `replace` is not available in the SQL console`` | `at(1, 8)` |

**Existing tests** must pass unchanged, in particular:
- `views::session::tests::registry_keeps_the_functions_users_need`;
- the hex rule tests (`views::hex`);
- the A1 trace list fast path tests (`query::trace_list`, `query::traces`);
- every `reads::` and `sql::` test.

## Done when

- `cargo test -p llmobs-storage --lib` passes (the whole library: the guard is in every query's path).
- `just check` passes.
