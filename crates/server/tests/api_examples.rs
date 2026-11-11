//! Generated example responses and the checks that keep them aligned with the dataset.

mod support;

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use llmobs_core::enums::PayloadFormat;
use llmobs_core::ids::{SpanId, TraceId};
use llmobs_core::message::{Message, Part};
use llmobs_core::score::{Score, ScoreTarget, ScoreValue};
use llmobs_core::span::Span;
use llmobs_server::api::account::{
    AuthConfig, Me, OrgMembership, OrgRole, Project, SetupResponse, SignupResponse,
};
use llmobs_server::api::common::Page;
use llmobs_server::api::members::{OrgMember, ProjectMember};
use llmobs_server::api::metrics::MetricsResponse;
use llmobs_server::api::openapi::openapi;
use llmobs_server::api::payload::Payload;
use llmobs_server::api::saved_views::SavedView;
use llmobs_server::api::schema::SchemaResponse;
use llmobs_server::api::sessions::{SessionDetail, SessionSummary, SessionTurn};
use llmobs_server::api::traces::{SpanDetail, SpanSummary, TraceDetail, TraceSummary};
use llmobs_server::api::users::{EndUserDetail, EndUserSummary};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use support::{
    api_dir, canonical_json, dataset_scores, dataset_spans, is_default_visible, live_scores,
    read_json, session_summary, trace_summary, user_summary,
};

const ORG: &str = "01900000-0000-7000-8000-000000000010";
const PROJECT: &str = "01900000-0000-7000-8000-000000000001";
const PROJECT_PLAYGROUND: &str = "01900000-0000-7000-8000-000000000002";
const USER: &str = "01900000-0000-7000-8000-000000000100";
const TRACE_1: &str = "a1000000000000000000000000000001";
const TRACE_3: &str = "a1000000000000000000000000000003";
const TRACE_4: &str = "a1000000000000000000000000000004";
const TRACE_5: &str = "a1000000000000000000000000000005";
const TRACE_6: &str = "a1000000000000000000000000000006";
const SPAN_T4_S3: &str = "b100000000000043";
const MEDIA: &str = "497790947d4666760ce38f3c00e852c71fdb66cae849bae8e9ede352719e1581";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Example {
    operation_id: String,
    path_params: BTreeMap<String, String>,
    query: BTreeMap<String, String>,
    status: u16,
    content_type: String,
    file: String,
}

struct Built {
    files: BTreeMap<String, String>,
    manifest: Vec<Example>,
}

fn params(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

fn example_status(operation_id: &str, file: &str, status: u16) -> Example {
    Example {
        operation_id: operation_id.to_owned(),
        path_params: BTreeMap::new(),
        query: BTreeMap::new(),
        status,
        content_type: "application/json".to_owned(),
        file: file.to_owned(),
    }
}

fn example(
    operation_id: &str,
    path_params: BTreeMap<String, String>,
    query: BTreeMap<String, String>,
    file: &str,
    content_type: &str,
) -> Example {
    Example {
        operation_id: operation_id.to_owned(),
        path_params,
        query,
        status: 200,
        content_type: content_type.to_owned(),
        file: file.to_owned(),
    }
}

fn page<T: Serialize + DeserializeOwned>(data: Vec<T>) -> String {
    canonical_json(&Page {
        data,
        next_cursor: None,
    })
}

fn group_spans(spans: Vec<Span>) -> BTreeMap<TraceId, Vec<Span>> {
    let mut groups: BTreeMap<TraceId, Vec<Span>> = BTreeMap::new();
    for span in spans {
        groups.entry(span.trace_id).or_default().push(span);
    }
    groups
}

fn summaries(groups: &BTreeMap<TraceId, Vec<Span>>, scores: &[Score]) -> Vec<TraceSummary> {
    let mut traces: Vec<TraceSummary> = groups
        .values()
        .map(|spans| trace_summary(spans, scores))
        .collect();
    traces.sort_by(|left, right| {
        right
            .start_time
            .cmp(&left.start_time)
            .then(right.trace_id.cmp(&left.trace_id))
    });
    traces
}

fn build() -> Built {
    let scores = live_scores(dataset_scores());
    let groups = group_spans(dataset_spans());
    let traces = summaries(&groups, &scores);
    let visible: Vec<&TraceSummary> = traces
        .iter()
        .filter(|trace| is_default_visible(trace))
        .collect();

    let mut files = BTreeMap::new();
    let mut manifest = Vec::new();

    let traces_file = "responses/traces.json";
    files.insert(
        traces_file.to_owned(),
        page(
            visible
                .iter()
                .map(|trace| (*trace).clone())
                .collect::<Vec<_>>(),
        ),
    );
    manifest.push(example(
        "list_traces",
        BTreeMap::new(),
        BTreeMap::new(),
        traces_file,
        "application/json",
    ));

    let include_file = "responses/traces-include-eval.json";
    files.insert(include_file.to_owned(), page(traces.clone()));
    manifest.push(example(
        "list_traces",
        BTreeMap::new(),
        params(&[("include_eval", "true")]),
        include_file,
        "application/json",
    ));

    for trace in &traces {
        let spans = &groups[&trace.trace_id];
        let mut summaries: Vec<SpanSummary> = spans.iter().map(SpanSummary::from).collect();
        summaries.sort_by_key(|span| (span.start_time, span.span_id));
        let detail = TraceDetail {
            trace: trace.clone(),
            spans: llmobs_server::api::traces::build_tree(summaries),
            truncated: false,
        };
        let file = format!("responses/traces/{}.json", trace.trace_id);
        files.insert(file.clone(), canonical_json(&detail));
        manifest.push(example(
            "get_trace",
            params(&[("trace_id", &trace.trace_id.to_string())]),
            BTreeMap::new(),
            &file,
            "application/json",
        ));
        for span in spans {
            let detail = SpanDetail::from(span.clone());
            let file = format!("responses/spans/{}/{}.json", span.trace_id, span.span_id);
            files.insert(file.clone(), canonical_json(&detail));
            manifest.push(example(
                "get_span",
                params(&[
                    ("trace_id", &span.trace_id.to_string()),
                    ("span_id", &span.span_id.to_string()),
                ]),
                BTreeMap::new(),
                &file,
                "application/json",
            ));
        }
    }

    let mut span_rows: Vec<SpanSummary> = visible
        .iter()
        .flat_map(|trace| groups[&trace.trace_id].iter().map(SpanSummary::from))
        .collect();
    span_rows.sort_by(|left, right| {
        right
            .start_time
            .cmp(&left.start_time)
            .then(right.span_id.cmp(&left.span_id))
    });
    let spans_file = "responses/spans.json";
    files.insert(spans_file.to_owned(), page(span_rows));
    manifest.push(example(
        "list_spans",
        BTreeMap::new(),
        BTreeMap::new(),
        spans_file,
        "application/json",
    ));

    let (sessions, session_details) = session_views(&visible, &groups, &scores);
    let sessions_file = "responses/sessions.json";
    files.insert(sessions_file.to_owned(), page(sessions));
    manifest.push(example(
        "list_sessions",
        BTreeMap::new(),
        BTreeMap::new(),
        sessions_file,
        "application/json",
    ));
    for (session_id, detail) in &session_details {
        let file = format!("responses/sessions/{session_id}.json");
        files.insert(file.clone(), canonical_json(detail));
        manifest.push(example(
            "get_session",
            params(&[("session_id", session_id)]),
            BTreeMap::new(),
            &file,
            "application/json",
        ));
    }

    let eval_traces: Vec<&TraceSummary> = traces.iter().collect();
    let (eval_sessions, eval_session_details) = session_views(&eval_traces, &groups, &scores);
    let sessions_eval_file = "responses/sessions-include-eval.json";
    files.insert(sessions_eval_file.to_owned(), page(eval_sessions));
    manifest.push(example(
        "list_sessions",
        BTreeMap::new(),
        params(&[("include_eval", "true")]),
        sessions_eval_file,
        "application/json",
    ));
    let session_eval = eval_session_details
        .get("sess-001")
        .unwrap_or_else(|| panic!("sess-001"));
    let session_eval_file = "responses/sessions/sess-001-include-eval.json";
    files.insert(session_eval_file.to_owned(), canonical_json(session_eval));
    manifest.push(example(
        "get_session",
        params(&[("session_id", "sess-001")]),
        params(&[("include_eval", "true")]),
        session_eval_file,
        "application/json",
    ));

    let (users, user_details) = user_views(&visible, &groups, &scores);
    let users_file = "responses/users.json";
    files.insert(users_file.to_owned(), page(users));
    manifest.push(example(
        "list_users",
        BTreeMap::new(),
        BTreeMap::new(),
        users_file,
        "application/json",
    ));
    for (user_id, detail) in &user_details {
        let file = format!("responses/users/{user_id}.json");
        files.insert(file.clone(), canonical_json(detail));
        manifest.push(example(
            "get_user",
            params(&[("user_id", user_id)]),
            BTreeMap::new(),
            &file,
            "application/json",
        ));
    }

    let (eval_users, eval_user_details) = user_views(&eval_traces, &groups, &scores);
    let users_eval_file = "responses/users-include-eval.json";
    files.insert(users_eval_file.to_owned(), page(eval_users));
    manifest.push(example(
        "list_users",
        BTreeMap::new(),
        params(&[("include_eval", "true")]),
        users_eval_file,
        "application/json",
    ));
    let user_eval = eval_user_details
        .get("user-42")
        .unwrap_or_else(|| panic!("user-42"));
    let user_eval_file = "responses/users/user-42-include-eval.json";
    files.insert(user_eval_file.to_owned(), canonical_json(user_eval));
    manifest.push(example(
        "get_user",
        params(&[("user_id", "user-42")]),
        params(&[("include_eval", "true")]),
        user_eval_file,
        "application/json",
    ));

    let mut score_rows: Vec<Score> = scores.into_iter().filter(|score| !score.deleted).collect();
    score_rows.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then(right.score_id.cmp(&left.score_id))
    });
    let scores_file = "responses/scores.json";
    files.insert(scores_file.to_owned(), page(score_rows));
    manifest.push(example(
        "list_scores",
        BTreeMap::new(),
        BTreeMap::new(),
        scores_file,
        "application/json",
    ));

    manifest.extend([
        example(
            "get_auth_config",
            BTreeMap::new(),
            BTreeMap::new(),
            "static/auth-config.json",
            "application/json",
        ),
        example_status("post_setup", "static/setup-response.json", 201),
        example(
            "post_login",
            BTreeMap::new(),
            BTreeMap::new(),
            "static/me.json",
            "application/json",
        ),
        example_status("post_signup", "static/signup-response.json", 201),
        example(
            "get_me",
            BTreeMap::new(),
            BTreeMap::new(),
            "static/me.json",
            "application/json",
        ),
        example(
            "list_orgs",
            BTreeMap::new(),
            BTreeMap::new(),
            "static/orgs.json",
            "application/json",
        ),
        example(
            "get_org",
            params(&[("org", "acme")]),
            BTreeMap::new(),
            "static/org.json",
            "application/json",
        ),
        example(
            "list_org_members",
            params(&[("org", "acme")]),
            BTreeMap::new(),
            "static/org-members.json",
            "application/json",
        ),
        example(
            "list_project_members",
            BTreeMap::new(),
            BTreeMap::new(),
            "static/project-members.json",
            "application/json",
        ),
        example(
            "list_projects",
            params(&[("org", "acme")]),
            BTreeMap::new(),
            "static/projects.json",
            "application/json",
        ),
        example(
            "get_project",
            BTreeMap::new(),
            BTreeMap::new(),
            "static/project.json",
            "application/json",
        ),
        example(
            "get_schema",
            BTreeMap::new(),
            BTreeMap::new(),
            "static/schema.json",
            "application/json",
        ),
        example(
            "list_saved_views",
            BTreeMap::new(),
            BTreeMap::new(),
            "static/saved-views.json",
            "application/json",
        ),
        example(
            "get_metrics",
            BTreeMap::new(),
            BTreeMap::new(),
            "static/metrics.json",
            "application/json",
        ),
        example(
            "get_media",
            params(&[("sha256", MEDIA)]),
            BTreeMap::new(),
            &format!("media/{MEDIA}.png"),
            "image/png",
        ),
    ]);
    manifest.sort_by(|left, right| {
        left.operation_id
            .cmp(&right.operation_id)
            .then(left.file.cmp(&right.file))
    });
    Built { files, manifest }
}

fn session_views(
    traces: &[&TraceSummary],
    groups: &BTreeMap<TraceId, Vec<Span>>,
    scores: &[Score],
) -> (Vec<SessionSummary>, BTreeMap<String, SessionDetail>) {
    let mut by_session: BTreeMap<String, Vec<TraceSummary>> = BTreeMap::new();
    for trace in traces {
        if let Some(session_id) = &trace.session_id {
            by_session
                .entry(session_id.clone())
                .or_default()
                .push((*trace).clone());
        }
    }
    let mut list: Vec<SessionSummary> = by_session
        .iter()
        .map(|(session_id, session_traces)| session_summary(session_id, session_traces, scores))
        .collect();
    list.sort_by(|left, right| {
        right
            .start_time
            .cmp(&left.start_time)
            .then(right.session_id.cmp(&left.session_id))
    });
    let mut details = BTreeMap::new();
    for (session_id, session_traces) in &by_session {
        let mut turns: Vec<SessionTurn> = session_traces
            .iter()
            .map(|trace| turn(trace, &groups[&trace.trace_id]))
            .collect();
        turns.sort_by(|left, right| {
            left.trace
                .start_time
                .cmp(&right.trace.start_time)
                .then(left.trace.trace_id.cmp(&right.trace.trace_id))
        });
        details.insert(
            session_id.clone(),
            SessionDetail {
                session: session_summary(session_id, session_traces, scores),
                turns,
                truncated: false,
            },
        );
    }
    (list, details)
}

fn user_views(
    traces: &[&TraceSummary],
    groups: &BTreeMap<TraceId, Vec<Span>>,
    scores: &[Score],
) -> (Vec<EndUserSummary>, BTreeMap<String, EndUserDetail>) {
    let (session_list, _) = session_views(traces, groups, scores);
    let sessions_by_id: BTreeMap<&str, &SessionSummary> = session_list
        .iter()
        .map(|session| (session.session_id.as_str(), session))
        .collect();
    let mut by_user: BTreeMap<String, Vec<TraceSummary>> = BTreeMap::new();
    for trace in traces {
        if let Some(user_id) = &trace.user_id {
            by_user
                .entry(user_id.clone())
                .or_default()
                .push((*trace).clone());
        }
    }
    let mut list: Vec<EndUserSummary> = by_user
        .iter()
        .map(|(user_id, user_traces)| user_summary(user_id, user_traces))
        .collect();
    list.sort_by(|left, right| {
        right
            .last_seen
            .cmp(&left.last_seen)
            .then(right.user_id.cmp(&left.user_id))
    });
    let mut details = BTreeMap::new();
    for (user_id, user_traces) in &by_user {
        let mut sessions: Vec<SessionSummary> = user_traces
            .iter()
            .filter_map(|trace| trace.session_id.as_deref())
            .collect::<HashSet<_>>()
            .into_iter()
            .map(|session_id| sessions_by_id[session_id].clone())
            .collect();
        sessions.sort_by(|left, right| {
            right
                .start_time
                .cmp(&left.start_time)
                .then(right.session_id.cmp(&left.session_id))
        });
        details.insert(
            user_id.clone(),
            EndUserDetail {
                user: user_summary(user_id, user_traces),
                sessions,
            },
        );
    }
    (list, details)
}

fn turn(trace: &TraceSummary, spans: &[Span]) -> SessionTurn {
    let root = spans
        .iter()
        .filter(|span| span.parent_span_id.is_none())
        .min_by_key(|span| (span.start_time, span.span_id));
    let (input, output) = root.map_or((None, None), |span| {
        let detail = SpanDetail::from(span.clone());
        (detail.input, detail.output)
    });
    SessionTurn {
        trace: trace.clone(),
        input,
        output,
    }
}

fn expected_files(built: &Built) -> BTreeMap<String, String> {
    let mut files = built.files.clone();
    files.insert("manifest.json".to_owned(), canonical_json(&built.manifest));
    files
}

fn on_disk_files() -> BTreeMap<String, String> {
    let mut files = BTreeMap::new();
    collect_dir(&api_dir().join("responses"), "responses", &mut files);
    let manifest = api_dir().join("manifest.json");
    if manifest.is_file() {
        let text = fs::read_to_string(&manifest)
            .unwrap_or_else(|error| panic!("{}: {error}", manifest.display()));
        files.insert("manifest.json".to_owned(), text);
    }
    files
}

fn collect_dir(dir: &Path, prefix: &str, files: &mut BTreeMap<String, String>) {
    if !dir.is_dir() {
        return;
    }
    let entries = fs::read_dir(dir).unwrap_or_else(|error| panic!("{}: {error}", dir.display()));
    for entry in entries {
        let entry = entry.unwrap_or_else(|error| panic!("{error}"));
        let path = entry.path();
        let name = entry.file_name();
        let name = name
            .to_str()
            .unwrap_or_else(|| panic!("{} is not utf-8", path.display()));
        if path.is_dir() {
            collect_dir(&path, &format!("{prefix}/{name}"), files);
        } else {
            let text = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            files.insert(format!("{prefix}/{name}"), text);
        }
    }
}

fn write_files(files: &BTreeMap<String, String>) {
    let responses = api_dir().join("responses");
    if responses.exists() {
        fs::remove_dir_all(&responses).unwrap_or_else(|error| panic!("{error}"));
    }
    for (file, text) in files {
        let path = api_dir().join(file);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .unwrap_or_else(|error| panic!("{}: {error}", parent.display()));
        }
        fs::write(&path, text).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    }
}

fn examples_write_requested() -> bool {
    match std::env::var("LLMOBS_API_EXAMPLES_WRITE") {
        Err(std::env::VarError::NotPresent) => false,
        Ok(value) if value == "1" => true,
        Ok(value) if value.is_empty() => false,
        Ok(_) | Err(_) => panic!("set LLMOBS_API_EXAMPLES_WRITE=1 to write, or unset it"),
    }
}

#[test]
fn examples_are_up_to_date() {
    let expected = expected_files(&build());
    if examples_write_requested() {
        write_files(&expected);
        return;
    }
    let actual = on_disk_files();
    for file in expected.keys() {
        let Some(text) = actual.get(file) else {
            panic!("{file} is missing; run `just api-examples`");
        };
        assert_eq!(
            text, &expected[file],
            "{file} differs; run `just api-examples`"
        );
    }
    for file in actual.keys() {
        assert!(
            expected.contains_key(file),
            "{file} is stale; run `just api-examples`"
        );
    }
}

#[test]
fn examples_match_the_openapi_document() {
    let manifest: Vec<Example> = read_json(&api_dir().join("manifest.json"));
    let document = read_json::<Value>(&contract_document());
    let registry = jsonschema::Registry::new()
        .add(OPENAPI_URI, document.clone())
        .unwrap_or_else(|error| panic!("openapi registry: {error}"))
        .prepare()
        .unwrap_or_else(|error| panic!("openapi registry: {error}"));
    let mut failures = Vec::new();
    for entry in &manifest {
        if entry.content_type != "application/json" {
            continue;
        }
        let Some(pointer) = schema_pointer(&document, entry, &mut failures) else {
            continue;
        };
        let schema = serde_json::json!({ "$ref": format!("{OPENAPI_URI}#{pointer}") });
        let validator = match jsonschema::options()
            .with_draft(jsonschema::Draft::Draft202012)
            .with_registry(&registry)
            .offline()
            .build(&schema)
        {
            Ok(validator) => validator,
            Err(error) => {
                failures.push(format!("{}: schema: {error}", entry.file));
                continue;
            }
        };
        let instance: Value = read_json(&api_dir().join(&entry.file));
        for error in validator.iter_errors(&instance) {
            failures.push(format!(
                "{}: {}: {}",
                entry.file,
                error.instance_path(),
                kind_label(error.kind())
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn manifest_matches_the_operations() {
    let manifest: Vec<Example> = read_json(&api_dir().join("manifest.json"));
    let document = read_json::<Value>(&contract_document());
    let mut failures = Vec::new();
    for entry in &manifest {
        let Some((path_item, operation)) = get_operation(&document, &entry.operation_id) else {
            failures.push(format!(
                "{}: {} is not a GET operation",
                entry.file, entry.operation_id
            ));
            continue;
        };
        let mut path_names = parameter_names(path_item, operation, "path");
        // Examples omit `{project}`: the mock client fills that segment.
        path_names.remove("project");
        let declared: BTreeSet<&str> = entry.path_params.keys().map(String::as_str).collect();
        if declared != path_names {
            failures.push(format!(
                "{}: path params {:?} != {:?}",
                entry.file,
                entry.path_params.keys().collect::<Vec<_>>(),
                path_names
            ));
        }
        let query_names = parameter_names(path_item, operation, "query");
        for name in entry.query.keys() {
            if !query_names.contains(name.as_str()) {
                failures.push(format!(
                    "{}: query {name} is not a parameter of {}",
                    entry.file, entry.operation_id
                ));
            }
        }
        let status = entry.status.to_string();
        let response = &operation["responses"][&status];
        if response.is_null() {
            failures.push(format!(
                "{}: status {} is not declared on {}",
                entry.file, entry.status, entry.operation_id
            ));
            continue;
        }
        let content = &response["content"];
        if content.get(&entry.content_type).is_none() {
            failures.push(format!(
                "{}: {} is not declared for status {} of {}",
                entry.file, entry.content_type, entry.status, entry.operation_id
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn every_fixture_file_is_used() {
    let manifest: Vec<Example> = read_json(&api_dir().join("manifest.json"));
    let used: BTreeSet<String> = manifest.into_iter().map(|entry| entry.file).collect();
    let uris = media_uris(&dataset_spans());
    let mut unused = Vec::new();
    for file in files_under(&api_dir().join("static"), "static") {
        if !used.contains(&file) {
            unused.push(file);
        }
    }
    for file in files_under(&api_dir().join("media"), "media") {
        let stem = Path::new(&file)
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("");
        let referenced = used.contains(&file) || uris.iter().any(|uri| uri.contains(stem));
        if !referenced {
            unused.push(file);
        }
    }
    assert!(
        unused.is_empty(),
        "unused fixture files: {}",
        unused.join(", ")
    );
}

fn contract_document() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("openapi.json")
}

const OPENAPI_URI: &str = "urn:llmobs:openapi";

fn schema_pointer(document: &Value, entry: &Example, failures: &mut Vec<String>) -> Option<String> {
    let Some((path, method, operation)) = operation_at(document, &entry.operation_id) else {
        failures.push(format!(
            "{}: operation {} is missing",
            entry.file, entry.operation_id
        ));
        return None;
    };
    let status = entry.status.to_string();
    let schema = &operation["responses"][&status]["content"][&entry.content_type]["schema"];
    if schema.is_null() {
        failures.push(format!(
            "{}: no schema for {} {}",
            entry.file, entry.status, entry.content_type
        ));
        return None;
    }
    let path = uri_escape(&pointer_token(&path));
    let method = uri_escape(&pointer_token(&method));
    let status = uri_escape(&pointer_token(&status));
    let content = uri_escape(&pointer_token(&entry.content_type));
    Some(format!(
        "/paths/{path}/{method}/responses/{status}/content/{content}/schema"
    ))
}

fn operation_at(document: &Value, operation_id: &str) -> Option<(String, String, Value)> {
    let paths = document.get("paths")?.as_object()?;
    for (path, item) in paths {
        let Some(item) = item.as_object() else {
            continue;
        };
        for method in ["get", "post", "put", "patch", "delete", "head"] {
            let Some(operation) = item.get(method) else {
                continue;
            };
            if operation["operationId"] == operation_id {
                return Some((path.clone(), method.to_owned(), operation.clone()));
            }
        }
    }
    None
}

fn get_operation<'a>(document: &'a Value, operation_id: &str) -> Option<(&'a Value, &'a Value)> {
    let paths = document.get("paths")?.as_object()?;
    for item in paths.values() {
        if let Some(operation) = method_operation(item, operation_id) {
            return Some((item, operation));
        }
    }
    None
}

fn method_operation<'a>(item: &'a Value, operation_id: &str) -> Option<&'a Value> {
    let item = item.as_object()?;
    for method in ["get", "post", "put", "patch", "delete", "head"] {
        let Some(operation) = item.get(method) else {
            continue;
        };
        if operation["operationId"] == operation_id {
            return Some(operation);
        }
    }
    None
}

fn parameter_names<'a>(
    path_item: &'a Value,
    operation: &'a Value,
    location: &str,
) -> BTreeSet<&'a str> {
    let mut names = BTreeSet::new();
    for source in [path_item, operation] {
        let Some(parameters) = source.get("parameters").and_then(Value::as_array) else {
            continue;
        };
        for parameter in parameters {
            if parameter["in"] == location
                && let Some(name) = parameter["name"].as_str()
            {
                names.insert(name);
            }
        }
    }
    names
}

fn pointer_token(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

fn uri_escape(segment: &str) -> String {
    segment.replace('{', "%7B").replace('}', "%7D")
}

fn kind_label(kind: &jsonschema::error::ValidationErrorKind) -> String {
    let debug = format!("{kind:?}");
    debug
        .split([' ', '{', '('])
        .next()
        .unwrap_or("error")
        .to_owned()
}

fn media_uris(spans: &[Span]) -> Vec<String> {
    let mut uris = Vec::new();
    for span in spans {
        for (format, payload) in [
            (span.input_format, span.input.as_deref()),
            (span.output_format, span.output.as_deref()),
        ] {
            if format != Some(PayloadFormat::Messages) {
                continue;
            }
            let Some(payload) = payload else {
                continue;
            };
            let Ok(messages) = serde_json::from_str::<Vec<Message>>(payload) else {
                continue;
            };
            for message in messages {
                for part in message.parts {
                    if let Part::Uri { uri, .. } = part {
                        uris.push(uri);
                    }
                }
            }
        }
    }
    uris
}

fn files_under(dir: &Path, prefix: &str) -> Vec<String> {
    let mut files = Vec::new();
    collect_names(dir, prefix, &mut files);
    files.sort();
    files
}

fn collect_names(dir: &Path, prefix: &str, files: &mut Vec<String>) {
    if !dir.is_dir() {
        return;
    }
    let entries = fs::read_dir(dir).unwrap_or_else(|error| panic!("{}: {error}", dir.display()));
    for entry in entries {
        let entry = entry.unwrap_or_else(|error| panic!("{error}"));
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let relative = format!("{prefix}/{name}");
        if path.is_dir() {
            collect_names(&path, &relative, files);
        } else {
            files.push(relative);
        }
    }
}

#[test]
fn static_files_match_their_types() {
    roundtrip::<AuthConfig>("static/auth-config.json");
    roundtrip::<SetupResponse>("static/setup-response.json");
    roundtrip::<SignupResponse>("static/signup-response.json");
    roundtrip::<Me>("static/me.json");
    roundtrip::<Page<OrgMembership>>("static/orgs.json");
    roundtrip::<OrgMembership>("static/org.json");
    roundtrip::<Page<OrgMember>>("static/org-members.json");
    roundtrip::<Page<ProjectMember>>("static/project-members.json");
    roundtrip::<Page<Project>>("static/projects.json");
    roundtrip::<Project>("static/project.json");
    roundtrip::<SchemaResponse>("static/schema.json");
    roundtrip::<Page<SavedView>>("static/saved-views.json");
    roundtrip::<MetricsResponse>("static/metrics.json");
}

#[test]
fn generated_files_roundtrip() {
    let manifest: Vec<Example> = read_json(&api_dir().join("manifest.json"));
    for example in &manifest {
        if example.content_type != "application/json" {
            continue;
        }
        match example.operation_id.as_str() {
            "list_traces" => roundtrip::<Page<TraceSummary>>(&example.file),
            "get_trace" => roundtrip::<TraceDetail>(&example.file),
            "list_spans" => roundtrip::<Page<SpanSummary>>(&example.file),
            "get_span" => roundtrip::<SpanDetail>(&example.file),
            "list_sessions" => roundtrip::<Page<SessionSummary>>(&example.file),
            "get_session" => roundtrip::<SessionDetail>(&example.file),
            "list_users" => roundtrip::<Page<EndUserSummary>>(&example.file),
            "get_user" => roundtrip::<EndUserDetail>(&example.file),
            "list_scores" => roundtrip::<Page<Score>>(&example.file),
            "get_auth_config"
            | "get_me"
            | "list_orgs"
            | "get_org"
            | "list_org_members"
            | "list_project_members"
            | "list_projects"
            | "get_project"
            | "get_schema"
            | "list_saved_views"
            | "get_metrics"
            | "post_setup"
            | "post_login"
            | "post_signup" => {}
            other => panic!("{other} has no round-trip"),
        }
    }
}

#[test]
fn manifest_covers_the_read_operations() {
    let manifest: Vec<Example> = read_json(&api_dir().join("manifest.json"));
    let document = serde_json::to_value(openapi()).unwrap_or_else(|error| panic!("{error}"));
    let mut known = HashSet::new();
    let mut gets = HashSet::new();
    let paths = document["paths"]
        .as_object()
        .unwrap_or_else(|| panic!("paths"));
    for item in paths.values() {
        let Some(item) = item.as_object() else {
            continue;
        };
        for (method, operation) in item {
            if !matches!(method.as_str(), "get" | "post" | "patch" | "delete" | "put") {
                continue;
            }
            let id = operation["operationId"]
                .as_str()
                .unwrap_or_else(|| panic!("operation id"));
            known.insert(id.to_owned());
            if method == "get" && id != "stream_traces" && id != "stream_trace" {
                gets.insert(id.to_owned());
            }
        }
    }
    let covered: HashSet<&str> = manifest
        .iter()
        .map(|example| example.operation_id.as_str())
        .collect();
    for id in &gets {
        assert!(
            covered.contains(id.as_str()),
            "{id} is missing from the manifest"
        );
    }
    for example in &manifest {
        assert!(
            known.contains(&example.operation_id),
            "{} is not an operation",
            example.operation_id
        );
        assert!(
            api_dir().join(&example.file).is_file(),
            "{} is missing",
            example.file
        );
    }
}

#[test]
fn dataset_is_consistent() {
    let dir = api_dir();
    let spans_path = dir.join("dataset/spans.json");
    let spans_text = fs::read_to_string(&spans_path).unwrap_or_else(|error| panic!("{error}"));
    let spans: Vec<Span> =
        serde_json::from_str(&spans_text).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        canonical_json(&spans),
        spans_text,
        "spans.json is not canonical"
    );
    let mut previous: Option<(TraceId, i64, SpanId)> = None;
    let mut seen: HashSet<(TraceId, SpanId)> = HashSet::new();
    for span in &spans {
        let key = (span.trace_id, span.start_time, span.span_id);
        if let Some(previous) = previous {
            assert!(previous <= key, "spans.json is not sorted");
        }
        assert!(
            seen.insert((span.trace_id, span.span_id)),
            "duplicate span id"
        );
        previous = Some(key);
        if let Some(session_id) = &span.session_id {
            assert!(safe_id(session_id), "session id {session_id}");
        }
        if let Some(user_id) = &span.user_id {
            assert!(safe_id(user_id), "user id {user_id}");
        }
        for sha in &span.media_refs {
            let path = dir.join(format!("media/{sha}.png"));
            let bytes =
                fs::read(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            assert_eq!(hex(&Sha256::digest(&bytes)), *sha);
        }
    }

    let scores_path = dir.join("dataset/scores.json");
    let scores_text = fs::read_to_string(&scores_path).unwrap_or_else(|error| panic!("{error}"));
    let scores: Vec<Score> =
        serde_json::from_str(&scores_text).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        canonical_json(&scores),
        scores_text,
        "scores.json is not canonical"
    );
    let sessions: HashSet<&str> = spans
        .iter()
        .filter_map(|span| span.session_id.as_deref())
        .collect();
    for score in &scores {
        match &score.target {
            ScoreTarget::Trace { trace_id } => {
                assert!(spans.iter().any(|span| span.trace_id == *trace_id));
            }
            ScoreTarget::Span { trace_id, span_id } => {
                assert!(
                    spans
                        .iter()
                        .any(|span| span.trace_id == *trace_id && span.span_id == *span_id)
                );
            }
            ScoreTarget::Session { session_id } => assert!(sessions.contains(session_id.as_str())),
        }
    }

    let project: Project = read_json(&dir.join("static/project.json"));
    assert_eq!(project.id.to_string(), PROJECT);
    assert_eq!(project.org_id.to_string(), ORG);
    let projects: Page<Project> = read_json(&dir.join("static/projects.json"));
    let ids: Vec<String> = projects
        .data
        .iter()
        .map(|project| project.id.to_string())
        .collect();
    assert_eq!(ids, vec![PROJECT, PROJECT_PLAYGROUND]);
    assert!(
        projects
            .data
            .iter()
            .all(|project| project.org_id.to_string() == ORG && project.role == OrgRole::Owner)
    );
    let me: Me = read_json(&dir.join("static/me.json"));
    assert_eq!(me.user.id.to_string(), USER);
    assert_eq!(me.orgs.len(), 1);
    assert_eq!(me.orgs[0].id.to_string(), ORG);
}

#[test]
fn example_scenarios() {
    let traces: Page<TraceSummary> = read_json(&api_dir().join("responses/traces.json"));
    let first = traces
        .data
        .iter()
        .find(|trace| trace.trace_id.to_string() == TRACE_1)
        .unwrap_or_else(|| panic!("T1"));
    let helpful = first
        .scores
        .iter()
        .find(|score| score.name == "helpfulness")
        .unwrap_or_else(|| panic!("helpfulness"));
    assert_eq!(helpful.value, ScoreValue::Numeric(0.9));

    let listed = fs::read_to_string(api_dir().join("responses/traces.json"))
        .unwrap_or_else(|error| panic!("{error}"));
    let spans = fs::read_to_string(api_dir().join("responses/spans.json"))
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(!listed.contains(TRACE_5));
    assert!(!spans.contains(TRACE_5));
    // T5 carries `sess-001` and `user-42`: the default exclusion must keep it out of the
    // session turns and out of the session and user aggregates. The include_eval files keep it.
    for file in [
        "responses/sessions.json",
        "responses/sessions/sess-001.json",
        "responses/sessions/sess-002.json",
        "responses/users.json",
        "responses/users/user-42.json",
        "responses/users/user-7.json",
    ] {
        let text =
            fs::read_to_string(api_dir().join(file)).unwrap_or_else(|error| panic!("{error}"));
        assert!(!text.contains(TRACE_5), "{file}");
    }
    let included_session_detail =
        fs::read_to_string(api_dir().join("responses/sessions/sess-001-include-eval.json"))
            .unwrap_or_else(|error| panic!("{error}"));
    assert!(included_session_detail.contains(TRACE_5));
    let session_list: Page<SessionSummary> = read_json(&api_dir().join("responses/sessions.json"));
    let session = session_list
        .data
        .iter()
        .find(|session| session.session_id == "sess-001")
        .unwrap_or_else(|| panic!("sess-001"));
    assert_eq!(session.trace_count, 2);
    assert_eq!(session.tags, ["beta".to_owned(), "refund".to_owned()]);
    let other = session_list
        .data
        .iter()
        .find(|session| session.session_id == "sess-002")
        .unwrap_or_else(|| panic!("sess-002"));
    assert_eq!(other.tags, Vec::<String>::new());
    let included_sessions: Page<SessionSummary> =
        read_json(&api_dir().join("responses/sessions-include-eval.json"));
    let included_session = included_sessions
        .data
        .iter()
        .find(|session| session.session_id == "sess-001")
        .unwrap_or_else(|| panic!("sess-001 include_eval"));
    assert_eq!(included_session.trace_count, 3);
    assert_eq!(
        included_session.tags,
        ["beta".to_owned(), "eval".to_owned(), "refund".to_owned()]
    );
    let user_list: Page<EndUserSummary> = read_json(&api_dir().join("responses/users.json"));
    let user = user_list
        .data
        .iter()
        .find(|user| user.user_id == "user-42")
        .unwrap_or_else(|| panic!("user-42"));
    assert_eq!(user.trace_count, 3);
    let included_users: Page<EndUserSummary> =
        read_json(&api_dir().join("responses/users-include-eval.json"));
    let included_user = included_users
        .data
        .iter()
        .find(|user| user.user_id == "user-42")
        .unwrap_or_else(|| panic!("user-42 include_eval"));
    assert_eq!(included_user.trace_count, 4);
    let detail: EndUserDetail = read_json(&api_dir().join("responses/users/user-42.json"));
    assert_eq!(detail.sessions.len(), 1);
    assert_eq!(detail.sessions[0].trace_count, 2);
    let included = fs::read_to_string(api_dir().join("responses/traces-include-eval.json"))
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(included.contains(TRACE_5));

    let detail: TraceDetail =
        read_json(&api_dir().join(format!("responses/traces/{TRACE_6}.json")));
    assert!(!detail.trace.has_root);
    assert_eq!(detail.spans.iter().filter(|span| span.detached).count(), 1);

    let third = traces
        .data
        .iter()
        .find(|trace| trace.trace_id.to_string() == TRACE_3)
        .unwrap_or_else(|| panic!("T3"));
    assert_eq!(third.status, llmobs_core::enums::SpanStatus::Error);
    assert_eq!(third.error_count, 2);

    let span: SpanDetail =
        read_json(&api_dir().join(format!("responses/spans/{TRACE_4}/{SPAN_T4_S3}.json")));
    let Some(Payload::Messages(messages)) = span.input else {
        panic!("T4 S3 input is not messages");
    };
    assert!(
        messages
            .iter()
            .flat_map(|message| &message.parts)
            .any(|part| matches!(part, Part::Uri { .. }))
    );
}

fn roundtrip<T: Serialize + DeserializeOwned>(file: &str) {
    let path = api_dir().join(file);
    let text =
        fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let value: T = serde_json::from_str(&text).unwrap_or_else(|error| panic!("{file}: {error}"));
    assert_eq!(canonical_json(&value), text, "{file} is not canonical");
}

fn safe_id(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    out
}
