//! MCP tools over SQL and saved views (D-H-6, D-H-9, D-H-10, D-H-11).

use std::time::Duration;

use llmobs_core::ids::ProjectId;
use llmobs_storage::{QueryContext, SqlRequest, TimeWindow};
use rmcp::model::JsonObject;
use serde::Deserialize;
use serde_json::{Map, Value, json};

use super::output::{self, DEFAULT_MAX_CHARS};
use super::tools::arguments;
use crate::api::common::TimeParam;
use crate::api::saved_views::SavedViewKind;
use crate::auth::principal::AuthUser;
use crate::auth::roles::ProjectPerm;
use crate::http::AppState;
use crate::http::access::user_project;
use crate::http::error::ApiError;
use crate::meta::saved_views::{self, Viewer};
use crate::routes::read::open_query_windowed;

pub(crate) const QUERY_SQL: &str = "query_sql";
pub(crate) const LIST_SAVED_VIEWS: &str = "list_saved_views";

/// How long `query_sql` may run (D-H-10).
const SQL_TIMEOUT: Duration = Duration::from_secs(30);
/// Default and largest `limit` of `query_sql` (D-H-10).
const DEFAULT_ROWS: i64 = 100;
const MAX_ROWS: usize = 1_000;
/// A string longer than this is cut: top-level SQL cells and saved-view state values (D-H-11).
const CELL_CUT: usize = 2_000;
/// Longest window of `query_sql` and `search_traces`, in nanoseconds: 30 days (D-H-9).
const MAX_WINDOW_NS: i128 = 30 * 24 * 3_600 * 1_000_000_000;
/// Most saved views `list_saved_views` returns.
const MAX_VIEWS: usize = 1_000;

/// `[from, to)` from a tool's arguments: both required, `from < to`, at most 30 days apart
/// (D-H-9). `search_traces` (H8) uses it too.
///
/// # Errors
///
/// `400 bad_request`, checked in this order: `from and to are required (RFC 3339, at most 30
/// days apart)` when either is missing; `from must be before to` (A2's message, through
/// `ApiError::Query`) when `from >= to`; `the window is at most 30 days` when `to - from`
/// exceeds 30 days (exactly 30 days is accepted).
pub(crate) fn required_window(
    from: Option<TimeParam>,
    to: Option<TimeParam>,
) -> Result<TimeWindow, ApiError> {
    let (Some(from), Some(to)) = (from, to) else {
        return Err(ApiError::BadRequest {
            detail: Some("from and to are required (RFC 3339, at most 30 days apart)".to_owned()),
            errors: Vec::new(),
        });
    };
    let (from, to) = (from.ns(), to.ns());
    let window = TimeWindow::new(from, to)?;
    if i128::from(to) - i128::from(from) > MAX_WINDOW_NS {
        return Err(ApiError::BadRequest {
            detail: Some("the window is at most 30 days".to_owned()),
            errors: Vec::new(),
        });
    }
    Ok(window)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct QuerySqlArguments {
    project: String,
    query: String,
    #[serde(default)]
    params: Map<String, Value>,
    from: Option<TimeParam>,
    to: Option<TimeParam>,
    limit: Option<i64>,
    max_chars: Option<i64>,
}

/// Runs `query` on the project (`ProjectPerm::Query`) over the required window, with no
/// default exclusions, like the console (spec 05).
///
/// Answers `{"columns": [{"name", "type"}], "rows": [...], "row_count", "truncated", "notes"}`.
pub(crate) async fn query_sql(
    state: &AppState,
    user: &AuthUser,
    raw: JsonObject,
) -> Result<Value, ApiError> {
    let args: QuerySqlArguments = arguments(raw)?;
    let window = required_window(args.from, args.to)?;
    let limit = usize::try_from(args.limit.unwrap_or(DEFAULT_ROWS))
        .ok()
        .filter(|limit| (1..=MAX_ROWS).contains(limit))
        .ok_or_else(|| ApiError::BadRequest {
            detail: Some("limit must be between 1 and 1000".to_owned()),
            errors: Vec::new(),
        })?;
    let max_chars = output::max_chars(args.max_chars)?;
    let access = user_project(state, user, &args.project, ProjectPerm::Query).await?;
    let ctx = open_query_windowed(state, access.project.id, Some(window)).await?;
    run_query(
        &ctx,
        &args.query,
        &args.params,
        limit,
        SQL_TIMEOUT,
        max_chars,
    )
    .await
}

/// Steps 7–11 of `query_sql`, on an open context. Separate so tests can pass a tiny timeout.
async fn run_query(
    ctx: &QueryContext,
    query: &str,
    params: &Map<String, Value>,
    limit: usize,
    timeout: Duration,
    max_chars: usize,
) -> Result<Value, ApiError> {
    let result = ctx
        .sql_json(SqlRequest {
            query,
            params,
            limit,
            timeout,
        })
        .await
        .map_err(ApiError::Sql)?;
    let columns: Vec<Value> = result
        .columns
        .iter()
        .map(|column| json!({"name": column.name, "type": column.data_type}))
        .collect();
    let mut rows = result.rows;
    let cut = cut_cells(&mut rows);

    let mut notes = Vec::new();
    if result.truncated {
        notes.push(if limit < MAX_ROWS {
            format!(
                "rows truncated at {limit}; pass a larger limit (at most 1000) or narrow the query"
            )
        } else {
            "rows truncated at 1000; narrow the query or the window".to_owned()
        });
    }
    if cut {
        notes.push("string values longer than 2000 characters were cut".to_owned());
    }

    let mut body = Map::new();
    body.insert("columns".to_owned(), Value::Array(columns));
    body.insert(
        "rows".to_owned(),
        Value::Array(rows.into_iter().map(Value::Object).collect()),
    );
    // Set again below, once the rows are fitted.
    body.insert("row_count".to_owned(), json!(0));
    body.insert("truncated".to_owned(), Value::Bool(result.truncated));
    body.insert(
        "notes".to_owned(),
        Value::Array(notes.into_iter().map(Value::String).collect()),
    );
    output::fit_list(
        &mut body,
        "rows",
        max_chars,
        &format!("result cut at {max_chars} characters; later rows left out"),
    );
    let row_count = body["rows"].as_array().map_or(0, Vec::len);
    body.insert("row_count".to_owned(), json!(row_count));
    Ok(Value::Object(body))
}

/// Cuts every top-level string cell longer than `CELL_CUT` characters (see `output::cut`).
/// Strings inside lists and maps are left alone: a cell that is a list or map is returned
/// whole, and `max_chars` bounds the result. Returns whether anything was cut.
fn cut_cells(rows: &mut [Map<String, Value>]) -> bool {
    let mut cut = false;
    for row in rows {
        cut |= cut_values(row);
    }
    cut
}

/// [`cut_cells`] for one object: cuts its top-level string values.
fn cut_values(values: &mut Map<String, Value>) -> bool {
    let mut cut = false;
    for value in values.values_mut() {
        if let Value::String(text) = value
            && let Some(short) = output::cut(text, CELL_CUT)
        {
            *text = short;
            cut = true;
        }
    }
    cut
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListSavedViewsArguments {
    project: String,
    kind: Option<SavedViewKind>,
}

/// Saved views and SQL queries the user sees in the project (`ProjectPerm::View`): their own
/// and every shared one (D-H-6), oldest first (by id).
///
/// Answers `{"views": [{"id", "kind", "name", "shared", "owned", "is_default", "state"}],
/// "truncated", "notes"}`.
pub(crate) async fn list_saved_views(
    state: &AppState,
    user: &AuthUser,
    raw: JsonObject,
) -> Result<Value, ApiError> {
    let args: ListSavedViewsArguments = arguments(raw)?;
    let access = user_project(state, user, &args.project, ProjectPerm::View).await?;
    views_body(state, access.project.id, user, args.kind, MAX_VIEWS).await
}

/// Steps 3–7 of `list_saved_views`, with the view limit as a parameter so a test can pass a
/// small one.
async fn views_body(
    state: &AppState,
    project: ProjectId,
    user: &AuthUser,
    kind: Option<SavedViewKind>,
    max_views: usize,
) -> Result<Value, ApiError> {
    let page = saved_views::list_visible(
        state.meta.reader(),
        project,
        Viewer::User(user.id),
        kind,
        max_views,
        None,
    )
    .await?;
    let truncated = page.next_cursor.is_some();
    let mut state_cut = false;
    let mut views = Vec::with_capacity(page.rows.len());
    for row in page.rows {
        let mut view_state = row.state;
        state_cut |= cut_values(&mut view_state);
        views.push(json!({
            "id": row.id.to_string(),
            "kind": saved_views::kind_str(row.kind),
            "name": row.name,
            "shared": row.shared,
            "owned": row.owner_user_id == Some(user.id),
            "is_default": row.is_default,
            "state": Value::Object(view_state),
        }));
    }

    let mut notes = Vec::new();
    if truncated {
        notes.push(format!(
            "more than {max_views} saved views; only the first {max_views} are listed"
        ));
    }
    if state_cut {
        notes.push("saved-view state values longer than 2000 characters were cut".to_owned());
    }
    let mut body = Map::new();
    body.insert("views".to_owned(), Value::Array(views));
    body.insert("truncated".to_owned(), Value::Bool(truncated));
    body.insert(
        "notes".to_owned(),
        Value::Array(notes.into_iter().map(Value::String).collect()),
    );
    output::fit_list(
        &mut body,
        "views",
        DEFAULT_MAX_CHARS,
        "result cut at 20000 characters; later views left out",
    );
    Ok(Value::Object(body))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::{Map, Value, json};
    use uuid::Uuid;

    use super::*;
    use crate::auth::principal::AuthUser;
    use crate::auth::roles::OrgRole;
    use crate::mcp::output;
    use crate::mcp::test_helpers::*;
    use crate::meta::saved_views::NewSavedView;
    use crate::routes::read::open_query_windowed;
    use crate::test_support::{Seeded, TestApp};

    const T1: &str = "a1000000000000000000000000000001";
    const T2: &str = "a1000000000000000000000000000002";
    const T3: &str = "a1000000000000000000000000000003";
    const T4: &str = "a1000000000000000000000000000004";
    const T5: &str = "a1000000000000000000000000000005";
    const T6: &str = "a1000000000000000000000000000006";

    fn t(text: &str) -> TimeParam {
        serde_json::from_value(Value::String(text.to_owned())).unwrap()
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

    /// `query_sql` over `[from, to)` of `project`, with `extra` merged into the arguments.
    async fn run_sql(
        app: &TestApp,
        token: &str,
        project: &str,
        from: &str,
        to: &str,
        extra: Value,
    ) -> (bool, Value) {
        let arguments = with(json!({"project": project, "from": from, "to": to}), extra);
        call(app, token, QUERY_SQL, &arguments).await
    }

    /// `query_sql` over the whole example day ([`FROM`], [`TO`]).
    async fn sql(app: &TestApp, token: &str, project: &str, extra: Value) -> (bool, Value) {
        run_sql(app, token, project, FROM, TO, extra).await
    }

    /// The example dataset loaded into the seeded project.
    async fn with_dataset() -> (TestApp, Seeded, String) {
        let (app, seeded, token) = app_with_token().await;
        app.load_example_dataset(seeded.project.id).await;
        (app, seeded, token)
    }

    /// The `trace_id` of every row, in order.
    fn trace_ids(body: &Value) -> Vec<&str> {
        body["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["trace_id"].as_str().unwrap())
            .collect()
    }

    /// The `id` of every view in a `list_saved_views` body, in order.
    fn view_ids(body: &Value) -> Vec<String> {
        body["views"]
            .as_array()
            .unwrap()
            .iter()
            .map(|view| view["id"].as_str().unwrap().to_owned())
            .collect()
    }

    async fn expect_bad_request(app: &TestApp, token: &str, arguments: &Value, message: &str) {
        let (is_error, body) = call(app, token, QUERY_SQL, arguments).await;
        assert!(is_error, "{arguments}");
        assert_eq!(body["error"]["code"], json!("bad_request"), "{arguments}");
        assert_eq!(body["error"]["message"], json!(message), "{arguments}");
    }

    /// A view with the given columns; `shared` and `is_default` are false.
    fn new_view(
        project: ProjectId,
        owner: Uuid,
        kind: SavedViewKind,
        name: &str,
        state: Value,
        shared: bool,
        is_default: bool,
    ) -> NewSavedView {
        NewSavedView {
            project_id: project,
            kind,
            name: name.to_owned(),
            state: match state {
                Value::Object(map) => map,
                _ => panic!("a view state is a JSON object"),
            },
            owner_user_id: owner,
            shared,
            is_default,
        }
    }

    /// Inserts `view` and returns its id.
    async fn insert_view(app: &TestApp, view: NewSavedView) -> Uuid {
        let now = app.state.clock.now_ns();
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    let row = saved_views::insert_view(conn, view, now).await?;
                    Ok(row.id)
                })
            })
            .await
            .unwrap()
    }

    /// The views of the `list_saved_views` tests. Ada owns views 1, 2 and 5; bob owns 3, 4
    /// and 6. Returns bob's id and the ids of views 1 to 6, in that order.
    async fn seed_views(app: &TestApp, seeded: &Seeded) -> (Uuid, Vec<Uuid>) {
        let bob = app
            .add_member(seeded, "bob@example.com", OrgRole::Member)
            .await;
        let (playground, _) = app.add_project(seeded, "playground", None).await;
        let (project, ada) = (seeded.project.id, seeded.user_id);
        let mut ids = Vec::new();
        ids.push(
            insert_view(
                app,
                NewSavedView {
                    is_default: true,
                    ..new_view(
                        project,
                        ada,
                        SavedViewKind::Traces,
                        "Mine",
                        json!({"filter": "status = 'error'"}),
                        false,
                        false,
                    )
                },
            )
            .await,
        );
        ids.push(
            insert_view(
                app,
                new_view(
                    project,
                    ada,
                    SavedViewKind::Sql,
                    "Costs",
                    json!({"query": "SELECT 1", "params": {}}),
                    true,
                    false,
                ),
            )
            .await,
        );
        ids.push(
            insert_view(
                app,
                new_view(
                    project,
                    bob.id,
                    SavedViewKind::Traces,
                    "Bob only",
                    json!({}),
                    false,
                    false,
                ),
            )
            .await,
        );
        ids.push(
            insert_view(
                app,
                new_view(
                    project,
                    bob.id,
                    SavedViewKind::Traces,
                    "Team",
                    json!({"search": "refund"}),
                    true,
                    false,
                ),
            )
            .await,
        );
        ids.push(
            insert_view(
                app,
                new_view(
                    playground.id,
                    ada,
                    SavedViewKind::Traces,
                    "Elsewhere",
                    json!({}),
                    true,
                    false,
                ),
            )
            .await,
        );
        ids.push(
            insert_view(
                app,
                new_view(
                    project,
                    bob.id,
                    SavedViewKind::Traces,
                    "Orphan",
                    json!({}),
                    false,
                    false,
                ),
            )
            .await,
        );
        let orphan = ids[5];
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    sqlx::query("UPDATE saved_views SET owner_user_id = NULL WHERE id = ?")
                        .bind(orphan.to_string())
                        .execute(&mut *conn)
                        .await?;
                    Ok(())
                })
            })
            .await
            .unwrap();
        (bob.id, ids)
    }

    fn ada_user(seeded: &Seeded) -> AuthUser {
        AuthUser {
            id: seeded.user_id,
            is_instance_admin: true,
            email: "ada@example.com".to_owned(),
        }
    }

    // query_sql

    #[tokio::test(flavor = "multi_thread")]
    async fn query_sql_returns_rows_without_exclusions() {
        let (app, seeded, tok) = with_dataset().await;
        let project = seeded.project.id.to_string();
        let (is_error, body) = sql(
            &app,
            &tok,
            &project,
            json!({"query": "SELECT trace_id, name FROM traces ORDER BY start_time"}),
        )
        .await;
        assert!(!is_error);
        assert_eq!(
            body,
            json!({
                "columns": [{"name": "trace_id", "type": "Utf8"}, {"name": "name", "type": "Utf8"}],
                "rows": [
                    {"trace_id": T1, "name": "support-agent"},
                    {"trace_id": T5, "name": "judge helpfulness"},
                    {"trace_id": T2, "name": "support-agent"},
                    {"trace_id": T3, "name": "answer-question"},
                    {"trace_id": T4, "name": "describe-image"},
                    {"trace_id": T6, "name": "chat claude-sonnet-4-5"},
                ],
                "row_count": 6,
                "truncated": false,
                "notes": [],
            })
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn query_sql_window_is_half_open() {
        let (app, seeded, tok) = with_dataset().await;
        let project = seeded.project.id.to_string();
        let (is_error, body) = run_sql(
            &app,
            &tok,
            &project,
            "2026-01-01T10:10:00Z",
            "2026-01-01T10:20:00Z",
            json!({"query": "SELECT trace_id, name FROM traces ORDER BY start_time"}),
        )
        .await;
        assert!(!is_error);
        assert_eq!(trace_ids(&body), [T3]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn query_sql_windows_every_view() {
        let (app, seeded, tok) = with_dataset().await;
        let project = seeded.project.id.to_string();
        let from = "2026-01-01T10:06:00Z";

        let (_, body) = run_sql(
            &app,
            &tok,
            &project,
            from,
            TO,
            json!({"query": "SELECT count(*) AS n FROM scores"}),
        )
        .await;
        assert_eq!(body["rows"], json!([{"n": 3}]));

        let (_, body) = run_sql(
            &app,
            &tok,
            &project,
            from,
            TO,
            json!({"query": "SELECT count(*) AS n FROM (SELECT * FROM spans) AS s"}),
        )
        .await;
        assert_eq!(body["rows"], json!([{"n": 6}]));

        let (_, body) = run_sql(
            &app,
            &tok,
            &project,
            from,
            TO,
            json!({"query": "WITH t AS (SELECT trace_id FROM traces) SELECT count(*) AS n FROM t"}),
        )
        .await;
        assert_eq!(body["rows"], json!([{"n": 3}]));

        let (_, body) = sql(
            &app,
            &tok,
            &project,
            json!({"query": "SELECT count(*) AS n FROM scores"}),
        )
        .await;
        assert_eq!(body["rows"], json!([{"n": 6}]));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn query_sql_binds_parameters() {
        let (app, seeded, tok) = with_dataset().await;
        let (is_error, body) = sql(
            &app,
            &tok,
            &seeded.project.id.to_string(),
            json!({
                "query": "SELECT count(*) AS n FROM spans WHERE span_kind = {kind:String}",
                "params": {"kind": "llm"},
            }),
        )
        .await;
        assert!(!is_error);
        assert_eq!(
            body,
            json!({
                "columns": [{"name": "n", "type": "Int64"}],
                "rows": [{"n": 7}],
                "row_count": 1,
                "truncated": false,
                "notes": [],
            })
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn query_sql_reports_the_row_cap() {
        let (app, seeded, tok) = with_dataset().await;
        let project = seeded.project.id.to_string();
        let ordered = "SELECT trace_id FROM traces ORDER BY start_time";

        let (_, body) = sql(&app, &tok, &project, json!({"query": ordered, "limit": 2})).await;
        assert_eq!(trace_ids(&body), [T1, T5]);
        assert_eq!(body["row_count"], json!(2));
        assert_eq!(body["truncated"], json!(true));
        assert_eq!(
            body["notes"],
            json!(["rows truncated at 2; pass a larger limit (at most 1000) or narrow the query"])
        );

        let (_, body) = sql(
            &app,
            &tok,
            &project,
            json!({"query": "SELECT trace_id FROM traces ORDER BY start_time LIMIT 2", "limit": 2}),
        )
        .await;
        assert_eq!(body["row_count"], json!(2));
        assert_eq!(body["truncated"], json!(false));
        assert_eq!(body["notes"], json!([]));

        let (_, body) = sql(
            &app,
            &tok,
            &project,
            json!({"query": ordered, "limit": 1000}),
        )
        .await;
        assert_eq!(body["row_count"], json!(6));
        assert_eq!(body["truncated"], json!(false));

        let (_, body) = sql(
            &app,
            &tok,
            &project,
            json!({
                "query": "SELECT s.span_id FROM spans s CROSS JOIN spans a CROSS JOIN spans b",
                "limit": 1000,
            }),
        )
        .await;
        assert_eq!(body["truncated"], json!(true));
        assert_eq!(
            body["notes"][0],
            json!("rows truncated at 1000; narrow the query or the window")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn query_sql_cuts_long_strings() {
        let (app, seeded, tok) = with_dataset().await;
        let (is_error, body) = sql(
            &app,
            &tok,
            &seeded.project.id.to_string(),
            json!({"query": "SELECT concat(input, input, input, input, input, input, input) AS big FROM spans WHERE span_id = 'b100000000000014'"}),
        )
        .await;
        assert!(!is_error);
        let big = body["rows"][0]["big"].as_str().unwrap();
        assert_eq!(big.chars().count(), 2000);
        assert!(big.ends_with('…'));
        assert_eq!(body["truncated"], json!(false));
        assert_eq!(
            body["notes"],
            json!(["string values longer than 2000 characters were cut"])
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn query_sql_orders_its_notes() {
        let (app, seeded, tok) = with_dataset().await;
        let (_, body) = sql(
            &app,
            &tok,
            &seeded.project.id.to_string(),
            json!({
                "query": "SELECT concat(input, input, input, input, input, input, input) AS big FROM spans WHERE input IS NOT NULL ORDER BY length(input) DESC",
                "limit": 1,
            }),
        )
        .await;
        assert_eq!(
            body["notes"],
            json!([
                "rows truncated at 1; pass a larger limit (at most 1000) or narrow the query",
                "string values longer than 2000 characters were cut",
            ])
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn query_sql_fits_max_chars() {
        let (app, seeded, tok) = with_dataset().await;
        let (is_error, body) = sql(
            &app,
            &tok,
            &seeded.project.id.to_string(),
            json!({"query": "SELECT span_id, input FROM spans ORDER BY span_id", "max_chars": 1000}),
        )
        .await;
        assert!(!is_error);
        assert_eq!(body["truncated"], json!(true));
        assert_eq!(
            body["notes"],
            json!(["result cut at 1000 characters; later rows left out"])
        );
        assert!(output::char_len(&output::render(&body)) <= 1000);
        let rows = body["rows"].as_array().unwrap();
        assert_eq!(body["row_count"], json!(rows.len()));
        assert!(!rows.is_empty() && rows.len() < 14);
        assert_eq!(rows[0]["span_id"], json!("b100000000000011"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn query_sql_validates_arguments() {
        let (app, seeded, tok) = with_dataset().await;
        let base = json!({
            "project": seeded.project.id.to_string(),
            "from": FROM,
            "to": TO,
            "query": "SELECT 1 AS one",
        });

        expect_bad_request(
            &app,
            &tok,
            &without(base.clone(), "to"),
            "from and to are required (RFC 3339, at most 30 days apart)",
        )
        .await;
        expect_bad_request(
            &app,
            &tok,
            &with(base.clone(), json!({"to": FROM})),
            "from must be before to",
        )
        .await;
        expect_bad_request(
            &app,
            &tok,
            &with(base.clone(), json!({"to": "2026-01-31T00:00:01Z"})),
            "the window is at most 30 days",
        )
        .await;
        for limit in [0, 1001, -1] {
            expect_bad_request(
                &app,
                &tok,
                &with(base.clone(), json!({"limit": limit})),
                "limit must be between 1 and 1000",
            )
            .await;
        }
        for max_chars in [999, 100_001, -5] {
            expect_bad_request(
                &app,
                &tok,
                &with(base.clone(), json!({"max_chars": max_chars})),
                "max_chars must be between 1000 and 100000",
            )
            .await;
        }
        expect_bad_request(
            &app,
            &tok,
            &with(
                base.clone(),
                json!({"query": "SELECT {kind:String} AS k", "params": {}}),
            ),
            "missing value for parameter kind",
        )
        .await;

        let (is_error, body) = call(
            &app,
            &tok,
            QUERY_SQL,
            &with(base.clone(), json!({"from": "yesterday"})),
        )
        .await;
        assert!(is_error);
        assert_eq!(body["error"]["code"], json!("bad_request"));
        assert!(body["error"]["message"].as_str().unwrap().starts_with(
            "invalid arguments: expected an RFC 3339 timestamp or epoch milliseconds"
        ));

        let (is_error, body) =
            call(&app, &tok, QUERY_SQL, &with(base.clone(), json!({"x": 1}))).await;
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
            QUERY_SQL,
            &with(base.clone(), json!({"to": "2026-01-31T00:00:00Z"})),
        )
        .await;
        assert!(!is_error);
        assert_eq!(body["rows"], json!([{"one": 1}]));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn query_sql_reports_sql_errors() {
        let (app, seeded, tok) = with_dataset().await;
        let project = seeded.project.id.to_string();

        let (is_error, body) =
            sql(&app, &tok, &project, json!({"query": "DELETE FROM traces"})).await;
        assert!(is_error);
        assert_eq!(body["error"]["code"], json!("invalid_filter"));
        assert_eq!(
            body["error"]["message"],
            json!("only SELECT queries are allowed")
        );

        let (is_error, body) = sql(&app, &tok, &project, json!({"query": "SELEC 1"})).await;
        assert!(is_error);
        assert_eq!(body["error"]["code"], json!("invalid_filter"));
        assert!(!body["error"]["message"].as_str().unwrap().is_empty());
        assert_eq!(body["error"]["position"]["line"], json!(1));
        assert!(body["error"]["position"]["column"].as_u64().unwrap() >= 1);

        let (is_error, body) =
            sql(&app, &tok, &project, json!({"query": "SELECT 1 / 0 AS x"})).await;
        assert!(is_error);
        assert_eq!(body["error"]["code"], json!("query_failed"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn query_sql_needs_a_visible_project() {
        let (app, _, tok) = with_dataset().await;
        let (is_error, body) = sql(&app, &tok, "acme/nope", json!({"query": "SELECT 1"})).await;
        assert!(is_error);
        assert_eq!(
            body,
            json!({"error": {"code": "not_found", "message": "Not Found"}})
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn query_sql_works_for_a_viewer_with_a_read_token() {
        let (app, seeded, _) = with_dataset().await;
        let viewer = app
            .add_member(&seeded, "viewer@example.com", OrgRole::Viewer)
            .await;
        let token = app.personal_token(viewer.id, false).await;
        let (is_error, body) = sql(
            &app,
            &token,
            &seeded.project.id.to_string(),
            json!({"query": "SELECT 1 AS one"}),
        )
        .await;
        assert!(!is_error);
        assert_eq!(body["rows"], json!([{"one": 1}]));
    }

    /// The query must be one that cannot finish on its first poll: `tokio::time::timeout`
    /// polls the future before the deadline, so a query that runs to completion inline (a
    /// plain aggregate over the in-memory example data) wins the race however small the
    /// timeout. The `GROUP BY` plan is executed through a `RepartitionExec`: its partition
    /// tasks are spawned during the first poll and deliver through a channel, so that poll
    /// always returns `Pending` — a task spawned microseconds ago cannot have produced a
    /// batch yet — and the already-elapsed 1 ns deadline then maps to `RunError::Timeout`.
    #[tokio::test(flavor = "multi_thread")]
    async fn run_query_times_out() {
        // The GROUP BY runs through a RepartitionExec only with more than one partition, so
        // pin it: on a one-core runner the default would let the query finish on its first poll.
        let app = TestApp::with_config(|config| {
            config.storage.query.target_partitions = 4;
        })
        .await;
        let seeded = app.seed().await;
        app.load_example_dataset(seeded.project.id).await;
        let window = required_window(Some(t(FROM)), Some(t(TO))).unwrap();
        let ctx = open_query_windowed(&app.state, seeded.project.id, Some(window))
            .await
            .unwrap();
        let result = run_query(
            &ctx,
            "SELECT span_kind, count(*) AS n FROM spans GROUP BY span_kind",
            &Map::new(),
            10,
            Duration::from_nanos(1),
            20_000,
        )
        .await;
        assert!(matches!(
            result,
            Err(ApiError::Sql(llmobs_storage::RunError::Timeout))
        ));
    }

    // list_saved_views

    #[tokio::test(flavor = "multi_thread")]
    async fn list_saved_views_shows_own_and_shared() {
        let (app, seeded, tok) = app_with_token().await;
        let (_, ids) = seed_views(&app, &seeded).await;
        let (is_error, body) = call(
            &app,
            &tok,
            LIST_SAVED_VIEWS,
            &json!({"project": seeded.project.id.to_string()}),
        )
        .await;
        assert!(!is_error);
        assert_eq!(
            body,
            json!({
                "views": [
                    {"id": ids[0].to_string(), "kind": "traces", "name": "Mine", "shared": false,
                     "owned": true, "is_default": true, "state": {"filter": "status = 'error'"}},
                    {"id": ids[1].to_string(), "kind": "sql", "name": "Costs", "shared": true,
                     "owned": true, "is_default": false, "state": {"query": "SELECT 1", "params": {}}},
                    {"id": ids[3].to_string(), "kind": "traces", "name": "Team", "shared": true,
                     "owned": false, "is_default": false, "state": {"search": "refund"}},
                ],
                "truncated": false,
                "notes": [],
            })
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_saved_views_filters_by_kind() {
        let (app, seeded, tok) = app_with_token().await;
        let (_, ids) = seed_views(&app, &seeded).await;
        let (is_error, body) = call(
            &app,
            &tok,
            LIST_SAVED_VIEWS,
            &json!({"project": seeded.project.id.to_string(), "kind": "sql"}),
        )
        .await;
        assert!(!is_error);
        assert_eq!(view_ids(&body), [ids[1].to_string()]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_saved_views_for_another_user() {
        let (app, seeded, _) = app_with_token().await;
        let (bob, ids) = seed_views(&app, &seeded).await;
        let token = app.personal_token(bob, false).await;
        let (is_error, body) = call(
            &app,
            &token,
            LIST_SAVED_VIEWS,
            &json!({"project": seeded.project.id.to_string()}),
        )
        .await;
        assert!(!is_error);
        assert_eq!(
            view_ids(&body),
            [ids[1], ids[2], ids[3]].map(|id| id.to_string())
        );
        let owned: Vec<bool> = body["views"]
            .as_array()
            .unwrap()
            .iter()
            .map(|view| view["owned"].as_bool().unwrap())
            .collect();
        assert_eq!(owned, [false, true, true]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_saved_views_errors() {
        let (app, seeded, tok) = app_with_token().await;
        seed_views(&app, &seeded).await;
        let (is_error, body) = call(
            &app,
            &tok,
            LIST_SAVED_VIEWS,
            &json!({"project": seeded.project.id.to_string(), "kind": "nope"}),
        )
        .await;
        assert!(is_error);
        assert_eq!(body["error"]["code"], json!("bad_request"));
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .starts_with("invalid arguments: unknown variant `nope`")
        );

        let (is_error, body) = call(
            &app,
            &tok,
            LIST_SAVED_VIEWS,
            &json!({"project": "acme/nope"}),
        )
        .await;
        assert!(is_error);
        assert_eq!(
            body,
            json!({"error": {"code": "not_found", "message": "Not Found"}})
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_saved_views_reports_more_views() {
        let (app, seeded, _) = app_with_token().await;
        let (_, ids) = seed_views(&app, &seeded).await;
        let body = views_body(&app.state, seeded.project.id, &ada_user(&seeded), None, 2)
            .await
            .unwrap();
        assert_eq!(view_ids(&body), [ids[0].to_string(), ids[1].to_string()]);
        assert_eq!(body["truncated"], json!(true));
        assert_eq!(
            body["notes"],
            json!(["more than 2 saved views; only the first 2 are listed"])
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_saved_views_fits_its_budget() {
        let (app, seeded, tok) = app_with_token().await;
        let (_, ids) = seed_views(&app, &seeded).await;
        for i in 0..300 {
            insert_view(
                &app,
                new_view(
                    seeded.project.id,
                    seeded.user_id,
                    SavedViewKind::Traces,
                    &format!("v{i:03}"),
                    json!({"filter": "f".repeat(150)}),
                    false,
                    false,
                ),
            )
            .await;
        }
        let (is_error, body) = call(
            &app,
            &tok,
            LIST_SAVED_VIEWS,
            &json!({"project": seeded.project.id.to_string()}),
        )
        .await;
        assert!(!is_error);
        assert_eq!(body["truncated"], json!(true));
        assert_eq!(
            body["notes"].as_array().unwrap().last().unwrap(),
            &json!("result cut at 20000 characters; later views left out")
        );
        assert!(output::char_len(&output::render(&body)) <= 20_000);
        assert_eq!(body["views"][0]["id"], json!(ids[0].to_string()));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_saved_views_cuts_long_state_values() {
        let (app, seeded, tok) = app_with_token().await;
        seed_views(&app, &seeded).await;
        insert_view(
            &app,
            new_view(
                seeded.project.id,
                seeded.user_id,
                SavedViewKind::Traces,
                "Long",
                json!({"filter": "a".repeat(2500)}),
                false,
                false,
            ),
        )
        .await;
        let (is_error, body) = call(
            &app,
            &tok,
            LIST_SAVED_VIEWS,
            &json!({"project": seeded.project.id.to_string()}),
        )
        .await;
        assert!(!is_error);
        let long = body["views"]
            .as_array()
            .unwrap()
            .iter()
            .find(|view| view["name"] == json!("Long"))
            .unwrap();
        let filter = long["state"]["filter"].as_str().unwrap();
        assert_eq!(filter.chars().count(), 2000);
        assert!(filter.ends_with('…'));
        assert_eq!(
            body["notes"],
            json!(["saved-view state values longer than 2000 characters were cut"])
        );
    }

    // Logs

    #[tokio::test]
    async fn query_sql_logs_no_sql_text() {
        let (app, seeded, tok) = app_with_token().await;
        app.load_example_dataset(seeded.project.id).await;
        let project = seeded.project.id.to_string();
        let (_, logs) = capture_info_logs(async {
            let (is_error, _) = sql(
                &app,
                &tok,
                &project,
                json!({
                    "query": "SELECT 'canary-sql-1' AS c FROM spans WHERE name = {p:String}",
                    "params": {"p": "canary-param-2"},
                }),
            )
            .await;
            assert!(!is_error);
            let (is_error, _) = sql(
                &app,
                &tok,
                &project,
                json!({"query": "SELEC 'canary-sql-3'"}),
            )
            .await;
            assert!(is_error);
        })
        .await;
        assert!(logs.contains("tool call"), "{logs}");
        assert!(logs.contains("query_sql"), "{logs}");
        for canary in ["canary-sql-1", "canary-param-2", "canary-sql-3"] {
            assert!(!logs.contains(canary), "{logs}");
        }
    }

    // Unit tests

    #[test]
    fn cut_cells_cuts_top_level_strings_only() {
        let mut rows = vec![
            json!({
                "a": "x".repeat(2001),
                "b": "x".repeat(2000),
                "c": ["x".repeat(3000)],
                "d": {"k": "x".repeat(3000)},
                "e": 5,
            })
            .as_object()
            .unwrap()
            .clone(),
        ];
        assert!(cut_cells(&mut rows));
        let row = &rows[0];
        let a = row["a"].as_str().unwrap();
        assert_eq!(a.chars().count(), 2000);
        assert!(a.ends_with('…'));
        assert_eq!(row["b"].as_str().unwrap().chars().count(), 2000);
        assert_eq!(row["c"], json!(["x".repeat(3000)]));
        assert_eq!(row["d"], json!({"k": "x".repeat(3000)}));
        assert_eq!(row["e"], json!(5));
        // A cut value is at the cap already, so a second call changes nothing.
        let a = a.to_owned();
        assert!(!cut_cells(&mut rows));
        assert_eq!(rows[0]["a"].as_str().unwrap(), a);
    }

    #[test]
    fn required_window_bounds() {
        assert!(matches!(
            required_window(None, Some(t(TO))),
            Err(ApiError::BadRequest { .. })
        ));
        assert!(matches!(
            required_window(Some(t(FROM)), Some(t(FROM))),
            Err(ApiError::Query(llmobs_storage::RunError::InvalidRequest(_)))
        ));
        assert!(required_window(Some(t(FROM)), Some(t("2026-01-31T00:00:00Z"))).is_ok());
        assert!(matches!(
            required_window(Some(t(FROM)), Some(t("2026-01-31T00:00:00.000000001Z"))),
            Err(ApiError::BadRequest { .. })
        ));
    }
}
