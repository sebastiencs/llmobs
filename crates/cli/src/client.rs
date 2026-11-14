//! The API client of the query commands (D-H-12).

use std::fmt::{self, Write as _};
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use anyhow::{Context, bail};
use clap::Args;
use llmobs_server::api::common::Problem;
use serde::Serialize;

use crate::credentials;
use crate::output;

/// A bearer token. `Debug` prints `Token(***)`; there is no `Display`.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Token(String);

impl Token {
    /// The token text, for the `Authorization` header only.
    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Token(***)")
    }
}

impl FromStr for Token {
    type Err = std::convert::Infallible;

    /// Trims surrounding whitespace.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Ok(Self(text.trim().to_owned()))
    }
}

/// Server and token flags shared by every API command.
#[derive(Debug, Clone, Args)]
pub(crate) struct ConnectionArgs {
    /// Server URL. Defaults to the only server saved by `llmobs login`.
    #[arg(long, env = "LLMOBS_URL", value_name = "URL")]
    pub(crate) url: Option<String>,
    /// Personal access token. Defaults to the token `llmobs login` saved for the server. A value given as a flag is visible to other local users in the process list; prefer LLMOBS_TOKEN or `llmobs login`.
    #[arg(
        long,
        env = "LLMOBS_TOKEN",
        hide_env_values = true,
        value_name = "TOKEN"
    )]
    pub(crate) token: Option<Token>,
}

/// The server and the token one command uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Target {
    /// Normalized: no trailing slash; starts with `http://` or `https://`; carries no credentials.
    pub(crate) server: String,
    pub(crate) token: Token,
}

/// Whether the URL embeds credentials: `user[:password]@` before the host. Such a URL would be
/// stored, printed and requested with the password inside, so it is refused instead.
fn has_userinfo(url: &str) -> bool {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority_end = after_scheme
        .find(['/', '?', '#'])
        .unwrap_or(after_scheme.len());
    after_scheme[..authority_end].contains('@')
}

/// `credentials` is called only when the credentials file is needed (rows 3 and 4 below).
pub(crate) fn resolve_target(
    args: &ConnectionArgs,
    credentials: impl FnOnce() -> anyhow::Result<PathBuf>,
) -> anyhow::Result<Target> {
    let url = args
        .url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty());
    if url.is_some_and(has_userinfo) {
        bail!("the server URL must not contain credentials");
    }
    let token = args
        .token
        .as_ref()
        .filter(|token| !token.expose().is_empty());
    let target = match (url, token) {
        (Some(url), Some(token)) => Target {
            server: credentials::normalize_server_url(url),
            token: token.clone(),
        },
        (None, Some(_)) => bail!("--token needs --url (or LLMOBS_URL)"),
        (Some(url), None) => {
            let server = credentials::normalize_server_url(url);
            let file = credentials::load(&credentials()?)?;
            let saved = file
                .servers
                .get(&server)
                .filter(|saved| !saved.token.trim().is_empty())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "no saved token for {server}; run `llmobs login --url {server}`"
                    )
                })?;
            Target {
                server,
                token: Token(saved.token.trim().to_owned()),
            }
        }
        (None, None) => {
            let file = credentials::load(&credentials()?)?;
            if file.servers.len() > 1 {
                bail!("more than one saved server; pass --url");
            }
            let Some((server, saved)) = file.servers.iter().next() else {
                bail!(
                    "not logged in; run `llmobs login --url <URL>` or set LLMOBS_URL and LLMOBS_TOKEN"
                );
            };
            if has_userinfo(server) {
                bail!("the server URL must not contain credentials");
            }
            if saved.token.trim().is_empty() {
                bail!("no saved token for {server}; run `llmobs login --url {server}`");
            }
            Target {
                server: server.clone(),
                token: Token(saved.token.trim().to_owned()),
            }
        }
    };
    let lower = target.server.to_ascii_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        bail!("--url must start with http:// or https://");
    }
    Ok(target)
}

/// `{project}` path segment: bytes outside `A–Z a–z 0–9 - . _ ~` become `%XX` (uppercase hex).
pub(crate) fn project_segment(project: &str) -> String {
    let mut out = String::with_capacity(project.len());
    for byte in project.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// The most of an error body that is read (bytes past this are dropped).
const PROBLEM_LIMIT: usize = 64 * 1024;

pub(crate) struct ApiClient {
    http: reqwest::Client,
    target: Target,
}

impl ApiClient {
    pub(crate) fn new(target: Target) -> anyhow::Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .read_timeout(Duration::from_secs(300))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("llmobs-cli/", env!("CARGO_PKG_VERSION")))
            .build()
            .context("building the http client")?;
        Ok(Self { http, target })
    }

    /// `POST` a JSON body and parse the 2xx answer. `timeout` covers the whole exchange.
    pub(crate) async fn post_json(
        &self,
        path: &str,
        body: &impl Serialize,
        timeout: Duration,
    ) -> anyhow::Result<serde_json::Value> {
        let response = self
            .request(path, body)
            .header(reqwest::header::ACCEPT, "application/json")
            .timeout(timeout)
            .send()
            .await
            .map_err(|error| self.unreachable(error))?;
        self.json_answer(response).await
    }

    /// `GET` with `Accept: application/json`; `query` pairs are form-encoded in order. `timeout`
    /// covers the whole exchange.
    pub(crate) async fn get_json(
        &self,
        path: &str,
        query: &[(&str, String)],
        timeout: Duration,
    ) -> anyhow::Result<serde_json::Value> {
        let response = self
            .http
            .get(format!("{}{path}", self.target.server))
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", self.target.token.expose()),
            )
            .header(reqwest::header::ACCEPT, "application/json")
            .query(query)
            .timeout(timeout)
            .send()
            .await
            .map_err(|error| self.unreachable(error))?;
        self.json_answer(response).await
    }

    /// The JSON body of a 2xx answer; any other status is a problem error.
    async fn json_answer(&self, response: reqwest::Response) -> anyhow::Result<serde_json::Value> {
        let status = response.status();
        if !status.is_success() {
            let text = read_limited(response).await;
            return Err(problem_error(status, &text, &self.target.server));
        }
        let bytes = response.bytes().await.map_err(|error| {
            anyhow::Error::new(error)
                .context(format!("reading the answer from {}", self.target.server))
        })?;
        serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("the server sent an invalid response"))
    }

    /// `POST` with `Accept: accept`. Returns the response once its status is 2xx; the caller
    /// streams the body with `chunk()`. `header_timeout` bounds the wait for the status line
    /// and headers only.
    pub(crate) async fn post_stream(
        &self,
        path: &str,
        body: &impl Serialize,
        accept: &str,
        header_timeout: Duration,
    ) -> anyhow::Result<reqwest::Response> {
        let request = self
            .request(path, body)
            .header(reqwest::header::ACCEPT, accept);
        let response = tokio::time::timeout(header_timeout, request.send())
            .await
            .map_err(|_| {
                anyhow::anyhow!(
                    "could not reach {}: no answer within {}s",
                    self.target.server,
                    header_timeout.as_secs()
                )
            })?
            .map_err(|error| self.unreachable(error))?;
        if response.status().is_success() {
            return Ok(response);
        }
        let status = response.status();
        let text = read_limited(response).await;
        Err(problem_error(status, &text, &self.target.server))
    }

    fn request(&self, path: &str, body: &impl Serialize) -> reqwest::RequestBuilder {
        self.http
            .post(format!("{}{path}", self.target.server))
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", self.target.token.expose()),
            )
            .json(body)
    }

    fn unreachable(&self, error: reqwest::Error) -> anyhow::Error {
        anyhow::Error::new(error).context(format!("could not reach {}", self.target.server))
    }
}

/// Reads at most [`PROBLEM_LIMIT`] bytes of a body. A read error ends the read.
async fn read_limited(mut response: reqwest::Response) -> Vec<u8> {
    let mut body = Vec::new();
    while let Ok(Some(chunk)) = response.chunk().await {
        let room = PROBLEM_LIMIT - body.len();
        if chunk.len() >= room {
            body.extend_from_slice(&chunk[..room]);
            break;
        }
        body.extend_from_slice(&chunk);
    }
    body
}

/// The error for a non-2xx answer: the token message for 401, the problem's title, code, detail
/// and field errors when the body is a problem, else the status line.
pub(crate) fn problem_error(
    status: reqwest::StatusCode,
    body: &[u8],
    server: &str,
) -> anyhow::Error {
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return anyhow::anyhow!("the server rejected the token; run `llmobs login --url {server}`");
    }
    if let Ok(problem) = serde_json::from_slice::<Problem>(body) {
        let code = serde_json::to_value(problem.code)
            .ok()
            .and_then(|value| value.as_str().map(str::to_owned))
            .unwrap_or_default();
        let mut text = format!("{} ({code})", output::clean(&problem.title));
        if let Some(detail) = &problem.detail {
            text.push_str(": ");
            text.push_str(&output::clean(detail));
        }
        for field in &problem.errors {
            text.push_str("\n  ");
            text.push_str(&output::clean(&field.field));
            text.push_str(": ");
            text.push_str(&output::clean(&field.message));
            if let Some(position) = field.position {
                let _ = write!(
                    text,
                    " (line {}, column {})",
                    position.line, position.column
                );
            }
        }
        return anyhow::anyhow!("{text}");
    }
    anyhow::anyhow!("the server answered {status}")
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use clap::CommandFactory;

    use super::*;

    const PROBLEM_400: &str = r#"{"type":"about:blank","title":"Bad Request","status":400,"code":"invalid_filter","detail":"SQL parse error","errors":[{"field":"query","message":"unexpected token","position":{"line":1,"column":8}}]}"#;

    fn no_credentials() -> anyhow::Result<PathBuf> {
        bail!("credentials must not be read")
    }

    fn saved_file(entries: &[(&str, &str)]) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("llmobs").join("credentials.toml");
        let mut file = credentials::CredentialsFile::default();
        for (server, token) in entries {
            file.servers.insert(
                (*server).to_owned(),
                credentials::SavedToken {
                    token: (*token).to_owned(),
                },
            );
        }
        credentials::save(&path, &file).unwrap();
        (dir, path)
    }

    fn args(url: Option<&str>, token: Option<&str>) -> ConnectionArgs {
        ConnectionArgs {
            url: url.map(str::to_owned),
            token: token.map(|token| Token::from_str(token).unwrap()),
        }
    }

    #[test]
    fn resolve_target_follows_flags_env_then_file() {
        let (_dir, path) = saved_file(&[("http://h:1", "pat-saved")]);
        let file = || -> anyhow::Result<PathBuf> { Ok(path.clone()) };

        let flags =
            resolve_target(&args(Some("http://h:1/"), Some("pat-x")), no_credentials).unwrap();
        assert_eq!(flags.server, "http://h:1");
        assert_eq!(flags.token.expose(), "pat-x");

        let token_only = resolve_target(&args(None, Some("pat-x")), no_credentials).unwrap_err();
        assert_eq!(
            token_only.to_string(),
            "--token needs --url (or LLMOBS_URL)"
        );

        let saved = resolve_target(&args(Some("http://h:1"), None), file).unwrap();
        assert_eq!(saved.token.expose(), "pat-saved");

        let missing = resolve_target(&args(Some("http://other:1"), None), file).unwrap_err();
        assert_eq!(
            missing.to_string(),
            "no saved token for http://other:1; run `llmobs login --url http://other:1`"
        );

        let one = resolve_target(&args(None, None), file).unwrap();
        assert_eq!(one.server, "http://h:1");
        assert_eq!(one.token.expose(), "pat-saved");

        let (_empty_dir, empty) = saved_file(&[]);
        let not_logged_in = resolve_target(&args(None, None), || Ok(empty.clone())).unwrap_err();
        assert_eq!(
            not_logged_in.to_string(),
            "not logged in; run `llmobs login --url <URL>` or set LLMOBS_URL and LLMOBS_TOKEN"
        );

        let (_two_dir, two) = saved_file(&[("http://a:1", "pat-a"), ("http://b:1", "pat-b")]);
        let ambiguous = resolve_target(&args(None, None), || Ok(two.clone())).unwrap_err();
        assert_eq!(
            ambiguous.to_string(),
            "more than one saved server; pass --url"
        );

        let blank_token = resolve_target(&args(Some("http://h:1"), Some("   ")), file).unwrap();
        assert_eq!(blank_token.token.expose(), "pat-saved");

        let padded =
            resolve_target(&args(Some("http://h:1"), Some("  pat-x  ")), no_credentials).unwrap();
        assert_eq!(padded.token.expose(), "pat-x");

        let bare = resolve_target(&args(Some("127.0.0.1:8080"), Some("pat-x")), no_credentials)
            .unwrap_err();
        assert_eq!(
            bare.to_string(),
            "--url must start with http:// or https://"
        );

        let upper =
            resolve_target(&args(Some("HTTPS://h"), Some("pat-x")), no_credentials).unwrap();
        assert_eq!(upper.server, "HTTPS://h");
    }

    #[test]
    fn resolve_target_refuses_urls_with_credentials() {
        for url in [
            "http://user@h:1",
            "https://user:pass@h",
            "http://u@h/path",
            "http://u@h?q=1",
        ] {
            let error =
                resolve_target(&args(Some(url), Some("pat-x")), no_credentials).unwrap_err();
            assert_eq!(
                error.to_string(),
                "the server URL must not contain credentials",
                "{url}"
            );
        }
        resolve_target(
            &args(Some("http://h:1/@path"), Some("pat-x")),
            no_credentials,
        )
        .unwrap();

        let (_dir, path) = saved_file(&[("http://user:pass@h:1", "pat-x")]);
        let saved = resolve_target(&args(None, None), || Ok(path.clone())).unwrap_err();
        assert_eq!(
            saved.to_string(),
            "the server URL must not contain credentials"
        );
    }

    #[test]
    fn blank_saved_token_counts_as_absent() {
        let (_dir, path) = saved_file(&[("http://h:1", "   ")]);
        let file = || -> anyhow::Result<PathBuf> { Ok(path.clone()) };
        for case in [&args(Some("http://h:1"), None), &args(None, None)] {
            let error = resolve_target(case, file).unwrap_err();
            assert_eq!(
                error.to_string(),
                "no saved token for http://h:1; run `llmobs login --url http://h:1`"
            );
        }
    }

    #[tokio::test]
    async fn a_body_cut_mid_read_is_a_reading_error() {
        let canned = crate::test_server::Canned::raw(vec![
            b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{\"a\":"
                .to_vec(),
        ])
        .await;
        let api = ApiClient::new(Target {
            server: canned.url.clone(),
            token: Token::from_str("x").unwrap(),
        })
        .unwrap();
        let error = api
            .post_json(
                "/api/v1/projects/p/sql",
                &serde_json::json!({}),
                Duration::from_secs(5),
            )
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").starts_with(&format!("reading the answer from {}", canned.url)),
            "{error:#}"
        );
    }

    #[test]
    fn project_segment_encodes_the_slug_slash() {
        assert_eq!(project_segment("acme/support-bot"), "acme%2Fsupport-bot");
        assert_eq!(
            project_segment("01900000-0000-7000-8000-000000000001"),
            "01900000-0000-7000-8000-000000000001"
        );
        assert_eq!(project_segment("a b"), "a%20b");
        assert_eq!(project_segment("é"), "%C3%A9");
    }

    #[test]
    fn problem_error_formats_each_case() {
        let error = problem_error(
            reqwest::StatusCode::BAD_REQUEST,
            PROBLEM_400.as_bytes(),
            "http://h:1",
        );
        assert_eq!(
            error.to_string(),
            "Bad Request (invalid_filter): SQL parse error\n  query: unexpected token (line 1, column 8)"
        );

        let timeout = problem_error(
            reqwest::StatusCode::UNPROCESSABLE_ENTITY,
            br#"{"type":"about:blank","title":"Unprocessable Entity","status":422,"code":"query_timeout"}"#,
            "http://h:1",
        );
        assert_eq!(timeout.to_string(), "Unprocessable Entity (query_timeout)");

        let escaped = problem_error(
            reqwest::StatusCode::BAD_REQUEST,
            br#"{"type":"about:blank","title":"Bad Request","status":400,"code":"bad_request","detail":"bad\u001b[2Jthing"}"#,
            "http://h:1",
        );
        assert_eq!(
            escaped.to_string(),
            "Bad Request (bad_request): bad [2Jthing"
        );

        let rejected = problem_error(reqwest::StatusCode::UNAUTHORIZED, b"", "http://h:1");
        assert_eq!(
            rejected.to_string(),
            "the server rejected the token; run `llmobs login --url http://h:1`"
        );

        let bad_gateway = problem_error(reqwest::StatusCode::BAD_GATEWAY, b"oops", "http://h:1");
        assert_eq!(
            bad_gateway.to_string(),
            "the server answered 502 Bad Gateway"
        );

        let redirect = problem_error(reqwest::StatusCode::FOUND, b"", "http://h:1");
        assert_eq!(redirect.to_string(), "the server answered 302 Found");
    }

    #[test]
    fn token_debug_is_redacted() {
        let token = Token::from_str("pat-secret").unwrap();
        assert_eq!(format!("{token:?}"), "Token(***)");
        let target = Target {
            server: "http://h:1".to_owned(),
            token,
        };
        assert!(!format!("{target:?}").contains("pat-secret"));
    }

    #[test]
    fn token_flag_hides_its_env_value() {
        let command = crate::Cli::command();
        let query = command.find_subcommand("query").expect("query subcommand");
        let token = query
            .get_arguments()
            .find(|argument| argument.get_id() == "token")
            .expect("token argument");
        assert!(token.is_hide_env_values_set());
        assert_eq!(
            token.get_env().and_then(|value| value.to_str()),
            Some("LLMOBS_TOKEN")
        );
    }

    #[test]
    fn url_flag_reads_the_llmobs_url_env() {
        let command = crate::Cli::command();
        let query = command.find_subcommand("query").expect("query subcommand");
        let url = query
            .get_arguments()
            .find(|argument| argument.get_id() == "url")
            .expect("url argument");
        assert_eq!(
            url.get_env().and_then(|value| value.to_str()),
            Some("LLMOBS_URL")
        );
    }
}
