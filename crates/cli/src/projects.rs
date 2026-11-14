//! `llmobs projects list`: every project the caller can read, across their orgs (spec 09
//! § Orgs, projects, members, keys; D-H-12).

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use clap::{Args, Subcommand};
use llmobs_server::api::account::{OrgMembership, OrgRole, Project};
use llmobs_server::api::common::Page;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::client::{self, ApiClient, ConnectionArgs};
use crate::output::{self, Column, Io, ListFormat};

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

/// Runs `llmobs projects …`. Credentials are read only when the target needs them.
pub(crate) async fn run(
    command: &ProjectsCommand,
    credentials: impl FnOnce() -> anyhow::Result<PathBuf>,
    streams: &mut Io<'_>,
) -> anyhow::Result<()> {
    match command {
        ProjectsCommand::List(args) => list(args, credentials, streams).await,
    }
}

async fn list(
    args: &ProjectListArgs,
    credentials: impl FnOnce() -> anyhow::Result<PathBuf>,
    streams: &mut Io<'_>,
) -> anyhow::Result<()> {
    let target = client::resolve_target(&args.connection, credentials)?;
    let api = ApiClient::new(target)?;
    let mut listed = Vec::new();
    for org in all_pages::<OrgMembership>(&api, "/api/v1/orgs").await? {
        let path = format!(
            "/api/v1/orgs/{}/projects",
            client::project_segment(&org.slug)
        );
        for project in all_pages::<Project>(&api, &path).await? {
            listed.push(ListedProject {
                project: format!("{}/{}", org.slug, project.slug),
                id: project.id.to_string(),
                name: project.name,
                org_id: project.org_id.to_string(),
                role: project.role.map(role_str),
            });
        }
    }

    match args.format {
        ListFormat::Json => {
            let mut bytes = serde_json::to_string_pretty(&listed)?.into_bytes();
            bytes.push(b'\n');
            if !output::write_output(&mut *streams.out, &bytes)? {
                return Ok(());
            }
        }
        ListFormat::Table => {
            let columns: Vec<Column> = ["project", "name", "role", "id"]
                .into_iter()
                .map(|name| Column {
                    name: name.to_owned(),
                    right: false,
                })
                .collect();
            let rows: Vec<Vec<String>> = listed
                .iter()
                .map(|project| {
                    vec![
                        project.project.clone(),
                        project.name.clone(),
                        project.role.clone().unwrap_or_default(),
                        project.id.clone(),
                    ]
                })
                .collect();
            let mut bytes = Vec::new();
            output::render_table(&mut bytes, &columns, &rows)?;
            if !output::write_output(&mut *streams.out, &bytes)? {
                return Ok(());
            }
            output::write_rows_note(&mut *streams.err, listed.len(), None)?;
        }
    }
    Ok(())
}

/// Every item of a paged list endpoint, following `next_cursor` with 1000 items per page. A
/// cursor the server already returned ends the walk with an error: following it would loop
/// forever.
async fn all_pages<T: DeserializeOwned>(api: &ApiClient, path: &str) -> anyhow::Result<Vec<T>> {
    let mut items = Vec::new();
    let mut cursor: Option<String> = None;
    let mut seen = HashSet::new();
    loop {
        let mut params = vec![("limit", "1000".to_owned())];
        if let Some(next) = &cursor {
            params.push(("cursor", next.clone()));
        }
        let value = api.get_json(path, &params, Duration::from_secs(60)).await?;
        let page: Page<T> = serde_json::from_value(value)
            .map_err(|_| anyhow::anyhow!("the server sent an invalid response"))?;
        items.extend(page.data);
        match page.next_cursor {
            Some(next) => {
                if !seen.insert(next.clone()) {
                    anyhow::bail!("the server repeated a page cursor");
                }
                cursor = Some(next);
            }
            None => return Ok(items),
        }
    }
}

/// `owner`, `admin`, `member` or `viewer`.
fn role_str(role: OrgRole) -> String {
    match role {
        OrgRole::Owner => "owner",
        OrgRole::Admin => "admin",
        OrgRole::Member => "member",
        OrgRole::Viewer => "viewer",
    }
    .to_owned()
}

#[cfg(test)]
mod tests {
    use std::io;

    use clap::Parser;
    use serde_json::{Value, json};

    use super::*;
    use crate::test_server::{BrokenPipe, Canned, TestServer};

    /// `llmobs projects …` parsed from `argv` (after `projects`).
    fn projects_args(argv: &[&str]) -> ProjectsCommand {
        let mut full = vec!["llmobs", "projects"];
        full.extend_from_slice(argv);
        match crate::Cli::try_parse_from(full).unwrap().command {
            crate::Command::Projects { command } => *command,
            other => panic!("unexpected command: {other:?}"),
        }
    }

    /// Runs `projects …` with `argv` and returns its stdout and stderr text.
    async fn capture(argv: &[&str]) -> anyhow::Result<(String, String)> {
        let dir = tempfile::tempdir().unwrap();
        let command = projects_args(argv);
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

    /// A project as the API returns it.
    fn project_json(id: &str, org_id: &str, slug: &str, name: &str, role: Option<&str>) -> Value {
        let mut value = json!({
            "id": id,
            "org_id": org_id,
            "slug": slug,
            "name": name,
            "created_at": "2026-01-01T09:00:00.000000000Z"
        });
        if let Some(role) = role {
            value["role"] = json!(role);
        }
        value
    }

    /// The first project of the seeded `acme` org, read with the bearer token.
    async fn direct_project(server: &TestServer) -> Value {
        let page: Value = reqwest::Client::new()
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
        page["data"][0].clone()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn projects_list_shows_slug_paths() {
        let server = TestServer::start().await;
        let project = direct_project(&server).await;
        let id = project["id"].as_str().unwrap();

        let (out, err) = capture(&[
            "list",
            "--url",
            server.url.as_str(),
            "--token",
            server.token.as_str(),
        ])
        .await
        .unwrap();

        assert_eq!(
            out,
            format!(
                "project   name  role   id\n--------  ----  -----  {}\nacme/app  App   admin  {id}\n",
                "-".repeat(36)
            )
        );
        assert_eq!(err, "1 row\n");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn projects_list_json() {
        let server = TestServer::start().await;
        let project = direct_project(&server).await;

        let (out, err) = capture(&[
            "list",
            "--format",
            "json",
            "--url",
            server.url.as_str(),
            "--token",
            server.token.as_str(),
        ])
        .await
        .unwrap();

        let parsed: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(
            parsed,
            json!([{
                "project": "acme/app",
                "id": project["id"],
                "name": "App",
                "org_id": project["org_id"],
                "role": "admin"
            }])
        );
        assert_eq!(err, "");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn projects_list_pages_orgs_and_projects() {
        let orgs_first = json!({
            "data": [{"id": "01900000-0000-7000-8000-000000000010", "slug": "acme", "name": "Acme", "role": "owner"}],
            "next_cursor": "o2"
        });
        let orgs_second = json!({
            "data": [{"id": "01900000-0000-7000-8000-000000000011", "slug": "beta", "name": "Beta", "role": "viewer"}],
            "next_cursor": null
        });
        let acme_first = json!({
            "data": [project_json(
                "01900000-0000-7000-8000-000000000001",
                "01900000-0000-7000-8000-000000000010",
                "support-bot",
                "Support bot",
                Some("owner"),
            )],
            "next_cursor": "p2"
        });
        let acme_second = json!({
            "data": [project_json(
                "01900000-0000-7000-8000-000000000002",
                "01900000-0000-7000-8000-000000000010",
                "playground",
                "Playground",
                Some("owner"),
            )],
            "next_cursor": null
        });
        let beta = json!({
            "data": [project_json(
                "01900000-0000-7000-8000-000000000003",
                "01900000-0000-7000-8000-000000000011",
                "web",
                "Web",
                None,
            )],
            "next_cursor": null
        });
        let canned = Canned::json(
            [orgs_first, orgs_second, acme_first, acme_second, beta]
                .into_iter()
                .map(|page| (200, page.to_string()))
                .collect(),
        )
        .await;

        let (out, err) = capture(&[
            "list",
            "--format",
            "json",
            "--url",
            canned.url.as_str(),
            "--token",
            "x",
        ])
        .await
        .unwrap();

        let targets: Vec<String> = canned
            .recorded()
            .into_iter()
            .map(|request| request.target)
            .collect();
        assert_eq!(
            targets,
            [
                "/api/v1/orgs?limit=1000",
                "/api/v1/orgs?limit=1000&cursor=o2",
                "/api/v1/orgs/acme/projects?limit=1000",
                "/api/v1/orgs/acme/projects?limit=1000&cursor=p2",
                "/api/v1/orgs/beta/projects?limit=1000",
            ]
        );
        let listed: Value = serde_json::from_str(&out).unwrap();
        let names: Vec<&str> = listed
            .as_array()
            .unwrap()
            .iter()
            .map(|project| project["project"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["acme/support-bot", "acme/playground", "beta/web"]);
        assert!(listed[2].get("role").is_none());
        assert_eq!(err, "");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn projects_list_stops_when_a_cursor_repeats() {
        let org_page = json!({
            "data": [{"id": "01900000-0000-7000-8000-000000000010", "slug": "acme", "name": "Acme", "role": "owner"}],
            "next_cursor": "o2"
        });
        let canned = Canned::json(vec![
            (200, org_page.to_string()),
            (200, org_page.to_string()),
        ])
        .await;
        let error = capture(&["list", "--url", canned.url.as_str(), "--token", "x"])
            .await
            .unwrap_err();
        assert_eq!(error.to_string(), "the server repeated a page cursor");
        assert_eq!(canned.recorded().len(), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn projects_list_cleans_a_control_character_in_a_cell() {
        let orgs = json!({
            "data": [{"id": "01900000-0000-7000-8000-000000000010", "slug": "acme", "name": "Acme", "role": "owner"}],
            "next_cursor": null
        });
        let projects = json!({
            "data": [project_json(
                "01900000-0000-7000-8000-000000000001",
                "01900000-0000-7000-8000-000000000010",
                "app",
                "a\u{1b}b",
                Some("admin"),
            )],
            "next_cursor": null
        });
        let canned = Canned::json(vec![(200, orgs.to_string()), (200, projects.to_string())]).await;

        let (out, err) = capture(&["list", "--url", canned.url.as_str(), "--token", "x"])
            .await
            .unwrap();

        assert_eq!(
            out,
            format!(
                "project   name  role   id\n--------  ----  -----  {}\nacme/app  a b   admin  {}\n",
                "-".repeat(36),
                "01900000-0000-7000-8000-000000000001"
            )
        );
        assert_eq!(err, "1 row\n");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn projects_list_refuses_a_key() {
        let server = TestServer::start().await;
        let error = capture(&[
            "list",
            "--url",
            server.url.as_str(),
            "--token",
            server.key.as_str(),
        ])
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), "Forbidden (forbidden)");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn broken_pipe_ends_quietly() {
        let canned = Canned::json(vec![
            (
                200,
                json!({
                    "data": [{"id": "01900000-0000-7000-8000-000000000010", "slug": "acme", "name": "Acme", "role": "owner"}],
                    "next_cursor": null
                })
                .to_string(),
            ),
            (200, r#"{"data":[],"next_cursor":null}"#.to_owned()),
        ])
        .await;
        let dir = tempfile::tempdir().unwrap();
        let command = projects_args(&["list", "--url", canned.url.as_str(), "--token", "x"]);
        let mut broken = BrokenPipe { on_write: true };
        let mut err = Vec::new();
        let mut input = io::empty();
        let mut streams = Io {
            out: &mut broken,
            err: &mut err,
            input: &mut input,
            stdout_is_terminal: false,
        };
        run(
            &command,
            || Ok(dir.path().join("missing.toml")),
            &mut streams,
        )
        .await
        .unwrap();
        assert!(err.is_empty());
    }
}
