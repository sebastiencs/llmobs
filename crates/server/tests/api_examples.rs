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
use llmobs_server::api::admin::{AdminSettings, AdminUser, CreatedAdminUser};
use llmobs_server::api::alerts::{AlertEvent, AlertRule, NotificationChannel};
use llmobs_server::api::common::Page;
use llmobs_server::api::connections::{
    CreateLlmConnectionRequest, LlmConnection, LlmConnectionTestResult,
};
use llmobs_server::api::invites::{CreatedInvite, Invite, InvitePreview};
use llmobs_server::api::keys::{ApiKey, CreatedApiKey, CreatedPersonalToken, PersonalToken};
use llmobs_server::api::members::{OrgMember, ProjectMember};
use llmobs_server::api::metrics::MetricsResponse;
use llmobs_server::api::openapi::openapi;
use llmobs_server::api::payload::Payload;
use llmobs_server::api::saved_views::SavedView;
use llmobs_server::api::schema::SchemaResponse;
use llmobs_server::api::scores::{FeedbackRequest, ScoreBatch};
use llmobs_server::api::sessions::{SessionDetail, SessionSummary};
use llmobs_server::api::sql::{SqlQueryRequest, SqlQueryResult};
use llmobs_server::api::traces::{SpanDetail, SpanSummary, TraceDetail, TraceSummary};
use llmobs_server::api::users::{EndUserDetail, EndUserSummary};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use support::{api_dir, canonical_json, dataset_spans, read_json};

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Example {
    operation_id: String,
    path_params: BTreeMap<String, String>,
    query: BTreeMap<String, String>,
    status: u16,
    content_type: String,
    file: String,
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
    // `requests/` is not walked: its files are checked by
    // `request_examples_match_their_types_and_the_openapi_document`, not through the manifest.
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
    roundtrip::<Page<Invite>>("static/invites.json");
    roundtrip::<Page<AdminUser>>("static/admin-users.json");
    roundtrip::<CreatedAdminUser>("static/admin-user-created.json");
    roundtrip::<AdminSettings>("static/admin-settings.json");
    roundtrip::<CreatedInvite>("static/invite-created.json");
    roundtrip::<InvitePreview>("static/invite-preview.json");
    roundtrip::<Page<ProjectMember>>("static/project-members.json");
    roundtrip::<Page<Project>>("static/projects.json");
    roundtrip::<Project>("static/project.json");
    roundtrip::<Page<ApiKey>>("static/api-keys.json");
    roundtrip::<Page<NotificationChannel>>("static/channels.json");
    roundtrip::<Page<AlertRule>>("static/alert-rules.json");
    roundtrip::<AlertRule>("static/alert-rule.json");
    roundtrip::<Page<AlertEvent>>("static/alert-events.json");
    roundtrip::<CreatedApiKey>("static/api-key-created.json");
    roundtrip::<Page<LlmConnection>>("static/llm-connections.json");
    roundtrip::<LlmConnection>("static/llm-connection.json");
    roundtrip::<LlmConnectionTestResult>("static/llm-connection-test.json");
    roundtrip::<Page<PersonalToken>>("static/personal-tokens.json");
    roundtrip::<CreatedPersonalToken>("static/personal-token-created.json");
    roundtrip::<SchemaResponse>("static/schema.json");
    roundtrip::<Page<SavedView>>("static/saved-views.json");
    roundtrip::<SqlQueryResult>("static/sql-result.json");
}

#[test]
fn request_examples_match_their_types_and_the_openapi_document() {
    roundtrip::<ScoreBatch>("requests/score-batch.json");
    roundtrip::<FeedbackRequest>("requests/feedback.json");
    roundtrip::<CreateLlmConnectionRequest>("requests/llm-connection-create.json");
    roundtrip::<SqlQueryRequest>("requests/sql-query.json");
    let document = read_json::<Value>(&contract_document());
    let registry = jsonschema::Registry::new()
        .add(OPENAPI_URI, document.clone())
        .unwrap_or_else(|error| panic!("openapi registry: {error}"))
        .prepare()
        .unwrap_or_else(|error| panic!("openapi registry: {error}"));
    let requests = [
        ("create_key_scores", "requests/score-batch.json"),
        ("post_feedback", "requests/feedback.json"),
        ("post_llm_connection", "requests/llm-connection-create.json"),
        ("run_sql", "requests/sql-query.json"),
    ];
    let mut failures = Vec::new();
    for (operation_id, file) in requests {
        let Some((path, method, operation)) = operation_at(&document, operation_id) else {
            failures.push(format!("{file}: operation {operation_id} is missing"));
            continue;
        };
        let schema = &operation["requestBody"]["content"]["application/json"]["schema"];
        if schema.is_null() {
            failures.push(format!("{file}: no request schema for {operation_id}"));
            continue;
        }
        let pointer = format!(
            "/paths/{}/{}/requestBody/content/{}/schema",
            uri_escape(&pointer_token(&path)),
            uri_escape(&pointer_token(&method)),
            uri_escape(&pointer_token("application/json")),
        );
        let schema = serde_json::json!({ "$ref": format!("{OPENAPI_URI}#{pointer}") });
        let validator = match jsonschema::options()
            .with_draft(jsonschema::Draft::Draft202012)
            .with_registry(&registry)
            .offline()
            .build(&schema)
        {
            Ok(validator) => validator,
            Err(error) => {
                failures.push(format!("{file}: schema: {error}"));
                continue;
            }
        };
        let instance: Value = read_json(&api_dir().join(file));
        for error in validator.iter_errors(&instance) {
            failures.push(format!(
                "{file}: {}: {}",
                error.instance_path(),
                kind_label(error.kind())
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
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
            "list_api_keys" => roundtrip::<Page<ApiKey>>(&example.file),
            "post_api_key" => roundtrip::<CreatedApiKey>(&example.file),
            "list_personal_tokens" => roundtrip::<Page<PersonalToken>>(&example.file),
            "post_personal_token" => roundtrip::<CreatedPersonalToken>(&example.file),
            "get_metrics" => roundtrip::<MetricsResponse>(&example.file),
            "run_sql" => roundtrip::<SqlQueryResult>(&example.file),
            "get_auth_config"
            | "get_me"
            | "list_orgs"
            | "get_org"
            | "list_org_members"
            | "list_org_invites"
            | "post_org_invite"
            | "get_invite_preview"
            | "list_project_members"
            | "list_projects"
            | "get_project"
            | "get_schema"
            | "list_saved_views"
            | "list_channels"
            | "list_alert_rules"
            | "get_alert_rule"
            | "list_alert_events"
            | "post_setup"
            | "post_login"
            | "post_signup"
            | "list_admin_users"
            | "post_admin_user"
            | "get_admin_settings" => {}
            "list_llm_connections" => roundtrip::<Page<LlmConnection>>(&example.file),
            "get_llm_connection" | "post_llm_connection" => {
                roundtrip::<LlmConnection>(&example.file);
            }
            "post_llm_connection_test" => roundtrip::<LlmConnectionTestResult>(&example.file),
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
            // Streams and the OAuth redirects have no JSON body to fixture.
            if method == "get"
                && id != "stream_traces"
                && id != "stream_trace"
                && id != "get_oauth_authorize"
                && id != "get_oauth_callback"
            {
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
        projects.data.iter().all(
            |project| project.org_id.to_string() == ORG && project.role == Some(OrgRole::Owner)
        )
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
