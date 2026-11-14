# H — Console and agents

Implementation plan for track H: the SQL console endpoint and its exports, saved views and saved SQL queries on the server, the MCP server for coding agents, the CLI query commands, and the SQL console page. It has 13 sub-phases, H1 to H13. Each one is done by one agent in one go, in order, and ends with `just check` green and one commit.

**Why a plan and not a design doc (D-H-1).** The query engine (A2, with its review fixes P1–P7), the access layer, the route table, the OpenAPI builder and the fixture pipeline (D, G) already exist. H wires them to new routes. The one area with real design questions, MCP, fits in § Design below.

**Prerequisites** (checked when this plan was written, 2026-11-14):
- G M1–M5 are merged into `main` (last: PR #55). L is implemented.
- The A2 fixes (P1–P7, PRs #42 and #48) are merged: the SQL console's statement caps, guarded functions and Parquet chunking are in `crates/storage/src/sql/`.
- Node `26.13.0` is installed at `$HOME/.local/share/nvm/v26.13.0/bin`. It is not on the non-interactive `PATH`.

## Read first (every sub-phase)

- `AGENTS.md`, all of it.
- This README, all of it, then your own sub-phase file. Do not read the other sub-phase files unless yours says so.
- `docs/spec/09-api.md` § Conventions and the rows your sub-phase touches; `docs/spec/05-query.md` § SQL console; `docs/spec/08-auth.md` § Authorization (RBAC).
- `docs/mvp-decisions.md`: D94–D101, D-A2-1, D-A2-2, D-G-14, and every `D-H-n` (§ Track H — Console and agents).
- Your sub-phase file lists the code to read.

## Scope

**In:**
- `saved_views` in `meta.db` and the four saved-view routes that `openapi.json` already declares (G left them for H: the UI's saved-views menu gets `404` on the real server today).
- `POST /api/v1/projects/{project}/sql`: JSON results and CSV, NDJSON and Parquet exports over A2's `sql_json` / `sql_export`.
- The MCP server at `/mcp`: streamable HTTP through `rmcp`, PAT auth, seven read-only tools.
- CLI: `llmobs query`, `llmobs traces list`, `llmobs traces get`, `llmobs projects list`.
- Web: the SQL console page `/:org/:project/sql` with saved queries, parameters, history, examples, exports, a chart tab and a schema browser.

**Out:**
- `llmobs mcp-stdio` (D-H-2). MCP OAuth (D-H-3).
- The `experiment_items` and `dataset_items` SQL views: track J creates their tables and registers them (D93). Until then the console has `spans`, `traces`, `sessions` and `scores`, which is what `GET /schema` already lists.
- Bulk actions from the console (add to dataset, labeling). Tracks J and I.
- The 21 A2 review minors and the 27 C review minors.

## Design

### Access (D-H-4, D-H-5)

`POST /sql` and `POST /mcp` are reads that use POST. Today a read-only PAT (`Principal::Pat { write: false }`) and an `sk` key with only the `read` scope are refused on every non-GET method (`http/access.rs`: the PAT check at the top of `prepare`, the key check in `grant_key_project`). H adds two access shapes that count as reads:

| Route | Access | Session | PAT read | PAT read,write | `sk` read | `sk` without read |
|---|---|---|---|---|---|---|
| `POST /api/v1/projects/{project}/sql` | `Access::Project { perm: ProjectPerm::Query, session_only: false }` | viewer+ | yes | yes | yes (own project) | 403 |
| `POST /mcp` | `Access::Pat` | 403 | yes | yes | 403 | 403 |

- `ProjectPerm::Query` (H1): `min_role` viewer, `key_scope` `Read`. The read-PAT and read-key method checks skip it.
- `Access::Pat` (H6): a personal access token, read or read-write; no `{project}` in the path. Each tool resolves its `project` argument with the same rules as `grant_session_project` (404 for a project the user cannot see).
- **Exports** (H5) need effective role `member` for sessions and PATs (spec 08: "run SQL exports"); a viewer gets `403 forbidden`. An `sk` key with `read` may export: it can already read everything through the paged read API.

### Saved views (D-H-6, D-H-7)

One table, `saved_views` (spec 04), five kinds: `traces`, `spans`, `sessions`, `users`, `sql`. A saved SQL query is a view of kind `sql`.

- **Visibility.** A caller sees their own views (shared or not) and every shared view. A private view whose owner was deleted (`owner_user_id` NULL, `shared = 0`) is visible to nobody and is only removed with the project. An `sk` key sees shared views only.
- **Create.** Any role (viewer included) may create a private view. `shared: true` needs role member. Keys cannot write (403).
- **Update.** Only the owner changes `name`, `state` and `is_default`. Setting `shared: true`, or any change to a view that stays shared, needs the owner with role member; an owner may unshare their own view at any role. Unsharing a shared view whose owner was deleted is `409 conflict` (`UNSHARE_ORPHAN`): it would become invisible, so it must be deleted instead. A project admin may also set `shared: false` on, or delete, any shared view (moderation). Everything else on another user's view is `403` when the view is visible and `404` when it is not.
- **Defaults.** `is_default` scopes: a private view is "my default" among the owner's private views of that kind; a shared view is "the project default" among the shared views of that kind. Setting a default clears the other default of the same scope in the same transaction. Setting a shared default needs role member. Unsharing a view clears its `is_default`.
- **Limits.** `name` is trimmed, 1–200 characters. `state` is a JSON object of at most 64 KiB serialized (compact). Request bodies are limited to 128 KiB; a client that escapes far more than needed can hit `413` with a valid state, which is accepted. At most 1,000 views per `(project, owner)`; one more is `409 conflict`.
- **SQL state (D-H-7).** A `sql` view's state is `{"query": string, "params": {name: string}, "range"?: string, "from"?: string, "to"?: string}`: parameter values are kept as the text the user typed, and clients convert them by the placeholder's type when they run the query. The server checks only that it is an object (spec 09 § Response shapes); the UI owns the shape (H12).

### SQL console endpoint (D-H-8)

`POST /api/v1/projects/{project}/sql`, body `SqlQueryRequest {query, params?, from?, to?, limit?, timeout_ms?}` (spec 09). Body limit 256 KiB.

- `from`/`to`: RFC 3339 strings, both or neither, `from < to`; otherwise `400 bad_request` with a field error. They become a `TimeWindow` passed to `QueryContext::new` (D101).
- `limit`: 1–10,000, default 10,000. `timeout_ms`: 1,000–60,000, default 60,000. Outside → `400 bad_request` naming the field.
- **Format** comes from `Accept`. Its media ranges are read in order, ignoring parameters such as `q`; the first of `text/csv`, `application/x-ndjson`, `application/vnd.apache.parquet`, `application/json` decides; anything else (`*/*`, absent) is JSON.
- JSON answers have no byte cap beyond the 10,000 rows: a query of heavy columns (`input`, `output`) can build a large body outside the query memory pool. Known limit, inherited from A2 (D98).
- JSON answers `200 SqlQueryResult {columns: [{name, type}], rows: [...], truncated}` (D98).
- Exports answer `200` with that content type, `Content-Disposition: attachment; filename="query-<UTC yyyymmddThhmmssZ>.<csv|ndjson|parquet>"` and `Cache-Control: no-store`, streamed. `limit` is ignored (exports stop at 10M rows). Preparation errors (parse, sandbox, params, planning, admission) answer a problem before any byte. The handler also waits, within the timeout, for the stream's first data item (for CSV, the item after the header) before it sends the status line: an error there (a failing cast in the first batch, for example) is a problem too. A failure after the first byte aborts the body without its final chunk, so clients see a truncated transfer, or a closed connection with no status line when nothing had been flushed yet; it never looks like a complete download, and nothing is appended to the body.
- `timeout_ms` bounds the whole request. For an export, preparation runs inside it (running out there is a `422 query_timeout` problem) and it then covers the entire stream: a timeout after the response started cuts the body like any other mid-stream failure. A 10M-row export is in practice bounded by time. The query's memory admission is held until the stream ends or the client disconnects. The deadline is checked when the body is polled, so an authenticated client that stalls without disconnecting can hold its admission longer; accepted. The admission wait before the query starts (`query.admission_wait`) is outside `timeout_ms`.
- Errors: A2's `RunError` mapping in `http/error.rs` (`invalid_filter` 400 with `position` and field `query`, detail `invalid query`, `query_failed` 400, `query_timeout` 422, `query_too_large` 422, `busy` 503).
- Logs: one `info` line per request that reaches the handler, when it ends (for an export, when the stream ends, fails or is dropped) with `project_id`, `format`, `query_len`, `rows` and `truncated` (JSON only), `bytes` (exports only), `duration_ms` and, on failure, the `RunError::kind()`. The existing `query refused` line that `ApiError::into_response` writes for a refused query is kept. Never the query text, parameter values or result values.

### MCP server (D-H-2, D-H-3, D-H-9, D-H-10, D-H-11)

- **Transport.** Streamable HTTP at `POST /mcp` through `rmcp` 3.10.0's `StreamableHttpService`, stateless (`legacy_session_mode: false`) with JSON responses (`json_response: true`). Only `POST /mcp` is in the route table; other methods get the existing `405 method_not_allowed` fallback. `/mcp` is not in the OpenAPI document (`documented: false`), like `/v1/traces`. No stdio bridge (D-H-2).
- **Auth.** `Authorization: Bearer pat-llmobs-…` only (`Access::Pat`); missing or invalid credentials are `401` before rmcp sees the request. A cookie session or an `sk` key is `403`. MCP OAuth is after the MVP (D-H-3). The bearer requirement is what stops DNS rebinding, so rmcp's own Host and Origin checks are turned off (a proxy that rewrites `Host` would otherwise get `403`; H6 states the exact config). A client must send `Accept` with both `application/json` and `text/event-stream`, as the MCP spec requires; rmcp answers `406` in plain text otherwise.
- **Limits (D-H-10).** Per PAT: 60 `POST /mcp` requests per minute and 4 at a time; one more is `429 rate_limited` with `Retry-After`, before rmcp sees it. `query_sql` returns at most 1,000 rows (default 100) and runs at most 30 s. Every tool result except `get_schema` is capped at `max_chars` characters (default 20,000, 1,000–100,000). Quotas are per token.
- **Windows (D-H-9).** `query_sql` and `search_traces` require `from` and `to` (RFC 3339, `from < to`, at most 30 days apart). `get_trace` and `get_span` take an optional `around` hint instead.
- **Tools (D-H-11).** All read-only (`readOnlyHint: true`), all answering one text content item holding compact JSON (no `structuredContent`, so results are not sent twice). Errors in arguments or queries are tool results with `isError: true` and a JSON body `{"error": {"code", "message", "position"?}}` using the spec 09 problem codes; protocol errors (unknown tool, malformed JSON-RPC) are JSON-RPC errors.

| Tool | Input | Output | Phase |
|---|---|---|---|
| `list_projects` | — | `{projects: [{id, org, slug, name, role}], truncated, notes}`: active projects the user can read | H6 |
| `get_schema` | `project` | `{views: [...], functions: [...]}`: `GET /schema`'s response (about 15,000 characters; no `max_chars`) | H6 |
| `query_sql` | `project, query, params?, from, to, limit?` (1–1,000, default 100), `max_chars?` | `{columns, rows, row_count, truncated, notes}`; string cells cut at 2,000 characters | H7 |
| `list_saved_views` | `project, kind?` | `{views: [{id, kind, name, shared, owned, is_default, state}], truncated, notes}`, visible to the user | H7 |
| `search_traces` | `project, from, to, view?, filter?, search?, limit?` (1–100, default 20), `include_eval?`, `include_experiments?`, `max_chars?` | `{traces: [compact summary], truncated, notes}` | H8 |
| `get_trace` | `project, trace_id, around?, include_payloads?` (default false), `max_chars?` | `{trace, spans: [{depth, detached, span}], truncated, notes}`; `include_payloads` adds the stored input/output previews cut to 200 characters | H8 |
| `get_span` | `project, trace_id, span_id, around?, max_chars?` | `{span, notes}`: the full span; to fit `max_chars` its large fields (input, output, system instructions, tool definitions, request params, events, links) are cut to one common length | H8 |

`project` is a project UUID or `org-slug/project-slug`. In compact output every string attribute, metadata or resource-attribute value is cut to 200 characters; `notes` lists each cut; the exact note strings are defined in H7 and H8.
- **Privacy of rmcp's own logs.** rmcp logs whole requests and results at `debug`/`trace`. H6 makes `llmobs serve` always add the directive `rmcp=warn` to the log filter, whatever `log.level` says, so tool arguments (SQL, filters) and results never reach the logs.

### CLI (D-H-12)

`llmobs query`, `llmobs traces list|get`, `llmobs projects list` call the HTTP API with a PAT. Their flags are D-H-12's. The server URL and token come from, in order: `--url`/`--token`; `LLMOBS_URL`/`LLMOBS_TOKEN`; the credentials file of `llmobs login` (the only saved server, or the one `--url` names). `--project` takes a UUID or `org-slug/project-slug`, or `LLMOBS_PROJECT`.

### Web (D-H-13)

`/:org/:project/sql`, a new sidebar entry "SQL". URL search params: `range` or `from`+`to` (the shared time picker, default `24h`), `view` (a saved query id). The editor text and parameter values live in a per-project localStorage draft, not in the URL. Opening a `?view=` link replaces the local draft with the saved query, unless the draft was itself loaded from that view (the draft records the view id), so unsaved edits survive a reload. The console lists saved queries with `limit=1000`, the per-owner cap. Runs and exports always send the picker's range as `from`/`to`. History: the last 50 runs per project in localStorage. Exports download through `fetch` into a Blob, so the browser holds the whole file; the CLI is the tool for large exports.

## Decisions

All in `docs/mvp-decisions.md` § Track H — Console and agents. Summary:

| # | Summary |
|---|---|
| D-H-1 | One plan, no separate design doc. |
| D-H-2 | MCP is streamable HTTP at `/mcp` only (`rmcp` 3.10, stateless, JSON responses); no `llmobs mcp-stdio` in the MVP. |
| D-H-3 | MCP auth is a PAT (read or read-write) acting as its user; cookies and `sk` keys are refused; OAuth after the MVP. |
| D-H-4 | Reads by POST: `ProjectPerm::Query` (`POST /sql`) and `Access::Pat` (`POST /mcp`) admit read-only PATs, and `Query` admits `sk` keys with `read`. |
| D-H-5 | SQL exports need role member (sessions, PATs); an `sk` key needs `read`. |
| D-H-6 | Saved-view visibility, ownership, moderation, defaults and limits as in § Saved views. |
| D-H-7 | A saved SQL query is a `sql` view whose state is `{query, params, range \| from+to}`. |
| D-H-8 | `POST /sql` request validation, `Accept` negotiation, export headers and mid-stream failure as in § SQL console endpoint. |
| D-H-9 | MCP `query_sql` and `search_traces` require a window of at most 30 days. |
| D-H-10 | MCP limits: 60 requests/min and 4 concurrent per PAT (429), `query_sql` ≤ 1,000 rows and 30 s, results ≤ `max_chars` (default 20,000, max 100,000). |
| D-H-11 | The seven read-only MCP tools, compact JSON text output, 200-character value cuts, `notes`, tool errors with problem codes. |
| D-H-12 | CLI commands and credential resolution. |
| D-H-13 | SQL console page state: range and `view` in the URL, draft and history in localStorage, Blob exports. |

## Contracts between sub-phases

Later sub-phases rely on these names. A sub-phase that defines one must use exactly this signature.

```rust
// crates/server/src/auth/roles.rs (H1)
pub(crate) enum ProjectPerm { View, ManageSettings, ManageMembers, Annotate, Query }

// crates/server/src/http/access.rs (H6)
pub(crate) enum Access { /* existing variants */ Pat }

// crates/server/src/meta/saved_views.rs (H2). User ids are `uuid::Uuid`, as in `meta::users`.
pub(crate) enum Viewer { User(Uuid), Key }
pub(crate) struct SavedViewRow {
    pub(crate) id: Uuid,
    pub(crate) kind: SavedViewKind,          // crate::api::saved_views::SavedViewKind
    pub(crate) name: String,
    pub(crate) state: serde_json::Map<String, serde_json::Value>,
    pub(crate) owner_user_id: Option<Uuid>,
    pub(crate) shared: bool,
    pub(crate) is_default: bool,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
}
pub(crate) struct SavedViewPage { pub(crate) rows: Vec<SavedViewRow>, pub(crate) next_cursor: Option<String> }
pub(crate) async fn list_visible(pool: &SqlitePool, project: ProjectId, viewer: Viewer,
    kind: Option<SavedViewKind>, limit: usize, cursor: Option<&str>) -> Result<SavedViewPage, MetaError>;
pub(crate) async fn get_visible(pool: &SqlitePool, project: ProjectId, viewer: Viewer, id: Uuid)
    -> Result<Option<SavedViewRow>, MetaError>;
/// Every visible view of `kind` named exactly `name`, the viewer's own first, then by id.
pub(crate) async fn find_visible_by_name(pool: &SqlitePool, project: ProjectId, viewer: Viewer,
    kind: SavedViewKind, name: &str) -> Result<Vec<SavedViewRow>, MetaError>;
impl From<SavedViewRow> for crate::api::saved_views::SavedView { /* field by field */ }
// H2 also defines visible_to, kind_str, view_by_id, insert_view, update_view, delete_view,
// NewSavedView, SavedViewChanges, MAX_VIEWS_PER_OWNER and UNSHARE_ORPHAN (see 02-saved-views-store.md).
```

Test helpers later phases reuse (do not write new ones for the same job):
- H3, in `crates/server/src/test_support/mod.rs`: `Seeded.user_id: Uuid`; `TestApp::add_member(&self, seeded: &Seeded, email: &str, role: OrgRole) -> Member` with `Member { id: Uuid, cookie: String }`; `TestApp::personal_token(&self, user_id: Uuid, write: bool) -> String` (a `pat-llmobs-…` plaintext).
- H5: `TestStream::read_to_end`.

HTTP contract (H4, H5), used by the CLI (H9) and the web (H11–H13):
- operation id `run_sql`, tag `sql`, `POST /api/v1/projects/{project}/sql`;
- request schema `SqlQueryRequest {query: string, params?: object, from?: date-time string, to?: date-time string, limit?: integer, timeout_ms?: integer}` (`deny_unknown_fields`);
- JSON response schema `SqlQueryResult {columns: SqlColumnInfo[], rows: object[], truncated: boolean}`, `SqlColumnInfo {name: string, type: string}`;
- the `200` response lists `application/json` (`SqlQueryResult`) first, then `text/csv`, `application/x-ndjson` and `application/vnd.apache.parquet` as binary;
- examples: `fixtures/api/requests/sql-query.json` (a request body) and `fixtures/api/static/sql-result.json` (its JSON answer over the example dataset, checked against the real handler by H4's test).

## Sub-phases

| # | File | Commit subject | What |
|---|---|---|---|
| H1 | [`01-query-access.md`](01-query-access.md) | `server: admit read credentials on query POSTs` | `ProjectPerm::Query` and its access rules |
| H2 | [`02-saved-views-store.md`](02-saved-views-store.md) | `server: store saved views in meta.db` | migration `0003_saved_views.sql`, `meta/saved_views.rs` |
| H3 | [`03-saved-views-api.md`](03-saved-views-api.md) | `server: add the saved-view routes` | four handlers, permissions, OpenAPI errors, fixtures |
| H4 | [`04-sql-json.md`](04-sql-json.md) | `server: add the SQL console endpoint` | `POST /sql` JSON, request types, OpenAPI, fixtures |
| H5 | [`05-sql-export.md`](05-sql-export.md) | `server: stream SQL exports` | CSV, NDJSON, Parquet through `Accept`; member rule |
| H6 | [`06-mcp-server.md`](06-mcp-server.md) | `server: add the MCP endpoint` | `rmcp`, `Access::Pat`, rate limits, `list_projects`, `get_schema` |
| H7 | [`07-mcp-sql-tools.md`](07-mcp-sql-tools.md) | `server: add the MCP query tools` | `query_sql`, `list_saved_views`, output capping |
| H8 | [`08-mcp-trace-tools.md`](08-mcp-trace-tools.md) | `server: add the MCP trace tools` | `search_traces`, `get_trace`, `get_span` |
| H9 | [`09-cli-query.md`](09-cli-query.md) | `cli: add llmobs query` | API client, credential resolution, `query`, output formats |
| H10 | [`10-cli-traces.md`](10-cli-traces.md) | `cli: add traces and projects commands` | `traces list`, `traces get`, `projects list` |
| H11 | [`11-web-console.md`](11-web-console.md) | `web: add the SQL console page` | route, editor, run, result grid, errors, MSW mock |
| H12 | [`12-web-saved-queries.md`](12-web-saved-queries.md) | `web: add saved queries, parameters and history` | params bar, saved queries menu, examples, history |
| H13 | [`13-web-export-chart.md`](13-web-export-chart.md) | `web: add console exports, chart and schema browser` | export menu, chart tab, schema browser; roadmap status |

Order: strictly H1 → H13. H6–H8 need H1 (`ProjectPerm::Query` exists) and H3 (`meta::saved_views` lists visible views). H9–H10 need H4–H5. H11–H13 need H3–H5's OpenAPI changes (`schema.gen.ts`).

## Versions

One new crate, in H6: `rmcp = { version = "3.10.0", default-features = false, features = ["server", "transport-streamable-http-server"] }` in the root `[workspace.dependencies]`, used by `llmobs-server` only. `cargo deny` must stay green (it is Apache-2.0; licenses are deferred anyway, D57). No other new Rust crate. No new JS package: the editor uses the `@codemirror/*` packages already in `web/package.json`, the chart uses `recharts`.

## Conventions for every sub-phase

- **Node.** Before any `just` recipe that runs web or TypeScript checks (`just check`, `just check-web`, `just openapi`), run `export PATH="$HOME/.local/share/nvm/v26.13.0/bin:$PATH"` in the same shell command. `node --version` must print `v26.13.0`.
- **Formatting.** Plan snippets may exceed the line width; run `just fmt` before `just check`.
- **Checks.** While iterating, `just check-rust` (Rust phases) or `just check-web` (web phases) is enough. Before the commit, run `just check`; it must pass. Never commit with a failing check.
- **Contract.** When a phase changes `crates/server/src/api/` types or `api/openapi.rs`, run `just openapi` (rewrites `crates/server/openapi.json` and `web/src/api/schema.gen.ts`) and commit both files. When it changes what a read handler returns for the example dataset, run `just api-examples` and commit `fixtures/api/`.
- **Errors and panics.** No `unwrap()`/`expect()` outside tests unless a comment states the invariant (AGENTS.md). Library errors are `thiserror` enums; `ApiError` is the handler error; `anyhow` only in `llmobs-cli`.
- **Privacy.** Never log SQL text, filter or search text, parameter values, payloads, attribute values, saved-view names or state, emails or tokens. Log ids, counts, sizes, durations and `RunError::kind()`.
- **Rust tests.** HTTP tests build a `TestApp` (`crates/server/src/test_support/mod.rs`), call `seed()`, and load data with `load_example_dataset` when they read observability data. They use `#[tokio::test(flavor = "multi_thread")]`, except tests that capture logs (`assert_logs_clean`, `capture_logs`), which use `#[tokio::test]` (current thread) so that every log line is captured. Assert exact values.
- **Web tests.** Vitest + Testing Library, with the MSW server from `web/src/test/setup.ts`. A test overrides an endpoint with `server.use(...)`; never `vi.mock` a module. Page tests call `renderApp(url)` from `web/src/test/render.tsx`, as `web/src/routes/$org/$project/traces/index.test.tsx` does.
- **Names.** Storage types share names with API types. In server code refer to storage items as `llmobs_storage::…` (or `use llmobs_storage as storage;`), never with a bare `use` that shadows an API type.
- **Ownership.** Edit only the files your sub-phase lists. If you need another change, stop and report it.
- **STOP rules.** Stop and report instead of improvising when:
  - code this plan describes is not where it says, or has another signature;
  - a check fails because of code your phase did not touch;
  - an external crate's API differs from what your phase states.
- **Commit.** One commit per sub-phase, with the subject from the table and no trailer lines.
- **Report.** What you did; every deviation and why; the exact version of any new dependency (or "no new dependency"); anything left unverified.

## Spec and decision changes made with this plan

This plan's commit already updates `docs/spec/04-metadata-db.md`, `05-query.md`, `08-auth.md`, `09-api.md`, `11-ui.md`, `docs/mvp-decisions.md` (§ Track H) and `docs/roadmap.md` (row H). Sub-phases do not edit specs unless their file says so; H13 sets the roadmap status.

## Contract gaps this plan closes

- `POST /sql` is in spec 09 but not in `openapi.json` (H4, H5).
- The saved-view operations are declared but have no handlers and no table (H2, H3); their declared errors are `QUERY_ERRORS`, which H3 replaces with the real set.
- No access shape lets a read-only credential call a read that uses POST (H1, H6).
- Spec 09 lists `llmobs mcp-stdio`; D-H-2 removes it.
- Spec 09's MCP table has no window, row or rate rules; D-H-9, D-H-10, D-H-11 add them.
