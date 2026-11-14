//! MCP tools over traces and spans (D-H-9, D-H-10, D-H-11).

use llmobs_core::ids::ProjectId;
use llmobs_storage as storage;
use rmcp::model::JsonObject;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use uuid::Uuid;

use super::output::{self, char_len, render};
use super::sql::required_window;
use super::tools::arguments;
use crate::api::common::TimeParam;
use crate::api::convert;
use crate::api::saved_views::SavedViewKind;
use crate::api::traces::{SpanDetail, build_tree};
use crate::auth::principal::AuthUser;
use crate::auth::roles::ProjectPerm;
use crate::http::AppState;
use crate::http::access::user_project;
use crate::http::error::ApiError;
use crate::meta::saved_views::{self, Viewer};
use crate::routes::read::{READ_TIMEOUT, attach_trace_scores, open_query};

pub(crate) const SEARCH_TRACES: &str = "search_traces";
pub(crate) const GET_TRACE: &str = "get_trace";
pub(crate) const GET_SPAN: &str = "get_span";

/// Default and largest `limit` of `search_traces` (README § Design → MCP server).
const DEFAULT_TRACES: i64 = 20;
const MAX_TRACES: usize = 100;
/// Metadata, attribute and preview values longer than this are cut (D-H-11).
const VALUE_CUT: usize = 200;
/// The note added when [`cut_values`] cut something.
const CUT_NOTE: &str = "metadata, attribute and preview values longer than 200 characters were cut to 200 characters plus …";
/// The two preview fields of trace and span summaries.
const PREVIEWS: [&str; 2] = ["input_preview", "output_preview"];
/// The fields of a span detail that can be large. They are cut, all to the same length, only
/// when the span does not fit `max_chars`.
const HEAVY: [&str; 7] = [
    "input",
    "output",
    "system_instructions",
    "tool_definitions",
    "request_params",
    "events",
    "links",
];

/// Serializes an API type. It cannot fail for these types, but it is not unwrapped.
fn to_json<T: serde::Serialize>(value: &T) -> Result<Value, ApiError> {
    serde_json::to_value(value).map_err(|error| ApiError::Internal(Box::new(error)))
}

/// A `limit` or similar count, as given or `default`, inside `1..=max`.
///
/// # Errors
///
/// `400 bad_request` with `message` otherwise (negative numbers included).
fn bounded(given: Option<i64>, default: i64, max: usize, message: &str) -> Result<usize, ApiError> {
    match usize::try_from(given.unwrap_or(default)) {
        Ok(value) if (1..=max).contains(&value) => Ok(value),
        _ => Err(ApiError::BadRequest {
            detail: Some(message.to_owned()),
            errors: Vec::new(),
        }),
    }
}

/// Cuts to `VALUE_CUT` characters (with `…`, see `output::cut`) every string value of the
/// object fields `maps` of `object`, and the string fields `strings` of `object`. Fields that
/// are absent, `null` or of another type are left alone. Returns whether anything was cut.
fn cut_values(object: &mut Map<String, Value>, maps: &[&str], strings: &[&str]) -> bool {
    let mut cut = false;
    for name in maps {
        if let Some(Value::Object(inner)) = object.get_mut(*name) {
            for value in inner.values_mut() {
                if let Value::String(text) = value
                    && let Some(short) = output::cut(text, VALUE_CUT)
                {
                    *text = short;
                    cut = true;
                }
            }
        }
    }
    for name in strings {
        if let Some(Value::String(text)) = object.get_mut(*name)
            && let Some(short) = output::cut(text, VALUE_CUT)
        {
            *text = short;
            cut = true;
        }
    }
    cut
}

/// A `TraceSummary` or `SpanSummary` as JSON, with `metadata` values and the previews cut, or
/// the previews removed when `previews` is false. Returns the value and whether it cut.
fn compact_summary(value: Value, previews: bool) -> (Value, bool) {
    let Value::Object(mut object) = value else {
        return (value, false);
    };
    if !previews {
        for key in PREVIEWS {
            object.remove(key);
        }
    }
    let cut = cut_values(&mut object, &["metadata"], &PREVIEWS);
    (Value::Object(object), cut)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchTracesArguments {
    project: String,
    from: Option<TimeParam>,
    to: Option<TimeParam>,
    view: Option<String>,
    filter: Option<String>,
    search: Option<String>,
    limit: Option<i64>,
    include_eval: Option<bool>,
    include_experiments: Option<bool>,
    max_chars: Option<i64>,
}

/// Traces of the project (`ProjectPerm::View`) whose `start_time` is in the required window,
/// newest first, with the default exclusions unless the arguments or the view lift them.
///
/// Answers `{"traces": [TraceSummary, compacted], "truncated", "notes"}`.
pub(crate) async fn search_traces(
    state: &AppState,
    user: &AuthUser,
    raw: JsonObject,
) -> Result<Value, ApiError> {
    let args: SearchTracesArguments = arguments(raw)?;
    let window = required_window(args.from, args.to)?;
    let limit = bounded(
        args.limit,
        DEFAULT_TRACES,
        MAX_TRACES,
        "limit must be between 1 and 100",
    )?;
    let max_chars = output::max_chars(args.max_chars)?;
    let access = user_project(state, user, &args.project, ProjectPerm::View).await?;
    let view = match args.view.as_deref() {
        Some(key) => view_state(state, access.project.id, user, key).await?,
        None => Map::new(),
    };

    // Explicit arguments first; the view fills what is left (`filter`, `search`, `eval` and
    // `exp` are list-page URL params a saved view stores, spec 11). The view's `range`, `from`,
    // `to`, `sort`, `order` and `cols` are ignored: the window comes from the arguments and
    // results are always `start_time` descending.
    let filter = args.filter.or_else(|| view_string(&view, "filter"));
    let search = args.search.or_else(|| view_string(&view, "search"));
    let include_eval = args
        .include_eval
        .unwrap_or_else(|| view_flag(&view, "eval"));
    let include_experiments = args
        .include_experiments
        .unwrap_or_else(|| view_flag(&view, "exp"));

    // The window goes into `ListScope`, which filters the list; the context itself is opened
    // without a window, as for every read endpoint (D101).
    let ctx = open_query(state, access.project.id, None).await?;
    let page = storage::trace_list(
        &ctx,
        storage::TraceListRequest {
            filter: filter.as_deref(),
            search: search.as_deref(),
            scope: storage::ListScope {
                window: Some(window),
                include_eval,
                include_experiments,
            },
            sort: storage::TraceSort::StartTime,
            order: storage::SortOrder::Desc,
            limit,
            cursor: None,
            timeout: READ_TIMEOUT,
        },
    )
    .await?;
    let mut traces = convert::trace_summaries(&page.batches)?;
    attach_trace_scores(&ctx, &mut traces).await?;

    let more = page.next_cursor.is_some();
    let mut cut = false;
    let mut items = Vec::with_capacity(traces.len());
    for summary in &traces {
        let (value, item_cut) = compact_summary(to_json(summary)?, true);
        cut |= item_cut;
        items.push(value);
    }

    let mut notes = Vec::new();
    if more {
        notes.push(if limit < MAX_TRACES {
            "more traces match; narrow the window or the filter, or raise limit (at most 100)"
                .to_owned()
        } else {
            "more traces match; narrow the window or the filter".to_owned()
        });
    }
    if cut {
        notes.push(CUT_NOTE.to_owned());
    }

    let mut body = Map::new();
    body.insert("traces".to_owned(), Value::Array(items));
    body.insert("truncated".to_owned(), Value::Bool(more));
    body.insert(
        "notes".to_owned(),
        Value::Array(notes.into_iter().map(Value::String).collect()),
    );
    output::fit_list(
        &mut body,
        "traces",
        max_chars,
        &format!("result cut at {max_chars} characters; later traces left out"),
    );
    Ok(Value::Object(body))
}

/// A non-empty string value of `view` at `key`.
fn view_string(view: &Map<String, Value>, key: &str) -> Option<String> {
    match view.get(key) {
        Some(Value::String(text)) if !text.is_empty() => Some(text.clone()),
        _ => None,
    }
}

/// Whether `view` holds `"1"` or `1` at `key` (a list-page URL param).
fn view_flag(view: &Map<String, Value>, key: &str) -> bool {
    view.get(key) == Some(&json!("1")) || view.get(key) == Some(&json!(1))
}

/// The state of the `traces` saved view `key` names. A key of 36 characters that parses as a
/// hyphenated UUID is a view id and is looked up by id only (never by name); any other key is
/// an exact name, and among several visible views with that name the user's own wins, then the
/// lowest id (`find_visible_by_name` order).
///
/// # Errors
///
/// `400 bad_request` `no saved traces view with that name or id` when no view matches;
/// `400 bad_request` `view is not a traces view` when the id names a visible view of
/// another kind.
async fn view_state(
    state: &AppState,
    project: ProjectId,
    user: &AuthUser,
    key: &str,
) -> Result<Map<String, Value>, ApiError> {
    let not_found = || ApiError::BadRequest {
        detail: Some("no saved traces view with that name or id".to_owned()),
        errors: Vec::new(),
    };
    if key.len() == 36
        && let Ok(id) = Uuid::try_parse(key)
    {
        // `parse_str` would also accept 32 hex digits, braces or a `urn:uuid:` prefix; those
        // keys are names, and the length check above keeps them out of this branch.
        let Some(row) =
            saved_views::get_visible(state.meta.reader(), project, Viewer::User(user.id), id)
                .await?
        else {
            return Err(not_found());
        };
        if row.kind != SavedViewKind::Traces {
            return Err(ApiError::BadRequest {
                detail: Some("view is not a traces view".to_owned()),
                errors: Vec::new(),
            });
        }
        return Ok(row.state);
    }
    let rows = saved_views::find_visible_by_name(
        state.meta.reader(),
        project,
        Viewer::User(user.id),
        SavedViewKind::Traces,
        key,
    )
    .await?;
    rows.into_iter()
        .next()
        .map(|row| row.state)
        .ok_or_else(not_found)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GetTraceArguments {
    project: String,
    trace_id: String,
    around: Option<TimeParam>,
    #[serde(default)]
    include_payloads: bool,
    max_chars: Option<i64>,
}

/// One trace (`ProjectPerm::View`), its summary and its span tree as a flat pre-order list,
/// like `GET /traces/{trace_id}`. Default exclusions do not apply. `include_payloads` keeps
/// the `input_preview` and `output_preview` of the trace and of every span, cut to 200
/// characters; the full input and output of one span come from `get_span`.
///
/// Answers `{"trace": TraceSummary, "spans": [{"depth", "detached", "span": SpanSummary}],
/// "truncated", "notes"}`.
pub(crate) async fn get_trace(
    state: &AppState,
    user: &AuthUser,
    raw: JsonObject,
) -> Result<Value, ApiError> {
    let args: GetTraceArguments = arguments(raw)?;
    let max_chars = output::max_chars(args.max_chars)?;
    let access = user_project(state, user, &args.project, ProjectPerm::View).await?;

    let ctx = open_query(state, access.project.id, args.around.map(TimeParam::ns)).await?;
    let data = storage::trace_detail(&ctx, &args.trace_id, READ_TIMEOUT)
        .await?
        .ok_or(ApiError::NotFound)?;
    let mut trace = convert::trace_summaries(std::slice::from_ref(&data.trace))?
        .pop()
        .ok_or(ApiError::NotFound)?;
    attach_trace_scores(&ctx, std::slice::from_mut(&mut trace)).await?;
    let spans = build_tree(convert::span_summaries(&data.spans)?);

    let (trace, mut cut) = compact_summary(to_json(&trace)?, args.include_payloads);
    let mut items = Vec::with_capacity(spans.len());
    for tree_span in &spans {
        let value = to_json(tree_span)?;
        let Value::Object(mut object) = value else {
            items.push(value);
            continue;
        };
        if let Some(inner) = object.remove("span") {
            let (span, span_cut) = compact_summary(inner, args.include_payloads);
            cut |= span_cut;
            // `span` is the last key, so `remove` + `insert` keeps the order.
            object.insert("span".to_owned(), span);
        }
        items.push(Value::Object(object));
    }

    let mut notes = Vec::new();
    if data.truncated {
        notes.push(truncated_note());
    }
    if cut {
        notes.push(CUT_NOTE.to_owned());
    }

    let mut body = Map::new();
    body.insert("trace".to_owned(), trace);
    body.insert("spans".to_owned(), Value::Array(items));
    body.insert("truncated".to_owned(), Value::Bool(data.truncated));
    body.insert(
        "notes".to_owned(),
        Value::Array(notes.into_iter().map(Value::String).collect()),
    );
    output::fit_list(
        &mut body,
        "spans",
        max_chars,
        &format!("result cut at {max_chars} characters; later spans left out"),
    );
    Ok(Value::Object(body))
}

/// The note of a trace larger than `MAX_TRACE_SPANS`.
fn truncated_note() -> String {
    format!(
        "the trace has more than {0} spans; only the first {0} by start time are listed",
        storage::MAX_TRACE_SPANS
    )
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GetSpanArguments {
    project: String,
    trace_id: String,
    span_id: String,
    around: Option<TimeParam>,
    max_chars: Option<i64>,
}

/// One span with every column (`ProjectPerm::View`), like
/// `GET /traces/{trace_id}/spans/{span_id}`: parsed input and output, system instructions,
/// tool definitions, attributes, events and links.
///
/// Answers `{"span": SpanDetail, "notes"}`.
pub(crate) async fn get_span(
    state: &AppState,
    user: &AuthUser,
    raw: JsonObject,
) -> Result<Value, ApiError> {
    let args: GetSpanArguments = arguments(raw)?;
    let max_chars = output::max_chars(args.max_chars)?;
    let access = user_project(state, user, &args.project, ProjectPerm::View).await?;

    // The HTTP span endpoint opens its context with `None`; the tool takes `around` (D154).
    // It only speeds up the lookup.
    let ctx = open_query(state, access.project.id, args.around.map(TimeParam::ns)).await?;
    let batch = storage::span_detail(&ctx, &args.trace_id, &args.span_id, READ_TIMEOUT)
        .await?
        .ok_or(ApiError::NotFound)?;
    let span = SpanDetail::from(convert::span(&batch, access.project.id)?);
    let Value::Object(mut span) = to_json(&span)? else {
        return Err(ApiError::Internal(Box::new(NotAnObject)));
    };
    // Event and link `attributes` are not cut by this rule: they belong to the `HEAVY` fields
    // `events` and `links`, which are cut only when the span overflows.
    let cut = cut_values(
        &mut span,
        &["metadata", "attributes", "resource_attributes"],
        &PREVIEWS,
    );
    let notes = if cut {
        vec![CUT_NOTE.to_owned()]
    } else {
        Vec::new()
    };
    Ok(fit_span(&span, &notes, max_chars))
}

/// A `SpanDetail` that did not serialize to a JSON object.
#[derive(Debug, thiserror::Error)]
#[error("span detail is not a JSON object")]
struct NotAnObject;

/// `{"span": span, "notes": notes}`, fitted to `max_chars`.
///
/// When it does not fit, every `HEAVY` field that is present, not `null`, and whose compact
/// JSON (`render`) is longer than `n` characters is replaced by the string
/// `output::cut(&render(field), n)` (its JSON text cut to `n` characters plus `…`), and the
/// note `"fields cut to {n} characters of their JSON text: {names}"` is appended (`names` =
/// the cut fields in `HEAVY` order, joined with ", "). `n` is the largest value in
/// `0..=max_chars` for which the whole body fits, found by binary search; if even `n = 0`
/// does not fit, the body with `n = 0` is returned with the extra last note
/// "the span exceeds max_chars even with its large fields cut".
fn fit_span(span: &Map<String, Value>, notes: &[String], max_chars: usize) -> Value {
    let body = build(span, notes, None);
    if char_len(&render(&body)) <= max_chars {
        return body;
    }
    let fits = |n: usize| char_len(&render(&build(span, notes, Some(n)))) <= max_chars;
    if !fits(0) {
        let mut over = build(span, notes, Some(0));
        if let Some(Value::Array(items)) = over.get_mut("notes") {
            items.push(Value::String(
                "the span exceeds max_chars even with its large fields cut".to_owned(),
            ));
        }
        return over;
    }
    // The body's length is not strictly monotonic in `n` (the note's digits change), but the
    // search only returns an `n` it checked, so the result always fits.
    let (mut low, mut high) = (0, max_chars);
    while low < high {
        let mid = (low + high).div_ceil(2);
        if fits(mid) {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    build(span, notes, Some(low))
}

/// `{"span": …, "notes": …}` with the `HEAVY` fields cut to `n` characters of their JSON text
/// when `n` is `Some` (see [`fit_span`]).
fn build(span: &Map<String, Value>, notes: &[String], n: Option<usize>) -> Value {
    let mut map = span.clone();
    let mut names: Vec<&str> = Vec::new();
    if let Some(n) = n {
        for name in HEAVY {
            let Some(field) = map.get(name) else {
                continue;
            };
            if matches!(field, Value::Null) {
                // Null is never cut.
                continue;
            }
            let text = render(field);
            if let Some(short) = output::cut(&text, n) {
                map.insert(name.to_owned(), Value::String(short));
                names.push(name);
            }
        }
    }
    let mut all_notes: Vec<Value> = notes.iter().cloned().map(Value::String).collect();
    if !names.is_empty() {
        all_notes.push(Value::String(format!(
            "fields cut to {} characters of their JSON text: {}",
            n.unwrap_or(0),
            names.join(", ")
        )));
    }
    json!({"span": Value::Object(map), "notes": Value::Array(all_notes)})
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::str::FromStr;

    use llmobs_core::ids::{SpanId, TraceId};
    use serde_json::{Map, Value, json};

    use super::*;
    use crate::api::saved_views::SavedViewKind;
    use crate::auth::roles::{OrgRole, ProjectOverride};
    use crate::mcp::test_helpers::*;
    use crate::meta::members::set_project_override;
    use crate::meta::saved_views::NewSavedView;
    use crate::test_support::{TestApp, dataset_span};

    const T1: &str = "a1000000000000000000000000000001";
    const T2: &str = "a1000000000000000000000000000002";
    const T3: &str = "a1000000000000000000000000000003";
    const T4: &str = "a1000000000000000000000000000004";
    const T5: &str = "a1000000000000000000000000000005";
    const T6: &str = "a1000000000000000000000000000006";

    /// The example dataset loaded into a seeded app, with ada's read token.
    async fn with_dataset() -> (TestApp, crate::test_support::Seeded, String) {
        let (app, seeded, token) = app_with_token().await;
        app.load_example_dataset(seeded.project.id).await;
        (app, seeded, token)
    }

    /// `base` with the keys of the object `extra` set.
    fn with(mut base: Value, extra: Value) -> Value {
        if let Value::Object(extra) = extra {
            base.as_object_mut().unwrap().extend(extra);
        }
        base
    }

    /// `base` without `key`.
    fn without(mut base: Value, key: &str) -> Value {
        base.as_object_mut().unwrap().remove(key);
        base
    }

    /// The default `search_traces` arguments over the example day.
    fn base(project: &str) -> Value {
        json!({"project": project, "from": FROM, "to": TO})
    }

    /// `search_traces` over the example day, with `extra` merged into the arguments.
    async fn search(app: &TestApp, token: &str, project: &str, extra: Value) -> (bool, Value) {
        call(app, token, SEARCH_TRACES, &with(base(project), extra)).await
    }

    /// The `trace_id` of every trace, in order.
    fn ids(body: &Value) -> Vec<&str> {
        body["traces"]
            .as_array()
            .unwrap()
            .iter()
            .map(|trace| trace["trace_id"].as_str().unwrap())
            .collect()
    }

    /// Error `code` / `message` from a tool call.
    async fn expect_error(
        app: &TestApp,
        token: &str,
        tool: &str,
        arguments: &Value,
        code: &str,
        message: &str,
    ) {
        let (is_error, body) = call(app, token, tool, arguments).await;
        assert!(is_error, "{arguments}");
        assert_eq!(body["error"]["code"], json!(code), "{arguments}");
        assert_eq!(body["error"]["message"], json!(message), "{arguments}");
    }

    /// A view with the given fields; `is_default` is false. Returns its id.
    async fn insert(
        app: &TestApp,
        project_id: ProjectId,
        owner: Uuid,
        kind: SavedViewKind,
        name: &str,
        state: Value,
        shared: bool,
    ) -> Uuid {
        let now = app.state.clock.now_ns();
        let name = name.to_owned();
        let Value::Object(state) = state else {
            panic!("state must be a JSON object")
        };
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    let row = saved_views::insert_view(
                        conn,
                        NewSavedView {
                            project_id,
                            kind,
                            name,
                            state,
                            owner_user_id: owner,
                            shared,
                            is_default: false,
                        },
                        now,
                    )
                    .await?;
                    Ok(row.id)
                })
            })
            .await
            .unwrap()
    }

    /// The long-values span of § Long values, committed to the project: a root of its own
    /// trace at 10:40 UTC, with 300-character values everywhere.
    async fn commit_long_values(app: &TestApp, seeded: &crate::test_support::Seeded) {
        let mut span = dataset_span(13);
        span.trace_id = TraceId::from_str("a1000000000000000000000000000007").unwrap();
        span.span_id = SpanId::from_str("b100000000000071").unwrap();
        span.parent_span_id = None;
        span.name = "long-values".to_owned();
        span.start_time = 1_767_264_000_000_000_000;
        span.end_time = span.start_time + 1_000_000_000;
        span.metadata = BTreeMap::from([("note".to_owned(), "m".repeat(300))]);
        span.attributes = BTreeMap::from([("long".to_owned(), "a".repeat(300))]);
        span.resource_attributes = BTreeMap::from([("svc.long".to_owned(), "r".repeat(300))]);
        span.input_preview = Some("i".repeat(300));
        span.output_preview = Some("o".repeat(300));
        app.commit_spans(&seeded.project, vec![span]).await;
    }

    /// `value` without its two preview keys.
    fn without_previews(mut value: Value) -> Value {
        if let Some(object) = value.as_object_mut() {
            object.remove("input_preview");
            object.remove("output_preview");
        }
        value
    }

    // search_traces

    #[tokio::test(flavor = "multi_thread")]
    async fn search_traces_matches_the_list_endpoint() {
        let (app, seeded, tok) = with_dataset().await;
        let project = seeded.project.id.to_string();
        let (is_error, body) = search(&app, &tok, &project, json!({})).await;
        assert!(!is_error);
        let expected: Value = serde_json::from_str(include_str!(
            "../../../../fixtures/api/responses/traces.json"
        ))
        .unwrap();
        assert_eq!(body["traces"], expected["data"]);
        assert_eq!(body["truncated"], json!(false));
        assert_eq!(body["notes"], json!([]));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn search_traces_lifts_exclusions_on_request() {
        let (app, seeded, tok) = with_dataset().await;
        let (_, body) = search(
            &app,
            &tok,
            &seeded.project.id.to_string(),
            json!({"include_eval": true}),
        )
        .await;
        assert_eq!(ids(&body), [T6, T4, T3, T2, T5, T1]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn search_traces_filters_and_searches() {
        let (app, seeded, tok) = with_dataset().await;
        let project = seeded.project.id.to_string();

        let (_, body) = search(&app, &tok, &project, json!({"filter": "status = 'error'"})).await;
        assert_eq!(ids(&body), [T3]);

        let (_, body) = search(&app, &tok, &project, json!({"search": "refund"})).await;
        assert_eq!(ids(&body), [T3]);

        let (is_error, body) = call(
            &app,
            &tok,
            SEARCH_TRACES,
            &with(
                json!({"project": project, "to": TO}),
                json!({"from": "2026-01-01T10:06:00Z"}),
            ),
        )
        .await;
        assert!(!is_error);
        assert_eq!(ids(&body), [T6, T4, T3]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn search_traces_reports_more() {
        let (app, seeded, tok) = with_dataset().await;
        let project = seeded.project.id.to_string();

        let (_, body) = search(&app, &tok, &project, json!({"limit": 2})).await;
        assert_eq!(ids(&body), [T6, T4]);
        assert_eq!(body["truncated"], json!(true));
        assert_eq!(
            body["notes"],
            json!([
                "more traces match; narrow the window or the filter, or raise limit (at most 100)"
            ])
        );

        let (is_error, body) = call(
            &app,
            &tok,
            SEARCH_TRACES,
            &with(
                json!({"project": project, "from": FROM}),
                json!({"limit": 100, "to": "2026-01-01T10:06:00Z"}),
            ),
        )
        .await;
        assert!(!is_error);
        assert_eq!(ids(&body), [T2, T1]);
        assert_eq!(body["truncated"], json!(false));
        assert_eq!(body["notes"], json!([]));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn search_traces_validates_arguments() {
        let (app, seeded, tok) = with_dataset().await;
        let base = base(&seeded.project.id.to_string());

        expect_error(
            &app,
            &tok,
            SEARCH_TRACES,
            &without(base.clone(), "from"),
            "bad_request",
            "from and to are required (RFC 3339, at most 30 days apart)",
        )
        .await;
        expect_error(
            &app,
            &tok,
            SEARCH_TRACES,
            &with(base.clone(), json!({"to": FROM})),
            "bad_request",
            "from must be before to",
        )
        .await;
        expect_error(
            &app,
            &tok,
            SEARCH_TRACES,
            &with(base.clone(), json!({"to": "2026-01-31T00:00:01Z"})),
            "bad_request",
            "the window is at most 30 days",
        )
        .await;
        for limit in [0, 101, -1] {
            expect_error(
                &app,
                &tok,
                SEARCH_TRACES,
                &with(base.clone(), json!({"limit": limit})),
                "bad_request",
                "limit must be between 1 and 100",
            )
            .await;
        }
        for max_chars in [999, 100_001] {
            expect_error(
                &app,
                &tok,
                SEARCH_TRACES,
                &with(base.clone(), json!({"max_chars": max_chars})),
                "bad_request",
                "max_chars must be between 1000 and 100000",
            )
            .await;
        }

        let (is_error, body) = call(
            &app,
            &tok,
            SEARCH_TRACES,
            &with(base.clone(), json!({"x": 1})),
        )
        .await;
        assert!(is_error);
        assert_eq!(body["error"]["code"], json!("bad_request"));
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .starts_with("invalid arguments: unknown field `x`")
        );

        let (is_error, body) = call(
            &app,
            &tok,
            SEARCH_TRACES,
            &with(base.clone(), json!({"filter": "nope = 1"})),
        )
        .await;
        assert!(is_error);
        assert_eq!(body["error"]["code"], json!("invalid_filter"));
        assert!(!body["error"]["message"].as_str().unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn search_traces_fits_max_chars() {
        let (app, seeded, tok) = with_dataset().await;
        let (is_error, body) = search(
            &app,
            &tok,
            &seeded.project.id.to_string(),
            json!({"include_eval": true, "max_chars": 1000}),
        )
        .await;
        assert!(!is_error);
        assert_eq!(body["truncated"], json!(true));
        assert_eq!(
            body["notes"].as_array().unwrap().last().unwrap(),
            &json!("result cut at 1000 characters; later traces left out")
        );
        assert!(char_len(&render(&body)) <= 1000);
        let traces = body["traces"].as_array().unwrap();
        assert_eq!(traces.len(), 1);
        assert_eq!(traces[0]["trace_id"], json!(T6));
    }

    // Saved views in search_traces

    #[tokio::test(flavor = "multi_thread")]
    async fn view_by_name_and_id() {
        let (app, seeded, tok) = with_dataset().await;
        let project = seeded.project.id.to_string();
        let id = insert(
            &app,
            seeded.project.id,
            seeded.user_id,
            SavedViewKind::Traces,
            "Errors",
            json!({"filter": "status = 'error'"}),
            false,
        )
        .await;

        let (_, body) = search(&app, &tok, &project, json!({"view": "Errors"})).await;
        assert_eq!(ids(&body), [T3]);
        let (_, body) = search(&app, &tok, &project, json!({"view": id.to_string()})).await;
        assert_eq!(ids(&body), [T3]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn explicit_arguments_override_the_view() {
        let (app, seeded, tok) = with_dataset().await;
        let project = seeded.project.id.to_string();
        insert(
            &app,
            seeded.project.id,
            seeded.user_id,
            SavedViewKind::Traces,
            "Errors",
            json!({"filter": "status = 'error'"}),
            false,
        )
        .await;
        insert(
            &app,
            seeded.project.id,
            seeded.user_id,
            SavedViewKind::Traces,
            "With eval",
            json!({"eval": "1"}),
            false,
        )
        .await;

        let (_, body) = search(
            &app,
            &tok,
            &project,
            json!({"view": "Errors", "filter": "name = 'support-agent'"}),
        )
        .await;
        assert_eq!(ids(&body), [T2, T1]);

        let (_, body) = search(&app, &tok, &project, json!({"view": "With eval"})).await;
        assert_eq!(ids(&body), [T6, T4, T3, T2, T5, T1]);

        let (_, body) = search(
            &app,
            &tok,
            &project,
            json!({"view": "With eval", "include_eval": false}),
        )
        .await;
        assert_eq!(ids(&body), [T6, T4, T3, T2, T1]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn view_name_prefers_own_then_lowest_id() {
        let (app, seeded, tok) = with_dataset().await;
        let project = seeded.project.id.to_string();
        let bob = app
            .add_member(&seeded, "bob@example.com", OrgRole::Member)
            .await;
        let bob_tok = app.personal_token(bob.id, false).await;

        insert(
            &app,
            seeded.project.id,
            bob.id,
            SavedViewKind::Traces,
            "Pick",
            json!({"filter": "status = 'error'"}),
            true,
        )
        .await;
        insert(
            &app,
            seeded.project.id,
            seeded.user_id,
            SavedViewKind::Traces,
            "Pick",
            json!({"filter": "name = 'describe-image'"}),
            false,
        )
        .await;
        // Both `Shared` views are bob's; the lower id, the first inserted, wins.
        insert(
            &app,
            seeded.project.id,
            bob.id,
            SavedViewKind::Traces,
            "Shared",
            json!({"filter": "name = 'describe-image'"}),
            true,
        )
        .await;
        insert(
            &app,
            seeded.project.id,
            bob.id,
            SavedViewKind::Traces,
            "Shared",
            json!({"filter": "status = 'error'"}),
            true,
        )
        .await;

        // Ada's own view wins over bob's older shared view.
        let (_, body) = search(&app, &tok, &project, json!({"view": "Pick"})).await;
        assert_eq!(ids(&body), [T4]);
        let (_, body) = search(&app, &tok, &project, json!({"view": "Shared"})).await;
        assert_eq!(ids(&body), [T4]);
        // Ada's `Pick` is private, so bob sees his own.
        let (_, body) = search(&app, &bob_tok, &project, json!({"view": "Pick"})).await;
        assert_eq!(ids(&body), [T3]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn others_private_views_are_not_found() {
        let (app, seeded, tok) = with_dataset().await;
        let project = seeded.project.id.to_string();
        let bob = app
            .add_member(&seeded, "bob@example.com", OrgRole::Member)
            .await;
        let id = insert(
            &app,
            seeded.project.id,
            bob.id,
            SavedViewKind::Traces,
            "Secret",
            json!({"filter": "status = 'error'"}),
            false,
        )
        .await;

        expect_error(
            &app,
            &tok,
            SEARCH_TRACES,
            &with(base(&project), json!({"view": "Secret"})),
            "bad_request",
            "no saved traces view with that name or id",
        )
        .await;
        expect_error(
            &app,
            &tok,
            SEARCH_TRACES,
            &with(base(&project), json!({"view": id.to_string()})),
            "bad_request",
            "no saved traces view with that name or id",
        )
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn hex_names_resolve_by_name() {
        let (app, seeded, tok) = with_dataset().await;
        let project = seeded.project.id.to_string();
        insert(
            &app,
            seeded.project.id,
            seeded.user_id,
            SavedViewKind::Traces,
            "0190000000007000800000000000abcd",
            json!({"filter": "status = 'error'"}),
            false,
        )
        .await;

        // Not a 36-character UUID, so it is a name.
        let (_, body) = search(
            &app,
            &tok,
            &project,
            json!({"view": "0190000000007000800000000000abcd"}),
        )
        .await;
        assert_eq!(ids(&body), [T3]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn view_errors() {
        let (app, seeded, tok) = with_dataset().await;
        let project = seeded.project.id.to_string();
        let costs = insert(
            &app,
            seeded.project.id,
            seeded.user_id,
            SavedViewKind::Sql,
            "Costs",
            json!({"query": "SELECT 1", "params": {}}),
            true,
        )
        .await;
        insert(
            &app,
            seeded.project.id,
            seeded.user_id,
            SavedViewKind::Traces,
            "01900000-0000-7000-8000-00000000abcd",
            json!({"filter": "status = 'error'"}),
            false,
        )
        .await;

        expect_error(
            &app,
            &tok,
            SEARCH_TRACES,
            &with(base(&project), json!({"view": "Costs"})),
            "bad_request",
            "no saved traces view with that name or id",
        )
        .await;
        expect_error(
            &app,
            &tok,
            SEARCH_TRACES,
            &with(base(&project), json!({"view": costs.to_string()})),
            "bad_request",
            "view is not a traces view",
        )
        .await;
        expect_error(
            &app,
            &tok,
            SEARCH_TRACES,
            &with(base(&project), json!({"view": "nope"})),
            "bad_request",
            "no saved traces view with that name or id",
        )
        .await;
        // A UUID-shaped key is looked up by id only, never by name.
        expect_error(
            &app,
            &tok,
            SEARCH_TRACES,
            &with(
                base(&project),
                json!({"view": "01900000-0000-7000-8000-00000000abcd"}),
            ),
            "bad_request",
            "no saved traces view with that name or id",
        )
        .await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn view_flags_and_search() {
        let (app, seeded, tok) = with_dataset().await;
        let project = seeded.project.id.to_string();
        insert(
            &app,
            seeded.project.id,
            seeded.user_id,
            SavedViewKind::Traces,
            "Exp",
            json!({"exp": "1", "search": "refund"}),
            false,
        )
        .await;
        insert(
            &app,
            seeded.project.id,
            seeded.user_id,
            SavedViewKind::Traces,
            "Num",
            json!({"eval": 1}),
            false,
        )
        .await;

        // The view's search applies; no experiment traces exist, so `exp` changes nothing.
        let (_, body) = search(&app, &tok, &project, json!({"view": "Exp"})).await;
        assert_eq!(ids(&body), [T3]);

        // The search argument replaces the view's.
        let (_, body) = search(
            &app,
            &tok,
            &project,
            json!({"view": "Exp", "search": "order 1234"}),
        )
        .await;
        assert_eq!(ids(&body), [T1]);

        let (_, body) = search(&app, &tok, &project, json!({"view": "Num"})).await;
        assert_eq!(ids(&body), [T6, T4, T3, T2, T5, T1]);

        let (_, body) = search(&app, &tok, &project, json!({"include_experiments": true})).await;
        assert_eq!(ids(&body), [T6, T4, T3, T2, T1]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn hidden_projects_are_not_found() {
        let (app, seeded, _) = with_dataset().await;
        let (playground, _) = app.add_project(&seeded, "playground", None).await;
        let viewer = app
            .add_member(&seeded, "viewer@example.com", OrgRole::Viewer)
            .await;
        let (playground_id, viewer_id) = (playground.id, viewer.id);
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    set_project_override(conn, playground_id, viewer_id, ProjectOverride::None)
                        .await?;
                    Ok(())
                })
            })
            .await
            .unwrap();
        let vtok = app.personal_token(viewer.id, false).await;

        let not_found = json!({"error": {"code": "not_found", "message": "Not Found"}});
        let (is_error, body) = call(
            &app,
            &vtok,
            SEARCH_TRACES,
            &json!({"project": "acme/playground", "from": FROM, "to": TO}),
        )
        .await;
        assert!(is_error);
        assert_eq!(body, not_found);
        let (is_error, body) = call(
            &app,
            &vtok,
            GET_TRACE,
            &json!({"project": "acme/playground", "trace_id": T1}),
        )
        .await;
        assert!(is_error);
        assert_eq!(body, not_found);
    }

    // get_trace

    #[tokio::test(flavor = "multi_thread")]
    async fn get_trace_matches_the_detail_endpoint() {
        let (app, seeded, tok) = with_dataset().await;
        let (is_error, body) = call(
            &app,
            &tok,
            GET_TRACE,
            &json!({"project": seeded.project.id.to_string(), "trace_id": T1,
                    "include_payloads": true}),
        )
        .await;
        assert!(!is_error);
        let expected: Value = serde_json::from_str(include_str!(
            "../../../../fixtures/api/responses/traces/a1000000000000000000000000000001.json"
        ))
        .unwrap();
        assert_eq!(body["trace"], expected["trace"]);
        assert_eq!(body["spans"], expected["spans"]);
        assert_eq!(body["truncated"], json!(false));
        assert_eq!(body["notes"], json!([]));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_trace_leaves_previews_out_by_default() {
        let (app, seeded, tok) = with_dataset().await;
        let (_, body) = call(
            &app,
            &tok,
            GET_TRACE,
            &json!({"project": seeded.project.id.to_string(), "trace_id": T1}),
        )
        .await;
        let expected: Value = serde_json::from_str(include_str!(
            "../../../../fixtures/api/responses/traces/a1000000000000000000000000000001.json"
        ))
        .unwrap();
        assert_eq!(body["trace"], without_previews(expected["trace"].clone()));
        let spans: Vec<Value> = expected["spans"]
            .as_array()
            .unwrap()
            .iter()
            .map(|node| {
                let mut node = node.clone();
                node["span"] = without_previews(node["span"].clone());
                node
            })
            .collect();
        assert_eq!(body["spans"], json!(spans));
        let span_ids: Vec<&str> = body["spans"]
            .as_array()
            .unwrap()
            .iter()
            .map(|node| node["span"]["span_id"].as_str().unwrap())
            .collect();
        assert_eq!(
            span_ids,
            [
                "b100000000000011",
                "b100000000000012",
                "b100000000000013",
                "b100000000000014"
            ]
        );
        let depths: Vec<u64> = body["spans"]
            .as_array()
            .unwrap()
            .iter()
            .map(|node| node["depth"].as_u64().unwrap())
            .collect();
        assert_eq!(depths, [0, 1, 1, 1]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_trace_marks_a_detached_root() {
        let (app, seeded, tok) = with_dataset().await;
        let (_, body) = call(
            &app,
            &tok,
            GET_TRACE,
            &json!({"project": seeded.project.id.to_string(), "trace_id": T6}),
        )
        .await;
        let spans = body["spans"].as_array().unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0]["detached"], json!(true));
        assert_eq!(spans[0]["depth"], json!(0));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_trace_fits_max_chars() {
        let (app, seeded, tok) = with_dataset().await;
        let (_, body) = call(
            &app,
            &tok,
            GET_TRACE,
            &json!({"project": seeded.project.id.to_string(), "trace_id": T1,
                    "max_chars": 1500}),
        )
        .await;
        assert_eq!(body["truncated"], json!(true));
        assert_eq!(
            body["notes"],
            json!(["result cut at 1500 characters; later spans left out"])
        );
        assert!(char_len(&render(&body)) <= 1500);
        let spans = body["spans"].as_array().unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0]["span"]["span_id"], json!("b100000000000011"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_trace_takes_around() {
        let (app, seeded, tok) = with_dataset().await;
        let (is_error, body) = call(
            &app,
            &tok,
            GET_TRACE,
            &json!({"project": seeded.project.id.to_string(), "trace_id": T1,
                    "around": "2026-01-01T10:00:00Z"}),
        )
        .await;
        assert!(!is_error);
        assert_eq!(body["trace"]["trace_id"], json!(T1));

        // `around` only prunes stored files; the test data is still in memory, so a far-away
        // `around` cannot be shown to miss it. A malformed one is still refused.
        let (is_error, body) = call(
            &app,
            &tok,
            GET_TRACE,
            &json!({"project": seeded.project.id.to_string(), "trace_id": T1,
                    "around": "soon"}),
        )
        .await;
        assert!(is_error);
        assert_eq!(body["error"]["code"], json!("bad_request"));
        assert!(body["error"]["message"].as_str().unwrap().starts_with(
            "invalid arguments: expected an RFC 3339 timestamp or epoch milliseconds"
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_trace_summary_alone_over_budget() {
        let (app, seeded, tok) = with_dataset().await;
        // The long-values span of § Long values, with metadata holding 20 keys k00…k19 of
        // 190 characters (under the cut, but wide enough that the summary alone passes
        // 1,000 characters).
        let mut span = dataset_span(13);
        span.trace_id = TraceId::from_str("a1000000000000000000000000000007").unwrap();
        span.span_id = SpanId::from_str("b100000000000071").unwrap();
        span.parent_span_id = None;
        span.name = "long-values".to_owned();
        span.start_time = 1_767_264_000_000_000_000;
        span.end_time = span.start_time + 1_000_000_000;
        span.metadata = (0..20)
            .map(|i| (format!("k{i:02}"), "m".repeat(190)))
            .collect();
        span.input_preview = Some("i".repeat(300));
        span.output_preview = Some("o".repeat(300));
        app.commit_spans(&seeded.project, vec![span]).await;

        let (is_error, body) = call(
            &app,
            &tok,
            GET_TRACE,
            &json!({"project": seeded.project.id.to_string(),
                    "trace_id": "a1000000000000000000000000000007", "max_chars": 1000}),
        )
        .await;
        assert!(!is_error);
        assert_eq!(body["spans"], json!([]));
        assert_eq!(body["truncated"], json!(true));
        assert_eq!(
            body["notes"].as_array().unwrap().last().unwrap(),
            &json!("result cut at 1000 characters; later spans left out")
        );
        // The trace summary alone does not fit, as documented on `get_trace`; the body may
        // exceed 1,000 characters.
    }

    #[test]
    fn get_trace_truncated_note() {
        assert_eq!(
            truncated_note(),
            "the trace has more than 20000 spans; only the first 20000 by start time are listed"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_trace_errors() {
        let (app, seeded, tok) = with_dataset().await;
        let base = json!({"project": seeded.project.id.to_string(), "trace_id": T1});

        expect_error(
            &app,
            &tok,
            GET_TRACE,
            &with(
                base.clone(),
                json!({"trace_id": "a1000000000000000000000000000009"}),
            ),
            "not_found",
            "Not Found",
        )
        .await;
        expect_error(
            &app,
            &tok,
            GET_TRACE,
            &with(base.clone(), json!({"trace_id": "xyz"})),
            "bad_request",
            "trace_id must be 32 hex digits",
        )
        .await;
        expect_error(
            &app,
            &tok,
            GET_TRACE,
            &with(base.clone(), json!({"max_chars": 999})),
            "bad_request",
            "max_chars must be between 1000 and 100000",
        )
        .await;
        let (is_error, body) = call(&app, &tok, GET_TRACE, &with(base, json!({"x": 1}))).await;
        assert!(is_error);
        assert_eq!(body["error"]["code"], json!("bad_request"));
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .starts_with("invalid arguments: unknown field `x`")
        );
    }

    // get_span

    #[tokio::test(flavor = "multi_thread")]
    async fn get_span_matches_the_span_endpoint() {
        let (app, seeded, tok) = with_dataset().await;
        let expected: Value = serde_json::from_str(include_str!(
            "../../../../fixtures/api/responses/spans/a1000000000000000000000000000001/b100000000000012.json"
        ))
        .unwrap();
        for extra in [json!({}), json!({"around": "2026-01-01T10:00:00Z"})] {
            let (is_error, body) = call(
                &app,
                &tok,
                GET_SPAN,
                &with(
                    json!({"project": seeded.project.id.to_string(), "trace_id": T1,
                           "span_id": "b100000000000012"}),
                    extra,
                ),
            )
            .await;
            assert!(!is_error);
            assert_eq!(body["span"], expected);
            assert_eq!(body["notes"], json!([]));
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_span_cuts_large_fields_to_fit() {
        let (app, seeded, tok) = with_dataset().await;
        let (is_error, body) = call(
            &app,
            &tok,
            GET_SPAN,
            &json!({"project": seeded.project.id.to_string(), "trace_id": T1,
                    "span_id": "b100000000000012", "max_chars": 1300}),
        )
        .await;
        assert!(!is_error);
        assert!(char_len(&render(&body)) <= 1300);
        assert_eq!(body["span"]["span_id"], json!("b100000000000012"));
        assert!(body["span"]["output"].as_str().unwrap().ends_with('…'));
        let last_note = body["notes"].as_array().unwrap().last().unwrap();
        let last_note = last_note.as_str().unwrap();
        assert!(last_note.starts_with("fields cut to "), "{last_note}");
        assert!(last_note.contains("output"), "{last_note}");
        assert_eq!(body["span"]["events"], json!([]));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_span_errors() {
        let (app, seeded, tok) = with_dataset().await;
        let base = json!({"project": seeded.project.id.to_string(), "trace_id": T1,
                          "span_id": "b100000000000012"});

        expect_error(
            &app,
            &tok,
            GET_SPAN,
            &with(base.clone(), json!({"span_id": "b1000000000000ee"})),
            "not_found",
            "Not Found",
        )
        .await;
        expect_error(
            &app,
            &tok,
            GET_SPAN,
            &with(base.clone(), json!({"max_chars": 100_001})),
            "bad_request",
            "max_chars must be between 1000 and 100000",
        )
        .await;
        let (is_error, body) = call(&app, &tok, GET_SPAN, &with(base, json!({"x": 1}))).await;
        assert!(is_error);
        assert_eq!(body["error"]["code"], json!("bad_request"));
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .starts_with("invalid arguments: unknown field `x`")
        );
    }

    // Long values in all three tools

    #[tokio::test(flavor = "multi_thread")]
    async fn values_are_cut_to_200_characters() {
        let (app, seeded, tok) = with_dataset().await;
        commit_long_values(&app, &seeded).await;
        let project = seeded.project.id.to_string();
        let trace7 = "a1000000000000000000000000000007";
        let span71 = "b100000000000071";

        let (is_error, body) = call(
            &app,
            &tok,
            SEARCH_TRACES,
            &json!({"project": project, "from": "2026-01-01T10:35:00Z", "to": TO}),
        )
        .await;
        assert!(!is_error);
        assert_eq!(ids(&body), [trace7]);
        for field in ["metadata", "input_preview", "output_preview"] {
            let value = if field == "metadata" {
                body["traces"][0]["metadata"]["note"].as_str().unwrap()
            } else {
                body["traces"][0][field].as_str().unwrap()
            };
            assert_eq!(value.chars().count(), 201, "{field}");
            assert!(value.ends_with('…'), "{field}");
        }
        assert_eq!(body["notes"], json!([CUT_NOTE]));

        for include_payloads in [true, false] {
            let (_, body) = call(
                &app,
                &tok,
                GET_TRACE,
                &json!({"project": project, "trace_id": trace7,
                        "include_payloads": include_payloads}),
            )
            .await;
            if include_payloads {
                for value in [
                    body["trace"]["metadata"]["note"].as_str().unwrap(),
                    body["trace"]["input_preview"].as_str().unwrap(),
                    body["spans"][0]["span"]["metadata"]["note"]
                        .as_str()
                        .unwrap(),
                    body["spans"][0]["span"]["input_preview"].as_str().unwrap(),
                ] {
                    assert_eq!(value.chars().count(), 201);
                    assert!(value.ends_with('…'));
                }
            } else {
                assert!(body["trace"].get("input_preview").is_none());
                assert!(body["spans"][0]["span"].get("input_preview").is_none());
                let note = body["spans"][0]["span"]["metadata"]["note"]
                    .as_str()
                    .unwrap();
                assert_eq!(note.chars().count(), 201);
                assert!(note.ends_with('…'));
            }
            assert_eq!(body["notes"], json!([CUT_NOTE]));
        }

        let (is_error, body) = call(
            &app,
            &tok,
            GET_SPAN,
            &json!({"project": project, "trace_id": trace7, "span_id": span71}),
        )
        .await;
        assert!(!is_error);
        for value in [
            body["span"]["metadata"]["note"].as_str().unwrap(),
            body["span"]["attributes"]["long"].as_str().unwrap(),
            body["span"]["resource_attributes"]["svc.long"]
                .as_str()
                .unwrap(),
            body["span"]["input_preview"].as_str().unwrap(),
            body["span"]["output_preview"].as_str().unwrap(),
        ] {
            assert_eq!(value.chars().count(), 201);
            assert!(value.ends_with('…'));
        }
        assert_eq!(body["notes"], json!([CUT_NOTE]));
    }

    // Logs

    #[tokio::test]
    async fn trace_tools_log_no_arguments() {
        let (app, seeded, tok) = with_dataset().await;
        let project = seeded.project.id.to_string();
        let (_, logs) = capture_info_logs(async {
            let (_, _) = search(
                &app,
                &tok,
                &project,
                json!({"filter": "name = 'canary-f-1'", "search": "canary-s-2"}),
            )
            .await;
            let (_, _) = call(
                &app,
                &tok,
                GET_TRACE,
                &json!({"project": project, "trace_id": "canary-t-3"}),
            )
            .await;
        })
        .await;
        assert!(logs.contains("tool call"), "{logs}");
        assert!(logs.contains("search_traces"), "{logs}");
        for canary in ["canary-f-1", "canary-s-2", "canary-t-3"] {
            assert!(!logs.contains(canary), "{logs}");
        }
    }

    // Unit tests

    #[test]
    fn cut_values_cuts_maps_and_strings() {
        let mut object = json!({
            "metadata": {"a": "x".repeat(300), "b": "short", "n": 5},
            "input_preview": "y".repeat(250),
            "output_preview": null,
            "other": "z".repeat(300),
        })
        .as_object()
        .unwrap()
        .clone();
        assert!(cut_values(&mut object, &["metadata"], &PREVIEWS));
        let a = object["metadata"]["a"].as_str().unwrap();
        assert_eq!(a.chars().count(), 201);
        assert!(a.ends_with('…'));
        assert_eq!(object["metadata"]["b"], json!("short"));
        assert_eq!(object["metadata"]["n"], json!(5));
        assert_eq!(
            object["input_preview"].as_str().unwrap().chars().count(),
            201
        );
        assert_eq!(object["output_preview"], json!(null));
        assert_eq!(object["other"].as_str().unwrap().chars().count(), 300);
    }

    #[test]
    fn compact_summary_drops_previews_when_asked() {
        assert_eq!(
            compact_summary(
                json!({"input_preview": "a", "output_preview": "b", "name": "n"}),
                false
            ),
            (json!({"name": "n"}), false)
        );
    }

    #[test]
    fn fit_span_returns_a_fitting_span_unchanged() {
        let span = json!({"span_id": "x", "input": {"format": "text", "value": "hi"}})
            .as_object()
            .unwrap()
            .clone();
        assert_eq!(
            fit_span(&span, &[], 1000),
            json!({"span": {"span_id": "x", "input": {"format": "text", "value": "hi"}},
                   "notes": []})
        );
    }

    #[test]
    fn fit_span_cuts_heavy_fields_only() {
        let span = json!({
            "span_id": "x",
            "input": {"format": "text", "value": "a".repeat(5000)},
            "output": {"format": "text", "value": "b".repeat(5000)},
            "events": null,
        })
        .as_object()
        .unwrap()
        .clone();
        let body = fit_span(&span, &[], 1000);
        assert!(char_len(&render(&body)) <= 1000);
        assert_eq!(body["span"]["span_id"], json!("x"));
        // Null is never cut.
        assert_eq!(body["span"]["events"], json!(null));
        let input = body["span"]["input"].as_str().unwrap();
        let output = body["span"]["output"].as_str().unwrap();
        assert!(input.ends_with('…'));
        assert!(output.ends_with('…'));
        let count = input.chars().count();
        assert_eq!(output.chars().count(), count);
        assert_eq!(
            body["notes"],
            json!([format!(
                "fields cut to {} characters of their JSON text: input, output",
                count - 1
            )])
        );
    }

    #[test]
    fn fit_span_reports_when_nothing_fits() {
        let mut metadata = Map::new();
        for i in 0..100 {
            metadata.insert(format!("k{i}"), json!("m".repeat(190)));
        }
        let mut span = Map::new();
        span.insert("metadata".to_owned(), Value::Object(metadata));
        let body = fit_span(&span, &[], 1000);
        let notes = body["notes"].as_array().unwrap();
        assert_eq!(
            notes.last().unwrap(),
            &json!("the span exceeds max_chars even with its large fields cut")
        );
    }

    #[test]
    fn bounded_limits() {
        assert_eq!(bounded(None, 20, 100, "m").unwrap(), 20);
        assert_eq!(bounded(Some(100), 20, 100, "m").unwrap(), 100);
        for value in [0, 101, -1] {
            assert!(
                matches!(
                    bounded(Some(value), 20, 100, "m"),
                    Err(ApiError::BadRequest { .. })
                ),
                "{value}"
            );
        }
    }
}
