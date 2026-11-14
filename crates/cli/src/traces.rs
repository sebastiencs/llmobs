//! `llmobs traces`: list traces, and show one as a span tree, through the project's trace routes
//! (spec 09 § Observability data, D-H-12).

use std::fmt::Write as _;
use std::io::{self, Write};
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use chrono::{DateTime, Utc};
use clap::{Args, Subcommand, ValueEnum};
use llmobs_core::enums::SpanStatus;
use llmobs_core::ids::TraceId;
use llmobs_server::api::common::Page;
use llmobs_server::api::traces::{TraceDetail, TraceSummary};

use crate::client::{self, ApiClient, ConnectionArgs};
use crate::output::{self, Column, Io, ListFormat};
use crate::query;

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

/// Runs `llmobs traces …`. Credentials are read only when the target needs them.
pub(crate) async fn run(
    command: &TracesCommand,
    credentials: impl FnOnce() -> anyhow::Result<PathBuf>,
    streams: &mut Io<'_>,
) -> anyhow::Result<()> {
    match command {
        TracesCommand::List(args) => list(args, credentials, streams, Utc::now()).await,
        TracesCommand::Get(args) => get(args, credentials, streams).await,
    }
}

async fn list(
    args: &TraceListArgs,
    credentials: impl FnOnce() -> anyhow::Result<PathBuf>,
    streams: &mut Io<'_>,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    let (from, to) = match args.since.as_deref() {
        Some(text) => {
            let (from, to) = query::since_window(text, now)?;
            (Some(from), Some(to))
        }
        None => (args.from.clone(), args.to.clone()),
    };
    let mut params: Vec<(&str, String)> = Vec::new();
    if let Some(filter) = &args.filter {
        params.push(("filter", filter.clone()));
    }
    if let Some(search) = &args.search {
        params.push(("search", search.clone()));
    }
    if let Some(from) = from {
        params.push(("from", from));
    }
    if let Some(to) = to {
        params.push(("to", to));
    }
    params.push(("sort", args.sort.as_str().to_owned()));
    params.push(("order", args.order.as_str().to_owned()));
    params.push(("limit", args.limit.to_string()));
    if let Some(cursor) = &args.cursor {
        params.push(("cursor", cursor.clone()));
    }
    if args.include_eval {
        params.push(("include_eval", "true".to_owned()));
    }
    if args.include_experiments {
        params.push(("include_experiments", "true".to_owned()));
    }

    let target = client::resolve_target(&args.connection, credentials)?;
    let api = ApiClient::new(target)?;
    let path = format!(
        "/api/v1/projects/{}/traces",
        client::project_segment(&args.project)
    );
    let value = api
        .get_json(&path, &params, Duration::from_secs(60))
        .await?;
    match args.format {
        ListFormat::Json => {
            let mut bytes = serde_json::to_string_pretty(&value)?.into_bytes();
            bytes.push(b'\n');
            if !output::write_output(&mut *streams.out, &bytes)? {
                return Ok(());
            }
        }
        ListFormat::Table => {
            let page: Page<TraceSummary> = serde_json::from_value(value)
                .map_err(|_| anyhow::anyhow!("the server sent an invalid response"))?;
            let rows: Vec<Vec<String>> = page.data.iter().map(trace_cells).collect();
            let mut bytes = Vec::new();
            output::render_table(&mut bytes, &trace_columns(), &rows)?;
            if !output::write_output(&mut *streams.out, &bytes)? {
                return Ok(());
            }
            let more = page
                .next_cursor
                .as_deref()
                .map(|cursor| format!("next page: --cursor {}", output::clean(cursor)));
            output::write_rows_note(&mut *streams.err, page.data.len(), more.as_deref())?;
        }
    }
    Ok(())
}

async fn get(
    args: &TraceGetArgs,
    credentials: impl FnOnce() -> anyhow::Result<PathBuf>,
    streams: &mut Io<'_>,
) -> anyhow::Result<()> {
    let trace_id = TraceId::from_str(args.trace_id.trim())
        .map_err(|_| anyhow::anyhow!("the trace id must be 32 hex digits"))?;
    let params: Vec<(&str, String)> = args
        .around
        .iter()
        .map(|around| ("around", around.clone()))
        .collect();
    let target = client::resolve_target(&args.connection, credentials)?;
    let api = ApiClient::new(target)?;
    let path = format!(
        "/api/v1/projects/{}/traces/{trace_id}",
        client::project_segment(&args.project)
    );
    let value = api
        .get_json(&path, &params, Duration::from_secs(60))
        .await?;
    match args.format {
        TraceFormat::Json => {
            let mut bytes = serde_json::to_string_pretty(&value)?.into_bytes();
            bytes.push(b'\n');
            if !output::write_output(&mut *streams.out, &bytes)? {
                return Ok(());
            }
        }
        TraceFormat::Tree => {
            let detail: TraceDetail = serde_json::from_value(value)
                .map_err(|_| anyhow::anyhow!("the server sent an invalid response"))?;
            let mut bytes = Vec::new();
            write_tree(&mut bytes, &detail)?;
            if !output::write_output(&mut *streams.out, &bytes)? {
                return Ok(());
            }
            if detail.truncated {
                writeln!(
                    streams.err,
                    "note: the trace has more spans than the server returns; the tree shows the first {}",
                    detail.spans.len()
                )?;
            }
        }
    }
    Ok(())
}

/// The columns of `traces list`'s table.
fn trace_columns() -> Vec<Column> {
    [
        ("trace_id", false),
        ("start_time", false),
        ("name", false),
        ("status", false),
        ("duration_ms", true),
        ("spans", true),
        ("tokens", true),
        ("cost", true),
        ("models", false),
    ]
    .into_iter()
    .map(|(name, right)| Column {
        name: name.to_owned(),
        right,
    })
    .collect()
}

/// One row of `traces list`'s table, in the order of [`trace_columns`].
fn trace_cells(trace: &TraceSummary) -> Vec<String> {
    vec![
        trace.trace_id.to_string(),
        output::format_time(trace.start_time),
        trace.name.clone(),
        trace.status.as_str().to_owned(),
        format_ms(trace.duration_ns),
        trace.span_count.to_string(),
        trace
            .total_tokens
            .map(|tokens| tokens.to_string())
            .unwrap_or_default(),
        format_cost(trace.cost_total),
        trace.models.join(","),
    ]
}

/// Milliseconds with one decimal: 998000000 ns → "998.0".
fn format_ms(ns: i64) -> String {
    format!("{:.1}", ns as f64 / 1_000_000.0)
}

/// USD with six decimals, or empty when absent.
fn format_cost(cost: Option<f64>) -> String {
    cost.map_or_else(String::new, |value| format!("{value:.6}"))
}

/// The tree of `traces get --format tree`: the trace line, then one line per span in the order
/// received.
fn write_tree(out: &mut dyn Write, detail: &TraceDetail) -> io::Result<()> {
    let trace = &detail.trace;
    let mut head = format!(
        "{}  {}  {}  {}ms  status={}  spans={}",
        trace.trace_id,
        output::clean(&trace.name),
        output::format_time(trace.start_time),
        format_ms(trace.duration_ns),
        trace.status.as_str(),
        trace.span_count,
    );
    if let Some(tokens) = trace.total_tokens {
        let _ = write!(head, "  tokens={tokens}");
    }
    if trace.cost_total.is_some() {
        let _ = write!(head, "  cost={}", format_cost(trace.cost_total));
    }
    writeln!(out, "{head}")?;

    for node in &detail.spans {
        let span = &node.span;
        let depth = node.depth as usize;
        let mut line = format!(
            "{}  {}{} [{}] {}ms",
            span.span_id,
            "  ".repeat(depth),
            output::clean(&span.name),
            span.span_kind.as_str(),
            format_ms(span.duration_ns),
        );
        if span.status == SpanStatus::Error {
            line.push_str("  status=error");
        }
        if let Some(model) = &span.model {
            let _ = write!(line, "  model={}", output::clean(model));
        }
        if let Some(tokens) = span.total_tokens {
            let _ = write!(line, "  tokens={tokens}");
        }
        if span.cost_total.is_some() {
            let _ = write!(line, "  cost={}", format_cost(span.cost_total));
        }
        if node.detached {
            line.push_str("  (detached)");
        }
        writeln!(out, "{line}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use serde_json::{Value, json};

    use super::*;
    use crate::test_server::{BrokenPipe, Canned, TestServer};

    const EMPTY_PAGE: &str = r#"{"data":[],"next_cursor":null}"#;

    /// The `traces …` command of `argv`.
    fn traces_args(argv: &[&str]) -> TracesCommand {
        let mut full = vec!["llmobs", "traces"];
        full.extend_from_slice(argv);
        match crate::Cli::try_parse_from(full).unwrap().command {
            crate::Command::Traces { command } => *command,
            other => panic!("unexpected command: {other:?}"),
        }
    }

    /// Runs `traces …` with `argv` and returns its stdout and stderr text. The credentials file
    /// does not exist, so it is never read by a command that passes `--url` and `--token`.
    async fn capture(argv: &[&str]) -> anyhow::Result<(String, String)> {
        let dir = tempfile::tempdir().unwrap();
        let command = traces_args(argv);
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut input = io::empty();
        let mut streams = Io {
            out: &mut out,
            err: &mut err,
            input: &mut input,
            stdout_is_terminal: false,
        };
        run(
            &command,
            || Ok(dir.path().join("missing.toml")),
            &mut streams,
        )
        .await?;
        Ok((
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        ))
    }

    /// The trace of test 1: `chat`, 1.5 s, 3 spans, 42 tokens, cost 0.0123.
    fn trace_json() -> Value {
        json!({
            "trace_id": "a1000000000000000000000000000001",
            "has_root": true,
            "name": "chat",
            "start_time": "2026-01-01T10:00:00.000000000Z",
            "end_time": "2026-01-01T10:00:01.500000000Z",
            "duration_ns": 1500000000,
            "status": "error",
            "tags": [],
            "metadata": {},
            "span_count": 3,
            "error_count": 1,
            "llm_count": 2,
            "total_tokens": 42,
            "cost_total": 0.0123,
            "models": ["gpt-5", "gpt-5-mini"],
            "scores": []
        })
    }

    fn trace(value: Value) -> TraceSummary {
        serde_json::from_value(value).unwrap()
    }

    /// The three spans of test 3: a root `llm` span, a child `tool` span, and a detached span.
    fn tree_spans() -> Vec<Value> {
        vec![
            json!({"depth": 0, "detached": false, "span": {
                "trace_id": "a1000000000000000000000000000001",
                "span_id": "b100000000000011",
                "name": "chat",
                "span_kind": "llm",
                "start_time": "2026-01-01T10:00:00.000000000Z",
                "end_time": "2026-01-01T10:00:01.000000000Z",
                "duration_ns": 1000000000,
                "status": "ok",
                "tags": [],
                "metadata": {},
                "model": "gpt-5",
                "total_tokens": 15
            }}),
            json!({"depth": 1, "detached": false, "span": {
                "trace_id": "a1000000000000000000000000000001",
                "span_id": "b100000000000012",
                "parent_span_id": "b100000000000011",
                "name": "tool\tcall",
                "span_kind": "tool",
                "start_time": "2026-01-01T10:00:00.100000000Z",
                "end_time": "2026-01-01T10:00:00.102000000Z",
                "duration_ns": 2000000,
                "status": "error",
                "tags": [],
                "metadata": {}
            }}),
            json!({"depth": 0, "detached": true, "span": {
                "trace_id": "a1000000000000000000000000000001",
                "span_id": "b100000000000013",
                "parent_span_id": "b1000000000000ff",
                "name": "orphan",
                "span_kind": "span",
                "start_time": "2026-01-01T10:00:02.000000000Z",
                "end_time": "2026-01-01T10:00:02.000000000Z",
                "duration_ns": 0,
                "status": "unset",
                "tags": [],
                "metadata": {}
            }}),
        ]
    }

    /// A `GET /traces/{id}` body over [`trace_json`].
    fn detail_body(truncated: bool, spans: &[Value]) -> Value {
        json!({"trace": trace_json(), "spans": spans, "truncated": truncated})
    }

    #[test]
    fn trace_cells_format_each_column() {
        let cells = trace_cells(&trace(trace_json()));
        assert_eq!(
            cells,
            vec![
                "a1000000000000000000000000000001",
                "2026-01-01T10:00:00.000000000Z",
                "chat",
                "error",
                "1500.0",
                "3",
                "42",
                "0.012300",
                "gpt-5,gpt-5-mini",
            ]
        );

        let mut bare = trace_json();
        bare.as_object_mut().unwrap().remove("total_tokens");
        bare.as_object_mut().unwrap().remove("cost_total");
        let cells = trace_cells(&trace(bare));
        assert_eq!(cells[6], "");
        assert_eq!(cells[7], "");
    }

    #[test]
    fn format_ms_and_cost() {
        assert_eq!(format_ms(998_000_000), "998.0");
        assert_eq!(format_ms(0), "0.0");
        assert_eq!(format_cost(Some(0.0000036)), "0.000004");
        assert_eq!(format_cost(Some(0.0000001)), "0.000000");
        assert_eq!(format_cost(None), "");
    }

    #[test]
    fn tree_indents_cleans_and_marks_detached() {
        let mut trace_value = trace_json();
        trace_value["name"] = json!("chat\u{1b}[31m");
        trace_value["status"] = json!("ok");
        trace_value.as_object_mut().unwrap().remove("total_tokens");
        trace_value.as_object_mut().unwrap().remove("cost_total");
        let detail: TraceDetail = serde_json::from_value(json!({
            "trace": trace_value,
            "spans": tree_spans(),
            "truncated": false,
        }))
        .unwrap();

        let mut out = Vec::new();
        write_tree(&mut out, &detail).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "a1000000000000000000000000000001  chat [31m  2026-01-01T10:00:00.000000000Z  1500.0ms  status=ok  spans=3\n\
             b100000000000011  chat [llm] 1000.0ms  model=gpt-5  tokens=15\n\
             b100000000000012    tool call [tool] 2.0ms  status=error\n\
             b100000000000013  orphan [span] 0.0ms  (detached)\n"
        );
    }

    #[tokio::test]
    async fn trace_id_must_be_hex() {
        let command = traces_args(&[
            "get",
            "zz",
            "--project",
            "acme/app",
            "--url",
            "http://127.0.0.1:9",
            "--token",
            "x",
        ]);
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut input = io::empty();
        let mut streams = Io {
            out: &mut out,
            err: &mut err,
            input: &mut input,
            stdout_is_terminal: false,
        };
        let error = run(
            &command,
            || anyhow::bail!("credentials must not be read"),
            &mut streams,
        )
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), "the trace id must be 32 hex digits");
    }

    #[test]
    fn list_flags_reject_bad_values() {
        use clap::error::ErrorKind;
        let kind = |argv: &[&str]| crate::Cli::try_parse_from(argv).err().map(|e| e.kind());
        assert_eq!(
            kind(&["llmobs", "traces", "list", "--project", "p", "--limit", "0"]),
            Some(ErrorKind::ValueValidation)
        );
        assert_eq!(
            kind(&[
                "llmobs",
                "traces",
                "list",
                "--project",
                "p",
                "--limit",
                "1001"
            ]),
            Some(ErrorKind::ValueValidation)
        );
        assert_eq!(
            kind(&[
                "llmobs",
                "traces",
                "list",
                "--project",
                "p",
                "--since",
                "1h",
                "--from",
                "2026-01-01T00:00:00Z"
            ]),
            Some(ErrorKind::ArgumentConflict)
        );
        assert_eq!(
            kind(&[
                "llmobs",
                "traces",
                "list",
                "--project",
                "p",
                "--since",
                "1h",
                "--to",
                "2026-01-02T00:00:00Z"
            ]),
            Some(ErrorKind::ArgumentConflict)
        );
        assert_eq!(
            kind(&[
                "llmobs",
                "traces",
                "list",
                "--project",
                "p",
                "--sort",
                "nope"
            ]),
            Some(ErrorKind::InvalidValue)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_sends_every_query_parameter() {
        let canned = Canned::json(vec![(200, EMPTY_PAGE.to_owned())]).await;
        let (out, err) = capture(&[
            "list",
            "--project",
            "acme/app",
            "--filter",
            "status = 'error'",
            "--search",
            "refund policy",
            "--from",
            "2026-01-01T00:00:00Z",
            "--sort",
            "cost_total",
            "--order",
            "asc",
            "--limit",
            "10",
            "--cursor",
            "abc",
            "--include-eval",
            "--include-experiments",
            "--url",
            canned.url.as_str(),
            "--token",
            "x",
        ])
        .await
        .unwrap();

        assert_eq!(
            canned.recorded()[0].target,
            "/api/v1/projects/acme%2Fapp/traces?filter=status+%3D+%27error%27&search=refund+policy&from=2026-01-01T00%3A00%3A00Z&sort=cost_total&order=asc&limit=10&cursor=abc&include_eval=true&include_experiments=true"
        );
        assert_eq!(
            out,
            "trace_id  start_time  name  status  duration_ms  spans  tokens  cost  models\n\
             --------  ----------  ----  ------  -----------  -----  ------  ----  ------\n"
        );
        assert_eq!(err, "0 rows\n");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_defaults_and_since() {
        let canned = Canned::json(vec![(200, EMPTY_PAGE.to_owned())]).await;
        let TracesCommand::List(args) = traces_args(&[
            "list",
            "--project",
            "01900000-0000-7000-8000-000000000001",
            "--since",
            "24h",
            "--url",
            canned.url.as_str(),
            "--token",
            "x",
        ]) else {
            panic!("expected traces list");
        };
        let dir = tempfile::tempdir().unwrap();
        let now = DateTime::parse_from_rfc3339("2026-01-02T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut input = io::empty();
        let mut streams = Io {
            out: &mut out,
            err: &mut err,
            input: &mut input,
            stdout_is_terminal: false,
        };
        list(
            &args,
            || Ok(dir.path().join("missing.toml")),
            &mut streams,
            now,
        )
        .await
        .unwrap();

        assert_eq!(
            canned.recorded()[0].target,
            "/api/v1/projects/01900000-0000-7000-8000-000000000001/traces?from=2026-01-01T00%3A00%3A00.000000000Z&to=2026-01-02T00%3A00%3A00.000000000Z&sort=start_time&order=desc&limit=50"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_to_alone_is_the_only_bound() {
        let canned = Canned::json(vec![(200, EMPTY_PAGE.to_owned())]).await;
        let (_out, err) = capture(&[
            "list",
            "--project",
            "01900000-0000-7000-8000-000000000001",
            "--to",
            "2026-01-02T00:00:00Z",
            "--url",
            canned.url.as_str(),
            "--token",
            "x",
        ])
        .await
        .unwrap();

        assert_eq!(
            canned.recorded()[0].target,
            "/api/v1/projects/01900000-0000-7000-8000-000000000001/traces?to=2026-01-02T00%3A00%3A00Z&sort=start_time&order=desc&limit=50"
        );
        assert_eq!(err, "0 rows\n");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_sends_around_and_prints_json() {
        let body = detail_body(false, &[]).to_string();
        let canned = Canned::json(vec![(200, body.clone())]).await;
        let (out, err) = capture(&[
            "get",
            "A1000000000000000000000000000001",
            "--project",
            "acme/app",
            "--around",
            "2026-01-01T10:00:00Z",
            "--format",
            "json",
            "--url",
            canned.url.as_str(),
            "--token",
            "x",
        ])
        .await
        .unwrap();

        assert_eq!(
            canned.recorded()[0].target,
            "/api/v1/projects/acme%2Fapp/traces/a1000000000000000000000000000001?around=2026-01-01T10%3A00%3A00Z"
        );
        let expected: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            out,
            format!("{}\n", serde_json::to_string_pretty(&expected).unwrap())
        );
        assert_eq!(err, "");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_notes_a_truncated_tree() {
        let body = detail_body(true, &tree_spans()).to_string();
        let canned = Canned::json(vec![(200, body)]).await;
        let (out, err) = capture(&[
            "get",
            "a1000000000000000000000000000001",
            "--project",
            "acme/app",
            "--url",
            canned.url.as_str(),
            "--token",
            "x",
        ])
        .await
        .unwrap();

        assert_eq!(
            err,
            "note: the trace has more spans than the server returns; the tree shows the first 3\n"
        );
        assert_eq!(out.lines().count(), 4);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn traces_list_prints_the_ingested_trace() {
        let server = TestServer::start().await;
        let (out, err) = capture(&[
            "list",
            "--project",
            server.project.as_str(),
            "--from",
            "2025-12-31T00:00:00Z",
            "--to",
            "2026-01-02T00:00:00Z",
            "--url",
            server.url.as_str(),
            "--token",
            server.token.as_str(),
        ])
        .await
        .unwrap();

        assert_eq!(
            out,
            "trace_id                          start_time                      name  status  duration_ms  spans  tokens      cost  models\n\
             --------------------------------  ------------------------------  ----  ------  -----------  -----  ------  --------  ----------------------\n\
             00000000000000000000000000000001  2025-12-31T23:59:50.001000000Z  chat  unset         998.0      1      15  0.000004  gpt-4o-mini-2024-07-18\n"
        );
        assert_eq!(err, "1 row\n");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn traces_list_json_is_the_page() {
        let server = TestServer::start().await;
        let (out, err) = capture(&[
            "list",
            "--project",
            server.project.as_str(),
            "--from",
            "2025-12-31T00:00:00Z",
            "--to",
            "2026-01-02T00:00:00Z",
            "--format",
            "json",
            "--url",
            server.url.as_str(),
            "--token",
            server.token.as_str(),
        ])
        .await
        .unwrap();

        let page: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(page["data"].as_array().unwrap().len(), 1);
        assert_eq!(
            page["data"][0]["trace_id"],
            "00000000000000000000000000000001"
        );
        assert_eq!(page["next_cursor"], Value::Null);
        assert_eq!(err, "");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn traces_list_pages_with_the_cursor() {
        let server = TestServer::start().await;
        server
            .ingest("sdk-observe-tool", "00000000000000000000000000000002")
            .await;
        let window = [
            "--project",
            "acme/app",
            "--from",
            "2025-12-31T00:00:00Z",
            "--to",
            "2026-01-02T00:00:00Z",
            "--limit",
            "1",
            "--url",
            server.url.as_str(),
            "--token",
            server.token.as_str(),
        ];

        let mut first_args = vec!["list"];
        first_args.extend_from_slice(&window);
        let (out1, err1) = capture(&first_args).await.unwrap();
        let cursor = err1
            .strip_prefix("1 row; next page: --cursor ")
            .and_then(|rest| rest.strip_suffix('\n'))
            .unwrap()
            .to_owned();

        let direct: Value = reqwest::Client::new()
            .get(format!(
                "{}/api/v1/projects/acme%2Fapp/traces?from=2025-12-31T00%3A00%3A00Z&to=2026-01-02T00%3A00%3A00Z&sort=start_time&order=desc&limit=1",
                server.url
            ))
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", server.token),
            )
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(cursor, direct["next_cursor"].as_str().unwrap());

        let mut second_args = vec!["list", "--cursor", cursor.as_str()];
        second_args.extend_from_slice(&window);
        let (out2, err2) = capture(&second_args).await.unwrap();
        assert!(err2.starts_with("1 row"), "{err2}");

        let first_id = out1
            .lines()
            .nth(2)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap();
        let second_id = out2
            .lines()
            .nth(2)
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap();
        let mut ids = [first_id, second_id];
        ids.sort_unstable();
        assert_eq!(
            ids,
            [
                "00000000000000000000000000000001",
                "00000000000000000000000000000002"
            ]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn traces_list_reports_an_invalid_filter() {
        let server = TestServer::start().await;
        let error = capture(&[
            "list",
            "--project",
            "acme/app",
            "--filter",
            "status =",
            "--from",
            "2025-12-31T00:00:00Z",
            "--to",
            "2026-01-02T00:00:00Z",
            "--url",
            server.url.as_str(),
            "--token",
            server.token.as_str(),
        ])
        .await
        .unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("Bad Request (invalid_filter)"),
            "{error}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn traces_get_prints_the_tree() {
        let server = TestServer::start().await;
        let (out, err) = capture(&[
            "get",
            "00000000000000000000000000000001",
            "--project",
            "acme/app",
            "--url",
            server.url.as_str(),
            "--token",
            server.token.as_str(),
        ])
        .await
        .unwrap();

        assert_eq!(
            out,
            "00000000000000000000000000000001  chat  2025-12-31T23:59:50.001000000Z  998.0ms  status=unset  spans=1  tokens=15  cost=0.000004\n\
             0000000000000001  chat [llm] 998.0ms  model=gpt-4o-mini-2024-07-18  tokens=15  cost=0.000004\n"
        );
        assert_eq!(err, "");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn traces_get_unknown_trace_is_not_found() {
        let server = TestServer::start().await;
        let error = capture(&[
            "get",
            "ffffffffffffffffffffffffffffffff",
            "--project",
            "acme/app",
            "--url",
            server.url.as_str(),
            "--token",
            server.token.as_str(),
        ])
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), "Not Found (not_found)");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_only_token_and_key_list_traces() {
        let server = TestServer::start().await;
        let list_with = |token: String| {
            let server_url = server.url.clone();
            async move {
                capture(&[
                    "list",
                    "--project",
                    "acme/app",
                    "--from",
                    "2025-12-31T00:00:00Z",
                    "--to",
                    "2026-01-02T00:00:00Z",
                    "--url",
                    server_url.as_str(),
                    "--token",
                    token.as_str(),
                ])
                .await
                .unwrap()
            }
        };
        let expected = list_with(server.token.clone()).await;
        assert_eq!(list_with(server.read_token.clone()).await, expected);
        assert_eq!(list_with(server.key.clone()).await, expected);
    }

    #[tokio::test]
    async fn get_json_rejects_an_invalid_2xx_body() {
        for body in ["not json", r#"{"items":[]}"#] {
            let canned = Canned::json(vec![(200, body.to_owned())]).await;
            let error = capture(&[
                "list",
                "--project",
                "p",
                "--url",
                canned.url.as_str(),
                "--token",
                "x",
            ])
            .await
            .unwrap_err();
            assert_eq!(error.to_string(), "the server sent an invalid response");
        }
    }

    #[tokio::test]
    async fn get_json_names_an_unreachable_server() {
        let error = capture(&[
            "list",
            "--project",
            "p",
            "--url",
            "http://127.0.0.1:9",
            "--token",
            "x",
        ])
        .await
        .unwrap_err();
        assert!(
            format!("{error:#}").starts_with("could not reach http://127.0.0.1:9"),
            "{error:#}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn broken_pipe_ends_quietly() {
        let dir = tempfile::tempdir().unwrap();
        let list_canned = Canned::json(vec![
            (200, EMPTY_PAGE.to_owned()),
            (200, EMPTY_PAGE.to_owned()),
        ])
        .await;
        let get_canned = Canned::json(vec![(200, detail_body(false, &[]).to_string())]).await;

        let mut broken = BrokenPipe { on_write: true };
        let mut err = Vec::new();
        let mut input = io::empty();
        let mut streams = Io {
            out: &mut broken,
            err: &mut err,
            input: &mut input,
            stdout_is_terminal: false,
        };
        let list_command = traces_args(&[
            "list",
            "--project",
            "p",
            "--url",
            list_canned.url.as_str(),
            "--token",
            "x",
        ]);
        run(
            &list_command,
            || Ok(dir.path().join("missing.toml")),
            &mut streams,
        )
        .await
        .unwrap();
        assert!(err.is_empty());

        // A pipe that only breaks on flush stops the list just as quietly.
        let mut broken = BrokenPipe { on_write: false };
        let mut err = Vec::new();
        let mut streams = Io {
            out: &mut broken,
            err: &mut err,
            input: &mut input,
            stdout_is_terminal: false,
        };
        run(
            &list_command,
            || Ok(dir.path().join("missing.toml")),
            &mut streams,
        )
        .await
        .unwrap();
        assert!(err.is_empty());

        let mut broken = BrokenPipe { on_write: true };
        let mut err = Vec::new();
        let mut streams = Io {
            out: &mut broken,
            err: &mut err,
            input: &mut input,
            stdout_is_terminal: false,
        };
        let get_command = traces_args(&[
            "get",
            "a1000000000000000000000000000001",
            "--project",
            "p",
            "--url",
            get_canned.url.as_str(),
            "--token",
            "x",
        ]);
        run(
            &get_command,
            || Ok(dir.path().join("missing.toml")),
            &mut streams,
        )
        .await
        .unwrap();
        assert!(err.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cursor_note_is_cleaned() {
        let body = json!({"data": [trace_json()], "next_cursor": "a\u{1b}b"}).to_string();
        let canned = Canned::json(vec![(200, body)]).await;
        let (_out, err) = capture(&[
            "list",
            "--project",
            "p",
            "--url",
            canned.url.as_str(),
            "--token",
            "x",
        ])
        .await
        .unwrap();
        assert_eq!(err, "1 row; next page: --cursor a b\n");
    }
}
