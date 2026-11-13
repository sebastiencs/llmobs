//! Dashboard metrics (spec 05 § Dashboard metrics, spec 09 § Observability data).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use axum::extract::{Extension, State};
use axum::http::Method;
use axum::routing::get;
use llmobs_core::ids::ProjectId;
use llmobs_storage as storage;

use crate::api::convert;
use crate::api::metrics::{MetricsBucket, MetricsGroupBy, MetricsParams, MetricsResponse};
use crate::auth::roles::ProjectPerm;
use crate::http::AppState;
use crate::http::access::{Access, ProjectAccess, RouteDef};
use crate::http::error::{self, ApiError};
use crate::routes::read::{READ_TIMEOUT, open_query};

/// How long a `GET /metrics` answer is kept, in nanoseconds (spec 05).
const TTL_NS: i64 = 10_000_000_000;

/// Most cached answers. A full cache first drops the expired ones, then clears.
const MAX_ENTRIES: usize = 256;

/// `GET /metrics` answers, kept for [`TTL_NS`] per project and parameters (spec 05 § Dashboard metrics).
#[derive(Debug, Default)]
pub(crate) struct MetricsCache {
    entries: Mutex<HashMap<MetricsKey, Cached>>,
}

/// Everything that changes the answer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct MetricsKey {
    project: ProjectId,
    from: i64,
    to: i64,
    bucket: MetricsBucket,
    filter: Option<String>,
    group_by: Option<MetricsGroupBy>,
    include_eval: bool,
    include_experiments: bool,
}

#[derive(Debug)]
struct Cached {
    expires_at: i64,
    response: Arc<MetricsResponse>,
}

impl MetricsCache {
    /// The cached answer for `key`, when it has not expired at `now`.
    fn get(&self, key: &MetricsKey, now: i64) -> Option<Arc<MetricsResponse>> {
        let entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries
            .get(key)
            .filter(|cached| now < cached.expires_at)
            .map(|cached| Arc::clone(&cached.response))
    }

    /// Stores `response` for `key`, expiring [`TTL_NS`] after `now`.
    fn put(&self, key: MetricsKey, response: MetricsResponse, now: i64) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if entries.len() >= MAX_ENTRIES {
            entries.retain(|_, cached| now < cached.expires_at);
            if entries.len() >= MAX_ENTRIES {
                entries.clear();
            }
        }
        entries.insert(
            key,
            Cached {
                expires_at: now.saturating_add(TTL_NS),
                response: Arc::new(response),
            },
        );
    }
}

/// Metrics read route. Documented and needs project view.
pub(crate) fn routes() -> Vec<RouteDef> {
    vec![RouteDef {
        method: Method::GET,
        path: "/api/v1/projects/{project}/metrics",
        access: Access::Project {
            perm: ProjectPerm::View,
            session_only: false,
        },
        documented: true,
        handler: get(get_metrics),
    }]
}

async fn get_metrics(
    State(state): State<AppState>,
    Extension(access): Extension<ProjectAccess>,
    error::Query(params): error::Query<MetricsParams>,
) -> Result<error::Json<MetricsResponse>, ApiError> {
    let from = params.from.ns();
    let to = params.to.ns();
    let key = MetricsKey {
        project: access.project.id,
        from,
        to,
        bucket: params.bucket,
        filter: params.filter.clone(),
        group_by: params.group_by,
        include_eval: params.include_eval,
        include_experiments: params.include_experiments,
    };
    let now = state.clock.now_ns();
    if let Some(cached) = state.metrics_cache.get(&key, now) {
        return Ok(error::Json((*cached).clone()));
    }
    let window = storage::TimeWindow::new(from, to)?;
    let ctx = open_query(&state, access.project.id, None).await?;
    let data = storage::metrics(
        &ctx,
        storage::MetricsRequest {
            window,
            bucket: bucket(params.bucket),
            filter: params.filter.as_deref(),
            group_by: params.group_by.map(storage_group_by),
            include_eval: params.include_eval,
            include_experiments: params.include_experiments,
            timeout: READ_TIMEOUT,
        },
    )
    .await?;
    let response = convert::metrics_response(data, from, to, params.group_by);
    state.metrics_cache.put(key, response.clone(), now);
    Ok(error::Json(response))
}

fn bucket(bucket: MetricsBucket) -> storage::Bucket {
    use storage::BucketWidth;
    let width = match bucket {
        MetricsBucket::Auto => return storage::Bucket::Auto,
        MetricsBucket::M1 => BucketWidth::M1,
        MetricsBucket::M5 => BucketWidth::M5,
        MetricsBucket::M10 => BucketWidth::M10,
        MetricsBucket::M15 => BucketWidth::M15,
        MetricsBucket::M30 => BucketWidth::M30,
        MetricsBucket::H1 => BucketWidth::H1,
        MetricsBucket::H6 => BucketWidth::H6,
        MetricsBucket::H12 => BucketWidth::H12,
        MetricsBucket::D1 => BucketWidth::D1,
    };
    storage::Bucket::Fixed(width)
}

fn storage_group_by(group_by: MetricsGroupBy) -> storage::MetricsGroupBy {
    match group_by {
        MetricsGroupBy::Model => storage::MetricsGroupBy::Model,
        MetricsGroupBy::Environment => storage::MetricsGroupBy::Environment,
        MetricsGroupBy::Release => storage::MetricsGroupBy::Release,
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;
    use std::time::Duration;

    use axum::http::{Method, StatusCode};
    use llmobs_core::batch::spans_to_record_batch;
    use llmobs_core::ids::TraceId;
    use llmobs_core::span::Span;
    use serde_json::Value as Json;

    use super::*;
    use crate::api::metrics::KpiMetric;
    use crate::test_support::{Seeded, TestApp};

    const WINDOW: &str = "from=2026-01-01T10:00:00Z&to=2026-01-01T11:00:00Z&bucket=10m";

    /// A seeded app with the example dataset loaded.
    async fn loaded() -> (TestApp, Seeded) {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        app.load_example_dataset(seeded.project.id).await;
        (app, seeded)
    }

    async fn metrics(app: &TestApp, seeded: &Seeded, query: &str) -> (StatusCode, Json) {
        let response = app
            .request(
                Method::GET,
                &format!("/api/v1/projects/{}/metrics?{query}", seeded.project.id),
            )
            .header("cookie", &seeded.cookie)
            .send()
            .await;
        let body = response.json::<Json>();
        (response.status, body)
    }

    fn kpi_values(response: &MetricsResponse) -> Vec<(KpiMetric, Option<f64>)> {
        response
            .kpis
            .iter()
            .map(|kpi| (kpi.metric, kpi.value))
            .collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn window_gives_kpis_and_buckets() {
        let (app, seeded) = loaded().await;
        let (status, body) = metrics(&app, &seeded, WINDOW).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let response: MetricsResponse = serde_json::from_value(body).unwrap();
        assert_eq!(response.bucket_ns, 600_000_000_000);
        assert!(response.group_by.is_none());

        let kpis = kpi_values(&response);
        assert_eq!(kpis[0], (KpiMetric::Traces, Some(5.0)));
        assert_eq!(
            kpis.iter().map(|(metric, _)| *metric).collect::<Vec<_>>(),
            [
                KpiMetric::Traces,
                KpiMetric::ErrorRate,
                KpiMetric::LatencyP95,
                KpiMetric::CostTotal,
                KpiMetric::TotalTokens,
            ]
        );
        let cost = kpis[3].1.unwrap();
        assert!((cost - 0.00955).abs() < 1e-12, "{cost}");
        assert!(response.kpis.iter().all(|kpi| kpi.previous.is_none()));
        assert!(
            response
                .series
                .iter()
                .all(|series| series.points.len() == 6)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn missing_or_empty_range_is_bad_request() {
        let (app, seeded) = loaded().await;
        let (status, body) = metrics(&app, &seeded, "to=2026-01-01T11:00:00Z").await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["code"], "bad_request");

        let (status, body) = metrics(
            &app,
            &seeded,
            "from=2026-01-01T10:00:00Z&to=2026-01-01T10:00:00Z",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(body["code"], "bad_request");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn answers_are_cached_for_ten_seconds() {
        let (app, seeded) = loaded().await;
        let (status, body) = metrics(&app, &seeded, WINDOW).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let first: MetricsResponse = serde_json::from_value(body).unwrap();
        assert_eq!(kpi_values(&first)[0], (KpiMetric::Traces, Some(5.0)));

        // One more trace inside the window: a copy of span `…061` under a new root.
        let dataset = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/api/dataset");
        let text = std::fs::read_to_string(format!("{dataset}/spans.json")).unwrap();
        let spans: Vec<Span> = serde_json::from_str(&text).unwrap();
        let mut span = spans
            .into_iter()
            .find(|span| span.span_id.to_string() == "b100000000000061")
            .unwrap();
        span.project_id = seeded.project.id;
        span.trace_id = TraceId::from_str("a1000000000000000000000000000007").unwrap();
        span.parent_span_id = None;
        span.start_time = 1_767_261_600_000_000_000 + 40 * 60_000_000_000;
        span.end_time = span.start_time + 2_000_000_000;
        let batch = spans_to_record_batch(&[span]).unwrap();
        app.state
            .storage
            .append_spans(seeded.project.id, batch)
            .await
            .unwrap();

        let (_, body) = metrics(&app, &seeded, WINDOW).await;
        let cached: MetricsResponse = serde_json::from_value(body).unwrap();
        assert_eq!(kpi_values(&cached)[0], (KpiMetric::Traces, Some(5.0)));

        app.state.clock.advance(Duration::from_secs(11));
        let (_, body) = metrics(&app, &seeded, WINDOW).await;
        let fresh: MetricsResponse = serde_json::from_value(body).unwrap();
        assert_eq!(kpi_values(&fresh)[0], (KpiMetric::Traces, Some(6.0)));
    }
}
