//! Dataset loader and the reference aggregations the example responses are built from.
//!
//! Storage will implement the real aggregations later. These stay in the test suite so the
//! example files can be compared with that implementation.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::PathBuf;

use llmobs_core::enums::SpanKind;
use llmobs_core::ids::TraceId;
use llmobs_core::names::environment;
use llmobs_core::names::metadata_key;
use llmobs_core::score::{Score, ScoreTarget};
use llmobs_core::span::Span;
use llmobs_server::api::sessions::SessionSummary;
use llmobs_server::api::traces::{ScoreBrief, TraceSummary};
use llmobs_server::api::users::EndUserSummary;
use serde::Serialize;
use serde::de::DeserializeOwned;

pub(crate) fn api_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/api")
}

pub(crate) fn canonical_json<T: Serialize + DeserializeOwned>(value: &T) -> String {
    // One parse-and-print pass. serde_json's shortest float form is not always a fixed
    // point of its parser, and the example files must round-trip through the typed value.
    let once = serde_json::to_string_pretty(value).unwrap_or_else(|error| panic!("{error}"));
    let parsed: T = serde_json::from_str(&once).unwrap_or_else(|error| panic!("{error}"));
    let twice = serde_json::to_string_pretty(&parsed).unwrap_or_else(|error| panic!("{error}"));
    format!("{twice}\n")
}

pub(crate) fn read_json<T: DeserializeOwned>(path: &std::path::Path) -> T {
    let text =
        fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

pub(crate) fn dataset_spans() -> Vec<Span> {
    read_json(&api_dir().join("dataset/spans.json"))
}

pub(crate) fn dataset_scores() -> Vec<Score> {
    read_json(&api_dir().join("dataset/scores.json"))
}

/// One complete trace. `spans` must all belong to it and must not be empty.
pub(crate) fn trace_summary(spans: &[Span], scores: &[Score]) -> TraceSummary {
    let ordered = by_start(spans);
    let Some(earliest) = ordered.first().copied() else {
        panic!("trace_summary requires at least one span");
    };
    let root = ordered
        .iter()
        .copied()
        .filter(|span| span.parent_span_id.is_none())
        .min_by_key(|span| (span.start_time, span.span_id));
    let trace_id = earliest.trace_id;
    let start_time = ordered
        .iter()
        .map(|span| span.start_time)
        .min()
        .unwrap_or(0);
    let end_time = ordered
        .iter()
        .map(|span| span.end_time)
        .max()
        .unwrap_or(start_time);
    let status = if ordered
        .iter()
        .any(|span| span.status == llmobs_core::enums::SpanStatus::Error)
    {
        llmobs_core::enums::SpanStatus::Error
    } else {
        root.map(|span| span.status).unwrap_or_default()
    };
    let mut tags = BTreeSet::new();
    for span in &ordered {
        tags.extend(span.tags.iter().cloned());
    }
    let mut metadata = root.map(|span| span.metadata.clone()).unwrap_or_default();
    for span in &ordered {
        if root.is_some_and(|root| root.span_id == span.span_id) {
            continue;
        }
        for (key, value) in &span.metadata {
            metadata.entry(key.clone()).or_insert_with(|| value.clone());
        }
    }
    let mut models = BTreeSet::new();
    for span in &ordered {
        if let Some(model) = span.response_model.clone().or(span.request_model.clone()) {
            models.insert(model);
        }
    }
    TraceSummary {
        trace_id,
        root_span_id: root.map(|span| span.span_id),
        has_root: root.is_some(),
        name: root
            .map(|span| span.name.clone())
            .unwrap_or_else(|| earliest.name.clone()),
        start_time,
        end_time,
        duration_ns: end_time.saturating_sub(start_time),
        status,
        session_id: first_set(root, &ordered, |span| &span.session_id),
        user_id: first_set(root, &ordered, |span| &span.user_id),
        environment: first_set(root, &ordered, |span| &span.environment),
        release: first_set(root, &ordered, |span| &span.release),
        tags: tags.into_iter().collect(),
        metadata,
        input_preview: preview(root, &ordered, |span| &span.input_preview),
        output_preview: preview(root, &ordered, |span| &span.output_preview),
        span_count: i64::try_from(ordered.len()).unwrap_or(i64::MAX),
        error_count: i64::try_from(
            ordered
                .iter()
                .filter(|span| span.status == llmobs_core::enums::SpanStatus::Error)
                .count(),
        )
        .unwrap_or(i64::MAX),
        llm_count: i64::try_from(
            ordered
                .iter()
                .filter(|span| span.span_kind == SpanKind::Llm)
                .count(),
        )
        .unwrap_or(i64::MAX),
        input_tokens: sum_i64(ordered.iter().map(|span| span.input_tokens)),
        output_tokens: sum_i64(ordered.iter().map(|span| span.output_tokens)),
        cache_read_tokens: sum_i64(ordered.iter().map(|span| span.cache_read_tokens)),
        cache_write_tokens: sum_i64(ordered.iter().map(|span| span.cache_write_tokens)),
        reasoning_tokens: sum_i64(ordered.iter().map(|span| span.reasoning_tokens)),
        total_tokens: sum_i64(ordered.iter().map(|span| span.total_tokens)),
        cost_total: sum_f64(ordered.iter().map(|span| span.cost_total)),
        models: models.into_iter().collect(),
        scores: trace_scores(trace_id, scores),
    }
}

pub(crate) fn is_default_visible(trace: &TraceSummary) -> bool {
    trace.environment.as_deref() != Some(environment::EVAL)
        && !trace.metadata.contains_key(metadata_key::EXPERIMENT_ID)
}

pub(crate) fn session_summary(
    session_id: &str,
    traces: &[TraceSummary],
    scores: &[Score],
) -> SessionSummary {
    let mut ordered = traces.to_vec();
    ordered.sort_by_key(|trace| (trace.start_time, trace.trace_id));
    let start_time = ordered
        .iter()
        .map(|trace| trace.start_time)
        .min()
        .unwrap_or(0);
    let end_time = ordered
        .iter()
        .map(|trace| trace.end_time)
        .max()
        .unwrap_or(start_time);
    SessionSummary {
        session_id: session_id.to_owned(),
        start_time,
        end_time,
        duration_ns: end_time.saturating_sub(start_time),
        trace_count: i64::try_from(ordered.len()).unwrap_or(i64::MAX),
        error_count: ordered.iter().map(|trace| trace.error_count).sum(),
        total_tokens: sum_i64(ordered.iter().map(|trace| trace.total_tokens)),
        cost_total: sum_f64(ordered.iter().map(|trace| trace.cost_total)),
        user_id: ordered.iter().find_map(|trace| trace.user_id.clone()),
        scores: session_scores(session_id, scores),
    }
}

pub(crate) fn user_summary(user_id: &str, traces: &[TraceSummary]) -> EndUserSummary {
    let mut sessions = BTreeSet::new();
    for trace in traces {
        if let Some(session_id) = &trace.session_id {
            sessions.insert(session_id.clone());
        }
    }
    let first_seen = traces
        .iter()
        .map(|trace| trace.start_time)
        .min()
        .unwrap_or(0);
    let last_seen = traces
        .iter()
        .map(|trace| trace.end_time)
        .max()
        .unwrap_or(first_seen);
    EndUserSummary {
        user_id: user_id.to_owned(),
        first_seen,
        last_seen,
        trace_count: i64::try_from(traces.len()).unwrap_or(i64::MAX),
        session_count: i64::try_from(sessions.len()).unwrap_or(i64::MAX),
        error_count: traces.iter().map(|trace| trace.error_count).sum(),
        total_tokens: sum_i64(traces.iter().map(|trace| trace.total_tokens)),
        cost_total: sum_f64(traces.iter().map(|trace| trace.cost_total)),
    }
}

fn by_start(spans: &[Span]) -> Vec<&Span> {
    let mut ordered: Vec<&Span> = spans.iter().collect();
    ordered.sort_by_key(|span| (span.start_time, span.span_id));
    ordered
}

fn first_set(
    root: Option<&Span>,
    ordered: &[&Span],
    field: impl Fn(&Span) -> &Option<String>,
) -> Option<String> {
    if let Some(value) = root.and_then(|span| field(span).clone()) {
        return Some(value);
    }
    ordered.iter().find_map(|span| field(span).clone())
}

fn preview(
    root: Option<&Span>,
    ordered: &[&Span],
    field: impl Fn(&Span) -> &Option<String>,
) -> Option<String> {
    if let Some(value) = root.and_then(|span| field(span).clone()) {
        return Some(value);
    }
    ordered
        .iter()
        .filter(|span| span.span_kind == SpanKind::Llm)
        .find_map(|span| field(span).clone())
}

fn trace_scores(trace_id: TraceId, scores: &[Score]) -> Vec<ScoreBrief> {
    briefs(scores.iter().filter(move |score| {
        matches!(&score.target, ScoreTarget::Trace { trace_id: id } if *id == trace_id)
    }))
}

fn session_scores(session_id: &str, scores: &[Score]) -> Vec<ScoreBrief> {
    briefs(scores.iter().filter(move |score| {
        matches!(&score.target, ScoreTarget::Session { session_id: id } if id == session_id)
    }))
}

fn briefs<'a>(scores: impl Iterator<Item = &'a Score>) -> Vec<ScoreBrief> {
    let mut best: BTreeMap<String, &Score> = BTreeMap::new();
    for score in scores {
        if score.deleted {
            continue;
        }
        match best.get(&score.name) {
            Some(current)
                if (current.updated_at, current.score_id) >= (score.updated_at, score.score_id) => {
            }
            _ => {
                best.insert(score.name.clone(), score);
            }
        }
    }
    best.into_iter()
        .map(|(name, score)| ScoreBrief {
            score_id: score.score_id,
            name,
            value: score.value.clone(),
            source: score.source,
        })
        .collect()
}

fn sum_i64(values: impl Iterator<Item = Option<i64>>) -> Option<i64> {
    let mut total = None;
    for value in values.flatten() {
        *total.get_or_insert(0) += value;
    }
    total
}

fn sum_f64(values: impl Iterator<Item = Option<f64>>) -> Option<f64> {
    let mut total = None;
    for value in values.flatten() {
        *total.get_or_insert(0.0) += value;
    }
    total
}
