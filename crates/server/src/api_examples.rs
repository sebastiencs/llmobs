//! `just api-examples`: `fixtures/api` example responses, produced by the real read handlers over `fixtures/api/dataset` (D-G-17, D-G-25).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use axum::http::{Method, StatusCode};
use llmobs_core::ids::ProjectId;
use llmobs_core::score::Score;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::api::common::Page;
use crate::api::metrics::MetricsResponse;
use crate::api::schema::SchemaResponse;
use crate::api::sessions::{SessionDetail, SessionSummary};
use crate::api::sql::SqlQueryResult;
use crate::api::traces::{SpanDetail, SpanSummary, TraceDetail, TraceSummary};
use crate::api::users::{EndUserDetail, EndUserSummary};
use crate::test_support::{EXAMPLE_PROJECT, TestApp};

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

fn params(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

fn example_created(
    operation_id: &str,
    path_params: BTreeMap<String, String>,
    file: &str,
) -> Example {
    Example {
        operation_id: operation_id.to_owned(),
        path_params,
        query: BTreeMap::new(),
        status: 201,
        content_type: "application/json".to_owned(),
        file: file.to_owned(),
    }
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

/// Canonical text of a value: pretty JSON, one trailing newline.
///
/// Copied from `tests/support/mod.rs`, which the integration tests use.
fn canonical_json<T: Serialize + DeserializeOwned>(value: &T) -> String {
    // One parse-and-print pass. serde_json's shortest float form is not always a fixed
    // point of its parser, and the example files must round-trip through the typed value.
    let once = serde_json::to_string_pretty(value).unwrap_or_else(|error| panic!("{error}"));
    let parsed: T = serde_json::from_str(&once).unwrap_or_else(|error| panic!("{error}"));
    let twice = serde_json::to_string_pretty(&parsed).unwrap_or_else(|error| panic!("{error}"));
    format!("{twice}\n")
}

fn api_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/api")
}

fn read_json<T: DeserializeOwned>(path: &Path) -> T {
    let text =
        fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// The generated files and the manifest entries, in one place.
#[derive(Default)]
struct Output {
    files: BTreeMap<String, String>,
    manifest: Vec<Example>,
}

impl Output {
    /// A `200` JSON response with no path or query parameters beyond `path_params` and `query`.
    fn add<T: Serialize + DeserializeOwned>(
        &mut self,
        operation_id: &str,
        path_params: &[(&str, &str)],
        query: &[(&str, &str)],
        file: &str,
        value: &T,
    ) {
        self.files.insert(file.to_owned(), canonical_json(value));
        self.manifest.push(example(
            operation_id,
            params(path_params),
            params(query),
            file,
            "application/json",
        ));
    }
}

/// `GET` of the example project's `path_and_query`. Asserts `200` and parses the body as `T`.
async fn fetch<T: Serialize + DeserializeOwned>(
    app: &TestApp,
    cookie: &str,
    path_and_query: &str,
) -> T {
    let path = format!("/api/v1/projects/{EXAMPLE_PROJECT}{path_and_query}");
    let response = app
        .request(Method::GET, &path)
        .header("cookie", cookie)
        .send()
        .await;
    assert_eq!(
        response.status,
        StatusCode::OK,
        "{path}: {}",
        String::from_utf8_lossy(&response.body)
    );
    response.json()
}

/// `POST` of `body` to the example project's `path`. Asserts `200` and parses the body as `T`.
async fn post_json<T: Serialize + DeserializeOwned>(
    app: &TestApp,
    cookie: &str,
    path: &str,
    body: &Value,
) -> T {
    let path = format!("/api/v1/projects/{EXAMPLE_PROJECT}{path}");
    let response = app
        .request(Method::POST, &path)
        .header("cookie", cookie)
        .json(body)
        .send()
        .await;
    assert_eq!(
        response.status,
        StatusCode::OK,
        "{path}: {}",
        String::from_utf8_lossy(&response.body)
    );
    response.json()
}

async fn generate() -> (BTreeMap<String, String>, Vec<Example>) {
    let app = TestApp::new().await;
    let cookie = app.seed_examples().await;
    let project: ProjectId = EXAMPLE_PROJECT
        .parse()
        .unwrap_or_else(|error| panic!("{error}"));
    app.load_example_dataset(project).await;
    let cookie = cookie.as_str();
    let mut out = Output::default();

    let traces: Page<TraceSummary> = fetch(&app, cookie, "/traces").await;
    out.add("list_traces", &[], &[], "responses/traces.json", &traces);

    let traces_eval: Page<TraceSummary> = fetch(&app, cookie, "/traces?include_eval=true").await;
    out.add(
        "list_traces",
        &[],
        &[("include_eval", "true")],
        "responses/traces-include-eval.json",
        &traces_eval,
    );

    for trace in &traces_eval.data {
        let trace_id = trace.trace_id.to_string();
        let detail: TraceDetail = fetch(&app, cookie, &format!("/traces/{trace_id}")).await;
        out.add(
            "get_trace",
            &[("trace_id", &trace_id)],
            &[],
            &format!("responses/traces/{trace_id}.json"),
            &detail,
        );
        for node in &detail.spans {
            let span_id = node.span.span_id.to_string();
            let span: SpanDetail =
                fetch(&app, cookie, &format!("/traces/{trace_id}/spans/{span_id}")).await;
            out.add(
                "get_span",
                &[("trace_id", &trace_id), ("span_id", &span_id)],
                &[],
                &format!("responses/spans/{trace_id}/{span_id}.json"),
                &span,
            );
        }
    }

    let spans: Page<SpanSummary> = fetch(&app, cookie, "/spans").await;
    out.add("list_spans", &[], &[], "responses/spans.json", &spans);

    let sessions: Page<SessionSummary> = fetch(&app, cookie, "/sessions").await;
    out.add(
        "list_sessions",
        &[],
        &[],
        "responses/sessions.json",
        &sessions,
    );
    for session in &sessions.data {
        let session_id = session.session_id.as_str();
        let detail: SessionDetail = fetch(&app, cookie, &format!("/sessions/{session_id}")).await;
        out.add(
            "get_session",
            &[("session_id", session_id)],
            &[],
            &format!("responses/sessions/{session_id}.json"),
            &detail,
        );
    }

    let sessions_eval: Page<SessionSummary> =
        fetch(&app, cookie, "/sessions?include_eval=true").await;
    out.add(
        "list_sessions",
        &[],
        &[("include_eval", "true")],
        "responses/sessions-include-eval.json",
        &sessions_eval,
    );
    let session_eval: SessionDetail =
        fetch(&app, cookie, "/sessions/sess-001?include_eval=true").await;
    out.add(
        "get_session",
        &[("session_id", "sess-001")],
        &[("include_eval", "true")],
        "responses/sessions/sess-001-include-eval.json",
        &session_eval,
    );

    let users: Page<EndUserSummary> = fetch(&app, cookie, "/users").await;
    out.add("list_users", &[], &[], "responses/users.json", &users);
    for user in &users.data {
        let user_id = user.user_id.as_str();
        let detail: EndUserDetail = fetch(&app, cookie, &format!("/users/{user_id}")).await;
        out.add(
            "get_user",
            &[("user_id", user_id)],
            &[],
            &format!("responses/users/{user_id}.json"),
            &detail,
        );
    }

    let users_eval: Page<EndUserSummary> = fetch(&app, cookie, "/users?include_eval=true").await;
    out.add(
        "list_users",
        &[],
        &[("include_eval", "true")],
        "responses/users-include-eval.json",
        &users_eval,
    );
    let user_eval: EndUserDetail = fetch(&app, cookie, "/users/user-42?include_eval=true").await;
    out.add(
        "get_user",
        &[("user_id", "user-42")],
        &[("include_eval", "true")],
        "responses/users/user-42-include-eval.json",
        &user_eval,
    );

    let scores: Page<Score> = fetch(&app, cookie, "/scores").await;
    out.add("list_scores", &[], &[], "responses/scores.json", &scores);

    let metrics: MetricsResponse = fetch(
        &app,
        cookie,
        "/metrics?from=2026-01-01T10:00:00Z&to=2026-01-01T11:00:00Z&bucket=10m",
    )
    .await;
    out.add("get_metrics", &[], &[], "responses/metrics.json", &metrics);

    let request: Value = read_json(&api_dir().join("requests/sql-query.json"));
    let sql: SqlQueryResult = post_json(&app, cookie, "/sql", &request).await;
    out.add("run_sql", &[], &[], "static/sql-result.json", &sql);

    // Checked against the static files, not written.
    let schema: SchemaResponse = fetch(&app, cookie, "/schema").await;
    let static_schema: SchemaResponse = read_json(&api_dir().join("static/schema.json"));
    assert_eq!(
        schema, static_schema,
        "static/schema.json differs from GET /schema"
    );
    out.manifest.push(example(
        "get_schema",
        BTreeMap::new(),
        BTreeMap::new(),
        "static/schema.json",
        "application/json",
    ));

    let media_path = format!("/api/v1/projects/{EXAMPLE_PROJECT}/media/{MEDIA}");
    let media = app
        .request(Method::GET, &media_path)
        .header("cookie", cookie)
        .send()
        .await;
    assert_eq!(media.status, StatusCode::OK, "{media_path}");
    assert_eq!(
        media.header("content-type"),
        Some("image/png"),
        "{media_path}"
    );
    let media_file = format!("media/{MEDIA}.png");
    let media_bytes = fs::read(api_dir().join(&media_file))
        .unwrap_or_else(|error| panic!("{media_file}: {error}"));
    assert_eq!(
        media.body.as_ref(),
        media_bytes.as_slice(),
        "{media_path} differs from {media_file}"
    );
    out.manifest.push(example(
        "get_media",
        params(&[("sha256", MEDIA)]),
        BTreeMap::new(),
        &media_file,
        "image/png",
    ));

    // Static entries, copied from the hand-written list this generator replaces.
    out.manifest.extend([
        example(
            "list_admin_users",
            BTreeMap::new(),
            BTreeMap::new(),
            "static/admin-users.json",
            "application/json",
        ),
        example_created(
            "post_admin_user",
            BTreeMap::new(),
            "static/admin-user-created.json",
        ),
        example(
            "get_admin_settings",
            BTreeMap::new(),
            BTreeMap::new(),
            "static/admin-settings.json",
            "application/json",
        ),
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
            "list_org_invites",
            params(&[("org", "acme")]),
            BTreeMap::new(),
            "static/invites.json",
            "application/json",
        ),
        example_created(
            "post_org_invite",
            params(&[("org", "acme")]),
            "static/invite-created.json",
        ),
        example(
            "get_invite_preview",
            params(&[("token", "EXAMPLE0EXAMPLE0EXAMPLE0EXAMPLE0EXAMPLE0EXA")]),
            BTreeMap::new(),
            "static/invite-preview.json",
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
            "list_api_keys",
            BTreeMap::new(),
            BTreeMap::new(),
            "static/api-keys.json",
            "application/json",
        ),
        example_status("post_api_key", "static/api-key-created.json", 201),
        example(
            "list_personal_tokens",
            BTreeMap::new(),
            BTreeMap::new(),
            "static/personal-tokens.json",
            "application/json",
        ),
        example_status(
            "post_personal_token",
            "static/personal-token-created.json",
            201,
        ),
        example(
            "list_saved_views",
            BTreeMap::new(),
            BTreeMap::new(),
            "static/saved-views.json",
            "application/json",
        ),
    ]);
    out.manifest.sort_by(|left, right| {
        left.operation_id
            .cmp(&right.operation_id)
            .then(left.file.cmp(&right.file))
    });
    let Output {
        mut files,
        manifest,
    } = out;
    files.insert("manifest.json".to_owned(), canonical_json(&manifest));
    (files, manifest)
}

fn examples_write_requested() -> bool {
    match std::env::var("LLMOBS_API_EXAMPLES_WRITE") {
        Err(std::env::VarError::NotPresent) => false,
        Ok(value) if value == "1" => true,
        Ok(value) if value.is_empty() => false,
        Ok(_) | Err(_) => panic!("set LLMOBS_API_EXAMPLES_WRITE=1 to write, or unset it"),
    }
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
    let sql_result = api_dir().join("static/sql-result.json");
    if sql_result.is_file() {
        let text = fs::read_to_string(&sql_result)
            .unwrap_or_else(|error| panic!("{}: {error}", sql_result.display()));
        files.insert("static/sql-result.json".to_owned(), text);
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

#[tokio::test(flavor = "multi_thread")]
async fn examples_are_up_to_date() {
    let (expected, _manifest) = generate().await;
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
