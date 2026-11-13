//! Scalar functions that reserve the bytes they read in the query memory pool before each call (D-A2-2).

use std::sync::Arc;

use arrow::array::{Array, ArrayRef, AsArray};
use arrow::compute::kernels::length::length;
use arrow::datatypes::{DataType, FieldRef, Int64Type};
use datafusion::common::config::ConfigOptions;
use datafusion::common::{ExprSchema, Result, ScalarValue};
use datafusion::execution::SessionStateDefaults;
use datafusion::execution::memory_pool::{MemoryConsumer, MemoryPool};
use datafusion::logical_expr::interval_arithmetic::Interval;
use datafusion::logical_expr::preimage::PreimageResult;
use datafusion::logical_expr::ptr_eq::PtrEq;
use datafusion::logical_expr::simplify::{ExprSimplifyResult, SimplifyContext};
use datafusion::logical_expr::sort_properties::{ExprProperties, SortProperties};
use datafusion::logical_expr::udf_eq::UdfEq;
use datafusion::logical_expr::{
    ColumnarValue, Documentation, Expr, ExpressionPlacement, ReturnFieldArgs, ScalarFunctionArgs,
    ScalarUDF, ScalarUDFImpl, Signature, StructFieldMapping,
};

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
        Self {
            inner: inner.into(),
            pool: pool.into(),
            consumer,
        }
    }
}

#[warn(clippy::missing_trait_methods)] // Delegates, so it should implement every single trait method
impl ScalarUDFImpl for GuardedScalar {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn display_name(&self, args: &[Expr]) -> Result<String> {
        #[expect(deprecated)]
        self.inner.display_name(args)
    }

    fn schema_name(&self, args: &[Expr]) -> Result<String> {
        self.inner.schema_name(args)
    }

    fn signature(&self) -> &Signature {
        self.inner.signature()
    }

    fn return_type(&self, arg_types: &[DataType]) -> Result<DataType> {
        self.inner.return_type(arg_types)
    }

    fn return_field_from_args(&self, args: ReturnFieldArgs) -> Result<FieldRef> {
        self.inner.return_field_from_args(args)
    }

    fn is_nullable(&self, args: &[Expr], schema: &dyn ExprSchema) -> bool {
        #[expect(deprecated)]
        self.inner.is_nullable(args, schema)
    }

    fn is_strict(&self) -> bool {
        self.inner.is_strict()
    }

    fn invoke_with_args(&self, args: ScalarFunctionArgs) -> Result<ColumnarValue> {
        let bytes = call_bytes(self.inner.name(), &args.args, args.number_rows)?;
        let reservation = MemoryConsumer::new(self.consumer.clone()).register(&self.pool);
        reservation.try_grow(bytes)?;
        let result = self.inner.invoke_with_args(args);
        drop(reservation);
        result
    }

    fn with_updated_config(&self, config: &ConfigOptions) -> Option<ScalarUDF> {
        let updated = self.inner.with_updated_config(config)?;
        Some(ScalarUDF::new_from_impl(GuardedScalar::new(
            Arc::clone(updated.inner()),
            Arc::clone(&self.pool),
        )))
    }

    fn aliases(&self) -> &[String] {
        self.inner.aliases()
    }

    fn simplify(&self, args: Vec<Expr>, info: &SimplifyContext) -> Result<ExprSimplifyResult> {
        self.inner.simplify(args, info)
    }

    fn preimage(
        &self,
        args: &[Expr],
        lit_expr: &Expr,
        info: &SimplifyContext,
    ) -> Result<PreimageResult> {
        self.inner.preimage(args, lit_expr, info)
    }

    fn conditional_arguments<'a>(
        &self,
        args: &'a [Expr],
    ) -> Option<(Vec<&'a Expr>, Vec<&'a Expr>)> {
        self.inner.conditional_arguments(args)
    }

    fn short_circuits(&self) -> bool {
        self.inner.short_circuits()
    }

    fn evaluate_bounds(&self, input: &[&Interval]) -> Result<Interval> {
        self.inner.evaluate_bounds(input)
    }

    fn propagate_constraints(
        &self,
        interval: &Interval,
        inputs: &[&Interval],
    ) -> Result<Option<Vec<Interval>>> {
        self.inner.propagate_constraints(interval, inputs)
    }

    fn struct_field_mapping(
        &self,
        literal_args: &[Option<ScalarValue>],
    ) -> Option<StructFieldMapping> {
        self.inner.struct_field_mapping(literal_args)
    }

    fn output_ordering(&self, inputs: &[ExprProperties]) -> Result<SortProperties> {
        self.inner.output_ordering(inputs)
    }

    fn preserves_lex_ordering(&self, inputs: &[ExprProperties]) -> Result<bool> {
        self.inner.preserves_lex_ordering(inputs)
    }

    fn strictly_order_preserving(&self, inputs: &[ExprProperties]) -> Result<bool> {
        self.inner.strictly_order_preserving(inputs)
    }

    fn coerce_types(&self, arg_types: &[DataType]) -> Result<Vec<DataType>> {
        self.inner.coerce_types(arg_types)
    }

    fn documentation(&self) -> Option<&Documentation> {
        self.inner.documentation()
    }

    fn placement(&self, args: &[ExpressionPlacement]) -> ExpressionPlacement {
        self.inner.placement(args)
    }
}

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
            let lengths = length(array)?;
            let lengths = arrow::compute::cast(&lengths, &DataType::Int64)?;
            let longest = arrow::compute::max(lengths.as_primitive::<Int64Type>()).unwrap_or(0);
            Ok(usize::try_from(longest).unwrap_or(0))
        }
    }
}

/// Number of leaf values under a list argument, through nested lists.
/// A scalar list is broadcast to `rows` rows.
fn leaf_count(arg: &ColumnarValue, rows: usize) -> Result<usize> {
    let (mut array, broadcast): (ArrayRef, usize) = match arg {
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

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Arc;
    use std::time::Duration;

    use arrow::array::{Int64Array, ListBuilder, StringArray, StringBuilder};
    use datafusion::execution::runtime_env::{RuntimeEnv, RuntimeEnvBuilder};
    use serde_json::{Map, json};

    use super::*;
    use crate::query_error::RunError;
    use crate::sql::SqlRequest;
    use crate::test_support::{dataset_spans, example_query, spans_table, traces_table};
    use crate::views::QueryContext;

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

    async fn run(ctx: &QueryContext, query: &str) -> Result<crate::sql::SqlJson, RunError> {
        let params = Map::new();
        ctx.sql_json(SqlRequest {
            query,
            params: &params,
            limit: 100,
            timeout: Duration::from_secs(30),
        })
        .await
    }

    #[test]
    fn the_registry_holds_only_guarded_functions() {
        let ctx = QueryContext::new(example_query(), None).expect("context");
        let state = ctx.session().state();
        let functions = state.scalar_functions();
        for (name, udf) in functions {
            if UNGUARDED_FUNCTIONS.contains(&udf.name()) {
                assert!(!udf.inner().is::<GuardedScalar>(), "{name} is guarded");
            } else {
                assert!(udf.inner().is::<GuardedScalar>(), "{name} is not guarded");
            }
            assert!(
                !DENIED_FUNCTIONS.contains(&name.as_str()),
                "{name} is registered"
            );
        }
        for name in ["concat", "encode", "date_bin", "get_field"] {
            assert!(functions.contains_key(name), "{name} is missing");
        }
    }

    #[test]
    fn the_deny_list_names_every_alias() {
        let defaults = SessionStateDefaults::default_scalar_functions();
        let mut known = HashSet::new();
        for udf in &defaults {
            if DENIED_FUNCTIONS.contains(&udf.name()) {
                for alias in udf.aliases() {
                    assert!(
                        DENIED_FUNCTIONS.contains(&alias.as_str()),
                        "{} is an alias of {} but not denied",
                        alias,
                        udf.name()
                    );
                }
            }
            known.insert(udf.name().to_owned());
            known.extend(udf.aliases().iter().cloned());
        }
        for name in DENIED_FUNCTIONS.iter().chain(UNGUARDED_FUNCTIONS) {
            assert!(known.contains(*name), "{name} is not a DataFusion function");
        }
    }

    #[tokio::test]
    async fn a_call_over_the_pool_fails_and_releases_everything() {
        let (ctx, _) = big_input_context(1024 * 1024, 200_000);
        let error = run(&ctx, "SELECT length(upper(input)) AS n FROM spans")
            .await
            .expect_err("over the pool");
        assert!(matches!(error, RunError::TooLarge), "{error:?}");

        let (ctx, runtime) = big_input_context(256 * 1024 * 1024, 200_000);
        let json = run(&ctx, "SELECT length(upper(input)) AS n FROM spans")
            .await
            .expect("within the pool");
        assert_eq!(json.rows.len(), 14);
        for row in &json.rows {
            assert_eq!(row["n"], json!(200_000));
        }
        assert_eq!(runtime.memory_pool.reserved(), 0);
    }

    #[test]
    fn argument_bytes_counts_scalars_per_row() {
        let text = ScalarValue::Utf8(Some("abc".into()));
        assert_eq!(
            argument_bytes(&[ColumnarValue::Scalar(text.clone())], 10),
            text.size() * 10
        );

        let a: ArrayRef = Arc::new(Int64Array::from(vec![1, 2, 3]));
        assert_eq!(
            argument_bytes(
                &[
                    ColumnarValue::Array(Arc::clone(&a)),
                    ColumnarValue::Array(Arc::clone(&a)),
                ],
                3
            ),
            2 * a.get_array_memory_size()
        );
    }

    #[test]
    fn separators_count_once_per_place_written() {
        // concat_ws: separator "--" written between 3 arguments of 5 rows.
        let concat = [
            ColumnarValue::Scalar(ScalarValue::Utf8(Some("--".into()))),
            ColumnarValue::Array(Arc::new(StringArray::from(vec!["a"; 5]))),
            ColumnarValue::Array(Arc::new(StringArray::from(vec!["b"; 5]))),
            ColumnarValue::Array(Arc::new(StringArray::from(vec!["c"; 5]))),
        ];
        assert_eq!(
            call_bytes("concat_ws", &concat, 5).expect("bytes"),
            argument_bytes(&concat, 5) + 2 * 2 * 5
        );

        // array_to_string: 2 rows holding 3 and 4 strings (7 leaves).
        let mut builder = ListBuilder::new(StringBuilder::new());
        for row in [vec!["a", "b", "c"], vec!["d", "e", "f", "g"]] {
            for value in row {
                builder.values().append_value(value);
            }
            builder.append(true);
        }
        let list: ArrayRef = Arc::new(builder.finish());
        let delimiter = ColumnarValue::Scalar(ScalarValue::Utf8(Some(", ".into())));
        let plain = [ColumnarValue::Array(Arc::clone(&list)), delimiter.clone()];
        assert_eq!(
            call_bytes("array_to_string", &plain, 2).expect("bytes"),
            argument_bytes(&plain, 2) + 2 * 7
        );

        let with_null = [
            ColumnarValue::Array(Arc::clone(&list)),
            delimiter,
            ColumnarValue::Scalar(ScalarValue::Utf8(Some("NULL".into()))),
        ];
        assert_eq!(
            call_bytes("array_to_string", &with_null, 2).expect("bytes"),
            argument_bytes(&with_null, 2) + (2 + 4) * 7
        );

        // A delimiter column: the longest value counts, ";;;" (3 bytes).
        let per_row = [
            ColumnarValue::Array(Arc::clone(&list)),
            ColumnarValue::Array(Arc::new(StringArray::from(vec![",", ";;;"]))),
        ];
        assert_eq!(
            call_bytes("array_to_string", &per_row, 2).expect("bytes"),
            argument_bytes(&per_row, 2) + 3 * 7
        );

        // A nested list: 1 row, 2 inner lists of 2 strings each (4 leaves).
        let mut nested = ListBuilder::new(ListBuilder::new(StringBuilder::new()));
        for _ in 0..2 {
            for value in ["a", "b"] {
                nested.values().values().append_value(value);
            }
            nested.values().append(true);
        }
        nested.append(true);
        let nested: ArrayRef = Arc::new(nested.finish());
        assert_eq!(
            leaf_count(&ColumnarValue::Array(nested), 1).expect("leaves"),
            4
        );
    }

    #[tokio::test]
    async fn a_long_separator_hits_the_pool() {
        let (ctx, _) = big_input_context(1024 * 1024, 10);
        let names = vec!["name"; 100].join(", ");
        let separator = "x".repeat(20_000);
        let heavy = format!("SELECT length(concat_ws('{separator}', {names})) AS n FROM spans");
        let error = run(&ctx, &heavy)
            .await
            .expect_err("separator over the pool");
        assert!(matches!(error, RunError::TooLarge), "{error:?}");

        let light = run(
            &ctx,
            "SELECT length(concat_ws('-', name, name)) AS n FROM spans",
        )
        .await
        .expect("short separator");
        assert_eq!(light.rows.len(), 14);
    }
}
