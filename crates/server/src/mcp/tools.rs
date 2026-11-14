//! The tool list and the tools that need no query (D-H-11).

use std::sync::Arc;

use rmcp::model::{JsonObject, Tool, ToolAnnotations};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use super::output::{self, DEFAULT_MAX_CHARS};
use crate::api::convert;
use crate::auth::principal::AuthUser;
use crate::auth::roles::{self, ProjectPerm, Role};
use crate::http::AppState;
use crate::http::access::user_project;
use crate::http::error::ApiError;
use crate::meta::members::project_override;
use crate::meta::orgs::orgs_of_user;
use crate::meta::projects::projects_of_org;

pub(crate) const LIST_PROJECTS: &str = "list_projects";
pub(crate) const GET_SCHEMA: &str = "get_schema";

/// The description of the `project` argument, shared by every tool that takes one.
pub(crate) const PROJECT_ARG: &str =
    "Project id (UUID) or `org-slug/project-slug`, as `list_projects` returns them.";

/// Every tool, in the order `tools/list` returns them.
pub(crate) fn tool_list() -> Vec<Tool> {
    vec![
        tool(
            LIST_PROJECTS,
            "List projects",
            "Projects you can read, with your role in each. Call this first: every other tool takes a `project`.",
            json!({"type": "object", "properties": {}, "additionalProperties": false}),
        ),
        tool(
            GET_SCHEMA,
            "Get the schema",
            "The SQL views (traces, spans, sessions, scores), their columns with types and descriptions, and the functions the filter DSL and SQL accept.",
            json!({
                "type": "object",
                "properties": {"project": {"type": "string", "description": PROJECT_ARG}},
                "required": ["project"],
                "additionalProperties": false
            }),
        ),
        tool(
            super::sql::QUERY_SQL,
            "Run a SQL query",
            "Run one read-only SQL query (SELECT, WITH, UNION, VALUES or EXPLAIN) over the views traces, spans, sessions and scores; get_schema lists their columns. `from` and `to` are required and at most 30 days apart: they bound start_time on spans and traces and created_at on scores, in every part of the query; sessions are built from the windowed traces. No rows are hidden: evaluation traces (environment 'llmobs-eval') and experiment traces are included. Returns columns, rows, row_count, truncated and notes; string cells longer than 2000 characters are cut.",
            json!({
                "type": "object",
                "properties": {
                    "project": {"type": "string", "description": PROJECT_ARG},
                    "query": {"type": "string", "description": "The SQL statement. Use {name:Type} placeholders (Type: String, Int, Float, Bool, Timestamp) with `params` instead of inlining values."},
                    "params": {"type": "object", "description": "Values of the {name:Type} placeholders, by name."},
                    "from": {"type": "string", "description": "Window start, inclusive: an RFC 3339 timestamp, or epoch milliseconds as a string."},
                    "to": {"type": "string", "description": "Window end, exclusive: an RFC 3339 timestamp, or epoch milliseconds as a string. At most 30 days after from."},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 1000, "default": 100, "description": "Most rows to return."},
                    "max_chars": {"type": "integer", "minimum": 1000, "maximum": 100000, "default": 20000, "description": "Most characters in the result; later rows are left out beyond it."}
                },
                "required": ["project", "query", "from", "to"],
                "additionalProperties": false
            }),
        ),
        tool(
            super::sql::LIST_SAVED_VIEWS,
            "List saved views",
            "Saved list views (kind traces, spans, sessions, users) and saved SQL queries (kind sql) you can see in the project: yours and the shared ones. A view's `state` holds its filter, search and range; a SQL query's holds `query` and `params`. Pass a traces view's name or id to search_traces as `view`.",
            json!({
                "type": "object",
                "properties": {
                    "project": {"type": "string", "description": PROJECT_ARG},
                    "kind": {"type": "string", "enum": ["traces", "spans", "sessions", "users", "sql"], "description": "Only views of this kind."}
                },
                "required": ["project"],
                "additionalProperties": false
            }),
        ),
    ]
}

/// A read-only tool. `schema` is a JSON object; anything else becomes `{}`.
pub(crate) fn tool(
    name: &'static str,
    title: &'static str,
    description: &'static str,
    schema: Value,
) -> Tool {
    let schema: JsonObject = match schema {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    Tool::new(name, description, Arc::new(schema))
        .with_title(title)
        .with_annotations(ToolAnnotations::new().read_only(true))
}

/// Parses a tool's arguments. Unknown fields and wrong types are `400 bad_request` with
/// `invalid arguments: <serde message>`; the message goes to the caller and is never logged.
pub(crate) fn arguments<T: DeserializeOwned>(arguments: JsonObject) -> Result<T, ApiError> {
    serde_json::from_value(Value::Object(arguments)).map_err(|error| ApiError::BadRequest {
        detail: Some(format!("invalid arguments: {error}")),
        errors: Vec::new(),
    })
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NoArguments {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectArguments {
    project: String,
}

/// `{"projects": [{id, org, slug, name, role}], "truncated": false, "notes": []}`: the active
/// projects of the user's active orgs that no override hides, ordered like `orgs_of_user`
/// (org name, org id) then `projects_of_org` (project name, project id). `org` is the org slug;
/// `role` is the effective project role, `viewer`, `member` or `admin`. Fitted to
/// `DEFAULT_MAX_CHARS` with `fit_list` on `projects` and the note
/// `"result cut at 20000 characters; later projects left out"`.
///
/// This repeats the visibility rule of `routes::projects::list_projects` and of
/// `user_project`; keep them in sync.
pub(crate) async fn list_projects(
    state: &AppState,
    user: &AuthUser,
    raw: JsonObject,
) -> Result<Value, ApiError> {
    let _: NoArguments = arguments(raw)?;
    let mut projects = Vec::new();
    for (org, org_role) in orgs_of_user(state.meta.reader(), user.id).await? {
        if org.state != "active" {
            continue;
        }
        for project in projects_of_org(state.meta.reader(), org.id).await? {
            if project.state != "active" {
                continue;
            }
            let ov = project_override(state.meta.reader(), project.id, user.id).await?;
            let Some(role) = roles::effective_project_role(org_role, ov) else {
                continue;
            };
            projects.push(json!({
                "id": project.id.to_string(),
                "org": org.slug,
                "slug": project.slug,
                "name": project.name,
                "role": role_name(role),
            }));
        }
    }
    let mut body = Map::new();
    body.insert("projects".to_owned(), Value::Array(projects));
    body.insert("truncated".to_owned(), Value::Bool(false));
    body.insert("notes".to_owned(), Value::Array(Vec::new()));
    output::fit_list(
        &mut body,
        "projects",
        DEFAULT_MAX_CHARS,
        "result cut at 20000 characters; later projects left out",
    );
    Ok(Value::Object(body))
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::Viewer => "viewer",
        Role::Member => "member",
        Role::Admin => "admin",
    }
}

/// `GET /schema`'s body for a project the user can view (`ProjectPerm::View`). The catalog is
/// the same for every project and is about 15,000 characters, so it is not fitted and the tool
/// takes no `max_chars` (README § Design → MCP server).
pub(crate) async fn get_schema(
    state: &AppState,
    user: &AuthUser,
    raw: JsonObject,
) -> Result<Value, ApiError> {
    let ProjectArguments { project } = arguments(raw)?;
    user_project(state, user, &project, ProjectPerm::View).await?;
    serde_json::to_value(convert::schema_response())
        .map_err(|error| ApiError::Internal(Box::new(error)))
}
