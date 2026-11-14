//! `llmobs query`: one SQL statement through `POST /api/v1/projects/{project}/sql`
//! (spec 05 § SQL console, spec 09 § CLI, D-H-8, D-H-12, D101).

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, bail};
use chrono::{DateTime, SecondsFormat, TimeDelta, Utc};
use clap::{Args, ValueEnum};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

use crate::client::{self, ApiClient, ConnectionArgs};
use crate::output::{self, Column, Io};

/// Output of `llmobs query`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum QueryFormat {
    /// Aligned columns.
    Table,
    /// A JSON array of row objects, pretty-printed.
    Json,
    /// CSV export, streamed.
    Csv,
    /// One JSON object per line, streamed.
    Ndjson,
    /// Parquet export, streamed.
    Parquet,
}

/// Run a SQL query on a project (spec 05 § SQL console).
#[derive(Debug, Args)]
pub(crate) struct QueryArgs {
    /// The SQL statement, with optional {name:Type} parameters. Put `--` before a statement that starts with `-`.
    #[arg(
        value_name = "SQL",
        required_unless_present = "file",
        conflicts_with = "file"
    )]
    pub(crate) sql: Option<String>,
    /// Read the statement from a file; `-` reads stdin.
    #[arg(long, value_name = "PATH")]
    pub(crate) file: Option<PathBuf>,
    /// Project UUID or org-slug/project-slug.
    #[arg(long, env = "LLMOBS_PROJECT", value_name = "PROJECT")]
    pub(crate) project: String,
    /// A parameter value; repeat for each parameter. Int and Timestamp values that are all digits are sent as integers (Timestamp: epoch milliseconds).
    #[arg(long = "param", value_name = "NAME=VALUE")]
    pub(crate) params: Vec<String>,
    /// Window start, RFC 3339. Needs --to.
    #[arg(long, value_name = "TIME", requires = "to", conflicts_with = "since")]
    pub(crate) from: Option<String>,
    /// Window end (exclusive), RFC 3339. Needs --from.
    #[arg(long, value_name = "TIME", requires = "from", conflicts_with = "since")]
    pub(crate) to: Option<String>,
    /// Window ending now, for example 30m, 24h or 7d.
    #[arg(long, value_name = "DURATION")]
    pub(crate) since: Option<String>,
    /// Most rows for table and json (1–10000). The server's default is 10000.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=10_000))]
    pub(crate) limit: Option<u32>,
    /// The server's query time limit in milliseconds (1000–60000). Its default is 60000.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1_000..=60_000))]
    pub(crate) timeout_ms: Option<u32>,
    /// Output format.
    #[arg(long, value_enum, default_value = "table")]
    pub(crate) format: QueryFormat,
    /// Write the output to this file instead of stdout.
    #[arg(short = 'o', long, value_name = "PATH")]
    pub(crate) output: Option<PathBuf>,
    #[command(flatten)]
    pub(crate) connection: ConnectionArgs,
}

/// Body of `POST /sql` (spec 09, D-H-8).
#[derive(Serialize)]
struct SqlBody<'a> {
    query: &'a str,
    #[serde(skip_serializing_if = "Map::is_empty")]
    params: Map<String, Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timeout_ms: Option<u32>,
}

/// `SqlQueryResult` (README § Contracts). The CLI keeps its own copy: it needs only these
/// fields, and the server tests below check it against the real server.
#[derive(Deserialize)]
struct SqlResult {
    columns: Vec<SqlColumn>,
    rows: Vec<Map<String, Value>>,
    truncated: bool,
}

/// `SqlColumnInfo`. The Arrow `type` is not needed by the CLI, so it is not read.
#[derive(Deserialize)]
struct SqlColumn {
    name: String,
}

/// The message of a stream that the server cut (D-H-8).
const INCOMPLETE: &str = "the export stopped before its end; the output is incomplete";

/// What a successful answer writes: a table or JSON built in full, or an export streamed.
enum Body {
    Buffered(Vec<u8>),
    Streamed(reqwest::Response),
}

/// `llmobs query`. `args` is borrowed; `now` anchors `--since`.
pub(crate) async fn run(
    args: &QueryArgs,
    credentials: impl FnOnce() -> anyhow::Result<PathBuf>,
    streams: &mut Io<'_>,
    now: DateTime<Utc>,
) -> anyhow::Result<()> {
    let statement = read_statement(args, &mut *streams.input)?;
    let export = accept(args.format);
    if export.is_some() && args.limit.is_some() {
        bail!("--limit applies to the table and json formats; exports stop at 10,000,000 rows");
    }
    if args.format == QueryFormat::Parquet && args.output.is_none() && streams.stdout_is_terminal {
        bail!("refusing to write Parquet to a terminal; pass --output FILE");
    }
    let (from, to) = match args.since.as_deref() {
        Some(text) => {
            let (from, to) = since_window(text, now)?;
            (Some(from), Some(to))
        }
        None => (args.from.clone(), args.to.clone()),
    };
    let params = parameters(&statement, &args.params)?;
    let target = client::resolve_target(&args.connection, credentials)?;
    let api = ApiClient::new(target)?;
    let path = format!(
        "/api/v1/projects/{}/sql",
        client::project_segment(&args.project)
    );
    let body = SqlBody {
        query: &statement,
        params,
        from,
        to,
        limit: args.limit,
        timeout_ms: args.timeout_ms,
    };
    let timeout = Duration::from_millis(u64::from(args.timeout_ms.unwrap_or(60_000)))
        + Duration::from_secs(30);
    let destination = args.output.as_deref();
    match export {
        None => {
            let value = api.post_json(&path, &body, timeout).await?;
            let result: SqlResult = serde_json::from_value(value)
                .map_err(|_| anyhow::anyhow!("the server sent an invalid response"))?;
            let buffer = if args.format == QueryFormat::Json {
                json_bytes(&result.rows)?
            } else {
                table_bytes(&result)?
            };
            if !deliver(destination, &mut *streams.out, Body::Buffered(buffer)).await? {
                return Ok(());
            }
            let more = result.truncated.then_some(
                "more rows matched; raise --limit or export with --format csv, ndjson or parquet",
            );
            output::write_rows_note(&mut *streams.err, result.rows.len(), more)?;
        }
        Some(accept) => {
            let response = api.post_stream(&path, &body, accept, timeout).await?;
            deliver(destination, &mut *streams.out, Body::Streamed(response)).await?;
        }
    }
    Ok(())
}

/// The statement: the positional argument, the file, or all of stdin for `--file -`.
fn read_statement(args: &QueryArgs, input: &mut dyn Read) -> anyhow::Result<String> {
    let text = match (&args.sql, &args.file) {
        (Some(sql), None) => sql.clone(),
        (None, Some(path)) if path.as_os_str() == std::ffi::OsStr::new("-") => {
            let mut text = String::new();
            input.read_to_string(&mut text).context("reading stdin")?;
            text
        }
        (None, Some(path)) => {
            fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?
        }
        _ => bail!("pass a SQL statement or --file"),
    };
    if text.trim().is_empty() {
        bail!("the query is empty");
    }
    Ok(text)
}

/// `Accept` of an export format; `None` for table and json.
fn accept(format: QueryFormat) -> Option<&'static str> {
    match format {
        QueryFormat::Table | QueryFormat::Json => None,
        QueryFormat::Csv => Some("text/csv"),
        QueryFormat::Ndjson => Some("application/x-ndjson"),
        QueryFormat::Parquet => Some("application/vnd.apache.parquet"),
    }
}

/// A column is right-aligned when some row has a number in it and no row has a non-null value
/// that is not a number.
fn is_numeric(rows: &[Map<String, Value>], name: &str) -> bool {
    let mut any_number = false;
    for row in rows {
        match row.get(name) {
            Some(Value::Number(_)) => any_number = true,
            None | Some(Value::Null) => {}
            Some(_) => return false,
        }
    }
    any_number
}

fn table_bytes(result: &SqlResult) -> anyhow::Result<Vec<u8>> {
    let columns: Vec<Column> = result
        .columns
        .iter()
        .map(|column| Column {
            name: column.name.clone(),
            right: is_numeric(&result.rows, &column.name),
        })
        .collect();
    let cells: Vec<Vec<String>> = result
        .rows
        .iter()
        .map(|row| {
            result
                .columns
                .iter()
                .map(|column| {
                    output::json_cell(row.get(column.name.as_str()).unwrap_or(&Value::Null))
                })
                .collect()
        })
        .collect();
    let mut buffer = Vec::new();
    output::render_table(&mut buffer, &columns, &cells)?;
    Ok(buffer)
}

fn json_bytes(rows: &[Map<String, Value>]) -> anyhow::Result<Vec<u8>> {
    let mut buffer = Vec::new();
    serde_json::to_writer_pretty(&mut buffer, rows)?;
    buffer.push(b'\n');
    Ok(buffer)
}

/// Writes the answer to `destination` (through a `.part` file renamed into place) or to stdout.
/// `Ok(false)` means stdout's reader went away: the caller writes nothing more.
async fn deliver(
    destination: Option<&Path>,
    out: &mut dyn Write,
    body: Body,
) -> anyhow::Result<bool> {
    match destination {
        Some(path) => deliver_file(path, body).await.map(|()| true),
        None => match write_stdout(out, body).await {
            Err(error) if is_broken_pipe(&error) => Ok(false),
            other => other.map(|()| true),
        },
    }
}

async fn write_stdout(out: &mut dyn Write, body: Body) -> anyhow::Result<()> {
    match body {
        Body::Buffered(bytes) => out.write_all(&bytes)?,
        Body::Streamed(mut response) => copy_body(&mut response, out).await?,
    }
    out.flush()?;
    Ok(())
}

async fn deliver_file(path: &Path, body: Body) -> anyhow::Result<()> {
    let mut part_name = path.as_os_str().to_owned();
    part_name.push(".part");
    let part = PathBuf::from(part_name);
    let mut file =
        fs::File::create(&part).with_context(|| format!("creating {}", path.display()))?;
    let written = write_file(&mut file, body).await;
    drop(file);
    let result = written.and_then(|()| {
        fs::rename(&part, path).with_context(|| format!("writing {}", path.display()))
    });
    if result.is_err() {
        let _ = fs::remove_file(&part);
    }
    result
}

async fn write_file(file: &mut fs::File, body: Body) -> anyhow::Result<()> {
    match body {
        Body::Buffered(bytes) => file.write_all(&bytes)?,
        Body::Streamed(mut response) => copy_body(&mut response, file).await?,
    }
    file.flush()?;
    file.sync_all()?;
    Ok(())
}

/// Copies the rest of a streamed answer. A stream the server cut is [`INCOMPLETE`].
async fn copy_body(response: &mut reqwest::Response, sink: &mut dyn Write) -> anyhow::Result<()> {
    loop {
        let chunk = response
            .chunk()
            .await
            .map_err(|_| anyhow::anyhow!(INCOMPLETE))?;
        let Some(chunk) = chunk else {
            return Ok(());
        };
        sink.write_all(&chunk)?;
    }
}

fn is_broken_pipe(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<io::Error>()
        .is_some_and(|error| error.kind() == io::ErrorKind::BrokenPipe)
}

/// `[now - duration, now)` as two RFC 3339 strings with nanoseconds and `Z`.
pub(crate) fn since_window(text: &str, now: DateTime<Utc>) -> anyhow::Result<(String, String)> {
    let error = || anyhow::anyhow!("--since expects a duration such as 30m, 24h or 7d");
    let (digits, unit) = text
        .split_at_checked(text.len().saturating_sub(1))
        .ok_or_else(error)?;
    let multiplier: i64 = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3_600,
        "d" => 86_400,
        _ => return Err(error()),
    };
    let valid = !digits.is_empty()
        && !digits.starts_with('0')
        && digits.bytes().all(|byte| byte.is_ascii_digit());
    if !valid {
        return Err(error());
    }
    let seconds = digits
        .parse::<i64>()
        .ok()
        .and_then(|count| count.checked_mul(multiplier))
        .ok_or_else(error)?;
    let delta = TimeDelta::try_seconds(seconds).ok_or_else(error)?;
    let from = now.checked_sub_signed(delta).ok_or_else(error)?;
    Ok((
        from.to_rfc3339_opts(SecondsFormat::Nanos, true),
        now.to_rfc3339_opts(SecondsFormat::Nanos, true),
    ))
}

/// The five `{name:Type}` types of spec 05 § Parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParamType {
    String,
    Int,
    Float,
    Bool,
    Timestamp,
}

impl ParamType {
    fn parse(text: &str) -> Option<Self> {
        match text.to_ascii_lowercase().as_str() {
            "string" => Some(Self::String),
            "int" => Some(Self::Int),
            "float" => Some(Self::Float),
            "bool" => Some(Self::Bool),
            "timestamp" => Some(Self::Timestamp),
            _ => None,
        }
    }
}

/// `{name:Type}` placeholders of `statement`, first occurrence of each name.
fn placeholder_types(statement: &str) -> BTreeMap<String, ParamType> {
    let chars: Vec<char> = statement.chars().collect();
    let mut found = BTreeMap::new();
    let mut index = 0;
    while index < chars.len() {
        let character = chars[index];
        let next = chars.get(index + 1).copied();
        let after_word = index > 0 && is_ident_part(chars[index - 1]);
        index = match character {
            '\'' => skip_quoted(&chars, index + 1, '\''),
            'E' | 'e' if next == Some('\'') && !after_word => skip_escape(&chars, index + 2),
            '"' | '`' => skip_quoted(&chars, index + 1, character),
            '$' => skip_dollar(&chars, index).unwrap_or(index + 1),
            '-' if next == Some('-') => skip_line_comment(&chars, index + 2),
            '/' if next == Some('*') => {
                find_sequence(&chars, index + 2, &['*', '/']).unwrap_or(chars.len())
            }
            '{' => {
                if let Some((name, kind)) = placeholder_at(&chars, index) {
                    found.entry(name).or_insert(kind);
                }
                index + 1
            }
            _ => index + 1,
        };
    }
    found
}

fn is_ident_start(character: char) -> bool {
    character.is_alphabetic() || matches!(character, '_' | '#' | '@')
}

fn is_ident_part(character: char) -> bool {
    character.is_alphabetic()
        || character.is_ascii_digit()
        || matches!(character, '@' | '$' | '#' | '_')
}

/// Skips a quoted run that starts at `index` (just after the opening quote). A doubled quote is
/// an escaped quote. Returns the index after the closing quote, or the end of the text.
fn skip_quoted(chars: &[char], mut index: usize, quote: char) -> usize {
    while index < chars.len() {
        if chars[index] == quote {
            if chars.get(index + 1) == Some(&quote) {
                index += 2;
                continue;
            }
            return index + 1;
        }
        index += 1;
    }
    chars.len()
}

/// Skips an `E'…'` string body that starts at `index` (just after the opening quote). A backslash
/// escapes the next character.
fn skip_escape(chars: &[char], mut index: usize) -> usize {
    while index < chars.len() {
        match chars[index] {
            '\\' => index += 2,
            '\'' if chars.get(index + 1) == Some(&'\'') => index += 2,
            '\'' => return index + 1,
            _ => index += 1,
        }
    }
    chars.len()
}

fn skip_line_comment(chars: &[char], from: usize) -> usize {
    find_sequence(chars, from, &['\n']).unwrap_or(chars.len())
}

/// Skips a dollar-quoted string that starts with the `$` at `start`. `None` when that `$` is an
/// ordinary character.
fn skip_dollar(chars: &[char], start: usize) -> Option<usize> {
    if chars.get(start + 1) == Some(&'$') {
        return Some(find_sequence(chars, start + 2, &['$', '$']).unwrap_or(chars.len()));
    }
    let tag_start = start + 1;
    if !chars.get(tag_start).is_some_and(|&c| is_ident_start(c)) {
        return None;
    }
    let mut close = tag_start;
    while chars
        .get(close)
        .is_some_and(|&c| c != '$' && is_ident_part(c))
    {
        close += 1;
    }
    if chars.get(close) != Some(&'$') {
        return None;
    }
    let needle = &chars[start..=close];
    Some(find_sequence(chars, close + 1, needle).unwrap_or(chars.len()))
}

/// The index just after the first occurrence of `needle` at or after `from`.
fn find_sequence(chars: &[char], from: usize, needle: &[char]) -> Option<usize> {
    let last = chars.len().checked_sub(needle.len())?;
    (from..=last)
        .find(|&index| chars.get(index..index + needle.len()) == Some(needle))
        .map(|index| index + needle.len())
}

/// A `{name:Type}` placeholder whose `{` is at `open`, with nothing between its five parts.
fn placeholder_at(chars: &[char], open: usize) -> Option<(String, ParamType)> {
    let (name, after_name) = word_at(chars, open + 1)?;
    if chars.get(after_name) != Some(&':') {
        return None;
    }
    let (type_word, after_type) = word_at(chars, after_name + 1)?;
    if chars.get(after_type) != Some(&'}') {
        return None;
    }
    Some((name, ParamType::parse(&type_word)?))
}

/// An identifier starting at `start`: its text and the index after it.
fn word_at(chars: &[char], start: usize) -> Option<(String, usize)> {
    if !chars.get(start).is_some_and(|&c| is_ident_start(c)) {
        return None;
    }
    let mut end = start + 1;
    while chars.get(end).is_some_and(|&c| is_ident_part(c)) {
        end += 1;
    }
    Some((chars[start..end].iter().collect(), end))
}

/// `--param NAME=VALUE` values, typed by the placeholders of `statement` (spec 05 § Parameters).
fn parameters(statement: &str, raw: &[String]) -> anyhow::Result<Map<String, Value>> {
    let types = placeholder_types(statement);
    let mut values = Map::new();
    for entry in raw {
        let Some((name, text)) = entry.split_once('=').filter(|(name, _)| !name.is_empty()) else {
            bail!("--param expects NAME=VALUE");
        };
        if values.contains_key(name) {
            bail!("parameter {name} is given twice");
        }
        let value = match types.get(name) {
            Some(kind) => typed(name, *kind, text)?,
            None => Value::String(text.to_owned()),
        };
        values.insert(name.to_owned(), value);
    }
    Ok(values)
}

fn typed(name: &str, kind: ParamType, text: &str) -> anyhow::Result<Value> {
    match kind {
        ParamType::String => Ok(Value::String(text.to_owned())),
        ParamType::Int => integer(text)
            .map(Value::from)
            .ok_or_else(|| anyhow::anyhow!("parameter {name} must be an integer")),
        ParamType::Timestamp if !is_integer_text(text) => Ok(Value::String(text.to_owned())),
        ParamType::Timestamp => text.parse::<i64>().map(Value::from).map_err(|_| {
            anyhow::anyhow!("parameter {name} must be an RFC 3339 timestamp or epoch milliseconds")
        }),
        ParamType::Float => text
            .parse::<f64>()
            .ok()
            .and_then(Number::from_f64)
            .map(Value::Number)
            .ok_or_else(|| anyhow::anyhow!("parameter {name} must be a number")),
        ParamType::Bool => match text {
            "true" => Ok(Value::Bool(true)),
            "false" => Ok(Value::Bool(false)),
            _ => bail!("parameter {name} must be true or false"),
        },
    }
}

/// An optional `-` followed by one or more ASCII digits and nothing else.
fn is_integer_text(text: &str) -> bool {
    let digits = text.strip_prefix('-').unwrap_or(text);
    !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
}

fn integer(text: &str) -> Option<i64> {
    if is_integer_text(text) {
        text.parse().ok()
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use clap::Parser;
    use serde_json::json;

    use super::*;
    use crate::Cli;
    use crate::Command;
    use crate::client::Token;
    use crate::test_server::{BrokenPipe, Canned, TestServer};

    const PROBLEM_400: &str = r#"{"type":"about:blank","title":"Bad Request","status":400,"code":"invalid_filter","detail":"SQL parse error","errors":[{"field":"query","message":"unexpected token","position":{"line":1,"column":8}}]}"#;
    const ONE_ROW: &str =
        r#"{"columns":[{"name":"n","type":"Int64"}],"rows":[{"n":1}],"truncated":false}"#;
    const PROJECT: &str = "01900000-0000-7000-8000-000000000001";

    fn query_args(argv: &[&str]) -> QueryArgs {
        let mut full = vec!["llmobs", "query"];
        full.extend_from_slice(argv);
        match Cli::try_parse_from(full).unwrap().command {
            Command::Query(args) => *args,
            other => panic!("not a query: {other:?}"),
        }
    }

    /// Arguments for a real server: the trace of the fixture sits inside the window.
    fn server_args(server: &TestServer, sql: &str, extra: &[&str]) -> QueryArgs {
        let mut argv = vec![
            sql,
            "--url",
            server.url.as_str(),
            "--token",
            server.token.as_str(),
            "--project",
            server.project.as_str(),
            "--from",
            "2025-12-31T00:00:00Z",
            "--to",
            "2026-01-02T00:00:00Z",
        ];
        argv.extend_from_slice(extra);
        query_args(&argv)
    }

    fn at(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn no_credentials() -> anyhow::Result<PathBuf> {
        bail!("credentials must not be read")
    }

    /// Runs `args` and returns the captured (stdout, stderr).
    async fn execute(
        args: &QueryArgs,
        credentials: impl FnOnce() -> anyhow::Result<PathBuf>,
        now: DateTime<Utc>,
    ) -> anyhow::Result<(String, String)> {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut input = io::empty();
        let mut streams = Io {
            out: &mut out,
            err: &mut err,
            input: &mut input,
            stdout_is_terminal: false,
        };
        run(args, credentials, &mut streams, now).await?;
        Ok((
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        ))
    }

    fn typed_map(entries: &[(&str, ParamType)]) -> BTreeMap<String, ParamType> {
        entries
            .iter()
            .map(|(name, kind)| ((*name).to_owned(), *kind))
            .collect()
    }

    #[test]
    fn placeholder_types_mirrors_the_tokenizer() {
        let statement = r#"SELECT {a:Int}, {a:Float}, { b:Int}, {c :Int}, {_d:string}, {#e:BOOL}, {1x:Int}, {f:Text}, {g$1:Int}, {é:Int} -- {h:Bool}
/* {i:Bool} */ '{j:Bool}' 'it''s {k:Bool}' E'\' {l:Bool}' "{m:Bool}" `{n:Bool}` $$ {o:Bool} $$ $t$ {p:Bool} $t$ {q:TIMESTAMP}"#;
        assert_eq!(
            placeholder_types(statement),
            typed_map(&[
                ("a", ParamType::Int),
                ("_d", ParamType::String),
                ("#e", ParamType::Bool),
                ("g$1", ParamType::Int),
                ("é", ParamType::Int),
                ("q", ParamType::Timestamp),
            ])
        );
    }

    #[test]
    fn parameters_type_values() {
        let statement = "{a:Int} {f:Float} {b:Bool} {s:String} {t:Timestamp}";
        let value = |raw: &str| parameters(statement, &[raw.to_owned()]);
        let ok = |raw: &str| value(raw).unwrap().into_iter().next().unwrap().1;
        let err = |raw: &str| value(raw).unwrap_err().to_string();

        assert_eq!(ok("a=42"), json!(42));
        assert_eq!(ok("a=-7"), json!(-7));
        assert_eq!(err("a=+5"), "parameter a must be an integer");
        assert_eq!(err("a=5.0"), "parameter a must be an integer");
        assert_eq!(
            err("a=99999999999999999999"),
            "parameter a must be an integer"
        );
        assert_eq!(ok("f=1.5"), json!(1.5));
        assert_eq!(err("f=nan"), "parameter f must be a number");
        assert_eq!(err("f=inf"), "parameter f must be a number");
        assert_eq!(err("f=1e400"), "parameter f must be a number");
        assert_eq!(ok("b=true"), json!(true));
        assert_eq!(ok("b=false"), json!(false));
        assert_eq!(err("b=yes"), "parameter b must be true or false");
        assert_eq!(ok("s=42"), json!("42"));
        assert_eq!(ok("s=x=y"), json!("x=y"));
        assert_eq!(ok("t=2026-01-01T00:00:00Z"), json!("2026-01-01T00:00:00Z"));
        assert_eq!(ok("t=1767225600000"), json!(1_767_225_600_000_i64));
        assert_eq!(ok("t=2026"), json!(2026));
        assert_eq!(
            err("t=99999999999999999999"),
            "parameter t must be an RFC 3339 timestamp or epoch milliseconds"
        );
        assert_eq!(ok("x=1"), json!("1"));
        assert_eq!(err("novalue"), "--param expects NAME=VALUE");
        assert_eq!(err("=1"), "--param expects NAME=VALUE");
        let twice = parameters("{a:Int}", &["a=1".to_owned(), "a=1".to_owned()]).unwrap_err();
        assert_eq!(twice.to_string(), "parameter a is given twice");
    }

    #[test]
    fn since_window_parses_units() {
        let now = at("2026-01-02T00:00:00Z");
        let window = |text: &str| since_window(text, now).unwrap();
        assert_eq!(
            window("24h"),
            (
                "2026-01-01T00:00:00.000000000Z".to_owned(),
                "2026-01-02T00:00:00.000000000Z".to_owned()
            )
        );
        assert_eq!(window("30m").0, "2026-01-01T23:30:00.000000000Z");
        assert_eq!(window("45s").0, "2026-01-01T23:59:15.000000000Z");
        assert_eq!(window("7d").0, "2025-12-26T00:00:00.000000000Z");
        for text in [
            "0h",
            "5x",
            "h",
            "01h",
            "-1h",
            "",
            "99999999999999999999h",
            "9223372036854775807d",
            "9000000000000s",
        ] {
            assert!(since_window(text, now).is_err(), "{text}");
        }
    }

    #[test]
    fn query_flags_are_validated_by_clap() {
        let kind = |argv: &[&str]| {
            let mut full = vec!["llmobs", "query"];
            full.extend_from_slice(argv);
            Cli::try_parse_from(full).unwrap_err().kind()
        };
        use clap::error::ErrorKind;
        assert_eq!(
            kind(&[
                "SELECT 1",
                "--project",
                "p",
                "--from",
                "2026-01-01T00:00:00Z"
            ]),
            ErrorKind::MissingRequiredArgument
        );
        assert_eq!(
            kind(&[
                "SELECT 1",
                "--project",
                "p",
                "--since",
                "1h",
                "--from",
                "2026-01-01T00:00:00Z",
                "--to",
                "2026-01-02T00:00:00Z",
            ]),
            ErrorKind::ArgumentConflict
        );
        assert_eq!(
            kind(&["SELECT 1", "--project", "p", "--file", "q.sql"]),
            ErrorKind::ArgumentConflict
        );
        assert_eq!(
            kind(&["--project", "p"]),
            ErrorKind::MissingRequiredArgument
        );
        assert_eq!(
            kind(&["SELECT 1", "--project", "p", "--limit", "0"]),
            ErrorKind::ValueValidation
        );
        assert_eq!(
            kind(&["SELECT 1", "--project", "p", "--limit", "10001"]),
            ErrorKind::ValueValidation
        );
        assert_eq!(
            kind(&["SELECT 1", "--project", "p", "--timeout-ms", "999"]),
            ErrorKind::ValueValidation
        );
        assert_eq!(
            kind(&["SELECT 1", "--project", "p", "--format", "xml"]),
            ErrorKind::InvalidValue
        );
        let dashed = query_args(&["--project", "p", "--", "-1 AS x"]);
        assert_eq!(dashed.sql, Some("-1 AS x".to_owned()));
    }

    #[tokio::test]
    async fn exports_refuse_limit_and_parquet_on_a_terminal() {
        let limit = query_args(&[
            "SELECT 1",
            "--project",
            "p",
            "--format",
            "csv",
            "--limit",
            "5",
            "--url",
            "http://127.0.0.1:9",
            "--token",
            "x",
        ]);
        let error = execute(&limit, no_credentials, at("2026-01-02T00:00:00Z"))
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "--limit applies to the table and json formats; exports stop at 10,000,000 rows"
        );

        let parquet = query_args(&[
            "SELECT 1",
            "--project",
            "p",
            "--format",
            "parquet",
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
            stdout_is_terminal: true,
        };
        let error = run(
            &parquet,
            no_credentials,
            &mut streams,
            at("2026-01-02T00:00:00Z"),
        )
        .await
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "refusing to write Parquet to a terminal; pass --output FILE"
        );
    }

    #[tokio::test]
    async fn query_sends_the_request_body() {
        let canned = Canned::json(vec![(200, ONE_ROW.to_owned())]).await;
        let args = query_args(&[
            "SELECT {n:Int} AS n",
            "--url",
            &canned.url,
            "--token",
            "x",
            "--project",
            PROJECT,
            "--param",
            "n=1",
            "--since",
            "24h",
            "--limit",
            "5",
            "--timeout-ms",
            "2000",
        ]);
        let (out, _) = execute(&args, no_credentials, at("2026-01-02T00:00:00Z"))
            .await
            .unwrap();
        assert_eq!(out, "n\n-\n1\n");

        let recorded = canned.recorded();
        assert_eq!(recorded.len(), 1);
        let request = &recorded[0];
        assert_eq!(
            request.target,
            "/api/v1/projects/01900000-0000-7000-8000-000000000001/sql"
        );
        let head = request.head.to_ascii_lowercase();
        assert!(head.starts_with("post "), "{head}");
        assert!(head.contains("accept: application/json"), "{head}");
        assert!(head.contains("content-type: application/json"), "{head}");
        assert!(head.contains("authorization: bearer x"), "{head}");
        let body: Value = serde_json::from_str(&request.body).unwrap();
        assert_eq!(
            body,
            json!({
                "query": "SELECT {n:Int} AS n",
                "params": {"n": 1},
                "from": "2026-01-01T00:00:00.000000000Z",
                "to": "2026-01-02T00:00:00.000000000Z",
                "limit": 5,
                "timeout_ms": 2000
            })
        );
    }

    #[tokio::test]
    async fn query_aligns_mixed_columns_left() {
        let answer = r#"{"columns":[{"name":"m","type":"Utf8"}],"rows":[{"m":1},{"m":"a"}],"truncated":false}"#;
        let canned = Canned::json(vec![(200, answer.to_owned())]).await;
        let args = query_args(&[
            "SELECT m",
            "--url",
            &canned.url,
            "--token",
            "x",
            "--project",
            PROJECT,
        ]);
        let (out, _) = execute(&args, no_credentials, at("2026-01-02T00:00:00Z"))
            .await
            .unwrap();
        assert_eq!(out, "m\n-\n1\na\n");
    }

    #[tokio::test]
    async fn query_rejects_an_invalid_2xx_body() {
        for body in ["not json", r#"{"rows":[]}"#] {
            let canned = Canned::json(vec![(200, body.to_owned())]).await;
            let args = query_args(&[
                "SELECT 1",
                "--url",
                &canned.url,
                "--token",
                "x",
                "--project",
                PROJECT,
            ]);
            let error = execute(&args, no_credentials, at("2026-01-02T00:00:00Z"))
                .await
                .unwrap_err();
            assert_eq!(error.to_string(), "the server sent an invalid response");
        }
    }

    #[tokio::test]
    async fn query_reports_non_problem_statuses() {
        let canned = Canned::json(vec![(502, "oops".to_owned())]).await;
        let args = query_args(&[
            "SELECT 1",
            "--url",
            &canned.url,
            "--token",
            "x",
            "--project",
            PROJECT,
        ]);
        let error = execute(&args, no_credentials, at("2026-01-02T00:00:00Z"))
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "the server answered 502 Bad Gateway");

        let redirect = Canned::raw(vec![
            b"HTTP/1.1 302 Found\r\nLocation: /elsewhere\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                .to_vec(),
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{ONE_ROW}",
                ONE_ROW.len()
            )
            .into_bytes(),
        ])
        .await;
        let args = query_args(&[
            "SELECT 1",
            "--url",
            &redirect.url,
            "--token",
            "x",
            "--project",
            PROJECT,
        ]);
        let error = execute(&args, no_credentials, at("2026-01-02T00:00:00Z"))
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "the server answered 302 Found");
        assert_eq!(redirect.recorded().len(), 1);
    }

    #[tokio::test]
    async fn export_cut_mid_stream_keeps_the_old_file() {
        let canned = Canned::raw(vec![
            b"HTTP/1.1 200 OK\r\nContent-Type: text/csv\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n5\r\na,b\r\n\r\n"
                .to_vec(),
        ])
        .await;
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.csv");
        fs::write(&out, "old").unwrap();
        let out_text = out.to_str().unwrap();
        let args = query_args(&[
            "SELECT 1",
            "--url",
            &canned.url,
            "--token",
            "x",
            "--project",
            PROJECT,
            "--format",
            "csv",
            "--output",
            out_text,
        ]);
        let error = execute(&args, no_credentials, at("2026-01-02T00:00:00Z"))
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), INCOMPLETE);
        assert_eq!(fs::read_to_string(&out).unwrap(), "old");
        assert!(!dir.path().join("out.csv.part").exists());
    }

    #[tokio::test]
    async fn failed_query_leaves_files_alone() {
        let canned = Canned::json(vec![
            (400, PROBLEM_400.to_owned()),
            (400, PROBLEM_400.to_owned()),
        ])
        .await;
        let dir = tempfile::tempdir().unwrap();
        let new = dir.path().join("new.csv");
        let new_text = new.to_str().unwrap();
        let args = query_args(&[
            "SELECT 1",
            "--url",
            &canned.url,
            "--token",
            "x",
            "--project",
            PROJECT,
            "--format",
            "csv",
            "--output",
            new_text,
        ]);
        assert!(
            execute(&args, no_credentials, at("2026-01-02T00:00:00Z"))
                .await
                .is_err()
        );
        assert!(!new.exists());
        assert!(!dir.path().join("new.csv.part").exists());

        let old = dir.path().join("old.csv");
        fs::write(&old, "keep").unwrap();
        let old_text = old.to_str().unwrap();
        let args = query_args(&[
            "SELECT 1",
            "--url",
            &canned.url,
            "--token",
            "x",
            "--project",
            PROJECT,
            "--output",
            old_text,
        ]);
        assert!(
            execute(&args, no_credentials, at("2026-01-02T00:00:00Z"))
                .await
                .is_err()
        );
        assert_eq!(fs::read_to_string(&old).unwrap(), "keep");
    }

    #[tokio::test]
    async fn output_cannot_be_created() {
        let canned = Canned::json(vec![(200, ONE_ROW.to_owned())]).await;
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("no").join("such").join("dir").join("x.txt");
        let target_text = target.to_str().unwrap();
        let args = query_args(&[
            "SELECT 1",
            "--url",
            &canned.url,
            "--token",
            "x",
            "--project",
            PROJECT,
            "--output",
            target_text,
        ]);
        let error = execute(&args, no_credentials, at("2026-01-02T00:00:00Z"))
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").starts_with(&format!("creating {target_text}")),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn broken_pipe_stops_quietly() {
        let canned = Canned::json(vec![
            (200, ONE_ROW.to_owned()),
            (200, ONE_ROW.to_owned()),
            (200, ONE_ROW.to_owned()),
        ])
        .await;
        let args = query_args(&[
            "SELECT 1",
            "--url",
            &canned.url,
            "--token",
            "x",
            "--project",
            PROJECT,
        ]);
        let mut err = Vec::new();
        let mut input = io::empty();
        let mut out = BrokenPipe { on_write: true };
        let mut streams = Io {
            out: &mut out,
            err: &mut err,
            input: &mut input,
            stdout_is_terminal: false,
        };
        run(
            &args,
            no_credentials,
            &mut streams,
            at("2026-01-02T00:00:00Z"),
        )
        .await
        .unwrap();
        assert!(err.is_empty());

        let mut err = Vec::new();
        let mut input = io::empty();
        let mut out = BrokenPipe { on_write: false };
        let mut streams = Io {
            out: &mut out,
            err: &mut err,
            input: &mut input,
            stdout_is_terminal: false,
        };
        run(
            &args,
            no_credentials,
            &mut streams,
            at("2026-01-02T00:00:00Z"),
        )
        .await
        .unwrap();

        let csv = Canned::raw(vec![
            b"HTTP/1.1 200 OK\r\nContent-Type: text/csv\r\nContent-Length: 4\r\nConnection: close\r\n\r\nn\r\n1"
                .to_vec(),
        ])
        .await;
        let args = query_args(&[
            "SELECT 1",
            "--url",
            &csv.url,
            "--token",
            "x",
            "--project",
            PROJECT,
            "--format",
            "csv",
        ]);
        let mut err = Vec::new();
        let mut input = io::empty();
        let mut out = BrokenPipe { on_write: true };
        let mut streams = Io {
            out: &mut out,
            err: &mut err,
            input: &mut input,
            stdout_is_terminal: false,
        };
        run(
            &args,
            no_credentials,
            &mut streams,
            at("2026-01-02T00:00:00Z"),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn unreachable_server_names_it() {
        let args = query_args(&[
            "SELECT 1",
            "--url",
            "http://127.0.0.1:9",
            "--token",
            "x",
            "--project",
            "p",
        ]);
        let error = execute(&args, no_credentials, at("2026-01-02T00:00:00Z"))
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").starts_with("could not reach http://127.0.0.1:9"),
            "{error:#}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn query_prints_a_table() {
        let server = TestServer::start().await;
        let args = server_args(
            &server,
            "SELECT name, total_tokens FROM spans ORDER BY name",
            &[],
        );
        let (out, err) = execute(&args, no_credentials, Utc::now()).await.unwrap();
        assert_eq!(
            out,
            "name  total_tokens\n----  ------------\nchat            15\n"
        );
        assert_eq!(err, "1 row\n");
        server.stop().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn query_prints_json() {
        let server = TestServer::start().await;
        let args = server_args(
            &server,
            "SELECT name, total_tokens FROM spans ORDER BY name",
            &["--format", "json"],
        );
        let (out, err) = execute(&args, no_credentials, Utc::now()).await.unwrap();
        assert_eq!(
            out,
            "[\n  {\n    \"name\": \"chat\",\n    \"total_tokens\": 15\n  }\n]\n"
        );
        assert_eq!(err, "1 row\n");
        server.stop().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn query_writes_table_and_json_to_a_file() {
        let server = TestServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let sql = "SELECT name, total_tokens FROM spans ORDER BY name";

        let table_path = dir.path().join("t.txt");
        let table_text = table_path.to_str().unwrap();
        let args = server_args(&server, sql, &["--output", table_text]);
        let (out, err) = execute(&args, no_credentials, Utc::now()).await.unwrap();
        assert_eq!(out, "");
        assert_eq!(err, "1 row\n");
        assert_eq!(
            fs::read_to_string(&table_path).unwrap(),
            "name  total_tokens\n----  ------------\nchat            15\n"
        );

        let json_path = dir.path().join("j.json");
        let json_text = json_path.to_str().unwrap();
        let args = server_args(&server, sql, &["--format", "json", "--output", json_text]);
        let (out, _) = execute(&args, no_credentials, Utc::now()).await.unwrap();
        assert_eq!(out, "");
        assert_eq!(
            fs::read_to_string(&json_path).unwrap(),
            "[\n  {\n    \"name\": \"chat\",\n    \"total_tokens\": 15\n  }\n]\n"
        );
        server.stop().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn query_aligns_nulls_and_nested_values() {
        let server = TestServer::start().await;
        let (out, _) = execute(
            &server_args(&server, "SELECT 1 AS a, NULL AS b, 'x' AS c", &[]),
            no_credentials,
            Utc::now(),
        )
        .await
        .unwrap();
        assert_eq!(out, "a  b     c\n-  ----  -\n1  NULL  x\n");

        let (out, err) = execute(
            &server_args(&server, "SELECT 1 AS a WHERE false", &[]),
            no_credentials,
            Utc::now(),
        )
        .await
        .unwrap();
        assert_eq!(out, "a\n-\n");
        assert_eq!(err, "0 rows\n");

        let (out, _) = execute(
            &server_args(
                &server,
                "SELECT [1, 2] AS l, named_struct('k', 'v') AS s",
                &[],
            ),
            no_credentials,
            Utc::now(),
        )
        .await
        .unwrap();
        assert_eq!(out, "l      s\n-----  ---------\n[1,2]  {\"k\":\"v\"}\n");
        server.stop().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn query_marks_truncated_results() {
        let server = TestServer::start().await;
        let args = server_args(
            &server,
            "SELECT x FROM (VALUES (1), (2)) AS t(x)",
            &["--limit", "1"],
        );
        let (out, err) = execute(&args, no_credentials, Utc::now()).await.unwrap();
        assert_eq!(out, "x\n-\n1\n");
        assert_eq!(
            err,
            "1 row; more rows matched; raise --limit or export with --format csv, ndjson or parquet\n"
        );
        server.stop().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn query_binds_parameters() {
        let server = TestServer::start().await;
        let sql = "SELECT name FROM spans WHERE total_tokens = {n:Int}";
        let (out, _) = execute(
            &server_args(&server, sql, &["--param", "n=15"]),
            no_credentials,
            Utc::now(),
        )
        .await
        .unwrap();
        assert_eq!(out, "name\n----\nchat\n");

        let (out, err) = execute(
            &server_args(&server, sql, &["--param", "n=14"]),
            no_credentials,
            Utc::now(),
        )
        .await
        .unwrap();
        assert_eq!(out, "name\n----\n");
        assert_eq!(err, "0 rows\n");
        server.stop().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn query_exports_csv_ndjson_and_parquet() {
        let server = TestServer::start().await;
        let sql = "SELECT name, total_tokens FROM spans ORDER BY name";
        let dir = tempfile::tempdir().unwrap();

        let csv_path = dir.path().join("out.csv");
        let csv_text = csv_path.to_str().unwrap();
        let args = server_args(&server, sql, &["--format", "csv", "--output", csv_text]);
        let (out, err) = execute(&args, no_credentials, Utc::now()).await.unwrap();
        assert_eq!(out, "");
        assert_eq!(err, "");
        assert_eq!(
            fs::read_to_string(&csv_path).unwrap(),
            "name,total_tokens\r\nchat,15\r\n"
        );

        let args = server_args(&server, sql, &["--format", "ndjson"]);
        let (out, err) = execute(&args, no_credentials, Utc::now()).await.unwrap();
        assert_eq!(out, "{\"name\":\"chat\",\"total_tokens\":15}\n");
        assert_eq!(err, "");

        let parquet_path = dir.path().join("out.parquet");
        let parquet_text = parquet_path.to_str().unwrap();
        let args = server_args(
            &server,
            sql,
            &["--format", "parquet", "--output", parquet_text],
        );
        execute(&args, no_credentials, Utc::now()).await.unwrap();
        let contents = fs::read(&parquet_path).unwrap();
        assert!(contents.starts_with(b"PAR1"));
        assert!(contents.ends_with(b"PAR1"));
        let builder = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(
            bytes::Bytes::from(contents),
        )
        .unwrap();
        let names: Vec<String> = builder
            .schema()
            .fields()
            .iter()
            .map(|field| field.name().clone())
            .collect();
        assert_eq!(names, ["name", "total_tokens"]);
        let rows: usize = builder
            .build()
            .unwrap()
            .map(|batch| batch.unwrap().num_rows())
            .sum();
        assert_eq!(rows, 1);
        server.stop().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn query_reports_sql_errors_with_position() {
        let server = TestServer::start().await;
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut input = io::empty();
        let args = server_args(&server, "SELEC 1", &[]);
        let mut streams = Io {
            out: &mut out,
            err: &mut err,
            input: &mut input,
            stdout_is_terminal: false,
        };
        let error = run(&args, no_credentials, &mut streams, Utc::now())
            .await
            .unwrap_err();
        let text = error.to_string();
        assert!(text.starts_with("Bad Request (invalid_filter)"), "{text}");
        assert!(text.contains("(line 1, column"), "{text}");
        assert!(out.is_empty());
        server.stop().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn query_reads_the_saved_credentials() {
        let server = TestServer::start().await;
        let args = QueryArgs {
            sql: Some("SELECT name, total_tokens FROM spans ORDER BY name".to_owned()),
            file: None,
            project: server.project.clone(),
            params: Vec::new(),
            from: Some("2025-12-31T00:00:00Z".to_owned()),
            to: Some("2026-01-02T00:00:00Z".to_owned()),
            since: None,
            limit: None,
            timeout_ms: None,
            format: QueryFormat::Table,
            output: None,
            connection: ConnectionArgs {
                url: None,
                token: None,
            },
        };
        let (out, _) = execute(&args, || Ok(server.credentials()), Utc::now())
            .await
            .unwrap();
        assert_eq!(
            out,
            "name  total_tokens\n----  ------------\nchat            15\n"
        );
        server.stop().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn other_credentials_can_query() {
        let server = TestServer::start().await;
        let sql = "SELECT name, total_tokens FROM spans ORDER BY name";
        let expected = "name  total_tokens\n----  ------------\nchat            15\n";
        let read_token = server.read_token.clone();
        let key = server.key.clone();
        for token in [read_token, key] {
            let mut args = server_args(&server, sql, &[]);
            args.connection.token = Some(Token::from_str(&token).unwrap());
            let (out, _) = execute(&args, no_credentials, Utc::now()).await.unwrap();
            assert_eq!(out, expected);
        }
        server.stop().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn query_reports_a_rejected_token() {
        let server = TestServer::start().await;
        let args = query_args(&[
            "SELECT 1",
            "--url",
            server.url.as_str(),
            "--token",
            "pat-llmobs-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "--project",
            server.project.as_str(),
        ]);
        let error = execute(&args, no_credentials, Utc::now())
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            format!(
                "the server rejected the token; run `llmobs login --url {}`",
                server.url
            )
        );
        server.stop().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn query_reads_the_statement_from_a_file_or_stdin() {
        let server = TestServer::start().await;
        let sql = "SELECT name, total_tokens FROM spans ORDER BY name";
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("q.sql");
        fs::write(&file, sql).unwrap();
        let file_text = file.to_str().unwrap();
        let expected = "name  total_tokens\n----  ------------\nchat            15\n";

        let args = server_args(&server, "", &[]);
        let from_file = QueryArgs {
            sql: None,
            file: Some(PathBuf::from(file_text)),
            ..args
        };
        let (out, _) = execute(&from_file, no_credentials, Utc::now())
            .await
            .unwrap();
        assert_eq!(out, expected);

        let args = server_args(&server, "", &[]);
        let from_stdin = QueryArgs {
            sql: None,
            file: Some(PathBuf::from("-")),
            ..args
        };
        let mut out = Vec::new();
        let mut err = Vec::new();
        let mut input = sql.as_bytes();
        let mut streams = Io {
            out: &mut out,
            err: &mut err,
            input: &mut input,
            stdout_is_terminal: false,
        };
        run(&from_stdin, no_credentials, &mut streams, Utc::now())
            .await
            .unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), expected);

        fs::write(&file, "  \n").unwrap();
        let args = server_args(&server, "", &[]);
        let empty = QueryArgs {
            sql: None,
            file: Some(PathBuf::from(file_text)),
            ..args
        };
        let error = execute(&empty, no_credentials, Utc::now())
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "the query is empty");
        server.stop().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn query_accepts_a_project_uuid() {
        let server = TestServer::start().await;
        let listing: Value = reqwest::Client::new()
            .get(format!("{}/api/v1/orgs/acme/projects", server.url))
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
        let id = listing["data"][0]["id"].as_str().unwrap().to_owned();
        let mut args = server_args(
            &server,
            "SELECT name, total_tokens FROM spans ORDER BY name",
            &[],
        );
        args.project = id;
        let (out, _) = execute(&args, no_credentials, Utc::now()).await.unwrap();
        assert_eq!(
            out,
            "name  total_tokens\n----  ------------\nchat            15\n"
        );
        server.stop().await;
    }
}
