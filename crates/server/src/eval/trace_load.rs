//! Loads one trace for an evaluator, within D-I-23's caps (spans, payload bytes, time and
//! concurrent loads), and refuses a trace that is itself an evaluation trace (D-I-17).

use std::collections::BTreeMap;
use std::fmt;

use arrow::record_batch::RecordBatch;
use llmobs_core::ids::{ProjectId, SpanId, TraceId};
use llmobs_core::names::environment::EVAL;
use llmobs_core::span::Span;
use llmobs_storage::error::QueryError;
use llmobs_storage::{QueryContext, QueryOptions, RunError, TraceSpansRequest, trace_spans};
use serde::Serialize;

use crate::api::convert::{self, ConvertError};
use crate::http::AppState;
use crate::routes::read::READ_TIMEOUT;

/// At most this many spans besides the required ones (D-I-23).
#[allow(dead_code)] // test runs use it from I2.7 on
pub(crate) const MAX_SPANS: usize = 5_000;
/// At most this many required spans (parentless ones and the target). More is `TooLarge`.
#[allow(dead_code)] // test runs use it from I2.7 on
pub(crate) const MAX_REQUIRED: usize = 1_000;
/// At most this many payload bytes in all (D-I-23).
#[allow(dead_code)] // test runs use it from I2.7 on
pub(crate) const MAX_PAYLOAD_BYTES: usize = 64 * 1024 * 1024;
/// Loads running at once (D-I-23).
#[allow(dead_code)] // test runs use it from I2.7 on
pub(crate) const MAX_LOADS: u32 = 4;

/// Permits for trace loads, shared by test runs (M2) and jobs (M3).
#[allow(dead_code)] // test runs use it from I2.7 on
#[derive(Debug)]
pub(crate) struct TraceLoads(tokio::sync::Semaphore);

impl TraceLoads {
    /// `MAX_LOADS` permits.
    #[allow(dead_code)] // test runs use it from I2.7 on
    pub(crate) fn new() -> Self {
        Self(tokio::sync::Semaphore::new(
            usize::try_from(MAX_LOADS).unwrap_or(4),
        ))
    }

    /// The permits, for `load_trace` and for tests that hold them.
    #[allow(dead_code)] // test runs use it from I2.7 on
    pub(crate) fn semaphore(&self) -> &tokio::sync::Semaphore {
        &self.0
    }
}

impl Default for TraceLoads {
    fn default() -> Self {
        Self::new()
    }
}

/// One loaded trace. `Span` holds prompts and answers, so `Debug` prints counts only.
#[allow(dead_code)] // test runs use it from I2.7 on
pub(crate) struct LoadedTrace {
    /// Deduplicated spans: required ones first, then the earliest others, each in start order.
    pub(crate) spans: Vec<Span>,
    /// Spans were left out (the span cap or the byte cap).
    pub(crate) truncated: bool,
}

impl fmt::Debug for LoadedTrace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoadedTrace")
            .field("spans", &self.spans.len())
            .field("truncated", &self.truncated)
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
#[allow(dead_code)] // test runs use it from I2.7 on
pub(crate) enum LoadError {
    #[error("trace not found")]
    NotFound,
    #[error("target span not found")]
    NoTargetSpan,
    #[error("trace too large")]
    TooLarge,
    #[error("target is an evaluation trace")]
    EvaluationTrace,
    #[error("storage is busy")]
    Busy,
    #[error("storage read failed")]
    Failed(#[source] RunError),
    #[error("could not read the spans")]
    Convert(#[source] ConvertError),
}

/// Loads `trace_id` of `project` for an evaluator, with D-I-23's caps and D-I-17's guard.
#[allow(dead_code)] // test runs use it from I2.7 on
pub(crate) async fn load_trace(
    state: &AppState,
    project: ProjectId,
    trace_id: TraceId,
    target_span: Option<SpanId>,
    around: Option<i64>,
) -> Result<LoadedTrace, LoadError> {
    let _permit = tokio::time::timeout(READ_TIMEOUT, state.trace_loads.semaphore().acquire())
        .await
        .map_err(|_| LoadError::Busy)?
        .map_err(|_| LoadError::Busy)?;
    let source = state
        .storage
        .query(project, QueryOptions { around })
        .await
        .map_err(|error| match error {
            QueryError::Busy => LoadError::Busy,
            QueryError::DataFusion(error) => LoadError::Failed(RunError::Internal(error)),
        })?;
    let ctx = QueryContext::new(source, None).map_err(LoadError::Failed)?;
    let trace = trace_id.to_hex();
    let target = target_span.map(|span| span.to_hex());
    let data = trace_spans(
        &ctx,
        TraceSpansRequest {
            trace_id: &trace,
            target_span_id: target.as_deref(),
            max_spans: MAX_SPANS,
            max_required: MAX_REQUIRED,
            timeout: READ_TIMEOUT,
        },
    )
    .await
    .map_err(|error| match error {
        RunError::Timeout => LoadError::Busy,
        RunError::TooLarge => LoadError::TooLarge,
        error => LoadError::Failed(error),
    })?;
    let required_rows: usize = data.required.iter().map(RecordBatch::num_rows).sum();
    if required_rows > MAX_REQUIRED {
        return Err(LoadError::TooLarge);
    }
    let mut stored = convert::spans(&data.required, project).map_err(LoadError::Convert)?;
    stored.extend(convert::spans(&data.earliest, project).map_err(LoadError::Convert)?);
    if stored.is_empty() {
        return Err(LoadError::NotFound);
    }
    let spans = dedupe(stored);
    if let Some(target) = target_span
        && !spans.iter().any(|span| span.span_id == target)
    {
        return Err(LoadError::NoTargetSpan);
    }
    if spans
        .iter()
        .any(|span| span.environment.as_deref() == Some(EVAL))
    {
        return Err(LoadError::EvaluationTrace);
    }
    let (spans, skipped) = cap(spans, target_span, MAX_PAYLOAD_BYTES)?;
    Ok(LoadedTrace {
        spans,
        truncated: skipped || data.truncated,
    })
}

/// One span per id: the copy with the greatest `ingest_time` (a retried export can store a
/// span twice). The result is ordered by span id.
#[allow(dead_code)] // test runs use it from I2.7 on
fn dedupe(spans: Vec<Span>) -> Vec<Span> {
    let mut latest: BTreeMap<SpanId, Span> = BTreeMap::new();
    for span in spans {
        let keep = latest
            .get(&span.span_id)
            .is_none_or(|kept| span.ingest_time > kept.ingest_time);
        if keep {
            latest.insert(span.span_id, span);
        }
    }
    latest.into_values().collect()
}

/// Orders the spans (target, then the other required ones, then the rest, each group by
/// `(start_time, span_id)`) and keeps them while their payload fits `max_bytes`. The second
/// value is true when a span was skipped.
///
/// The target comes first, so it is judged on its own size: it fails only when it alone is
/// over the cap. Any other span that does not fit is skipped, roots included.
///
/// # Errors
///
/// [`LoadError::TooLarge`] when the target alone is over `max_bytes`.
#[allow(dead_code)] // test runs use it from I2.7 on
fn cap(
    spans: Vec<Span>,
    target: Option<SpanId>,
    max_bytes: usize,
) -> Result<(Vec<Span>, bool), LoadError> {
    let mut target_span = None;
    let mut required = Vec::new();
    let mut others = Vec::new();
    for span in spans {
        if Some(span.span_id) == target {
            target_span = Some(span);
        } else if span.parent_span_id.is_none() {
            required.push(span);
        } else {
            others.push(span);
        }
    }
    let order = |a: &Span, b: &Span| (a.start_time, a.span_id).cmp(&(b.start_time, b.span_id));
    required.sort_by(order);
    others.sort_by(order);

    let mut kept = Vec::new();
    let mut total = 0_usize;
    let mut skipped = false;
    for span in target_span.into_iter().chain(required).chain(others) {
        let bytes = payload_bytes(&span);
        if total.saturating_add(bytes) > max_bytes {
            if Some(span.span_id) == target {
                return Err(LoadError::TooLarge);
            }
            skipped = true;
            continue;
        }
        total = total.saturating_add(bytes);
        kept.push(span);
    }
    Ok((kept, skipped))
}

/// The payload bytes of a span: its text columns, its attribute maps (keys and values) and
/// its events and links as JSON. Columns that are not payload (ids, times, tokens) do not count.
#[allow(dead_code)] // test runs use it from I2.7 on
fn payload_bytes(span: &Span) -> usize {
    fn text(value: Option<&String>) -> usize {
        value.map_or(0, String::len)
    }
    fn map(entries: &BTreeMap<String, String>) -> usize {
        entries
            .iter()
            .map(|(key, value)| key.len().saturating_add(value.len()))
            .fold(0, usize::saturating_add)
    }
    fn json<T: Serialize>(value: &T) -> usize {
        serde_json::to_string(value).map_or(0, |text| text.len())
    }
    [
        text(span.input.as_ref()),
        text(span.output.as_ref()),
        text(span.input_preview.as_ref()),
        text(span.output_preview.as_ref()),
        text(span.system_instructions.as_ref()),
        text(span.tool_definitions.as_ref()),
        text(span.request_params.as_ref()),
        map(&span.attributes),
        map(&span.metadata),
        map(&span.resource_attributes),
        json(&span.events),
        json(&span.links),
    ]
    .into_iter()
    .fold(0, usize::saturating_add)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use llmobs_storage::{TraceSpansRequest, trace_spans};

    use super::*;
    use crate::routes::read::open_query;
    use crate::test_support::{TestApp, dataset_span};

    const TRACE: &str = "0af7651916cd43dd8448eb211c80319c";
    const ROOT: &str = "1000000000000001";
    const EARLY: &str = "1000000000000002";
    const LATE: &str = "1000000000000003";
    const TARGET: &str = "1000000000000004";

    /// A span of `trace` with no payload, so a test's byte counts are its own.
    fn bare(trace: &str, span: &str, parent: Option<&str>, start: i64) -> Span {
        let mut row = dataset_span(0);
        row.trace_id = trace.parse::<TraceId>().expect("trace id");
        row.span_id = span.parse::<SpanId>().expect("span id");
        row.parent_span_id = parent.map(|id| id.parse::<SpanId>().expect("span id"));
        row.start_time = start;
        row.end_time = start.saturating_add(10);
        row.environment = None;
        row.input = None;
        row.output = None;
        row.input_preview = None;
        row.output_preview = None;
        row.system_instructions = None;
        row.tool_definitions = None;
        row.request_params = None;
        row.attributes.clear();
        row.resource_attributes.clear();
        row.metadata.clear();
        row.events.clear();
        row.links.clear();
        row
    }

    fn ids(spans: &[Span]) -> Vec<String> {
        spans.iter().map(|span| span.span_id.to_hex()).collect()
    }

    fn trace_id() -> TraceId {
        TRACE.parse::<TraceId>().expect("trace id")
    }

    fn span_id(id: &str) -> SpanId {
        id.parse::<SpanId>().expect("span id")
    }

    #[tokio::test]
    async fn loads_and_orders_the_trace() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let now = app.state.clock.now_ns();
        let root = bare(TRACE, ROOT, None, now - 30);
        let early = bare(TRACE, EARLY, Some(ROOT), now - 20);
        let late = bare(TRACE, LATE, Some(ROOT), now - 10);
        app.commit_spans(
            &seeded.project,
            vec![late.clone(), root.clone(), early.clone()],
        )
        .await;

        let loaded = load_trace(&app.state, seeded.project.id, trace_id(), None, None)
            .await
            .expect("loaded");
        assert_eq!(
            ids(&loaded.spans),
            ids(&[root, early, late]),
            "the root first, then the others in start order"
        );
        assert!(!loaded.truncated);
    }

    #[tokio::test]
    async fn converts_every_row() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let now = app.state.clock.now_ns();
        let mut root = bare(TRACE, ROOT, None, now - 30);
        root.name = "agent".to_owned();
        root.input = Some("question".to_owned());
        root.metadata = BTreeMap::from([("team".to_owned(), "search".to_owned())]);
        let early = bare(TRACE, EARLY, Some(ROOT), now - 20);
        let late = bare(TRACE, LATE, Some(ROOT), now - 10);
        app.commit_spans(
            &seeded.project,
            vec![root.clone(), early.clone(), late.clone()],
        )
        .await;

        let ctx = open_query(&app.state, seeded.project.id, None)
            .await
            .expect("query");
        let data = trace_spans(
            &ctx,
            TraceSpansRequest {
                trace_id: TRACE,
                target_span_id: None,
                max_spans: 10,
                max_required: 10,
                timeout: READ_TIMEOUT,
            },
        )
        .await
        .expect("trace spans");
        let read = convert::spans(&data.earliest, seeded.project.id).expect("converted");
        assert_eq!(read.len(), 3);
        assert_eq!(read[0].name, "agent");
        assert_eq!(read[0].start_time, root.start_time);
        assert_eq!(read[0].input, root.input);
        assert_eq!(read[0].metadata, root.metadata);
    }

    #[tokio::test]
    async fn dedupes_by_span_id_keeping_the_latest() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let now = app.state.clock.now_ns();
        let mut old = bare(TRACE, ROOT, None, now - 30);
        old.name = "old".to_owned();
        old.ingest_time = now - 5;
        let mut new = bare(TRACE, ROOT, None, now - 30);
        new.name = "new".to_owned();
        new.ingest_time = now - 1;
        app.commit_spans(&seeded.project, vec![old]).await;
        app.commit_spans(&seeded.project, vec![new]).await;

        let loaded = load_trace(&app.state, seeded.project.id, trace_id(), None, None)
            .await
            .expect("loaded");
        assert_eq!(loaded.spans.len(), 1);
        assert_eq!(loaded.spans[0].name, "new");
    }

    #[tokio::test]
    async fn unknown_trace_is_not_found() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let result = load_trace(&app.state, seeded.project.id, trace_id(), None, None).await;
        assert!(matches!(result, Err(LoadError::NotFound)));
    }

    #[tokio::test]
    async fn missing_target_span() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let now = app.state.clock.now_ns();
        app.commit_spans(&seeded.project, vec![bare(TRACE, ROOT, None, now - 30)])
            .await;

        let result = load_trace(
            &app.state,
            seeded.project.id,
            trace_id(),
            Some(span_id(TARGET)),
            None,
        )
        .await;
        assert!(matches!(result, Err(LoadError::NoTargetSpan)));
    }

    #[tokio::test]
    async fn an_evaluation_trace_is_refused() {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let now = app.state.clock.now_ns();
        let mut judge = bare(TRACE, ROOT, None, now - 30);
        judge.environment = Some(EVAL.to_owned());
        app.commit_spans(&seeded.project, vec![judge]).await;

        let result = load_trace(&app.state, seeded.project.id, trace_id(), None, None).await;
        assert!(matches!(result, Err(LoadError::EvaluationTrace)));
    }

    #[tokio::test]
    async fn loads_are_limited() {
        let app = TestApp::new().await;
        let max = usize::try_from(MAX_LOADS).expect("u32 fits usize");
        let held = app
            .state
            .trace_loads
            .semaphore()
            .try_acquire_many(MAX_LOADS)
            .expect("permits");
        assert_eq!(app.state.trace_loads.semaphore().available_permits(), 0);
        assert!(app.state.trace_loads.semaphore().try_acquire().is_err());
        drop(held);
        assert_eq!(app.state.trace_loads.semaphore().available_permits(), max);
    }

    #[test]
    fn cap_keeps_the_target_and_skips_others_over_the_cap() {
        let mut root = bare(TRACE, ROOT, None, 100);
        root.input = Some("x".repeat(100));
        let mut early = bare(TRACE, EARLY, Some(ROOT), 200);
        early.input = Some("x".repeat(1000));
        let mut late = bare(TRACE, LATE, Some(ROOT), 300);
        late.input = Some("x".repeat(1000));
        let mut target = bare(TRACE, TARGET, Some(ROOT), 400);
        target.input = Some("x".repeat(5));
        let spans = vec![root, early, late, target];

        let (kept, skipped) = cap(spans, Some(span_id(TARGET)), 200).expect("within the cap");
        assert_eq!(ids(&kept), vec![TARGET.to_owned(), ROOT.to_owned()]);
        assert!(skipped);
    }

    #[test]
    fn cap_skips_a_large_root_that_is_not_the_target() {
        let mut root = bare(TRACE, ROOT, None, 100);
        root.input = Some("x".repeat(1000));
        let mut target = bare(TRACE, TARGET, Some(ROOT), 200);
        target.input = Some("x".repeat(5));
        let mut early = bare(TRACE, EARLY, Some(ROOT), 300);
        early.input = Some("x".repeat(10));

        let (kept, skipped) =
            cap(vec![root, target, early], Some(span_id(TARGET)), 100).expect("within the cap");
        assert_eq!(ids(&kept), vec![TARGET.to_owned(), EARLY.to_owned()]);
        assert!(skipped);
    }

    #[test]
    fn cap_judges_the_target_on_its_own_size() {
        // The root (104 bytes) fits the 120-byte cap on its own, but would leave no room for the
        // target (54 bytes) if it were counted first. The target is judged first, so it is kept
        // and the root is skipped.
        let mut root = bare(TRACE, ROOT, None, 100);
        root.input = Some("x".repeat(100));
        let mut target = bare(TRACE, TARGET, Some(ROOT), 200);
        target.input = Some("x".repeat(50));

        let (kept, skipped) = cap(vec![root, target], Some(span_id(TARGET)), 120).expect("fits");
        assert_eq!(ids(&kept), vec![TARGET.to_owned()]);
        assert!(skipped);
    }

    #[test]
    fn cap_refuses_a_target_over_the_cap() {
        let mut target = bare(TRACE, TARGET, None, 100);
        target.input = Some("x".repeat(1000));
        let result = cap(vec![target], Some(span_id(TARGET)), 100);
        assert!(matches!(result, Err(LoadError::TooLarge)));
    }
}
