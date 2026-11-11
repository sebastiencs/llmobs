//! Run one scenario: mock, receiver, program, then write or check the fixture.
//!
//! The program sees a cleared environment. Stdout and stderr are forwarded to
//! `tracing` at debug level, prefixed with the case path.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::{self, DirEntry};
use std::path::{Path, PathBuf};
use std::time::Duration;

use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::net::TcpListener;
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;

use crate::deterministic;
use crate::error::CaptureError;
use crate::mock::Mock;
use crate::receiver::Receiver;
use crate::scenario::{CasePath, Provider, Scenario};
use crate::scrub::{ScrubContext, scrub};
use crate::write::{Language, Source, write_case};

const DEFAULT_INGEST_TIME: &str = "2026-01-01T00:00:00.000000000Z";
const PROGRAM_TIMEOUT: Duration = Duration::from_secs(120);
const QUIET: Duration = Duration::from_millis(300);
const IDLE_MAX: Duration = Duration::from_secs(5);
const DIFF_LIMIT: usize = 20;
const MOCK_KEY: &str = "capture-mock-key";
const TIME_FIELDS: &[&str] = &["startTimeUnixNano", "endTimeUnixNano", "timeUnixNano"];

/// Command that runs one scenario program.
///
/// The runner spawns this argv with a cleared environment. [`Program::argv`]
/// passes a different command, such as `false`, through that same path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    argv: Vec<String>,
}

impl Program {
    /// `uv` for Python, or `node` for TypeScript.
    pub fn for_language(repo_root: &Path, language: Language) -> Self {
        match language {
            Language::Python => Self::python(repo_root),
            Language::Typescript => Self::typescript(),
        }
    }

    /// Spawn `argv` instead of the language command.
    pub fn argv(argv: Vec<String>) -> Self {
        Self { argv }
    }

    fn python(repo_root: &Path) -> Self {
        Self {
            argv: vec![
                "uv".to_owned(),
                "run".to_owned(),
                "--project".to_owned(),
                repo_root
                    .join("tools/capture/py")
                    .to_string_lossy()
                    .into_owned(),
                "--frozen".to_owned(),
                "python".to_owned(),
                "main.py".to_owned(),
            ],
        }
    }

    fn typescript() -> Self {
        Self {
            argv: vec!["node".to_owned(), "main.ts".to_owned()],
        }
    }
}

/// A case that finished and was written or checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// Case that ran.
    pub case: CasePath,
    /// `expected.json` is present and the written `input.json` changed.
    pub stale_expected: bool,
}

/// Scans `tools/capture/{py,ts}/scenarios/*/*/scenario.json`.
///
/// A missing `scenarios` directory is an empty result. The same `group/case`
/// in both languages fails. The vector is sorted by group, then case.
pub fn discover(repo_root: &Path) -> Result<Vec<(CasePath, Language, PathBuf)>, CaptureError> {
    let mut found = Vec::new();
    scan(repo_root, Language::Python, &mut found)?;
    scan(repo_root, Language::Typescript, &mut found)?;
    let mut seen = HashSet::new();
    for (case, _, _) in &found {
        if !seen.insert(case.to_string()) {
            return Err(CaptureError::DuplicateCase {
                case: case.to_string(),
            });
        }
    }
    found.sort_by(|left, right| {
        left.0
            .group
            .cmp(&right.0.group)
            .then(left.0.case.cmp(&right.0.case))
    });
    Ok(found)
}

/// Environment for one program, built from `parent` rather than the process.
///
/// `PATH`, `HOME`, and every `UV_*` and `XDG_*` variable are copied. The fixed
/// capture and OTel variables replace anything copied. `scenario.env` is
/// applied last. Nothing else from `parent` is present.
pub fn program_env(
    parent: &HashMap<String, String>,
    scenario: &Scenario,
    mock_port: u16,
    receiver_port: u16,
    meta_path: &str,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for (key, value) in parent {
        if key == "PATH" || key == "HOME" || key.starts_with("UV_") || key.starts_with("XDG_") {
            env.insert(key.clone(), value.clone());
        }
    }
    let receiver = format!("http://127.0.0.1:{receiver_port}/v1/traces");
    env.insert("LANG".to_owned(), "C.UTF-8".to_owned());
    env.insert("TZ".to_owned(), "UTC".to_owned());
    env.insert("PYTHONHASHSEED".to_owned(), "0".to_owned());
    env.insert(
        "LLMOBS_CAPTURE_MOCK_URL".to_owned(),
        format!("http://127.0.0.1:{mock_port}"),
    );
    env.insert("LLMOBS_CAPTURE_OTLP_ENDPOINT".to_owned(), receiver.clone());
    env.insert("LLMOBS_CAPTURE_META".to_owned(), meta_path.to_owned());
    env.insert(
        "LLMOBS_CAPTURE_LIBRARIES".to_owned(),
        scenario.libraries.join(","),
    );
    env.insert("LLMOBS_CAPTURE_CASE".to_owned(), scenario.case.to_string());
    for key in [
        "OPENAI_API_KEY",
        "ANTHROPIC_API_KEY",
        "GEMINI_API_KEY",
        "GOOGLE_API_KEY",
    ] {
        env.insert(key.to_owned(), MOCK_KEY.to_owned());
    }
    env.insert(
        "OTEL_EXPORTER_OTLP_PROTOCOL".to_owned(),
        "http/protobuf".to_owned(),
    );
    env.insert("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT".to_owned(), receiver);
    env.insert(
        "OTEL_EXPORTER_OTLP_COMPRESSION".to_owned(),
        "none".to_owned(),
    );
    env.insert("OTEL_TRACES_SAMPLER".to_owned(), "always_on".to_owned());
    env.insert("OTEL_SERVICE_NAME".to_owned(), "capture-app".to_owned());
    env.extend(scenario.env.clone());
    env
}

/// Loads `scenario_dir`, runs `program`, and writes or checks the fixture.
///
/// `check` compares the capture with `fixtures/otlp/{group}/{case}` and does
/// not replace those files. Times, timestamp attributes, timing attributes, and
/// `source.captured_at` are ignored in that comparison.
pub async fn run_case(
    repo_root: &Path,
    scenario_dir: &Path,
    language: Language,
    check: bool,
    program: &Program,
) -> Result<Outcome, CaptureError> {
    let scenario = Scenario::load(scenario_dir)?;
    let mock = Mock::new(scenario.turns.clone());
    let receiver = Receiver::new();
    let (mock_port, mock_server) = bind(mock.router()).await?;
    let (receiver_port, receiver_server) = bind(receiver.router()).await?;
    let meta = MetaFile::create(&scenario.case)?;
    let meta_path = utf8_path(meta.path())?;
    let parent = std::env::vars().collect();
    let env = program_env(&parent, &scenario, mock_port, receiver_port, meta_path);
    let mut child = spawn_program(program, scenario_dir, &env)?;
    let lines = forward_output(&mut child, &scenario.case);
    let waited = wait_program(&mut child).await;
    lines.drain().await;
    waited?;
    receiver.wait_idle(QUIET, IDLE_MAX).await;
    let (exports, libraries) = collect(
        &scenario.case,
        &mock,
        &receiver,
        meta.path(),
        &scenario.libraries,
    )?;
    let request = process(repo_root, &scenario, exports, mock_port, receiver_port)?;
    let source = source_from(&scenario, language, libraries)?;
    let outcome = emit(repo_root, &scenario, &request, &source, check)?;
    drop(mock_server);
    drop(receiver_server);
    drop(meta);
    Ok(outcome)
}

fn scan(
    repo_root: &Path,
    language: Language,
    found: &mut Vec<(CasePath, Language, PathBuf)>,
) -> Result<(), CaptureError> {
    let dir = repo_root
        .join("tools/capture")
        .join(language_dir(language))
        .join("scenarios");
    if !dir.exists() {
        return Ok(());
    }
    for group_entry in read_dir(&dir)? {
        if !group_entry.path().is_dir() {
            continue;
        }
        let group = entry_name(&group_entry)?;
        for case_entry in read_dir(&group_entry.path())? {
            if !case_entry.path().is_dir() {
                continue;
            }
            if !case_entry.path().join("scenario.json").is_file() {
                continue;
            }
            let case_name = entry_name(&case_entry)?;
            let case = format!("{group}/{case_name}").parse()?;
            found.push((case, language, case_entry.path()));
        }
    }
    Ok(())
}

fn language_dir(language: Language) -> &'static str {
    match language {
        Language::Python => "py",
        Language::Typescript => "ts",
    }
}

fn read_dir(path: &Path) -> Result<Vec<DirEntry>, CaptureError> {
    let entries = fs::read_dir(path).map_err(|source| CaptureError::ScenarioIo {
        path: path.to_path_buf(),
        source,
    })?;
    entries
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| CaptureError::ScenarioIo {
            path: path.to_path_buf(),
            source,
        })
}

fn entry_name(entry: &DirEntry) -> Result<String, CaptureError> {
    entry
        .file_name()
        .into_string()
        .map_err(|_| CaptureError::ScenarioInvalid {
            case: entry.path().to_string_lossy().into_owned(),
            reason: "directory name must be utf-8".to_owned(),
        })
}

async fn bind(router: axum::Router) -> Result<(u16, Server), CaptureError> {
    let listener =
        TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|source| CaptureError::Process {
                step: "bind a loopback port",
                source,
            })?;
    let port = listener
        .local_addr()
        .map_err(|source| CaptureError::Process {
            step: "read a loopback port",
            source,
        })?
        .port();
    let handle = tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, router).await {
            tracing::debug!("capture server stopped: {error}");
        }
    });
    Ok((port, Server { handle }))
}

struct Server {
    handle: JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

struct MetaFile {
    path: PathBuf,
}

impl MetaFile {
    fn create(case: &CasePath) -> Result<Self, CaptureError> {
        let path = unique_temp("llmobs-capture-meta", case);
        if path.exists() {
            fs::remove_file(&path).map_err(|source| CaptureError::Process {
                step: "prepare the meta file",
                source,
            })?;
        }
        Ok(Self { path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for MetaFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn unique_temp(prefix: &str, case: &CasePath) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    std::env::temp_dir().join(format!(
        "{prefix}-{}-{}-{}-{nanos}",
        std::process::id(),
        case.group,
        case.case,
    ))
}

fn utf8_path(path: &Path) -> Result<&str, CaptureError> {
    path.to_str().ok_or(CaptureError::MetaPath)
}

fn spawn_program(
    program: &Program,
    dir: &Path,
    env: &BTreeMap<String, String>,
) -> Result<Child, CaptureError> {
    let Some((bin, args)) = program.argv.split_first() else {
        return Err(CaptureError::ProgramCommand);
    };
    Command::new(bin)
        .args(args)
        .current_dir(dir)
        .kill_on_drop(true)
        .env_clear()
        .envs(env)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|source| CaptureError::Process {
            step: "spawn the program",
            source,
        })
}

fn forward_output(child: &mut Child, case: &CasePath) -> Lines {
    let mut tasks = Vec::new();
    if let Some(pipe) = child.stdout.take() {
        tasks.push(spawn_lines(pipe, case, "stdout"));
    }
    if let Some(pipe) = child.stderr.take() {
        tasks.push(spawn_lines(pipe, case, "stderr"));
    }
    Lines { tasks }
}

fn spawn_lines<R>(pipe: R, case: &CasePath, stream: &'static str) -> JoinHandle<()>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let case = case.to_string();
    tokio::spawn(async move {
        let mut lines = BufReader::new(pipe).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::debug!("{case} {stream}: {line}");
        }
    })
}

struct Lines {
    tasks: Vec<JoinHandle<()>>,
}

impl Lines {
    async fn drain(mut self) {
        for mut task in self.tasks.drain(..) {
            if tokio::time::timeout(Duration::from_secs(2), &mut task)
                .await
                .is_err()
            {
                task.abort();
            }
        }
    }
}

impl Drop for Lines {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

async fn wait_program(child: &mut Child) -> Result<(), CaptureError> {
    match tokio::time::timeout(PROGRAM_TIMEOUT, child.wait()).await {
        Ok(Ok(status)) if status.success() => Ok(()),
        Ok(Ok(status)) => Err(CaptureError::ProgramFailed {
            code: status.code(),
        }),
        Ok(Err(source)) => Err(CaptureError::Process {
            step: "wait for the program",
            source,
        }),
        Err(_elapsed) => {
            let _ = child.kill().await;
            let _ = child.wait().await;
            Err(CaptureError::ProgramTimeout)
        }
    }
}

fn collect(
    case: &CasePath,
    mock: &Mock,
    receiver: &Receiver,
    meta_path: &Path,
    required: &[String],
) -> Result<(Vec<ExportTraceServiceRequest>, BTreeMap<String, String>), CaptureError> {
    let received = receiver.finish();
    let mock_log = mock.finish();
    let mut reasons = Vec::new();
    reasons.extend(mock_log.errors);
    reasons.extend(received.errors);
    if !has_spans(&received.exports) {
        reasons.push("no spans arrived".to_owned());
    }
    let versions = match read_libraries(meta_path, required) {
        Ok(versions) => versions,
        Err(meta_reasons) => {
            reasons.extend(meta_reasons);
            return Err(CaptureError::CaptureIncomplete {
                case: case.to_string(),
                reasons,
            });
        }
    };
    if !reasons.is_empty() {
        return Err(CaptureError::CaptureIncomplete {
            case: case.to_string(),
            reasons,
        });
    }
    Ok((received.exports, versions))
}

fn has_spans(exports: &[ExportTraceServiceRequest]) -> bool {
    exports.iter().any(|export| {
        export.resource_spans.iter().any(|resource| {
            resource
                .scope_spans
                .iter()
                .any(|scope| !scope.spans.is_empty())
        })
    })
}

fn read_libraries(
    path: &Path,
    required: &[String],
) -> Result<BTreeMap<String, String>, Vec<String>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Err(vec!["meta file is missing".to_owned()]);
        }
        Err(_) => return Err(vec!["meta file could not be read".to_owned()]),
    };
    let value: Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(_) => return Err(vec!["meta file could not be parsed".to_owned()]),
    };
    let Some(libraries) = value.get("libraries").and_then(Value::as_object) else {
        return Err(vec!["meta file could not be parsed".to_owned()]);
    };
    let mut versions = BTreeMap::new();
    let mut bad = HashSet::new();
    let mut reasons = Vec::new();
    for (name, version) in libraries {
        match version.as_str() {
            Some(version) => {
                versions.insert(name.clone(), version.to_owned());
            }
            None => {
                bad.insert(name.clone());
                reasons.push(format!("meta file library {name} is not a string"));
            }
        }
    }
    for name in required {
        if !versions.contains_key(name) && !bad.contains(name) {
            reasons.push(format!("meta file is missing library {name}"));
        }
    }
    if reasons.is_empty() {
        Ok(versions)
    } else {
        Err(reasons)
    }
}

fn process(
    repo_root: &Path,
    scenario: &Scenario,
    exports: Vec<ExportTraceServiceRequest>,
    mock_port: u16,
    receiver_port: u16,
) -> Result<ExportTraceServiceRequest, CaptureError> {
    let mut request = deterministic(
        exports,
        &scenario.case,
        ingest_nanos(scenario)?,
        &scenario.timestamp_attributes,
    )?;
    scrub(
        &mut request,
        &ScrubContext {
            provider: provider(scenario)?,
            mock_port,
            receiver_port,
            home: home_dir()?,
            repo_root: repo_root.to_path_buf(),
            hostname: machine_hostname()?,
        },
    )?;
    Ok(request)
}

fn provider(scenario: &Scenario) -> Result<Provider, CaptureError> {
    match scenario.turns.first() {
        Some(turn) => Ok(turn.api.provider()),
        None => Err(CaptureError::ScenarioInvalid {
            case: scenario.case.to_string(),
            reason: "turns must be non-empty".to_owned(),
        }),
    }
}

fn ingest_nanos(scenario: &Scenario) -> Result<i64, CaptureError> {
    let text = match scenario
        .context
        .as_ref()
        .and_then(|context| context.get("ingest_time"))
    {
        None => DEFAULT_INGEST_TIME,
        Some(value) => value.as_str().ok_or(CaptureError::IngestTime)?,
    };
    let parsed =
        chrono::DateTime::parse_from_rfc3339(text).map_err(|_| CaptureError::IngestTime)?;
    parsed.timestamp_nanos_opt().ok_or(CaptureError::IngestTime)
}

fn home_dir() -> Result<PathBuf, CaptureError> {
    match std::env::var_os("HOME") {
        Some(value) if !value.is_empty() => Ok(PathBuf::from(value)),
        _ => Err(CaptureError::Home),
    }
}

fn machine_hostname() -> Result<String, CaptureError> {
    if let Some(name) = first_line("/etc/hostname") {
        return Ok(name);
    }
    if let Ok(name) = std::env::var("HOSTNAME") {
        let name = name.trim();
        if !name.is_empty() {
            return Ok(name.to_owned());
        }
    }
    Err(CaptureError::Hostname)
}

fn first_line(path: &str) -> Option<String> {
    let name = fs::read_to_string(path)
        .ok()?
        .lines()
        .next()?
        .trim()
        .to_owned();
    if name.is_empty() { None } else { Some(name) }
}

fn source_from(
    scenario: &Scenario,
    language: Language,
    libraries: BTreeMap<String, String>,
) -> Result<Source, CaptureError> {
    let Some(instrumentation_version) = libraries.get(&scenario.instrumentation).cloned() else {
        return Err(CaptureError::CaptureIncomplete {
            case: scenario.case.to_string(),
            reasons: vec![format!(
                "meta file is missing library {}",
                scenario.instrumentation
            )],
        });
    };
    Ok(Source {
        instrumentation: scenario.instrumentation.clone(),
        instrumentation_version,
        language,
        libraries,
        captured_at: captured_at_now(),
    })
}

fn captured_at_now() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn emit(
    repo_root: &Path,
    scenario: &Scenario,
    request: &ExportTraceServiceRequest,
    source: &Source,
    check: bool,
) -> Result<Outcome, CaptureError> {
    let fixture_dir = fixture_dir(repo_root, &scenario.case);
    let temp = check.then(|| TempCapture::new(&scenario.case));
    let dir = temp
        .as_ref()
        .map_or(fixture_dir.as_path(), TempCapture::path);
    let written = write_case(dir, scenario, request, source)?;
    if let Some(temp) = &temp {
        check_written(repo_root, scenario, temp.path())?;
        return Ok(Outcome {
            case: scenario.case.clone(),
            stale_expected: false,
        });
    }
    Ok(Outcome {
        case: scenario.case.clone(),
        stale_expected: written.stale_expected,
    })
}

fn fixture_dir(repo_root: &Path, case: &CasePath) -> PathBuf {
    repo_root
        .join("fixtures/otlp")
        .join(&case.group)
        .join(&case.case)
}

/// Renders with [`write_case`] so `--check` compares the writer's bytes.
struct TempCapture {
    path: PathBuf,
}

impl TempCapture {
    fn new(case: &CasePath) -> Self {
        Self {
            path: unique_temp("llmobs-capture-check", case),
        }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempCapture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn check_written(
    repo_root: &Path,
    scenario: &Scenario,
    produced_dir: &Path,
) -> Result<(), CaptureError> {
    let (committed_case, committed_input) =
        read_committed(&fixture_dir(repo_root, &scenario.case), &scenario.case)?;
    let produced_case = read_produced(&produced_dir.join("case.json"))?;
    let produced_input = read_produced(&produced_dir.join("input.json"))?;
    compare_texts(
        &scenario.case,
        &committed_case,
        &committed_input,
        &produced_case,
        &produced_input,
        scenario,
    )
}

fn read_committed(dir: &Path, case: &CasePath) -> Result<(String, String), CaptureError> {
    let case_json = read_committed_file(&dir.join("case.json"), case)?;
    let input_json = read_committed_file(&dir.join("input.json"), case)?;
    Ok((case_json, input_json))
}

fn read_committed_file(path: &Path, case: &CasePath) -> Result<String, CaptureError> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            Err(CaptureError::NotCaptured {
                case: case.to_string(),
            })
        }
        Err(source) => Err(CaptureError::ReadIo {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn read_produced(path: &Path) -> Result<String, CaptureError> {
    fs::read_to_string(path).map_err(|source| CaptureError::ReadIo {
        path: path.to_path_buf(),
        source,
    })
}

fn compare_texts(
    case: &CasePath,
    committed_case: &str,
    committed_input: &str,
    produced_case: &str,
    produced_input: &str,
    scenario: &Scenario,
) -> Result<(), CaptureError> {
    let paths = json_differences(
        &parse_json("case.json", committed_case)?,
        &parse_json("case.json", produced_case)?,
        &parse_json("input.json", committed_input)?,
        &parse_json("input.json", produced_input)?,
        scenario,
    );
    if paths.is_empty() {
        Ok(())
    } else {
        Err(CaptureError::CheckFailed {
            case: case.to_string(),
            paths,
        })
    }
}

fn parse_json(file: &'static str, text: &str) -> Result<Value, CaptureError> {
    serde_json::from_str(text).map_err(|_| CaptureError::FixtureJson { file })
}

fn json_differences(
    committed_case: &Value,
    produced_case: &Value,
    committed_input: &Value,
    produced_input: &Value,
    scenario: &Scenario,
) -> Vec<String> {
    let mut committed_case = committed_case.clone();
    let mut produced_case = produced_case.clone();
    let mut committed_input = committed_input.clone();
    let mut produced_input = produced_input.clone();
    mask_captured_at(&mut committed_case);
    mask_captured_at(&mut produced_case);
    let keys = attribute_keys(scenario);
    mask_tree(&mut committed_input, &keys);
    mask_tree(&mut produced_input, &keys);
    let mut paths = Vec::new();
    diff("case.json", "", &committed_case, &produced_case, &mut paths);
    diff(
        "input.json",
        "",
        &committed_input,
        &produced_input,
        &mut paths,
    );
    paths
}

fn attribute_keys(scenario: &Scenario) -> HashSet<&str> {
    scenario
        .timestamp_attributes
        .iter()
        .map(|attribute| attribute.key.as_str())
        .chain(scenario.timing_attributes.iter().map(String::as_str))
        .collect()
}

fn mask_captured_at(value: &mut Value) {
    let Some(source) = value.get_mut("source").and_then(Value::as_object_mut) else {
        return;
    };
    if source.contains_key("captured_at") {
        source.insert("captured_at".to_owned(), Value::Null);
    }
}

fn mask_tree(value: &mut Value, keys: &HashSet<&str>) {
    match value {
        Value::Array(items) => {
            for item in items {
                mask_tree(item, keys);
            }
        }
        Value::Object(map) => {
            let matched = map
                .get("key")
                .and_then(Value::as_str)
                .is_some_and(|key| keys.contains(key));
            if matched {
                map.insert("value".to_owned(), Value::Null);
            }
            for field in TIME_FIELDS {
                if map.contains_key(*field) {
                    map.insert((*field).to_owned(), Value::Null);
                }
            }
            for (field, child) in map.iter_mut() {
                if child.is_null() && (field == "value" || TIME_FIELDS.contains(&field.as_str())) {
                    continue;
                }
                mask_tree(child, keys);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn diff(file: &str, path: &str, left: &Value, right: &Value, out: &mut Vec<String>) {
    if out.len() >= DIFF_LIMIT {
        return;
    }
    match (left, right) {
        (Value::Object(left), Value::Object(right)) => {
            let mut keys = BTreeSet::new();
            keys.extend(left.keys().map(String::as_str));
            keys.extend(right.keys().map(String::as_str));
            for key in keys {
                let child = join_key(path, key);
                match (left.get(key), right.get(key)) {
                    (Some(left), Some(right)) => diff(file, &child, left, right, out),
                    _ => push_diff(file, &child, out),
                }
            }
        }
        (Value::Array(left), Value::Array(right)) => {
            if left.len() != right.len() {
                push_diff(file, &join_key(path, "length"), out);
            }
            for (index, (left, right)) in left.iter().zip(right).enumerate() {
                diff(file, &format!("{path}[{index}]"), left, right, out);
            }
        }
        (left, right) if left == right => {}
        _ => push_diff(file, path, out),
    }
}

fn join_key(path: &str, key: &str) -> String {
    if path.is_empty() {
        key.to_owned()
    } else {
        format!("{path}.{key}")
    }
}

fn push_diff(file: &str, path: &str, out: &mut Vec<String>) {
    if out.len() >= DIFF_LIMIT {
        return;
    }
    if path.is_empty() {
        out.push(file.to_owned());
    } else {
        out.push(format!("{file}: {path}"));
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::str::FromStr;

    use serde_json::{Value, json};

    use super::*;
    use crate::error::CaptureError;
    use crate::scenario::CasePath;
    use crate::write::Language;

    const SCENARIO: &str = r#"{
      "description": "A chat turn.",
      "spec_refs": ["02"],
      "instrumentation": "openinference-instrumentation-openai",
      "libraries": ["openinference-instrumentation-openai"],
      "turns": [{
        "api": "openai.chat",
        "model": "gpt-4o-mini-2024-07-18",
        "message": { "text": "hello" },
        "finish": "stop",
        "usage": { "input": 1, "output": 1 }
      }]
    }"#;

    struct Tmp(PathBuf);

    impl Tmp {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("llmobs-capture-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            Self(path)
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn write_scenario(root: &Path, language: Language, group: &str, case: &str) {
        let dir = root
            .join("tools/capture")
            .join(match language {
                Language::Python => "py",
                Language::Typescript => "ts",
            })
            .join("scenarios")
            .join(group)
            .join(case);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("scenario.json"), "{}\n").unwrap();
    }

    fn scenario(env: Value, context: Option<Value>) -> Scenario {
        let mut value = json!({
            "description": "A chat turn.",
            "spec_refs": ["02"],
            "instrumentation": "openinference-instrumentation-openai",
            "libraries": ["zeta", "alpha"],
            "timestamp_attributes": [{
                "key": "langfuse.observation.completion_start_time",
                "format": "rfc3339"
            }],
            "timing_attributes": ["ai.response.msToFirstChunk"],
            "turns": [{
                "api": "openai.chat",
                "model": "gpt-4o-mini-2024-07-18",
                "message": { "text": "hello" },
                "finish": "stop",
                "usage": { "input": 1, "output": 1 }
            }]
        });
        value["env"] = env;
        if let Some(context) = context {
            value["context"] = context;
        }
        let mut scenario: Scenario = serde_json::from_value(value).unwrap();
        scenario.case = CasePath::from_str("genai/alpha").unwrap();
        scenario
    }

    fn input_export(
        name: &str,
        start: &str,
        end: &str,
        event: &str,
        timing: &str,
        model: &str,
        stamp: &str,
    ) -> Value {
        json!({
            "resourceSpans": [{
                "scopeSpans": [{
                    "spans": [{
                        "name": name,
                        "startTimeUnixNano": start,
                        "endTimeUnixNano": end,
                        "attributes": [
                            {"key": "ai.response.msToFirstChunk", "value": {"intValue": timing}},
                            {"key": "gen_ai.request.model", "value": {"stringValue": model}},
                            {"key": "bag", "value": {"kvlistValue": {"values": [
                                {"key": "langfuse.observation.completion_start_time", "value": {"stringValue": stamp}}
                            ]}}}
                        ],
                        "events": [{
                            "name": "chunk",
                            "timeUnixNano": event,
                            "attributes": [
                                {"key": "ai.response.msToFirstChunk", "value": {"intValue": timing}}
                            ]
                        }]
                    }]
                }]
            }]
        })
    }

    fn case_file(description: &str, captured_at: &str) -> Value {
        json!({
            "description": description,
            "source": {
                "instrumentation": "openinference-instrumentation-openai",
                "captured_at": captured_at
            }
        })
    }

    #[test]
    fn discover_reads_both_languages_and_rejects_a_duplicate() {
        let tmp = Tmp::new("discover");
        write_scenario(&tmp.0, Language::Python, "genai", "alpha");
        write_scenario(&tmp.0, Language::Typescript, "genai", "zeta");
        fs::create_dir_all(tmp.0.join("tools/capture/py/scenarios/genai/skipped")).unwrap();
        let found = discover(&tmp.0).unwrap();
        let labels: Vec<_> = found
            .iter()
            .map(|(case, language, _)| (case.to_string(), *language))
            .collect();
        assert_eq!(
            labels,
            vec![
                ("genai/alpha".to_owned(), Language::Python),
                ("genai/zeta".to_owned(), Language::Typescript),
            ]
        );
        write_scenario(&tmp.0, Language::Typescript, "genai", "alpha");
        let error = discover(&tmp.0).unwrap_err();
        assert_eq!(
            error.to_string(),
            "case genai/alpha is in both the Python and TypeScript scenarios"
        );
    }

    #[test]
    fn discover_treats_a_missing_directory_as_empty() {
        let tmp = Tmp::new("discover-missing");
        fs::create_dir_all(&tmp.0).unwrap();
        assert!(discover(&tmp.0).unwrap().is_empty());
        write_scenario(&tmp.0, Language::Typescript, "openinference", "chat-tool");
        let found = discover(&tmp.0).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].1, Language::Typescript);
        assert_eq!(found[0].0.to_string(), "openinference/chat-tool");
    }

    #[test]
    fn discover_rejects_an_unknown_group() {
        let tmp = Tmp::new("discover-group");
        write_scenario(&tmp.0, Language::Python, "not-a-group", "alpha");
        let error = discover(&tmp.0).unwrap_err();
        assert!(error.to_string().contains("unknown group"), "{error}");
    }

    #[test]
    fn program_commands_match_the_plan() {
        assert_eq!(
            Program::python(Path::new("/repo")).argv,
            [
                "uv",
                "run",
                "--project",
                "/repo/tools/capture/py",
                "--frozen",
                "python",
                "main.py",
            ]
        );
        assert_eq!(Program::typescript().argv, ["node", "main.ts"]);
        assert_eq!(
            Program::for_language(Path::new("/repo"), Language::Typescript).argv,
            ["node", "main.ts"]
        );
    }

    #[test]
    fn program_env_drops_parent_secrets_and_applies_scenario_env() {
        let mut parent = HashMap::new();
        parent.insert("PATH".to_owned(), "/usr/bin".to_owned());
        parent.insert("HOME".to_owned(), "/home/capture-user".to_owned());
        parent.insert("UV_CACHE_DIR".to_owned(), "/tmp/uv".to_owned());
        parent.insert("XDG_CACHE_HOME".to_owned(), "/tmp/xdg".to_owned());
        parent.insert("OPENAI_API_KEY".to_owned(), "sk-parent-secret".to_owned());
        parent.insert(
            "AWS_SECRET_ACCESS_KEY".to_owned(),
            "aws-parent-secret".to_owned(),
        );
        parent.insert("LANG".to_owned(), "en_US.UTF-8".to_owned());
        parent.insert("OTEL_SERVICE_NAME".to_owned(), "from-parent".to_owned());
        parent.insert("OTEL_TRACES_SAMPLER".to_owned(), "always_off".to_owned());
        parent.insert("UNRELATED".to_owned(), "nope".to_owned());
        let scenario = scenario(json!({"OTEL_SERVICE_NAME": "from-scenario"}), None);
        let env = program_env(&parent, &scenario, 40123, 40124, "/tmp/meta.json");
        assert_eq!(env.get("PATH").map(String::as_str), Some("/usr/bin"));
        assert_eq!(
            env.get("HOME").map(String::as_str),
            Some("/home/capture-user")
        );
        assert_eq!(env.get("UV_CACHE_DIR").map(String::as_str), Some("/tmp/uv"));
        assert_eq!(
            env.get("XDG_CACHE_HOME").map(String::as_str),
            Some("/tmp/xdg")
        );
        assert_eq!(env.get("LANG").map(String::as_str), Some("C.UTF-8"));
        assert_eq!(env.get("TZ").map(String::as_str), Some("UTC"));
        assert_eq!(env.get("PYTHONHASHSEED").map(String::as_str), Some("0"));
        assert_eq!(
            env.get("LLMOBS_CAPTURE_MOCK_URL").map(String::as_str),
            Some("http://127.0.0.1:40123")
        );
        assert_eq!(
            env.get("LLMOBS_CAPTURE_OTLP_ENDPOINT").map(String::as_str),
            Some("http://127.0.0.1:40124/v1/traces")
        );
        assert_eq!(
            env.get("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT")
                .map(String::as_str),
            Some("http://127.0.0.1:40124/v1/traces")
        );
        assert_eq!(
            env.get("LLMOBS_CAPTURE_META").map(String::as_str),
            Some("/tmp/meta.json")
        );
        assert_eq!(
            env.get("LLMOBS_CAPTURE_LIBRARIES").map(String::as_str),
            Some("zeta,alpha")
        );
        assert_eq!(
            env.get("LLMOBS_CAPTURE_CASE").map(String::as_str),
            Some("genai/alpha")
        );
        for key in [
            "OPENAI_API_KEY",
            "ANTHROPIC_API_KEY",
            "GEMINI_API_KEY",
            "GOOGLE_API_KEY",
        ] {
            assert_eq!(env.get(key).map(String::as_str), Some(MOCK_KEY));
        }
        assert_eq!(
            env.get("OTEL_EXPORTER_OTLP_PROTOCOL").map(String::as_str),
            Some("http/protobuf")
        );
        assert_eq!(
            env.get("OTEL_EXPORTER_OTLP_COMPRESSION")
                .map(String::as_str),
            Some("none")
        );
        assert_eq!(
            env.get("OTEL_TRACES_SAMPLER").map(String::as_str),
            Some("always_on")
        );
        assert_eq!(
            env.get("OTEL_SERVICE_NAME").map(String::as_str),
            Some("from-scenario")
        );
        assert_eq!(env.len(), 21);
        assert!(!env.contains_key("AWS_SECRET_ACCESS_KEY"));
        assert!(!env.contains_key("UNRELATED"));
        let rendered = format!("{env:?}");
        assert!(!rendered.contains("sk-parent-secret"));
        assert!(!rendered.contains("aws-parent-secret"));
        assert!(!rendered.contains("hello"));
    }

    #[test]
    fn ingest_time_defaults_to_the_readme_instant() {
        let absent = scenario(json!({}), None);
        let explicit = scenario(
            json!({}),
            Some(json!({"ingest_time": "2026-01-01T00:00:00.000000000Z"})),
        );
        let project_only = scenario(
            json!({}),
            Some(json!({"project_id": "00000000-0000-7000-8000-000000000001"})),
        );
        assert_eq!(ingest_nanos(&absent).unwrap(), 1_767_225_600_000_000_000);
        assert_eq!(
            ingest_nanos(&explicit).unwrap(),
            ingest_nanos(&absent).unwrap()
        );
        assert_eq!(
            ingest_nanos(&project_only).unwrap(),
            ingest_nanos(&absent).unwrap()
        );
        let bad = scenario(json!({}), Some(json!({"ingest_time": "not-a-time"})));
        let error = ingest_nanos(&bad).unwrap_err();
        assert_eq!(error.to_string(), "context.ingest_time could not be parsed");
        assert!(!error.to_string().contains("not-a-time"));
        let number = scenario(json!({}), Some(json!({"ingest_time": 424242})));
        let error = ingest_nanos(&number).unwrap_err();
        assert!(!error.to_string().contains("424242"));
    }

    #[test]
    fn check_masks_times_and_reports_other_fields() {
        let scenario = scenario(json!({}), None);
        let committed_input = input_export("chat", "100", "200", "150", "9", "gpt", "t0");
        let produced_input = input_export(
            "chat",
            "SECRET-TIME",
            "SECRET-END",
            "SECRET-EVENT",
            "SECRET-TIMING",
            "gpt",
            "SECRET-STAMP",
        );
        let committed_case = case_file("A chat turn.", "2026-01-01T00:00:00Z");
        let produced_case = case_file("A chat turn.", "2026-11-11T01:44:00Z");
        let paths = json_differences(
            &committed_case,
            &produced_case,
            &committed_input,
            &produced_input,
            &scenario,
        );
        assert!(paths.is_empty(), "{paths:?}");

        let produced_input = input_export(
            "other",
            "SECRET-TIME",
            "SECRET-END",
            "SECRET-EVENT",
            "SECRET-TIMING",
            "SECRET-MODEL",
            "SECRET-STAMP",
        );
        let produced_case = case_file("Other", "2026-11-11T01:44:00Z");
        let paths = json_differences(
            &committed_case,
            &produced_case,
            &committed_input,
            &produced_input,
            &scenario,
        );
        assert_eq!(
            paths,
            vec![
                "case.json: description".to_owned(),
                "input.json: resourceSpans[0].scopeSpans[0].spans[0].attributes[1].value.stringValue".to_owned(),
                "input.json: resourceSpans[0].scopeSpans[0].spans[0].name".to_owned(),
            ]
        );
        let error = CaptureError::CheckFailed {
            case: "genai/alpha".to_owned(),
            paths,
        };
        let text = format!("{error} {error:?}");
        assert!(!text.contains("SECRET"));
    }

    #[test]
    fn check_stops_at_20_paths() {
        let scenario = scenario(json!({}), None);
        let mut left = serde_json::Map::new();
        let mut right = serde_json::Map::new();
        for index in 0..25 {
            let key = format!("k{index:02}");
            left.insert(key.clone(), Value::from("a"));
            right.insert(key, Value::from("b"));
        }
        let same = json!({});
        let paths = json_differences(
            &same,
            &same,
            &Value::Object(left),
            &Value::Object(right),
            &scenario,
        );
        assert_eq!(paths.len(), 20);
        assert_eq!(paths[0], "input.json: k00");
        assert_eq!(paths[19], "input.json: k19");
        assert!(paths.iter().all(|path| !path.ends_with("k20")));
    }

    #[test]
    fn missing_committed_case_is_not_captured_yet() {
        let tmp = Tmp::new("not-captured");
        fs::create_dir_all(&tmp.0).unwrap();
        let case = CasePath::from_str("genai/alpha").unwrap();
        let error = read_committed(&tmp.0.join("fixtures/otlp/genai/alpha"), &case).unwrap_err();
        assert_eq!(error.to_string(), "genai/alpha is not captured yet");
        let dir = tmp.0.join("fixtures/otlp/genai/alpha");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("case.json"), "{}\n").unwrap();
        let error = read_committed(&dir, &case).unwrap_err();
        assert_eq!(error.to_string(), "genai/alpha is not captured yet");
    }

    #[test]
    fn meta_file_must_name_every_library() {
        let tmp = Tmp::new("meta");
        fs::create_dir_all(&tmp.0).unwrap();
        let missing =
            read_libraries(&tmp.0.join("missing.json"), &["openai".to_owned()]).unwrap_err();
        assert_eq!(missing, vec!["meta file is missing".to_owned()]);

        let path = tmp.0.join("meta.json");
        fs::write(&path, r#"{"libraries":{"openai":"1.2.3","extra":"9"}}"#).unwrap();
        let versions = read_libraries(&path, &["openai".to_owned()]).unwrap();
        assert_eq!(versions.get("openai").map(String::as_str), Some("1.2.3"));
        assert_eq!(versions.get("extra").map(String::as_str), Some("9"));

        fs::write(&path, r#"{"libraries":{"openai":"SECRET-VER"}}"#).unwrap();
        let error = read_libraries(
            &path,
            &["openai".to_owned(), "opentelemetry-sdk".to_owned()],
        )
        .unwrap_err();
        assert_eq!(
            error,
            vec!["meta file is missing library opentelemetry-sdk".to_owned()]
        );
        assert!(!error.join(" ").contains("SECRET-VER"));

        fs::write(&path, r#"{"libraries":{"openai":424242}}"#).unwrap();
        let error = read_libraries(&path, &["openai".to_owned()]).unwrap_err();
        assert_eq!(
            error,
            vec!["meta file library openai is not a string".to_owned()]
        );
        assert!(!error[0].contains("424242"));

        fs::write(&path, "not-json SECRET-BODY").unwrap();
        let error = read_libraries(&path, &["openai".to_owned()]).unwrap_err();
        assert_eq!(error, vec!["meta file could not be parsed".to_owned()]);
        assert!(!error[0].contains("SECRET-BODY"));
    }

    #[test]
    fn span_count_ignores_empty_exports() {
        use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
        use opentelemetry_proto::tonic::trace::v1::{ResourceSpans, ScopeSpans, Span};

        assert!(!has_spans(&[]));
        assert!(!has_spans(&[ExportTraceServiceRequest::default()]));
        let export = ExportTraceServiceRequest {
            resource_spans: vec![ResourceSpans {
                scope_spans: vec![ScopeSpans {
                    spans: vec![Span {
                        name: "chat".to_owned(),
                        ..Default::default()
                    }],
                    ..Default::default()
                }],
                ..Default::default()
            }],
        };
        assert!(has_spans(&[export]));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn run_case_reports_a_failing_program() {
        let tmp = Tmp::new("run-case");
        let dir = tmp.0.join("genai/fails");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("scenario.json"), SCENARIO).unwrap();
        let error = run_case(
            &tmp.0,
            &dir,
            Language::Python,
            false,
            &Program::argv(vec!["false".to_owned()]),
        )
        .await
        .unwrap_err();
        assert!(
            matches!(error, CaptureError::ProgramFailed { code: Some(1) }),
            "{error}"
        );
        let rendered = format!("{error} {error:?}");
        assert!(!rendered.contains("hello"));
    }
}
