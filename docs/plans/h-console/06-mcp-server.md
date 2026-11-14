# H6 — The MCP endpoint

**Commit subject:** `server: add the MCP endpoint`

H6 adds `POST /mcp`: the MCP server for coding agents (D-H-2, D-H-3, D-H-10, D-H-11). It speaks streamable HTTP through `rmcp` 3.10.0, stateless, with JSON responses. Only personal access tokens may call it (`Access::Pat`). Each token gets 60 requests per minute and 4 at a time. H6 ships the server and two tools, `list_projects` and `get_schema`; H7 and H8 add the other five tools to the same dispatcher.

When H6 is done, an agent configured with `{"url": "http://<host>:8080/mcp", "headers": {"Authorization": "Bearer pat-llmobs-…"}}` can list its projects and read the schema.

## Read first

- README § Design → Access, § Design → MCP server, § Contracts between sub-phases.
- `crates/server/src/http/access.rs`: `Access`, `Access::reads_by_post` (H1), `prepare`, `grant_session_project`.
- `crates/server/src/http/access_suite.rs`: `checked_routes`, `expected`, `assert_every_route_declares_access`.
- `crates/server/src/http/mod.rs`: `AppInner` (`limits`, `clock`), `finish` (the 405 fallback; `/mcp` is already an API path in `fallback`).
- `crates/server/src/http/error.rs`: `ApiError` (including H4's `ApiError::Sql`), `ApiError::problem`, `IntoResponse for ApiError`.
- `crates/server/src/auth/principal.rs`: `Principal`, `AuthUser`.
- `crates/server/src/auth/limits.rs`: `KeyedLimiter`, `Bucket`, `RetryAfter`, `Limits`.
- `crates/server/src/meta/orgs.rs` (`orgs_of_user`), `meta/projects.rs` (`projects_of_org`, `ProjectRow`), `meta/members.rs` (`project_override`, `set_project_override`), `auth/roles.rs` (`effective_project_role`, `Role`).
- `crates/server/src/routes/projects.rs` `list_projects` (the same visibility rule, for one org).
- `crates/server/src/routes/schema.rs` and `api/convert.rs` `schema_response`.
- `crates/server/src/test_support/mod.rs`: `TestApp`, `seed`, `Seeded.user_id`, `add_project`, `add_member`, `personal_token` (H3), `TestRequest`, `assert_logs_clean`.

You do not need to read the `rmcp` source: § rmcp API below is everything H6 uses, checked against 3.10.0 with a scratch build. If a name below does not compile, stop and report (README STOP rules).

## Files

| File | Change |
|---|---|
| `Cargo.toml` (root) | `rmcp` in `[workspace.dependencies]` |
| `crates/server/Cargo.toml` | `rmcp.workspace = true` |
| `Cargo.lock` | updated by cargo |
| `crates/server/src/lib.rs` | `pub(crate) mod mcp;` |
| `crates/server/src/mcp/mod.rs` | new: route, handler, `McpServer` (`ServerHandler`), tests |
| `crates/server/src/mcp/tools.rs` | new: tool list, `list_projects`, `get_schema`, argument parsing |
| `crates/server/src/mcp/output.rs` | new: rendering, the list fitter, tool errors |
| `crates/server/src/http/access.rs` | `Access::Pat`, `user_project`, `Self::Pat` in `reads_by_post`, `prepare` arm, `routes()` entry, `Access::Pat` in the test `route_table_matches_parameters_and_openapi_security` |
| `crates/server/src/http/access_suite.rs` | `Access::Pat` in `checked_routes`, `expected`, `assert_every_route_declares_access` |
| `crates/server/src/http/error.rs` | `ApiError::problem` and a new `ApiError::log` become `pub(crate)` |
| `crates/server/src/auth/limits.rs` | `McpLimits`, `McpGuard`, `Limits::mcp`, a test-only `McpLimits::in_flight` |
| `crates/cli/src/main.rs` | `log_filter`: `llmobs serve` always caps `rmcp` at `warn`; one unit test |

No OpenAPI change: `/mcp` is `documented: false`, like `/v1/traces` (README § Design → MCP server). No spec change.

## rmcp API (3.10.0)

Everything H6 needs, with the exact paths:

```rust
use rmcp::model::{
    CallToolRequestParams,  // .name: Cow<'static, str>, .arguments: Option<JsonObject>
    CallToolResponse,       // impl From<CallToolResult>
    CallToolResult,         // ::success(Vec<ContentBlock>), ::error(Vec<ContentBlock>); #[non_exhaustive]
    ContentBlock,           // ::text(impl Into<String>)
    Implementation,         // ::new(name, version)
    JsonObject,             // = serde_json::Map<String, serde_json::Value>
    ListToolsResult,        // ::with_all_items(Vec<Tool>)
    PaginatedRequestParams,
    ServerCapabilities,     // ::builder().enable_tools().build()
    ServerConfig,           // = InitializeResult; ::new(caps).with_server_info(..).with_instructions(..)
    Tool,                   // ::new(name, description, Arc<JsonObject>).with_title(..).with_annotations(..)
    ToolAnnotations,        // ::new().read_only(true)
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler};
```

- `ServerHandler: Sized + Send + Sync + 'static`. Every method has a default. Implement three, with `async fn` where the trait returns a future:
  ```rust
  fn get_info(&self) -> ServerConfig;
  async fn list_tools(&self, request: Option<PaginatedRequestParams>, context: RequestContext<RoleServer>) -> Result<ListToolsResult, McpError>;
  async fn call_tool(&self, request: CallToolRequestParams, context: RequestContext<RoleServer>) -> Result<CallToolResponse, McpError>;
  ```
  Leave `initialize`, `supported_protocol_versions` (all known versions: 2024-11-05 through 2026-07-28), `ping` and the rest at their defaults.
- `McpError::invalid_params(message: impl Into<Cow<'static, str>>, data: Option<Value>)` is JSON-RPC error `-32602`.
- `StreamableHttpServerConfig` is built from `Default` with its `with_*` methods (it has private fields; no struct literal): `with_legacy_session_mode(bool)`, `with_json_response(bool)`, `with_sse_keep_alive(Option<Duration>)`, `with_sse_retry(Option<Duration>)`, `disable_allowed_hosts()`, `disable_allowed_origins()`, `with_max_request_body_bytes(usize)`.
- `StreamableHttpService::new(factory: impl Fn() -> Result<S, std::io::Error> + Send + Sync + 'static, session_manager: Arc<M>, config) -> Self`, and `pub async fn handle<B>(&self, request: http::Request<B>) -> http::Response<BoxBody<Bytes, Infallible>>` (`B: http_body::Body + Send + 'static`, `B::Error: Display`; `axum::body::Body` qualifies).
- Behaviour of that config (stateless, JSON), which the tests below pin down:
  - the request must carry `Accept` listing both `application/json` and `text/event-stream`, or rmcp answers `406` (plain text); `Content-Type` must be `application/json`, or `415`; a body that is not valid JSON-RPC is also `415`, with serde's error text (which does not quote the input); a body over the limit is `413`;
  - the request must carry a `Host` header (or an absolute URI); without one rmcp answers `400`. Real HTTP/1.1 clients always send it;
  - `initialize` answers `200 application/json` with the negotiated `protocolVersion`: the requested version when it is 2024-11-05, 2025-03-26, 2025-06-18 or 2025-11-25, else `2025-11-25` (the newest version with `initialize`). An `MCP-Protocol-Version` header on `initialize`, when present, must equal the body's version. A notification answers `202` with an empty body; every other request is served on its own, with no `Mcp-Session-Id`. A request without the header is treated as 2025-03-26; an unknown header version is `400` (plain text). Results to pre-2026 versions carry no `resultType`;
  - protocol `2026-07-28` has no `initialize`: each request carries `MCP-Protocol-Version: 2026-07-28`, `Mcp-Method: <method>`, `Mcp-Name: <tool name>` (for `tools/call`) and `params._meta` with `"io.modelcontextprotocol/protocolVersion": "2026-07-28"` and `"io.modelcontextprotocol/clientCapabilities": {}`. Its results carry `"resultType": "complete"`; rmcp adds that itself. Without the `_meta` keys rmcp answers `400` with JSON-RPC error `-32602`; without `Mcp-Method`, `400` with JSON-RPC error `-32020`. `server/discover` answers `supportedVersions` (the five versions above, oldest first).

**Host and Origin checks (D-H-2).** rmcp's default allows only loopback `Host` values, to stop DNS rebinding against a local server with no auth. `/mcp` refuses every request without a PAT in the `Authorization` header before rmcp sees it, and a browser cannot attach that header to a rebinding request, so the Host check adds nothing; behind a reverse proxy it would refuse requests whose `Host` is the upstream name. H6 therefore calls `disable_allowed_hosts()` and `disable_allowed_origins()`, as README § Design → MCP server says.

## Steps

### 1. Dependency

In the root `Cargo.toml`, `[workspace.dependencies]`, add after the `axum-extra` entry:

```toml
# MCP server (D-H-2): streamable HTTP, stateless, JSON responses. `server` gives
# `ServerHandler`; no `macros`: the server implements it by hand.
rmcp = { version = "3.10.0", default-features = false, features = [
    "server",
    "transport-streamable-http-server",
] }
```

In `crates/server/Cargo.toml` `[dependencies]`, add `rmcp.workspace = true` after `hex.workspace = true`. Run `cargo build -p llmobs-server` once so `Cargo.lock` updates. `cargo deny` warns about duplicate versions (`multiple-versions = "warn"`); that is expected and not a failure. Any `deny` *error* is a STOP.

### 2. `ApiError::problem` and `ApiError::log` (`http/error.rs`)

- Make `fn problem(&self) -> Problem` `pub(crate)`.
- Move the two logging blocks at the top of `IntoResponse::into_response` into a new method, and call it there in their place:

```rust
impl ApiError {
    /// Logs what the caller does not see: an internal source at `error`, a query failure's
    /// kind at `error` (internal) or `info` (refused). Never a message or the query (D94).
    pub(crate) fn log(&self) {
        if let Self::Internal(source) = self {
            tracing::error!(error = %source, "internal error");
        }
        if let Self::Query(error) | Self::Sql(error) = self {
            if matches!(error, RunError::Internal(_)) {
                tracing::error!(kind = error.kind(), "query failed");
            } else {
                tracing::info!(kind = error.kind(), "query refused");
            }
        }
    }
}
```

`into_response` starts with `self.log();` and is otherwise unchanged. (`ApiError::Sql` is H4's variant for SQL text errors; H4 already changed the logging block to match it.)

### 3. Access (`http/access.rs`)

**3a. The variant.** Add to `enum Access`, after `Key`:

```rust
    /// A personal access token, read or read-write; sessions and API keys get 403.
    /// No `{org}` or `{project}` in the path: the handler resolves projects itself (D-H-3).
    Pat,
```

**3b. `reads_by_post`.** H1 wrote `reads_by_post(self, method: &Method) -> bool` as `*method == Method::POST && matches!(self, Self::Project { perm: ProjectPerm::Query, .. })`. Extend the pattern inside the `matches!` to `Self::Project { perm: ProjectPerm::Query, .. } | Self::Pat` (a read-only PAT may POST to `/mcp`: every tool reads, D-H-4). Update its doc comment to mention `/mcp`.

**3c. `prepare`.** In the `match access` of `prepare`, add after the `Access::Key { scope }` arm:

```rust
        Access::Pat => {
            if !matches!(principal, Principal::Pat { .. }) {
                return Err(forbidden());
            }
        }
```

The read-PAT check above the match (H1: `&& !access.reads_by_post(&parts.method)`) already lets a read PAT through on `POST /mcp`.

**3d. `user_project`.** Split `grant_session_project` in two. The new function holds its whole body except the last `parts.extensions.insert(...)`, and returns the `ProjectAccess` instead:

```rust
/// The project `key` names, as `user` reaches it with `perm` (D138). `key` is a project UUID
/// or `org-slug/project-slug`. 404 when the project or its org is missing or not active, when
/// the user is not in the org, when an override hides it, and for `current` (a key's project);
/// 403 when the effective role is below `perm.min_role()`.
///
/// # Errors
///
/// [`ApiError::NotFound`], [`ApiError::Forbidden`], or a `meta.db` failure.
pub(crate) async fn user_project(
    state: &AppState,
    user: &crate::auth::principal::AuthUser,
    key: &str,
    perm: ProjectPerm,
) -> Result<ProjectAccess, ApiError> {
    // the old body of grant_session_project, ending with:
    // Ok(ProjectAccess { project, role: Some(role) })
}

async fn grant_session_project(
    state: &AppState,
    parts: &mut Parts,
    user: &crate::auth::principal::AuthUser,
    key: &str,
    perm: ProjectPerm,
) -> Result<(), ApiError> {
    let access = user_project(state, user, key, perm).await?;
    parts.extensions.insert(access);
    Ok(())
}
```

No behaviour change for the existing routes.

**3e. The route-table test.** In the test `route_table_matches_parameters_and_openapi_security` (test module of `access.rs`), the `let expected = match route.access { … }` match lists every variant; add `| Access::Pat` to its last arm (the one that yields `None`). `/mcp` is not documented, so the loop `continue`s before that match for it; the arm only keeps the match exhaustive. The first match of that test already has a `_` arm, which covers `Pat` (no `{org}` or `{project}` in `/mcp`).

**3f. Route table.** In `routes()`, add `routes.extend(crate::mcp::routes());` after `routes.extend(crate::routes::media::routes());`.

### 4. The authorization suite (`http/access_suite.rs`)

- `checked_routes`: the filter becomes `route.documented || matches!(route.access, Access::Key { .. } | Access::Pat)`. Update its doc comment: "Documented routes, every key route (which stays out of the document, D-G-6) and `/mcp`."
- `expected`: add an arm after `Access::Key { scope } => {…}`:
  ```rust
        Access::Pat => {
            if matches!(who, Who::PatRead | Who::PatWrite) {
                Outcome::Allowed
            } else {
                Outcome::Denied403
            }
        }
  ```
  (`Who::Anonymous` already returned 401 above. The read-PAT line, which H1 changed to `if who == Who::PatRead && !safe(method) && !access.reads_by_post(method)`, does not stop `PatRead` on `POST /mcp`; leave that line as H1 wrote it.)
- `assert_every_route_declares_access`: add `| Access::Pat` to the match.

The suite sends `POST /mcp` with body `{}` and no `Accept`; for allowed callers rmcp answers `406`, but the `x-test-access` header is `allowed`, which is what the suite checks.

### 5. Test helpers

Use the helpers H3 added to `crates/server/src/test_support/mod.rs` (README § Contracts between sub-phases): `Seeded.user_id` (ada's id), `TestApp::add_member(&seeded, email, role) -> Member { id, cookie }` and `TestApp::personal_token(user_id, write) -> String` (`write: false` gives a `[read]` token, `true` a `[read, write]` one). Do not add others, and do not touch `test_support/mod.rs`.

### 6. `McpLimits` (`auth/limits.rs`)

Add (with `use std::sync::Arc;`):

```rust
/// `POST /mcp` requests one token may make per minute (D-H-10).
pub(crate) const MCP_PER_MINUTE: u32 = 60;
/// `POST /mcp` requests one token may have in progress at once (D-H-10).
pub(crate) const MCP_IN_FLIGHT: u32 = 4;

/// Rate and concurrency of `POST /mcp`, per personal access token id.
pub(crate) struct McpLimits {
    rate: KeyedLimiter<Uuid>,
    /// Requests in progress per token. A token with none has no entry.
    in_flight: Arc<Mutex<HashMap<Uuid, u32>>>,
}

impl McpLimits {
    fn new(max_keys: usize) -> Self {
        Self {
            rate: KeyedLimiter::new(
                Bucket {
                    capacity: MCP_PER_MINUTE,
                    per: Duration::from_secs(60),
                },
                max_keys,
            ),
            in_flight: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Admits one request of `token`. The request counts as in progress until the guard drops.
    ///
    /// # Errors
    ///
    /// [`RetryAfter`] of 1 s when `token` already has [`MCP_IN_FLIGHT`] requests in progress
    /// (no rate token is spent), or the rate limiter's [`RetryAfter`].
    pub(crate) fn admit(&self, token: Uuid, now: i64) -> Result<McpGuard, RetryAfter> {
        let mut in_flight = lock(&self.in_flight);
        let current = in_flight.get(&token).copied().unwrap_or(0);
        if current >= MCP_IN_FLIGHT {
            return Err(RetryAfter(Duration::from_secs(1)));
        }
        self.rate.check(&token, now)?;
        in_flight.insert(token, current + 1);
        Ok(McpGuard {
            in_flight: Arc::clone(&self.in_flight),
            token,
        })
    }

    fn prune(&self, now: i64) {
        self.rate.prune(now);
    }

    /// Requests of `token` in progress. Tests only.
    #[cfg(test)]
    pub(crate) fn in_flight(&self, token: Uuid) -> u32 {
        lock(&self.in_flight).get(&token).copied().unwrap_or(0)
    }
}

/// One admitted `POST /mcp` request. Dropping it ends the request's in-progress slot.
pub(crate) struct McpGuard {
    in_flight: Arc<Mutex<HashMap<Uuid, u32>>>,
    token: Uuid,
}

impl Drop for McpGuard {
    fn drop(&mut self) {
        let mut in_flight = lock(&self.in_flight);
        if let Some(count) = in_flight.get_mut(&self.token) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                in_flight.remove(&self.token);
            }
        }
    }
}
```

In `struct Limits` add, after `feedback`:

```rust
    /// 60 requests per minute and 4 at once per personal access token on `POST /mcp` (D-H-10).
    pub(crate) mcp: McpLimits,
```

`Limits::new` sets `mcp: McpLimits::new(MAX_KEYS)`, and `Limits::prune` calls `self.mcp.prune(now);` after `self.feedback.prune(now);`.

### 7. `mcp/output.rs`

```rust
//! What an MCP tool returns: compact JSON text, fitted to a character budget (D-H-11).

use rmcp::model::{CallToolResult, ContentBlock};
use serde_json::{Map, Value, json};

use crate::http::error::ApiError;

/// The default `max_chars` of a tool result (D-H-10).
pub(crate) const DEFAULT_MAX_CHARS: usize = 20_000;

/// Compact JSON, as sent to the agent. `Value`'s `Display` cannot fail.
pub(crate) fn render(value: &Value) -> String {
    value.to_string()
}

/// Length in characters (Unicode scalar values), the unit of every budget.
pub(crate) fn char_len(text: &str) -> usize {
    text.chars().count()
}

/// A successful tool result: one text item holding `body` as compact JSON.
pub(crate) fn success(body: &Value) -> CallToolResult {
    CallToolResult::success(vec![ContentBlock::text(render(body))])
}

/// A failed tool result: `{"error": {"code", "message", "position"?}}` with the spec 09
/// problem code. The message is the first field error's message when there is one (it carries
/// the position), else the problem's detail, else its title. Logged like an HTTP error.
pub(crate) fn failure(error: &ApiError) -> CallToolResult {
    error.log();
    let problem = error.problem();
    // `errors` is borrowed by `first()`; `detail` and `title` are other fields, so moving them is allowed.
    let (message, position) = match problem.errors.first() {
        Some(field) => (field.message.clone(), field.position),
        None => (problem.detail.unwrap_or(problem.title), None),
    };
    let mut inner = Map::new();
    inner.insert("code".to_owned(), json!(problem.code));
    inner.insert("message".to_owned(), Value::String(message));
    if let Some(position) = position {
        inner.insert(
            "position".to_owned(),
            json!({"line": position.line, "column": position.column}),
        );
    }
    CallToolResult::error(vec![ContentBlock::text(render(&json!({"error": inner})))])
}

/// Fits `body` in `max_chars` by keeping the longest prefix of the array `body[list]`.
///
/// Returns `false` and leaves `body` unchanged when it already fits, or when `body[list]` is
/// not an array. Otherwise sets `body["truncated"]` to `true`, appends `note` to the array
/// `body["notes"]` (creating either key when missing), replaces `body[list]` in place (key order
/// is kept) by its longest prefix that makes the whole body fit — possibly empty — and returns
/// `true`. The prefix length is found by binary search.
pub(crate) fn fit_list(
    body: &mut Map<String, Value>,
    list: &str,
    max_chars: usize,
    note: &str,
) -> bool
```

Implementation of `fit_list`:

1. `if char_len(&render_map(body)) <= max_chars { return false; }` where `fn render_map(body: &Map<String, Value>) -> String { Value::Object(body.clone()).to_string() }` (private).
2. `let items = match body.get(list) { Some(Value::Array(items)) => items.clone(), _ => return false };`
3. `body.insert("truncated".to_owned(), Value::Bool(true));` (`insert` on an existing key keeps its position with `preserve_order`). Then append `Value::String(note.to_owned())` to `body["notes"]`: if `body.get_mut("notes")` is `Some(Value::Array(notes))` push; otherwise insert `"notes": [note]`.
4. Binary search: `let (mut low, mut high) = (0, items.len());` While `low < high`: `let mid = (low + high + 1) / 2;` set `body[list] = Value::Array(items[..mid].to_vec())` (through `if let Some(slot) = body.get_mut(list) { *slot = … }`); if `char_len(&render_map(body)) <= max_chars` then `low = mid` else `high = mid - 1`.
5. Set `body[list]` to `items[..low]` the same way; return `true`.

### 8. `mcp/tools.rs`

```rust
//! The tool list and the tools that need no query (D-H-11).

use std::sync::Arc;

use rmcp::model::{JsonObject, Tool, ToolAnnotations};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};

use crate::api::convert;
use crate::auth::principal::AuthUser;
use crate::auth::roles::{self, ProjectPerm, Role};
use crate::http::AppState;
use crate::http::access::user_project;
use crate::http::error::ApiError;
use crate::meta::members::project_override;
use crate::meta::orgs::orgs_of_user;
use crate::meta::projects::projects_of_org;
use super::output::{self, DEFAULT_MAX_CHARS};

pub(crate) const LIST_PROJECTS: &str = "list_projects";
pub(crate) const GET_SCHEMA: &str = "get_schema";

/// The description of the `project` argument, shared by every tool that takes one.
pub(crate) const PROJECT_ARG: &str =
    "Project id (UUID) or `org-slug/project-slug`, as `list_projects` returns them.";
```

**Tool definitions.** One function builds the list, in this order (H7 and H8 append theirs):

```rust
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
    ]
}

/// A read-only tool. `schema` is a JSON object; anything else becomes `{}`.
pub(crate) fn tool(name: &'static str, title: &'static str, description: &'static str, schema: Value) -> Tool {
    let schema: JsonObject = match schema {
        Value::Object(map) => map,
        _ => Map::new(),
    };
    Tool::new(name, description, Arc::new(schema))
        .with_title(title)
        .with_annotations(ToolAnnotations::new().read_only(true))
}
```

**Arguments.**

```rust
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
```

**`list_projects`.**

```rust
/// `{"projects": [{id, org, slug, name, role}], "truncated": false, "notes": []}`: the active
/// projects of the user's active orgs that no override hides, ordered like `orgs_of_user`
/// (org name, org id) then `projects_of_org` (project name, project id). `org` is the org slug;
/// `role` is the effective project role, `viewer`, `member` or `admin`. Fitted to
/// `DEFAULT_MAX_CHARS` with `fit_list` on `projects` and the note
/// `"result cut at 20000 characters; later projects left out"`.
pub(crate) async fn list_projects(state: &AppState, user: &AuthUser, raw: JsonObject) -> Result<Value, ApiError>
```

This repeats the visibility rule of `routes::projects::list_projects` and of `user_project` (active org and project, `effective_project_role`, an override `none` hides); keep them in sync (a comment on the function says so). Body: `let _: NoArguments = arguments(raw)?;` then for each `(org, org_role)` of `orgs_of_user(state.meta.reader(), user.id)` with `org.state == "active"`, for each `project` of `projects_of_org(state.meta.reader(), org.id)` with `project.state == "active"`: `let ov = project_override(state.meta.reader(), project.id, user.id).await?;` and `roles::effective_project_role(org_role, ov)`; skip `None`; push `json!({"id": project.id.to_string(), "org": org.slug, "slug": project.slug, "name": project.name, "role": role_name(role)})`. Build `let mut body = Map::new();` with keys in this order: `projects`, `truncated` (`false`), `notes` (`[]`). Call `output::fit_list(&mut body, "projects", DEFAULT_MAX_CHARS, "result cut at 20000 characters; later projects left out");` and return `Value::Object(body)`.

```rust
fn role_name(role: Role) -> &'static str {
    match role {
        Role::Viewer => "viewer",
        Role::Member => "member",
        Role::Admin => "admin",
    }
}
```

**`get_schema`.**

```rust
/// `GET /schema`'s body for a project the user can view (`ProjectPerm::View`). The catalog is
/// the same for every project and is about 15,000 characters, so it is not fitted and the tool
/// takes no `max_chars` (README § Design → MCP server).
pub(crate) async fn get_schema(state: &AppState, user: &AuthUser, raw: JsonObject) -> Result<Value, ApiError>
```

Body: `let ProjectArguments { project } = arguments(raw)?; user_project(state, user, &project, ProjectPerm::View).await?;` then `serde_json::to_value(convert::schema_response()).map_err(|error| ApiError::Internal(Box::new(error)))`.

### 9. `mcp/mod.rs`

```rust
//! The MCP server at `POST /mcp` (spec 09 § MCP server, D-H-2, D-H-3, D-H-10, D-H-11).
//!
//! Streamable HTTP through `rmcp`, stateless, with JSON responses: every POST is served on its
//! own by a fresh [`McpServer`] that holds the caller. Only personal access tokens reach it.

mod output;
mod tools;

use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use axum::extract::{Extension, Request, State};
use axum::http::Method;
use axum::response::Response;
use axum::routing::post;
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, Implementation, ListToolsResult,
    PaginatedRequestParams, ServerCapabilities, ServerConfig,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::session::never::NeverSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler};

use crate::api::common::ProblemCode;
use crate::auth::principal::{AuthUser, Principal};
use crate::http::AppState;
use crate::http::access::{Access, RouteDef};
use crate::http::error::ApiError;

/// Largest JSON-RPC body rmcp reads (it answers 413 above this).
const MCP_BODY_LIMIT: usize = 1024 * 1024;

/// Instructions sent with `initialize`.
const INSTRUCTIONS: &str = "Read-only access to LLM traces and spans. Call list_projects first: the other tools take a `project`. Each tool's description says what it needs. Results are compact JSON; when a result is cut, `truncated` is true and `notes` says what was left out.";

/// `POST /mcp`. Not in the OpenAPI document.
pub(crate) fn routes() -> Vec<RouteDef> {
    vec![RouteDef {
        method: Method::POST,
        path: "/mcp",
        access: Access::Pat,
        documented: false,
        handler: post(mcp),
    }]
}
```

**The handler.**

```rust
/// Admits the token (D-H-10), then hands the request to rmcp with a server bound to the caller.
async fn mcp(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    request: Request,
) -> Result<Response, ApiError> {
    // The access layer admits only a PAT here (`Access::Pat`).
    let Principal::Pat { user, token_id, .. } = principal else {
        return Err(ApiError::Forbidden { code: ProblemCode::Forbidden });
    };
    let _guard = state
        .limits
        .mcp
        .admit(token_id, state.clock.now_ns())
        .map_err(|retry| ApiError::RateLimited {
            retry_after_s: retry.0.as_secs().max(1),
        })?;
    let server = McpServer { state, user };
    let service = StreamableHttpService::new(
        move || Ok(server.clone()),
        Arc::new(NeverSessionManager::default()),
        service_config(),
    );
    Ok(service.handle(request).await.map(Body::new))
}
```

If clippy reports `large_futures` on `service.handle(request)`, write `Box::pin(service.handle(request)).await`; add no `#[allow]`.

```rust
fn service_config() -> StreamableHttpServerConfig {
    StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_sse_keep_alive(None)
        .with_sse_retry(None)
        .disable_allowed_hosts()
        .disable_allowed_origins()
        .with_max_request_body_bytes(MCP_BODY_LIMIT)
}
```

`_guard` is held until `handle` returns: with `json_response` the whole JSON answer is built by then.

**The server.**

```rust
/// One request's MCP server: the app and the token's user.
#[derive(Clone)]
pub(crate) struct McpServer {
    state: AppState,
    user: AuthUser,
}

impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                llmobs_core::names::PRODUCT,
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(tools::tool_list()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let started = Instant::now();
        let arguments = request.arguments.unwrap_or_default();
        let (tool, outcome) = match &*request.name {
            tools::LIST_PROJECTS => (
                tools::LIST_PROJECTS,
                tools::list_projects(&self.state, &self.user, arguments).await,
            ),
            tools::GET_SCHEMA => (
                tools::GET_SCHEMA,
                tools::get_schema(&self.state, &self.user, arguments).await,
            ),
            _ => return Err(McpError::invalid_params("unknown tool", None)),
        };
        let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let result = match outcome {
            Ok(body) => {
                tracing::info!(target: "mcp", tool, user_id = %self.user.id, status = 200u16, duration_ms, "tool call");
                output::success(&body)
            }
            Err(error) => {
                let status = error.problem().status;
                tracing::info!(target: "mcp", tool, user_id = %self.user.id, status, duration_ms, "tool call");
                output::failure(&error)
            }
        };
        Ok(result.into())
    }
}
```

`const` items of type `&str` are valid patterns, so the match above compiles as written. If clippy reports `large_futures` on a tool call, wrap that call in `Box::pin(...)`; add no `#[allow]`. Logs carry the tool name (a constant), never arguments or results. rmcp runs `call_tool` inside a task it spawns (`tokio::spawn`), so the log line is emitted on that task.

The factory passed to `StreamableHttpService::new` is an `Fn`, so it must clone `server` on each call (rmcp calls it once per stateless request); `server` itself is built by moving `state` and `user`, without extra clones.

The tools' `inputSchema` is documentation for the agent: rmcp does not validate arguments against it. The serde structs (`deny_unknown_fields`) are the validation.

### 10. `lib.rs`

Add `pub(crate) mod mcp;` after `pub(crate) mod live;`.

### 11. Cap rmcp's logs (`crates/cli/src/main.rs`)

rmcp logs whole JSON-RPC requests and results (tool arguments, SQL text, payloads) at `debug` and `trace` (`rmcp` `service.rs` and `transport/streamable_http_server/tower.rs`). README § Conventions → Privacy and spec 12 say `llmobs serve` always adds `rmcp=warn` to the log filter, whatever `log.level` says. In `crates/cli/src/main.rs`, replace the first line of `init_tracing`

```rust
    let filter = EnvFilter::try_new(&log.level).context("loading config")?;
```

with `let filter = log_filter(&log.level)?;` and add:

```rust
/// The `rmcp` crate logs whole MCP requests and results below `warn`; they can hold prompts
/// and SQL text, so its level is capped whatever `log.level` says (README § Privacy).
const RMCP_DIRECTIVE: &str = "rmcp=warn";

/// `log.level` as an `EnvFilter`, with [`RMCP_DIRECTIVE`] added last so it replaces any
/// `rmcp=…` directive in `level`.
fn log_filter(level: &str) -> anyhow::Result<EnvFilter> {
    let directive = RMCP_DIRECTIVE.parse().context("parsing the rmcp log directive")?;
    Ok(EnvFilter::try_new(level)
        .context("loading config")?
        .add_directive(directive))
}
```

Unit test in `main.rs`'s `mod tests`, `log_filter_caps_rmcp`: for each `level` in `"info"`, `"trace"` and `"info,rmcp=trace"`, `log_filter(level).unwrap().to_string()` contains `rmcp=warn` and does not contain `rmcp=trace`. If the last case still contains `rmcp=trace` (the directive did not replace it), STOP and report.

## Tests

### Shared helpers

Put these in a module at the end of `mcp/mod.rs`, so H7 and H8 can reuse them with `use crate::mcp::test_helpers::*;`:

```rust
#[cfg(test)]
pub(crate) mod test_helpers {
    use axum::http::{Method, StatusCode};
    use serde_json::{Value, json};

    use crate::test_support::{Seeded, TestApp, TestResponse};

    pub(crate) const V2025: &str = "2025-11-25";
    pub(crate) const V2026: &str = "2026-07-28";

    /// `POST /mcp` with `host` and `accept` set as every MCP client sends them, the optional
    /// bearer, an optional `mcp-protocol-version`, and `extra` headers.
    pub(crate) async fn post_mcp(
        app: &TestApp,
        bearer: Option<&str>,
        version: Option<&str>,
        extra: &[(&str, &str)],
        body: &Value,
    ) -> TestResponse {
        let mut request = app
            .request(Method::POST, "/mcp")
            .header("host", "127.0.0.1:8080")
            .header("accept", "application/json, text/event-stream")
            .json(body);
        if let Some(version) = version {
            request = request.header("mcp-protocol-version", version);
        }
        if let Some(bearer) = bearer {
            request = request.header("authorization", &format!("Bearer {bearer}"));
        }
        for (name, value) in extra {
            request = request.header(name, value);
        }
        request.send().await
    }

    /// [`post_mcp`] with protocol 2025-11-25 and no extra header.
    pub(crate) async fn rpc(app: &TestApp, bearer: Option<&str>, body: &Value) -> TestResponse {
        post_mcp(app, bearer, Some(V2025), &[], body).await
    }

    /// `tools/call` of `name`; returns `(is_error, parsed text content)`.
    /// Panics on an HTTP status other than 200 or a JSON-RPC error.
    pub(crate) async fn call(app: &TestApp, token: &str, name: &str, arguments: &Value) -> (bool, Value) {
        let response = rpc(
            app,
            Some(token),
            &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                    "params": {"name": name, "arguments": arguments}}),
        )
        .await;
        assert_eq!(response.status, StatusCode::OK);
        let body: Value = response.json();
        let result = &body["result"];
        let text = result["content"][0]["text"].as_str().expect("text content");
        (result["isError"] == json!(true), serde_json::from_str(text).unwrap())
    }

    /// Runs `future` under its own subscriber that keeps what production keeps: `info` and
    /// above, `rmcp` at `warn` (the directive `llmobs serve` adds, step 11). Returns the
    /// formatted output. Use it, not `test_support::capture_logs` (which captures every level,
    /// rmcp's debug dumps included), for MCP log tests, on a current-thread runtime.
    pub(crate) async fn capture_info_logs<T>(future: impl std::future::Future<Output = T>) -> (T, String) {
        use tracing_subscriber::Layer;
        use tracing_subscriber::layer::SubscriberExt;
        let buffer = std::sync::Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
        let writer = std::sync::Arc::clone(&buffer);
        let filter = tracing_subscriber::filter::Targets::new()
            .with_default(tracing::Level::INFO)
            .with_target("rmcp", tracing::Level::WARN);
        let layer = tracing_subscriber::fmt::layer()
            .with_writer(move || Buffer(std::sync::Arc::clone(&writer)))
            .with_ansi(false)
            .with_target(true)
            .with_filter(filter);
        let dispatch = tracing::Dispatch::new(tracing_subscriber::registry().with(layer));
        let value = {
            let _guard = tracing::dispatcher::set_default(&dispatch);
            future.await
        };
        drop(dispatch);
        let bytes = buffer.lock().expect("log buffer").clone();
        (value, String::from_utf8_lossy(&bytes).into_owned())
    }

    /// `io::Write` over the shared buffer of [`capture_info_logs`].
    struct Buffer(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Buffer {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("log buffer").write(data)
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A seeded app and a read-only PAT of ada (org owner, so project admin).
    pub(crate) async fn app_with_token() -> (TestApp, Seeded, String) {
        let app = TestApp::new().await;
        let seeded = app.seed().await;
        let token = app.personal_token(seeded.user_id, false).await;
        (app, seeded, token)
    }
}
```

Use every item above in H6's own tests (`V2026` in the 2026-07-28 tests, `post_mcp` in the header tests, `capture_info_logs` in the log tests), so no `dead_code` warning appears. In the tables below, a JSON argument or body is passed as `&json!(…)`.

### HTTP tests (`#[cfg(test)] mod tests` in `mcp/mod.rs`)

Imports: `std::time::Duration`; `axum::http::{Method, StatusCode}`; `serde_json::{Value, json}`; `super::test_helpers::*`; `super::{INSTRUCTIONS, output, tools}`; `crate::api::convert`; `crate::auth::roles::{OrgRole, ProjectOverride}`; `crate::auth::secret::{self, KeyKind}`; `crate::meta::members::set_project_override`; `crate::meta::personal_tokens::{self, NewToken}`; `crate::meta::users::{self, NewUser, insert_user}`; `crate::meta::orgs::insert_org`; `crate::meta::projects::insert_project`; `crate::meta::members::add_org_member`; `crate::auth::roles::KeyScope`; `crate::test_support::TestApp`. Drop any import the compiler reports unused.

Every test is `#[tokio::test(flavor = "multi_thread")]` except the two log tests, which are `#[tokio::test]` (current thread: `capture_info_logs` sets the thread's default subscriber, and rmcp runs `call_tool` in a `tokio::spawn`ed task, which on the current-thread runtime runs on the same thread). `tok` below is the token from `app_with_token()`. JSON ids are written with `.to_string()` (`seeded.project.id.to_string()`). `"tools/list"` means the body `{"jsonrpc":"2.0","id":2,"method":"tools/list"}`.

**Protocol**

| Test | Steps | Asserts |
|---|---|---|
| `initialize_negotiates_2025_11_25` | `rpc(&app, Some(&tok), {"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}})` | 200; `content-type` = `application/json`; no `mcp-session-id` header; `result.protocolVersion` = `"2025-11-25"`; `result.serverInfo.name` = `llmobs_core::names::PRODUCT`; `result.serverInfo.version` = `env!("CARGO_PKG_VERSION")`; `result.capabilities.tools` = `{}`; `result.instructions` = `INSTRUCTIONS` |
| `initialize_negotiates_older_and_unknown_versions` | for `v` in `2024-11-05`, `2025-03-26`, `2025-06-18`: `post_mcp(&app, Some(&tok), Some(v), &[], <initialize with protocolVersion v>)`; then `post_mcp(&app, Some(&tok), None, &[], <initialize with protocolVersion "2099-01-01">)` | each: 200 and `result.protocolVersion` = `v`; the unknown one: 200 and `"2025-11-25"` |
| `initialized_notification_is_accepted` | `rpc` with `{"jsonrpc":"2.0","method":"notifications/initialized"}` | 202, empty body |
| `tools_list_has_every_tool_read_only` | `rpc` `tools/list` | `result.tools[*].name` = `["list_projects", "get_schema"]`; every tool has `annotations.readOnlyHint` = `true` and `inputSchema.type` = `"object"`; `list_projects`'s `inputSchema.required` is absent; `get_schema`'s is `["project"]`; `result.resultType` is absent (pre-2026 result) |
| `unknown_tool_is_a_protocol_error` | `tools/call` with `name` `"nope"` | 200; `error.code` = `-32602`; `error.message` = `"unknown tool"`; no `result` |
| `stateless_2026_07_28_requests` | headers `extra` = `[("mcp-method", M), ("mcp-name", N)]` as listed, version `Some(V2026)`; every body has `params._meta` = `{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientCapabilities":{}}`. (a) `tools/call` `list_projects` (`M` = `tools/call`, `N` = `list_projects`, `arguments` `{}`); (b) `tools/list` (`M` = `tools/list`, no `mcp-name`); (c) `server/discover` (`M` = `server/discover`) | (a) 200; `result.resultType` = `"complete"`; `result.isError` = `false`; parsed `result.content[0].text` has `projects[0].slug` = `"support-bot"`. (b) 200; `result.resultType` = `"complete"`; `result.tools[*].name` as below. (c) 200; `result.supportedVersions` = `["2024-11-05","2025-03-26","2025-06-18","2025-11-25","2026-07-28"]` |
| `stateless_2026_07_28_rejects_incomplete_requests` | version `Some(V2026)`: (a) `tools/list` with `mcp-method: tools/list` but no `params._meta`; (b) `tools/list` with `_meta` but no `mcp-method`; (c) `tools/list` with version `Some("1999-01-01")` and no `_meta` | (a) 400, JSON body `error.code` = `-32602`; (b) 400, `error.code` = `-32020`; (c) 400 (plain text) |
| `client_headers_are_required` | `tools/list` with `tok`: (a) `app.request(POST, "/mcp")` with `host`, `authorization`, `.json(…)`, no `accept`; (b) `accept` but no `host`; (c) `host`, `accept`, `authorization`, `content-type: text/plain` and the JSON text as `.body(…)`; (d) `host`, `accept`, `authorization`, `content-type: application/json`, body `{not json`; (e) like (d) with a valid JSON-RPC `tools/list` whose `params` has `"x"` = 1,100,000 `a` characters; (f) `DELETE /mcp` with `tok` | (a) 406; (b) 400; (c) 415; (d) 415; (e) 413; (f) 405 problem `method_not_allowed`. (a)–(e) are plain-text answers from rmcp; assert the status only |
| `any_host_and_origin_are_accepted` | `post_mcp(&app, Some(&tok), Some(V2025), &[("host", "mcp.example.com"), ("origin", "https://evil.example")], &<tools/list>)` (the `extra` headers replace the defaults: `TestRequest::header` inserts) | 200 with `result.tools` (rmcp's Host and Origin checks are off, step 9; the PAT is the protection) |
| `get_is_method_not_allowed` | `GET /mcp` with `tok` | 405, problem `code` `method_not_allowed` |

In `stateless_2026_07_28_requests` (b), assert `result.tools[*].name` = `["list_projects", "get_schema"]` (H7 and H8 extend this list as they extend `tools_list_has_every_tool_read_only`).

**Auth**

| Test | Steps | Asserts |
|---|---|---|
| `only_personal_tokens_reach_mcp` | `tools/list` via `rpc` with: no bearer; `seeded.read_key`; `seeded.ingest_key`; `seeded.public_key`; `personal_token(seeded.user_id, false)`; `personal_token(seeded.user_id, true)`. Then two hand-built requests (headers as in `post_mcp`): the cookie `seeded.cookie` and no bearer; the cookie and the read PAT together | no bearer: 401 `unauthorized`; the three keys: 403 `forbidden`; both PATs: 200 with `result.tools`; cookie alone: 403 `forbidden`; cookie + PAT: 200 (the bearer wins, D140) |
| `dead_tokens_are_unauthorized` | (a) bearer `pat-llmobs-` followed by 32 `A`; (b) a token from `personal_token(seeded.user_id, false)`, revoked before its first use: id from `personal_tokens::token_by_hash(app.state.meta.reader(), &secret::hash_token(&token)).await.unwrap().unwrap().id`, then `revoke_token(conn, seeded.user_id, id, now)` in `app.state.meta.write`; (c) an expired token: `let minted = secret::new_key(KeyKind::Personal).unwrap();` and `personal_tokens::insert_token(conn, NewToken { user_id: seeded.user_id, name: "old".to_owned(), prefix: minted.prefix.clone(), hash: minted.hash.clone(), scopes: vec![KeyScope::Read], expires_at: Some(now - 1) }, now)`; (d) a token of `app.add_member(&seeded, "gone@example.com", OrgRole::Member)` whose user is then disabled with `users::set_disabled(conn, member.id, Some(now), now)` | each: `tools/list` → 401 `unauthorized`. `now` is `app.state.clock.now_ns()` |

**Limits**

| Test | Steps | Asserts |
|---|---|---|
| `rate_limit_is_sixty_per_minute_per_token` | 60 × `tools/list` with `tok`; one more; a second token (`personal_token(seeded.user_id, false)`) once; `app.state.clock.advance(Duration::from_secs(1))`; `tok` once more | first 60: 200; 61st: 429, problem `code` `rate_limited`, header `retry-after` = `"1"`; second token: 200; after the advance: 200. The test clock is manual, so nothing refills by itself |
| `concurrency_is_four_per_token` | `let id = <tok's id via token_by_hash as above>;` `let now = app.state.clock.now_ns();` hold `let guards: Vec<_> = (0..4).map(|_| app.state.limits.mcp.admit(id, now).unwrap()).collect();`; `tools/list` with `tok`; `drop(guards)`; `tools/list` again; then `call(&app, &tok, "get_schema", {"project": "acme/nope"})` (a tool error) | with the guards held: 429 `rate_limited`, `retry-after` = `"1"`; after the drop: 200 and `app.state.limits.mcp.in_flight(id)` = 0; after the tool error: `in_flight(id)` = 0 |

**Tools**

| Test | Steps | Asserts |
|---|---|---|
| `list_projects_lists_visible_projects` | `let (playground, _) = app.add_project(&seeded, "playground", None).await;` then `call(&app, &tok, "list_projects", {})` | not an error; body = `{"projects":[{"id": seeded.project.id.to_string(),"org":"acme","slug":"support-bot","name":"Support","role":"admin"},{"id": playground.id.to_string(),"org":"acme","slug":"playground","name":"playground","role":"admin"}],"truncated":false,"notes":[]}` (`"Support"` sorts before `"playground"`: SQLite compares bytes) |
| `list_projects_follows_roles_and_overrides` | `let viewer = app.add_member(&seeded, "viewer@example.com", OrgRole::Viewer).await;` `let (playground, _) = app.add_project(&seeded, "playground", None).await;` in `app.state.meta.write`: `set_project_override(conn, playground.id, viewer.id, ProjectOverride::None)`; `app.personal_token(viewer.id, false)`. A user in no org: `insert_user(conn, NewUser { email: "stranger@example.com".to_owned(), name: None, password_hash: None, email_verified_at: None, is_instance_admin: false }, now)` in `app.state.meta.write`, and a token for its id | viewer: `projects` = `[{"id": seeded.project.id.to_string(), "org": "acme", "slug": "support-bot", "name": "Support", "role": "viewer"}]`; stranger: `projects` = `[]` |
| `list_projects_skips_inactive_projects_and_orgs` | `let (gone, _) = app.add_project(&seeded, "gone", None).await;` then in `app.state.meta.write`: `sqlx::query("UPDATE projects SET state = 'deleting' WHERE id = ?").bind(gone.id.to_string()).execute(&mut *conn).await?;` and a second org: `let org = insert_org(conn, "Beta", "beta", now).await?; add_org_member(conn, org.id, seeded.user_id, OrgRole::Owner, now).await?; insert_project(conn, org.id, "Beta app", "beta-app", None, now).await?; sqlx::query("UPDATE orgs SET state = 'deleting' WHERE id = ?").bind(org.id.to_string()).execute(&mut *conn).await?;` | `projects[*].slug` = `["support-bot"]` |
| `list_projects_rejects_arguments` | `call(…, "list_projects", {"x": 1})` | `is_error`; `error.code` = `"bad_request"`; `error.message` starts with ``"invalid arguments: unknown field `x`"`` |
| `list_projects_fits_its_budget` | `for i in 0..200 { app.add_project(&seeded, &format!("p{i:03}"), None).await; }` then `call(…, "list_projects", {})` | `truncated` = `true`; `notes` = `["result cut at 20000 characters; later projects left out"]`; `output::char_len(&output::render(&body))` ≤ 20,000; `projects.len()` < 201; `projects[0].slug` = `"support-bot"`; `projects[1].slug` = `"p000"` |
| `get_schema_is_the_schema_endpoint_body` | `call(…, "get_schema", {"project": seeded.project.id.to_string()})`, and with `{"project": "acme/support-bot"}` | not an error; body = `serde_json::to_value(convert::schema_response()).unwrap()` both times; `output::char_len(&output::render(&body))` < 50,000 (the catalog is about 15,000 characters and takes no `max_chars`; the bound only catches a runaway catalog) |
| `get_schema_errors_are_tool_errors` | `{"project": "acme/nope"}`; `{"project": <id>, "x": 1}`; `{}` | first: `is_error`, body = `{"error":{"code":"not_found","message":"Not Found"}}`; second: `bad_request`, message starts with ``"invalid arguments: unknown field `x`"``; third: `bad_request`, message starts with ``"invalid arguments: missing field `project`"`` |

**Logs** (both `#[tokio::test]`, current thread)

| Test | Steps | Asserts |
|---|---|---|
| `mcp_logs_tool_calls_without_secrets` | `let (_, logs) = capture_info_logs(async { call(&app, &tok, "list_projects", {}).await; call(&app, &tok, "get_schema", {"project": "acme/canary-7f3a9"}).await; rpc(&app, None, <tools/list>).await; }).await;` | `logs` contains `tool call` and `list_projects` and `get_schema`; it contains neither `tok`, nor `canary-7f3a9` |
| `malformed_bodies_are_not_logged` | `let (response, logs) = capture_info_logs(async { <POST /mcp with tok, host, accept, content-type application/json, body {"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"get_schema","arguments":{"project":"CANARY-51b2"> (cut there, so it is not valid JSON)> }).await;` | `response.status` = 415; `logs` does not contain `CANARY-51b2`. If it does, STOP and report |

### Unit tests (plain `#[test]`)

- `auth/limits.rs`, `mcp_admits_four_at_once_and_sixty_a_minute`: `let limits = McpLimits::new(10); let token = Uuid::now_v7();` four `admit(token, 0)` succeed (keep the guards in a `Vec`); the fifth, kept in a variable, satisfies `matches!(fifth.err(), Some(RetryAfter(wait)) if wait == Duration::from_secs(1))` (`McpGuard` has no `Debug`/`PartialEq`, so compare through `.err()`, never `assert_eq!` on the `Result`); another token is admitted; `guards.pop()` drops one, then `admit(token, 0)` succeeds; `in_flight(token)` = 4. Then a fresh `McpLimits::new(10)` and a third token: 60 admits at `now = 0`, each guard dropped at once, succeed; the 61st satisfies the same `matches!` on `.err()`; at `now = 1_000_000_000` it succeeds (keep that guard in a variable and drop it explicitly); `in_flight` of it is then 0.
- `mcp/output.rs`:
  - `fit_list_leaves_a_fitting_body_alone`: `{"items":[1,2,3],"truncated":false,"notes":[]}`, `max_chars` 1000 → `false`, body unchanged.
  - `fit_list_keeps_the_longest_prefix`: `{"items": [100 × "aaaaaaaaaa"], "truncated": false, "notes": []}`, `max_chars` 300, note `"cut"` → `true`; `truncated` = `true`; `notes` = `["cut"]`; the rendered body has ≤ 300 characters and the same body with one more item has more; the key order is `items`, `truncated`, `notes`.
  - `fit_list_can_empty_the_list`: one item of 1,000 `a`, `max_chars` 50 → `items` = `[]`, `truncated` = `true`.
  - `failure_carries_code_message_and_position`: `failure(&ApiError::Query(RunError::Invalid(InvalidQuery { message: "unknown column x".to_owned(), position: Some(Position { line: 1, column: 8 }) })))` (`use llmobs_storage::{InvalidQuery, Position, RunError};`) → `is_error` = `Some(true)`, and the text, read with `result.content[0].as_text().map(|text| text.text.as_str())`, = `{"error":{"code":"invalid_filter","message":"unknown column x","position":{"line":1,"column":8}}}`; the same through `ApiError::Sql` gives the same text; `failure(&ApiError::NotFound)` → `{"error":{"code":"not_found","message":"Not Found"}}`.

The authorization suite (`every_route_against_every_principal`) must pass with `/mcp` in it.

## Commands

```bash
cargo build -p llmobs-server
cargo test -p llmobs-server mcp
cargo test -p llmobs-server access
cargo test -p llmobs-server limits
export PATH="$HOME/.local/share/nvm/v26.13.0/bin:$PATH" && just check
```

## Done when

- `POST /mcp` answers MCP over streamable HTTP for PATs only, statelessly, with JSON responses; `initialize` (2024-11-05 to 2025-11-25) and per-request 2026-07-28 calls both work.
- `tools/list` lists `list_projects` and `get_schema`, read-only; both tools answer compact JSON text; errors are tool results with spec 09 codes; an unknown tool is JSON-RPC `-32602`.
- A token is limited to 60 requests per minute and 4 at once (429 `rate_limited`), and its in-progress slot is released after every response.
- The authorization suite covers `/mcp`.
- `just check` passes; one commit, `server: add the MCP endpoint`, including `Cargo.lock`.

## Report

- New dependency: `rmcp` 3.10.0, `default-features = false`, features `server`, `transport-streamable-http-server`.
