//! Sandboxed DataFusion session (D91).
//!
//! Left out of [`SessionStateBuilder`], and why:
//! - table functions (`generate_series`, `range`): callers must not invent rows;
//! - higher-order (lambda) functions: not part of the filter or SQL surface;
//! - file formats and table factories: no `read_parquet` or URL tables;
//! - `with_default_features()`: it registers the table functions above.

use std::num::NonZeroUsize;
use std::sync::Arc;

use datafusion::common::config::ConfigNonZeroUsize;
use datafusion::execution::runtime_env::RuntimeEnv;
use datafusion::execution::{SessionState, SessionStateBuilder, SessionStateDefaults};
use datafusion::prelude::SessionConfig;

use super::hex::HexIdRule;

/// Parser recursion limit. A left-deep operator chain is bounded separately (D95).
pub(crate) const PARSER_RECURSION_LIMIT: usize = 50;

/// Builds a session state with the built-in scalar, aggregate, window and nested
/// functions, and nothing that reads the filesystem or invents tables.
pub(crate) fn session_state(
    runtime: Arc<RuntimeEnv>,
    target_partitions: NonZeroUsize,
) -> SessionState {
    let mut config = SessionConfig::new()
        .with_target_partitions(target_partitions.get())
        .with_batch_size(8192)
        .with_information_schema(false);
    let options = config.options_mut();
    options.sql_parser.collect_spans = true;
    // `PARSER_RECURSION_LIMIT` is 50, so the non-zero constructor cannot fail.
    #[allow(clippy::expect_used)]
    {
        options.sql_parser.recursion_limit = ConfigNonZeroUsize::try_new(PARSER_RECURSION_LIMIT)
            .expect("parser recursion limit is non-zero");
    }
    options.execution.enable_recursive_ctes = false;

    SessionStateBuilder::new()
        .with_config(config)
        .with_runtime_env(runtime)
        .with_scalar_functions(SessionStateDefaults::default_scalar_functions())
        .with_aggregate_functions(SessionStateDefaults::default_aggregate_functions())
        .with_window_functions(SessionStateDefaults::default_window_functions())
        .with_expr_planners(SessionStateDefaults::default_expr_planners())
        .with_optimizer_rule(Arc::new(HexIdRule))
        .build()
}

#[cfg(test)]
mod tests {
    use crate::test_support::{example_query, rows};
    use crate::views::QueryContext;

    async fn context() -> QueryContext {
        QueryContext::new(example_query(), None).expect("example context")
    }

    async fn fails(sql: &str) -> String {
        let ctx = context().await;
        match ctx.session().sql(sql).await {
            Err(error) => error.to_string(),
            Ok(frame) => match frame.collect().await {
                Err(error) => error.to_string(),
                Ok(_) => panic!("expected `{sql}` to fail"),
            },
        }
    }

    #[tokio::test]
    async fn sandbox_rejects_tables_outside_the_views() {
        let series = fails("SELECT * FROM generate_series(1, 3)").await;
        assert!(
            series.to_ascii_lowercase().contains("not found"),
            "{series}"
        );
        let range = fails("SELECT * FROM range(3)").await;
        assert!(range.to_ascii_lowercase().contains("not found"), "{range}");
        let schema = fails("SELECT * FROM information_schema.tables").await;
        let schema_lower = schema.to_ascii_lowercase();
        assert!(
            schema_lower.contains("not found") || schema_lower.contains("information_schema"),
            "{schema}"
        );
        let show = fails("SHOW TABLES").await;
        assert!(!show.is_empty(), "{show}");
        let recursive = fails(
            "WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r WHERE n < 3) SELECT * FROM r",
        )
        .await;
        assert!(
            recursive.to_ascii_lowercase().contains("recursive"),
            "{recursive}"
        );
        let raw = fails("SELECT * FROM raw_spans").await;
        assert!(
            raw.to_ascii_lowercase().contains("not found") || raw.contains("raw_spans"),
            "{raw}"
        );
        let url = fails("SELECT * FROM '/etc/hostname'").await;
        assert!(!url.is_empty(), "{url}");
    }

    #[tokio::test]
    async fn registry_keeps_the_functions_users_need() {
        let ctx = context().await;
        let statements = [
            "SELECT regexp_like(name, 'a') FROM spans",
            "SELECT encode(name, 'hex') FROM spans",
            "SELECT date_bin(INTERVAL '1 minute', start_time, TIMESTAMP '1970-01-01T00:00:00Z') FROM spans",
            "SELECT date_trunc('hour', start_time) FROM spans",
            "SELECT array_has_all(tags, ['beta']) FROM spans",
            "SELECT approx_percentile_cont(duration_ns, 0.5) FROM spans",
            "SELECT first_value(name) OVER (ORDER BY start_time) FROM spans",
            "SELECT row_number() OVER (ORDER BY start_time) FROM spans",
            "SELECT metadata['k'] FROM spans",
            "SELECT ['a'] FROM spans",
        ];
        for sql in statements {
            rows(&ctx, sql).await;
        }
    }
}
