//! Scalar functions that reserve the bytes they read in the query memory pool before each call (D-A2-2).

use std::sync::Arc;

use std::collections::HashMap;

use arrow::array::{Array, ArrayRef, AsArray, RecordBatch};
use arrow::compute::kernels::length::length;
use arrow::datatypes::{DataType, Field, FieldRef, Int64Type, Schema};
use datafusion::common::config::ConfigOptions;
use datafusion::common::tree_node::{Transformed, TreeNode};
use datafusion::common::{
    DFSchema, ExprSchema, Result, ScalarValue, exec_err, internal_err, plan_err,
};
use datafusion::error::DataFusionError;
use datafusion::execution::SessionStateDefaults;
use datafusion::execution::memory_pool::{MemoryConsumer, MemoryPool};
use datafusion::logical_expr::expr::ScalarFunction;
use datafusion::logical_expr::expr_rewriter::NamePreserver;
use datafusion::logical_expr::interval_arithmetic::Interval;
use datafusion::logical_expr::preimage::PreimageResult;
use datafusion::logical_expr::ptr_eq::PtrEq;
use datafusion::logical_expr::simplify::{ExprSimplifyResult, SimplifyContext};
use datafusion::logical_expr::sort_properties::{ExprProperties, SortProperties};
use datafusion::logical_expr::udf_eq::UdfEq;
use datafusion::logical_expr::{
    ColumnarValue, Documentation, Expr, ExprSchemable, ExpressionPlacement, LogicalPlan, Operator,
    ReturnFieldArgs, ScalarFunctionArgs, ScalarUDF, ScalarUDFImpl, Signature, StructFieldMapping,
    Volatility,
};
use datafusion::optimizer::{ApplyOrder, OptimizerConfig, OptimizerRule};
use datafusion::physical_expr::PhysicalExpr;
use datafusion::physical_expr::expressions::{BinaryExpr as PhysicalBinary, Column};

use super::hex::expression_schema;

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

impl ScalarUDFImpl for GuardedStringConcat {
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
        Ok(Arc::new(Field::new(
            self.name(),
            self.return_type.clone(),
            nullable,
        )))
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
}

/// `left || right` with DataFusion's `BinaryExpr` kernel. All-scalar arguments
/// give a scalar, as the operator would.
fn concat_strings(args: &[ColumnarValue], rows: usize) -> Result<ColumnarValue> {
    let [left, right] = args else {
        return internal_err!("guarded_string_concat takes two arguments");
    };
    let scalar =
        matches!(left, ColumnarValue::Scalar(_)) && matches!(right, ColumnarValue::Scalar(_));
    let rows = if scalar { 1 } else { rows };
    let left = left.to_array(rows)?;
    let right = right.to_array(rows)?;
    let schema = Arc::new(Schema::new(vec![
        Field::new("left", left.data_type().clone(), true),
        Field::new("right", right.data_type().clone(), true),
    ]));
    let batch = RecordBatch::try_new(schema, vec![left, right])?;
    let expr = PhysicalBinary::new(
        Arc::new(Column::new("left", 0)),
        Operator::StringConcat,
        Arc::new(Column::new("right", 1)),
    );
    let value = expr.evaluate(&batch)?;
    if scalar {
        let array = value.into_array(1)?;
        return Ok(ColumnarValue::Scalar(ScalarValue::try_from_array(
            &array, 0,
        )?));
    }
    Ok(value)
}

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
    /// `functions` is [`guarded_functions`]; the unguarded ones in it are skipped.
    pub(crate) fn new(functions: &[Arc<ScalarUDF>], pool: Arc<dyn MemoryPool>) -> Self {
        let mut by_name = HashMap::new();
        for udf in functions
            .iter()
            .filter(|udf| udf.inner().is::<GuardedScalar>())
        {
            by_name.insert(udf.name().to_owned(), Arc::clone(udf));
            for alias in udf.aliases() {
                by_name.insert(alias.clone(), Arc::clone(udf));
            }
        }
        Self {
            functions: by_name,
            pool,
        }
    }

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

    fn guard_one(
        &self,
        expr: Expr,
        schema: &DFSchema,
    ) -> Result<Transformed<Expr>, DataFusionError> {
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
                    return Ok(Transformed::yes(Expr::ScalarFunction(
                        ScalarFunction::new_udf(Arc::clone(guarded), call.args),
                    )));
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
                Ok(Transformed::yes(Expr::ScalarFunction(
                    ScalarFunction::new_udf(Arc::new(udf), vec![*binary.left, *binary.right]),
                )))
            }
            other => Ok(Transformed::no(other)),
        }
    }
}

impl OptimizerRule for GuardRule {
    fn name(&self) -> &str {
        "guard_scalar_functions"
    }

    fn apply_order(&self) -> Option<ApplyOrder> {
        Some(ApplyOrder::BottomUp)
    }

    #[allow(deprecated)]
    fn supports_rewrite(&self) -> bool {
        true
    }

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

    use datafusion::common::tree_node::{TreeNode, TreeNodeRecursion};

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

    /// Every scalar function call and `||` in `plan` and its subqueries that the
    /// guard does not cover, as SQL text.
    fn unguarded_expressions(plan: &LogicalPlan) -> Vec<String> {
        let mut found = Vec::new();
        plan.apply_with_subqueries(|node| {
            node.apply_expressions(|expr| {
                expr.apply(|e| {
                    match e {
                        Expr::ScalarFunction(call) => {
                            let inner = call.func.inner();
                            let guarded =
                                inner.is::<GuardedScalar>() || inner.is::<GuardedStringConcat>();
                            if !guarded && !UNGUARDED_FUNCTIONS.contains(&call.func.name()) {
                                found.push(e.to_string());
                            }
                        }
                        Expr::BinaryExpr(binary) if binary.op == Operator::StringConcat => {
                            found.push(e.to_string());
                        }
                        _ => {}
                    }
                    Ok(TreeNodeRecursion::Continue)
                })
            })
        })
        .expect("walk the plan");
        found
    }

    #[tokio::test]
    async fn every_call_in_an_optimized_plan_is_guarded() {
        let ctx = QueryContext::new(example_query(), None).expect("context");
        let queries = [
            "SELECT tags || tags AS a, name || 'x' AS b, metadata['k'] AS c, tags[1] AS d, upper(name) AS e FROM spans WHERE name || '!' <> ''",
            "SELECT span_id FROM spans WHERE trace_id IN (SELECT trace_id FROM traces WHERE name || 'x' <> '')",
            "WITH a AS (SELECT name || name AS x FROM spans) SELECT x || x AS y FROM a",
            "SELECT date_bin(INTERVAL '1 hour', start_time, TIMESTAMP '1970-01-01T00:00:00Z') AS b, count(*) AS n FROM spans GROUP BY 1",
            "SELECT name || 'x' AS a, count(*) AS n FROM spans GROUP BY ROLLUP(name || 'x')",
        ];
        for query in queries {
            let frame = ctx.session().sql(query).await.expect(query);
            let plan = ctx
                .session()
                .state()
                .optimize(frame.logical_plan())
                .expect("optimize");
            let found = unguarded_expressions(&plan);
            assert!(found.is_empty(), "{query}: unguarded {found:?}");
        }
    }

    #[tokio::test]
    async fn string_concatenation_keeps_its_semantics() {
        let ctx = QueryContext::new(example_query(), None).expect("context");
        let json = run(
            &ctx,
            "SELECT name, name || '!' AS a, name || CASE WHEN name = 'no such name' THEN 'x' END AS b FROM spans ORDER BY span_id LIMIT 3",
        )
        .await
        .expect("concatenation");
        assert_eq!(json.rows.len(), 3);
        for row in &json.rows {
            let name = row["name"].as_str().expect("name is text");
            assert_eq!(row["a"], json!(format!("{name}!")));
            assert_eq!(row["b"], json!(null));
        }

        let rolled = run(
            &ctx,
            "SELECT name || 'x' AS a, count(*) AS n FROM spans GROUP BY ROLLUP(name || 'x') ORDER BY a NULLS FIRST",
        )
        .await
        .expect("rollup over a concatenation");
        let total = rolled
            .rows
            .iter()
            .find(|row| row["a"].is_null())
            .expect("grand total row");
        assert_eq!(total["n"], json!(14));
    }

    #[tokio::test]
    async fn the_rule_keeps_the_plan_schema() {
        let ctx = QueryContext::new(example_query(), None).expect("context");
        let queries = [
            "SELECT name || 'x' FROM spans",
            "SELECT name || 'x' AS a, count(*) AS n FROM spans GROUP BY name || 'x'",
            "SELECT name || 'x' AS a, count(*) AS n FROM spans GROUP BY ROLLUP(name || 'x')",
        ];
        let fields = |plan: &LogicalPlan| -> Vec<(String, DataType, bool)> {
            plan.schema()
                .fields()
                .iter()
                .map(|field| {
                    (
                        field.name().clone(),
                        field.data_type().clone(),
                        field.is_nullable(),
                    )
                })
                .collect()
        };
        for query in queries {
            let frame = ctx.session().sql(query).await.expect(query);
            let unoptimized = frame.logical_plan().clone();
            let optimized = ctx
                .session()
                .state()
                .optimize(&unoptimized)
                .expect("optimize");
            assert_eq!(fields(&optimized), fields(&unoptimized), "{query}");
        }
    }

    #[tokio::test]
    async fn cte_doubling_hits_the_memory_limit() {
        let (ctx, runtime) = big_input_context(64 * 1024 * 1024, 100_000);
        let doubling = |levels: usize| {
            let mut sql = String::from("WITH a0 AS (SELECT input AS x FROM spans)");
            for level in 1..=levels {
                sql += &format!(", a{level} AS (SELECT x || x AS x FROM a{})", level - 1);
            }
            sql += &format!(" SELECT length(x) AS n FROM a{levels}");
            sql
        };

        let error = run(&ctx, &doubling(12))
            .await
            .expect_err("doubling over the pool");
        assert!(matches!(error, RunError::TooLarge), "{error:?}");
        assert_eq!(runtime.memory_pool.reserved(), 0);

        let json = run(&ctx, &doubling(3)).await.expect("three levels fit");
        assert_eq!(json.rows.len(), 14);
        for row in &json.rows {
            assert_eq!(row["n"], json!(800_000));
        }
        assert_eq!(runtime.memory_pool.reserved(), 0);
    }
}
