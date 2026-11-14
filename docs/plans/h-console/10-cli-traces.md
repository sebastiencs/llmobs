# H10 — `llmobs traces` and `llmobs projects`

One commit: `cli: add traces and projects commands`.

Adds `llmobs traces list`, `llmobs traces get <TRACE_ID>` and `llmobs projects list` (spec 09 § CLI, D-H-12) on top of H9's client and table renderer.

## Before you start

- H9 is committed: `crates/cli/src/{client.rs,output.rs,query.rs,credentials.rs,test_server.rs}` exist, `output.rs` has `Io`, `clean`, `render_table`, `write_rows_note`, `format_time`, and `test_server.rs` has `TestServer`, `Canned` and `Recorded`. If not, STOP.
- Read:
  - `crates/cli/src/main.rs`, `client.rs`, `output.rs`, `test_server.rs`, and in `query.rs` only `since_window`, `run`'s signature and the tests' `query_args` helper;
  - `crates/server/src/api/traces.rs`: `TraceSummary`, `SpanSummary`, `TreeSpan`, `TraceDetail`, `TraceListParams`, `TraceDetailParams`;
  - `crates/server/src/api/account.rs`: `OrgMembership`, `OrgRole`, `Project`;
  - `crates/server/src/api/common.rs`: `Page<T>`;
  - `crates/core/src/enums.rs`: `SpanKind`, `SpanStatus` and their `as_str()`; `crates/core/src/ids.rs`: `TraceId` (`FromStr`, `Display`);
  - `docs/spec/09-api.md` § Conventions (lists and time ranges), § Observability data (`GET /traces`, `GET /traces/{trace_id}`), § Orgs, projects, members, keys.

## Decisions this phase relies on

| # | Summary |
|---|---|
| D-H-12 | Commands, flags (including `--include-eval`, `--include-experiments` and `traces get --around`) and credential resolution (H9's `resolve_target`). |
| spec 08 § Authorization (RBAC) | `GET` project routes accept read-only PATs and `sk` keys with `read` on their own project. |
| spec 09 § Conventions | `GET /traces` takes `from` and `to` independently: one bound alone leaves the other side open. `traces list` sends them only when given. |

## Versions

No new dependency.

## Why the server's types

Responses are deserialized into `llmobs_server::api::traces::{TraceSummary, TraceDetail}`, `llmobs_server::api::account::{OrgMembership, Project, OrgRole}` and `llmobs_server::api::common::Page<T>`. They are `pub`, derive `Deserialize`, and are the types the server serializes, so the CLI cannot drift from the contract. (H9 kept a CLI-local copy for the SQL result only because it reads two fields of it.)

`--format json` parses the body as a `serde_json::Value` and prints `serde_json::to_string_pretty(&value)` plus `\n`. The root `Cargo.toml` enables serde_json's `preserve_order`, so object keys keep the server's order; the output is indented with two spaces; numbers are re-printed by serde_json, which is the library the server used to write them, so they print the same. No field is dropped or added.

## Credentials

`traces list` and `traces get` work with a PAT (read or read-write) or with an `sk` key that has the `read` scope, on the key's own project (spec 08; `--project` must then name that project). `projects list` needs a PAT: an `sk` key gets `403`, printed as `Forbidden (forbidden)`.

`--project` is required on `traces list` and `traces get`, through the flag or `LLMOBS_PROJECT`. When both are missing, clap prints its own "required argument" error and exits 2; that is intended.

## Files

| File | Change |
|---|---|
| `crates/cli/src/traces.rs` | **new**: `TracesCommand`, `list`, `get`, cell and tree formatting |
| `crates/cli/src/projects.rs` | **new**: `ProjectsCommand`, `run` |
| `crates/cli/src/output.rs` | add `ListFormat` (Step 1) |
| `crates/cli/src/client.rs` | add `ApiClient::get_json` (Step 2) |
| `crates/cli/src/main.rs` | the `Traces` and `Projects` commands; module declarations; its own `ListFormat` deleted, `admin list-users` uses `output::ListFormat`; test 21 |

## Step 1 — clap

In `output.rs`, add:

```rust
/// Output of the list commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ListFormat {
    /// Aligned columns.
    Table,
    /// The server's JSON, re-indented.
    Json,
}
```

`main.rs` already has a private `enum ListFormat { Table, Json }` for `admin list-users`. Delete it: `AdminCommand::ListUsers`'s `format` field becomes `output::ListFormat` (same values `table` and `json`, same default `table`), and `admin_command`'s match uses `output::ListFormat::Table` and `output::ListFormat::Json`. Test 21 checks that `admin list-users --format json` still parses.

In `main.rs`, add to `Command`:

```rust
    /// List and show traces.
    Traces {
        #[command(subcommand)]
        command: Box<traces::TracesCommand>,
    },
    /// List the projects you can read.
    Projects {
        #[command(subcommand)]
        command: Box<projects::ProjectsCommand>,
    },
```

The subcommands are boxed, like `Query` in H9, so `Command` stays small (clippy's `large_enum_variant`); clap implements `Subcommand` for `Box<T>`.

and declare `mod projects; mod traces;`. In `run_cli`, add the arms `Command::Traces { command }` and `Command::Projects { command }`. Each builds `streams: output::Io` with the same lines as the `Command::Query` arm (H9: `stdout`, `stdout_is_terminal`, the `(out, err, input)` tuple, `streams`) and calls `traces::run(&command, credentials::credentials_path, &mut streams).await?` or `projects::run(&command, credentials::credentials_path, &mut streams).await?`. Repeat those lines in each arm; add no helper.

In `traces.rs`:

```rust
/// `llmobs traces …`.
#[derive(Debug, Subcommand)]
pub(crate) enum TracesCommand {
    /// List traces, newest first by default.
    List(TraceListArgs),
    /// Show one trace as a span tree.
    Get(TraceGetArgs),
}

/// Sort key of `traces list`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum TraceSortArg {
    /// Earliest span start.
    #[value(name = "start_time")]
    StartTime,
    /// Trace duration.
    #[value(name = "duration")]
    Duration,
    /// Total cost.
    #[value(name = "cost_total")]
    CostTotal,
    /// Total tokens.
    #[value(name = "total_tokens")]
    TotalTokens,
}

impl TraceSortArg {
    /// The `sort` query value of `GET /traces`.
    fn as_str(self) -> &'static str {
        match self {
            Self::StartTime => "start_time",
            Self::Duration => "duration",
            Self::CostTotal => "cost_total",
            Self::TotalTokens => "total_tokens",
        }
    }
}

/// Sort direction of `traces list`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum OrderArg {
    /// Smallest or earliest first.
    Asc,
    /// Largest or latest first.
    Desc,
}

impl OrderArg {
    /// The `order` query value of `GET /traces`.
    fn as_str(self) -> &'static str {
        match self {
            Self::Asc => "asc",
            Self::Desc => "desc",
        }
    }
}

/// Output of `traces get`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum TraceFormat {
    /// One line per span, indented by depth.
    Tree,
    /// The server's JSON, re-indented.
    Json,
}

/// `llmobs traces list`.
#[derive(Debug, Args)]
pub(crate) struct TraceListArgs {
    /// Project UUID or org-slug/project-slug.
    #[arg(long, env = "LLMOBS_PROJECT", value_name = "PROJECT")]
    pub(crate) project: String,
    /// Filter DSL over the traces view (spec 05), for example "status = 'error'".
    #[arg(long, value_name = "DSL")]
    pub(crate) filter: Option<String>,
    /// Full-text search over span input, output, name and system instructions.
    #[arg(long, value_name = "TEXT")]
    pub(crate) search: Option<String>,
    /// Range start: RFC 3339 or epoch milliseconds.
    #[arg(long, value_name = "TIME", conflicts_with = "since")]
    pub(crate) from: Option<String>,
    /// Range end (exclusive): RFC 3339 or epoch milliseconds.
    #[arg(long, value_name = "TIME", conflicts_with = "since")]
    pub(crate) to: Option<String>,
    /// Range ending now, for example 30m, 24h or 7d.
    #[arg(long, value_name = "DURATION")]
    pub(crate) since: Option<String>,
    /// Sort key.
    #[arg(long, value_enum, default_value = "start_time")]
    pub(crate) sort: TraceSortArg,
    /// Sort direction.
    #[arg(long, value_enum, default_value = "desc")]
    pub(crate) order: OrderArg,
    /// Traces per page (1–1000).
    #[arg(long, default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..=1000))]
    pub(crate) limit: u32,
    /// Cursor printed by the previous page.
    #[arg(long, value_name = "CURSOR")]
    pub(crate) cursor: Option<String>,
    /// Include traces of the evaluation environment.
    #[arg(long)]
    pub(crate) include_eval: bool,
    /// Include experiment traces.
    #[arg(long)]
    pub(crate) include_experiments: bool,
    /// Output format.
    #[arg(long, value_enum, default_value = "table")]
    pub(crate) format: ListFormat,
    #[command(flatten)]
    pub(crate) connection: ConnectionArgs,
}

/// `llmobs traces get`.
#[derive(Debug, Args)]
pub(crate) struct TraceGetArgs {
    /// Trace id, 32 hex digits.
    #[arg(value_name = "TRACE_ID")]
    pub(crate) trace_id: String,
    /// Project UUID or org-slug/project-slug.
    #[arg(long, env = "LLMOBS_PROJECT", value_name = "PROJECT")]
    pub(crate) project: String,
    /// When the trace happened, RFC 3339 or epoch milliseconds; speeds up the lookup.
    #[arg(long, value_name = "TIME")]
    pub(crate) around: Option<String>,
    /// Output format.
    #[arg(long, value_enum, default_value = "tree")]
    pub(crate) format: TraceFormat,
    #[command(flatten)]
    pub(crate) connection: ConnectionArgs,
}
```

`OrderArg`'s clap names are `asc` and `desc`, `TraceFormat`'s `tree` and `json`, `ListFormat`'s `table` and `json` (clap's default kebab-case of one-word variants). Do not import the server's `TraceSort`: the CLI sends the wire strings of `as_str`.

In `projects.rs`:

```rust
/// `llmobs projects …`.
#[derive(Debug, Subcommand)]
pub(crate) enum ProjectsCommand {
    /// List every project you can read, across your orgs.
    List(ProjectListArgs),
}

/// `llmobs projects list`.
#[derive(Debug, Args)]
pub(crate) struct ProjectListArgs {
    /// Output format.
    #[arg(long, value_enum, default_value = "table")]
    pub(crate) format: ListFormat,
    #[command(flatten)]
    pub(crate) connection: ConnectionArgs,
}
```

## Step 2 — `traces list`

First add to `ApiClient` in `client.rs` (H9 left it out because it had no caller):

```rust
    /// `GET` with `Accept: application/json`; `query` pairs are form-encoded in order.
    pub(crate) async fn get_json(&self, path: &str, query: &[(&str, String)], timeout: Duration)
        -> anyhow::Result<serde_json::Value>;
```

It behaves like `post_json` (bearer header, `RequestBuilder::timeout(timeout)` over the whole exchange, send errors as `could not reach <server>`, non-2xx through `problem_error`, `the server sent an invalid response` on a bad 2xx body), with `RequestBuilder::query(query)` and no body.

```rust
pub(crate) async fn run(command: &TracesCommand, credentials: impl FnOnce() -> anyhow::Result<PathBuf>, streams: &mut Io<'_>) -> anyhow::Result<()>;
async fn list(args: &TraceListArgs, credentials: impl FnOnce() -> anyhow::Result<PathBuf>, streams: &mut Io<'_>, now: DateTime<Utc>) -> anyhow::Result<()>;
async fn get(args: &TraceGetArgs, credentials: impl FnOnce() -> anyhow::Result<PathBuf>, streams: &mut Io<'_>) -> anyhow::Result<()>;
```

`run` matches the command (`TracesCommand::List(args) => list(args, credentials, streams, Utc::now()).await`, `TracesCommand::Get(args) => get(args, credentials, streams).await`). As in H9, the `Io` value is named `streams`, never `io`. Every command of this phase builds its whole output in a `Vec<u8>`, `write_all`s it to `streams.out` and flushes; when that write or flush fails with `io::ErrorKind::BrokenPipe`, the command returns `Ok(())` at once, writing no note (H9's broken-pipe rule).

Each request has its own 60 s timeout, deliberately: list and detail reads are bounded by the server's 30 s read timeout (spec 05 § Errors), so 60 s only catches a stalled server. `projects list` makes one request per page and has no overall bound.

`list`:

1. The window: `--since` → `crate::query::since_window(text, now)?` gives `from` and `to`; otherwise `--from` and `--to` as given (either may be absent).
2. `client::resolve_target(&args.connection, credentials)?`, `ApiClient::new(target)?`.
3. `client.get_json(&format!("/api/v1/projects/{}/traces", client::project_segment(&args.project)), &query, Duration::from_secs(60))` where `query: Vec<(&str, String)>` holds these pairs, in this order, each only when stated:

   | Param | Sent |
   |---|---|
   | `filter` | when `--filter` is given |
   | `search` | when `--search` is given |
   | `from` | when set by step 1 |
   | `to` | when set by step 1 |
   | `sort` | always: `args.sort.as_str()` |
   | `order` | always: `args.order.as_str()` |
   | `limit` | always: `args.limit.to_string()` |
   | `cursor` | when `--cursor` is given |
   | `include_eval` | `"true"` when `--include-eval` |
   | `include_experiments` | `"true"` when `--include-experiments` |

   `RequestBuilder::query` form-encodes the values: a space becomes `+`, `=` `%3D`, `'` `%27`, `:` `%3A`.
4. `--format json`: write `serde_json::to_string_pretty(&value)?` and `\n` to `streams.out` (§ Why the server's types). Nothing on `streams.err`.
5. `--format table`: `serde_json::from_value::<Page<TraceSummary>>(value)` (error `the server sent an invalid response`), then `render_table(streams.out, &trace_columns(), &rows)` with `rows` = `trace_cells` of each trace. Then `write_rows_note(streams.err, page.data.len(), more.as_deref())` where `more: Option<String>` is `Some(format!("next page: --cursor {}", output::clean(&cursor)))` when `next_cursor` is set, else `None` (the cursor comes from the server, so it is cleaned like every server string).

### Trace row

```rust
fn trace_columns() -> Vec<Column>;
fn trace_cells(trace: &TraceSummary) -> Vec<String>;
```

| Header | Cell | Right-aligned |
|---|---|---|
| `trace_id` | `trace.trace_id.to_string()` (lowercase hex) | no |
| `start_time` | `output::format_time(trace.start_time)` | no |
| `name` | `trace.name.clone()` | no |
| `status` | `trace.status.as_str().to_owned()` | no |
| `duration_ms` | `format_ms(trace.duration_ns)` | yes |
| `spans` | `trace.span_count.to_string()` | yes |
| `tokens` | `trace.total_tokens.map(|n| n.to_string()).unwrap_or_default()` | yes |
| `cost` | `format_cost(trace.cost_total)` | yes |
| `models` | `trace.models.join(",")` | no |

`render_table` cleans and cuts every cell (H9), so `trace_cells` does not.

```rust
/// Milliseconds with one decimal: 998000000 ns → "998.0".
fn format_ms(ns: i64) -> String {
    format!("{:.1}", ns as f64 / 1_000_000.0)
}

/// USD with six decimals, or empty when absent.
fn format_cost(cost: Option<f64>) -> String {
    cost.map(|value| format!("{value:.6}")).unwrap_or_default()
}
```

Six decimals round: `Some(0.0000036)` prints `0.000004`, and a cost below `0.0000005` prints `0.000000`. That is accepted; `--format json` has the exact value.

## Step 3 — `traces get`

1. Parse `args.trace_id.trim()` with `llmobs_core::ids::TraceId::from_str`; failure is the error `the trace id must be 32 hex digits`. This happens before resolving the target.
2. Resolve the target; `client.get_json(&format!("/api/v1/projects/{}/traces/{}", client::project_segment(&args.project), trace_id.to_string()), &query, Duration::from_secs(60))`, where `query` is `[("around", value)]` when `--around` is given and empty otherwise. A `404` comes back as H9's `problem_error` text, `Not Found (not_found)`.
3. `--format json`: `serde_json::to_string_pretty(&value)?` and `\n` to `streams.out`.
4. `--format tree`: `serde_json::from_value::<TraceDetail>(value)` (error `the server sent an invalid response`), then `write_tree(streams.out, &detail)?`. When `detail.truncated`, write to `streams.err` exactly `note: the trace has more spans than the server returns; the tree shows the first <n>\n`, where `<n>` is `detail.spans.len()`.

### Tree format

```rust
fn write_tree(out: &mut dyn Write, detail: &TraceDetail) -> io::Result<()>;
```

Line 1, the trace, fields joined by two spaces:

`<trace.trace_id.to_string()>  <clean(&trace.name)>  <format_time(trace.start_time)>  <format_ms(trace.duration_ns)>ms  status=<trace.status.as_str()>  spans=<trace.span_count>`, then `  tokens=<total_tokens>` when set, then `  cost=<format_cost(cost_total)>` when set.

Then one line per `TreeSpan`, in the order received (pre-order):

`<span.span_id.to_string()>  <"  " repeated depth times><clean(&span.name)> [<span.span_kind.as_str()>] <format_ms(span.duration_ns)>ms`, then, each only when it applies and in this order: `  status=error` when `span.status` is `SpanStatus::Error`; `  model=<clean(model)>`; `  tokens=<total_tokens>`; `  cost=<format_cost(cost_total)>`; `  (detached)` when `detached`.

`clean` is H9's `output::clean`: control characters become spaces, nothing else (no 60-character cut in the tree; that cut belongs to `render_table`). As in H9, Unicode format characters such as bidi overrides are not control characters and pass through; that is accepted. Every line ends with `\n`. No line has trailing spaces: each part above starts with its own separator, so none is emitted when a part is absent.

## Step 4 — `projects list`

```rust
pub(crate) async fn run(command: &ProjectsCommand, credentials: impl FnOnce() -> anyhow::Result<PathBuf>, streams: &mut Io<'_>) -> anyhow::Result<()>;
```

1. Resolve the target and build the client.
2. Orgs: `client.get_json("/api/v1/orgs", &[("limit", "1000".to_owned())], Duration::from_secs(60))`, then, while `next_cursor` is `Some(c)`, the same with `("cursor", c.clone())` appended after `limit`. Deserialize each page as `Page<OrgMembership>` (error `the server sent an invalid response`).
3. For each org in the order received, its projects: `client.get_json(&format!("/api/v1/orgs/{}/projects", client::project_segment(&org.slug)), &[("limit", "1000".to_owned())], Duration::from_secs(60))`, paging the same way, each page as `Page<Project>`. Both walks stop with the error `the server repeated a page cursor` when the server returns a `next_cursor` they have already followed; following it again would loop forever.
4. One listed project per `Project`, in the order received:

   ```rust
   /// One row of `projects list`.
   #[derive(Debug, Serialize)]
   struct ListedProject {
       /// `org-slug/project-slug`, the form `--project` takes.
       project: String,
       /// `project.id.to_string()`.
       id: String,
       name: String,
       /// `project.org_id.to_string()`.
       org_id: String,
       /// The caller's effective role. Omitted when absent.
       #[serde(skip_serializing_if = "Option::is_none")]
       role: Option<String>,
   }
   ```

   `project` is `format!("{}/{}", org.slug, project.slug)`. `role` is `project.role.map(role_str)` with `fn role_str(role: OrgRole) -> String` matching `Owner` → `owner`, `Admin` → `admin`, `Member` → `member`, `Viewer` → `viewer`.
5. `--format json`: `serde_json::to_string_pretty(&listed)?` (an array) and `\n` to `streams.out`.
   `--format table`: `render_table` with columns `project`, `name`, `role`, `id`, all left-aligned, `role` empty when absent; then `write_rows_note(streams.err, listed.len(), None)`.

## Tests

All tests are in `#[cfg(test)] mod tests` of `traces.rs` and `projects.rs`. Server tests use `#[tokio::test(flavor = "multi_thread")]`.

**Argument helpers.** Like H9's `query_args`, add in `traces.rs`'s tests `fn traces_args(argv: &[&str]) -> TracesCommand` (parses `["llmobs", "traces", …argv]` with `crate::Cli::try_parse_from(...).unwrap()` and returns `*command` from `crate::Command::Traces { command }`, else `panic!`), and in `projects.rs`'s tests `fn projects_args(argv: &[&str]) -> ProjectsCommand` the same way. Tests call `run(&traces_args(…), provider, &mut streams).await` (or `projects::run`), except test 7, which needs a fixed `now`: it takes the arguments out with `let TracesCommand::List(args) = traces_args(&[…]) else { panic!() };` and calls `list(&args, provider, &mut streams, now).await`. `use clap::Parser;` in both test modules. Collect output in `Vec<u8>` writers through an `Io` with `input: &mut std::io::empty()` and `stdout_is_terminal: false`. The credentials provider is `|| Ok(dir.path().join("missing.toml"))` for a `tempfile::tempdir()` `dir`, which is never read because every test passes `--url` and `--token`.

**Canned server.** Requests whose exact shape matters are checked against H9's `crate::test_server::Canned`: `Canned::json(responses).await` answers each request in order, and `canned.recorded()` returns the `Recorded` requests (`target` is the path and query as sent), readable as soon as the command returns. Tests pass `--url <canned.url> --token x`.

Tests:

1. `trace_cells_format_each_column`: build a `TraceSummary` with `serde_json::from_value(json!({…}))`: `trace_id` `a1000000000000000000000000000001`, `has_root` true, `name` `chat`, `start_time` `2026-01-01T10:00:00.000000000Z`, `end_time` `2026-01-01T10:00:01.500000000Z`, `duration_ns` 1500000000, `status` `error`, `tags` `[]`, `metadata` `{}`, `span_count` 3, `error_count` 1, `llm_count` 2, `total_tokens` 42, `cost_total` 0.0123, `models` `["gpt-5","gpt-5-mini"]`, `scores` `[]` → cells `["a1000000000000000000000000000001", "2026-01-01T10:00:00.000000000Z", "chat", "error", "1500.0", "3", "42", "0.012300", "gpt-5,gpt-5-mini"]`. Without `total_tokens` and `cost_total` → those two cells are `""`.
2. `format_ms_and_cost`: `998000000` → `998.0`; `0` → `0.0`; `Some(0.0000036)` → `0.000004`; `Some(0.0000001)` → `0.000000`; `None` → `""`.
3. `tree_indents_cleans_and_marks_detached`: a `TraceDetail` from JSON with test 1's trace except `name` `"chat\u{1b}[31m"`, `status` `ok`, no `total_tokens`, no `cost_total`; `truncated` false; three spans (each `SpanSummary` also needs `trace_id`, `start_time`, `end_time`, `tags` `[]`, `metadata` `{}`): depth 0 `b100000000000011` `chat` (`llm`, `duration_ns` 1000000000, `status` `ok`, `model` `gpt-5`, `total_tokens` 15); depth 1 `b100000000000012` `"tool\tcall"` (`tool`, `duration_ns` 2000000, `status` `error`); depth 0 `b100000000000013` `orphan` (`span`, `duration_ns` 0, `status` `unset`, `detached: true`). `write_tree` writes exactly:

   ```
   a1000000000000000000000000000001  chat [31m  2026-01-01T10:00:00.000000000Z  1500.0ms  status=ok  spans=3
   b100000000000011  chat [llm] 1000.0ms  model=gpt-5  tokens=15
   b100000000000012    tool call [tool] 2.0ms  status=error
   b100000000000013  orphan [span] 0.0ms  (detached)
   ```

   with a final `\n` (the ESC became a space; the tab became a space).
4. `trace_id_must_be_hex`: `traces_args(&["get", "zz", "--project", "acme/app", "--url", "http://127.0.0.1:9", "--token", "x"])` with the provider `|| anyhow::bail!("credentials must not be read")` → the error is exactly `the trace id must be 32 hex digits`.
5. `list_flags_reject_bad_values`: `crate::Cli::try_parse_from` on `traces list --project p --limit 0` and `--limit 1001` → `Err` with `error.kind() == clap::error::ErrorKind::ValueValidation`; `--since 1h --from 2026-01-01T00:00:00Z` → `ErrorKind::ArgumentConflict`; `--sort nope` → `ErrorKind::InvalidValue`.
6. `list_sends_every_query_parameter`: `Canned::json(vec![(200, r#"{"data":[],"next_cursor":null}"#.to_owned())])`; `traces list --project acme/app --filter "status = 'error'" --search "refund policy" --from 2026-01-01T00:00:00Z --sort cost_total --order asc --limit 10 --cursor abc --include-eval --include-experiments` → the recorded `target` is exactly `/api/v1/projects/acme%2Fapp/traces?filter=status+%3D+%27error%27&search=refund+policy&from=2026-01-01T00%3A00%3A00Z&sort=cost_total&order=asc&limit=10&cursor=abc&include_eval=true&include_experiments=true` (one bound alone: no `to`); `out` is `trace_id  start_time  name  status  duration_ms  spans  tokens  cost  models\n` followed by the rule line `--------  ----------  ----  ------  -----------  -----  ------  ----  ------\n`; `err` is `0 rows\n`.
7. `list_defaults_and_since`: `Canned::json` with the same empty page; call `list` directly with `now = 2026-01-02T00:00:00Z` and `traces list --project 01900000-0000-7000-8000-000000000001 --since 24h` → `target` exactly `/api/v1/projects/01900000-0000-7000-8000-000000000001/traces?from=2026-01-01T00%3A00%3A00.000000000Z&to=2026-01-02T00%3A00%3A00.000000000Z&sort=start_time&order=desc&limit=50`.
8. `get_sends_around_and_prints_json`: `Canned::json` answering `200` with body `{"trace":{…test 1's trace…},"spans":[],"truncated":false}` (write it compact); `traces get A1000000000000000000000000000001 --project acme/app --around 2026-01-01T10:00:00Z --format json` → `target` exactly `/api/v1/projects/acme%2Fapp/traces/a1000000000000000000000000000001?around=2026-01-01T10%3A00%3A00Z` (the id is lowercased); `out` equals `serde_json::to_string_pretty(&serde_json::from_str::<Value>(body).unwrap()).unwrap() + "\n"`; `err` is empty.
9. `get_notes_a_truncated_tree`: `Canned::json` answering the body of test 8 with `"truncated":true` and test 3's three spans; `traces get a1000000000000000000000000000001 --project acme/app` → `err` is exactly `note: the trace has more spans than the server returns; the tree shows the first 3\n` and `out` has 4 lines.

Real-server tests (`crate::test_server::TestServer`, explicit `--url <server.url> --token <server.token> --project <server.project>`, credentials provider as above; the window `--from 2025-12-31T00:00:00Z --to 2026-01-02T00:00:00Z` where stated). The ingested trace (`sdk-llm-span`, see H9 § Step 5) is `00000000000000000000000000000001`, one root `llm` span `chat`, `duration_ns` 998000000, `status` `unset`, `total_tokens` 15, `cost_total` 0.0000036, model `gpt-4o-mini-2024-07-18` (as `crates/cli/tests/snapshots/e2e__sdk-llm-span.snap` shows).

10. `traces_list_prints_the_ingested_trace`: `traces list` with the window → `out` is exactly:

    ```
    trace_id                          start_time                      name  status  duration_ms  spans  tokens      cost  models
    --------------------------------  ------------------------------  ----  ------  -----------  -----  ------  --------  ----------------------
    00000000000000000000000000000001  2025-12-31T23:59:50.001000000Z  chat  unset         998.0      1      15  0.000004  gpt-4o-mini-2024-07-18
    ```

    (each line ends with `\n`); `err` is `1 row\n`.
11. `traces_list_json_is_the_page`: `--format json` with the window → `out` parses as JSON; `data` has one item whose `trace_id` is `00000000000000000000000000000001`; `next_cursor` is `null`; `err` is empty.
12. `traces_list_pages_with_the_cursor`: `server.ingest("sdk-observe-tool", "00000000000000000000000000000002").await` first (H9's helper). Both traces start at `2025-12-31T23:59:50.001Z`, so `start_time` desc ties and the `trace_id` tie-breaker orders them. `traces list` with the window and `--limit 1` → one trace; `err` is `1 row; next page: --cursor <c>\n`, where `<c>` equals the `next_cursor` of a direct `reqwest` GET of `{url}/api/v1/projects/acme%2Fapp/traces?from=2025-12-31T00%3A00%3A00Z&to=2026-01-02T00%3A00%3A00Z&sort=start_time&order=desc&limit=1` with `Authorization: Bearer <server.token>`. Running again with `--cursor <c>` lists the other trace id, and its `err` starts with `1 row` (whether the last page carries a cursor is the server's business). The two pages together hold `…01` and `…02` once each.
13. `traces_list_reports_an_invalid_filter`: `--filter "status ="` with the window → the error text starts with `Bad Request (invalid_filter)`.
14. `traces_get_prints_the_tree`: `traces get 00000000000000000000000000000001` → `out` is exactly `00000000000000000000000000000001  chat  2025-12-31T23:59:50.001000000Z  998.0ms  status=unset  spans=1  tokens=15  cost=0.000004\n0000000000000001  chat [llm] 998.0ms  model=gpt-4o-mini-2024-07-18  tokens=15  cost=0.000004\n`.
15. `traces_get_unknown_trace_is_not_found`: `traces get ffffffffffffffffffffffffffffffff` → the error text is exactly `Not Found (not_found)`.
16. `read_only_token_and_key_list_traces`: test 10 with `--token <server.read_token>` → the same `out`; and with `--token <server.key>` (the `sk` key, scope `read` among others) → the same `out`.

`projects.rs` tests:

17. `projects_list_shows_slug_paths`: real server, `projects list` → `out` is exactly `format!("project   name  role   id\n--------  ----  -----  {}\nacme/app  App   owner  {id}\n", "-".repeat(36))`, where `id` is the `id` of the single item of a direct `GET {url}/api/v1/orgs/acme/projects` with the bearer token; `err` is `1 row\n`.
18. `projects_list_json`: real server, `--format json` → parsed `out` equals `json!([{"project": "acme/app", "id": id, "name": "App", "org_id": org_id, "role": "owner"}])`, with `id` and `org_id` the `id` and `org_id` of that same direct response's item; `err` is empty.
19. `projects_list_pages_orgs_and_projects`: `Canned::json` with five responses, in this order: orgs page 1 `{"data":[{"id":"01900000-0000-7000-8000-000000000010","slug":"acme","name":"Acme","role":"owner"}],"next_cursor":"o2"}`; orgs page 2 `{"data":[{"id":"01900000-0000-7000-8000-000000000011","slug":"beta","name":"Beta","role":"viewer"}],"next_cursor":null}`; acme projects `{"data":[<project support-bot of acme, role owner>],"next_cursor":"p2"}`; acme projects page 2 `{"data":[<project playground of acme, role owner>],"next_cursor":null}`; beta projects `{"data":[<project web of beta, no role>],"next_cursor":null}` (write the three projects in full, each with `created_at` `2026-01-01T09:00:00.000000000Z`: `support-bot` = `{"id":"01900000-0000-7000-8000-000000000001","org_id":"01900000-0000-7000-8000-000000000010","slug":"support-bot","name":"Support bot","role":"owner",…}`; `playground` = id `01900000-0000-7000-8000-000000000002`, the same org, name `Playground`, role `owner`; `web` = id `01900000-0000-7000-8000-000000000003`, `org_id` `01900000-0000-7000-8000-000000000011`, name `Web`, no `role`). Targets are exactly `/api/v1/orgs?limit=1000`, `/api/v1/orgs?limit=1000&cursor=o2`, `/api/v1/orgs/acme/projects?limit=1000`, `/api/v1/orgs/acme/projects?limit=1000&cursor=p2`, `/api/v1/orgs/beta/projects?limit=1000`. With `--format json`, `out` lists `acme/support-bot`, `acme/playground`, `beta/web` in that order, the `beta/web` object has no `role` key, and `err` is empty.
20. `projects_list_refuses_a_key`: real server, `--token <server.key>` → the error text is exactly `Forbidden (forbidden)`.

`main.rs` tests:

21. `admin_list_users_keeps_its_format_flag`: `crate::Cli::try_parse_from(["llmobs", "admin", "list-users", "--format", "json"])` parses, and its `format` is `output::ListFormat::Json`; with no `--format` it is `Table`; `--format xml` → `ErrorKind::InvalidValue`.

More `traces.rs` tests (canned server unless stated):

22. `get_json_rejects_an_invalid_2xx_body`: `Canned::json` answering `(200, "not json")` → `traces list --project p` fails with `the server sent an invalid response`; answering `(200, r#"{"items":[]}"#)` → the same (the page does not deserialize).
23. `get_json_names_an_unreachable_server`: `traces list --project p --url http://127.0.0.1:9 --token x` → the error's `{:#}` text starts with `could not reach http://127.0.0.1:9`.
24. `broken_pipe_ends_quietly`: H9's `crate::test_server::BrokenPipe { on_write: true }` writer as `out`: `traces list` answering an empty page, `traces get` answering test 8's body, and `projects list` (in `projects.rs`) answering one org page and one empty projects page → each returns `Ok(())` and `err` is empty.
25. `cursor_note_is_cleaned`: answering `{"data":[<test 1's trace>],"next_cursor":"a\u001bb"}` → `err` is `1 row; next page: --cursor a b\n`.

## Commands

```bash
export PATH="$HOME/.local/share/nvm/v26.13.0/bin:$PATH"
cargo test -p llmobs-cli
just check-rust
just check
```

## Done when

- `llmobs traces list --help`, `llmobs traces get --help` and `llmobs projects list --help` show every flag of Step 1.
- Tests 1–25 and every H9 test pass.
- `just check` passes; `git status` shows only the files of § Files.
- Commit `cli: add traces and projects commands`.

## Report

What you did; every deviation and why; "no new dependency"; anything left unverified.
