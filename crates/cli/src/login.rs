//! `llmobs login` and `llmobs logout` (spec 08, D140).
//!
//! The callback server accepts one `GET /callback` whose `state` matches and
//! whose `token` parses as a personal access token. The token is checked with
//! `GET /api/auth/me` and stored in the credentials file. It is never logged.

use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use anyhow::Context;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::TryRng;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::credentials::{SavedToken, chosen_server, load, save, server_url};

const WAIT: Duration = Duration::from_secs(5 * 60);
const READ_LIMIT: usize = 8192;

const PAGE: &str = "<!DOCTYPE html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>llmobs</title></head><body><p>You can close this window.</p></body></html>";

/// `llmobs login`. `output` receives the browser URL, one line.
///
/// # Errors
///
/// Returns an error when the server URL is refused by `credentials::server_url`, the
/// callback times out, the token is rejected, or the credentials file cannot be written.
/// The message does not include the token, and a refused URL is never printed.
pub(crate) async fn login(
    raw: &str,
    no_browser: bool,
    credentials: &Path,
    mut output: impl Write + Send,
) -> anyhow::Result<()> {
    let server = server_url(raw)?;
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("binding the callback")?;
    let port = listener
        .local_addr()
        .context("binding the callback")?
        .port();
    let state = new_state().context("generating state")?;
    let browser = format!("{server}/cli-auth?port={port}&state={state}");
    writeln!(output, "{browser}").context("printing the login url")?;
    output.flush().context("printing the login url")?;
    drop(output);
    if !no_browser {
        open_browser(&browser);
    }
    let token = wait_for_callback(listener, &state).await?;
    verify(&server, &token).await?;
    let mut file = load(credentials)?;
    file.servers.insert(server, SavedToken { token });
    save(credentials, &file)?;
    Ok(())
}

/// `llmobs logout`. Removes the saved server even when the revoke call fails.
///
/// # Errors
///
/// Returns an error when no server can be chosen or the credentials file cannot
/// be updated. A failed revoke is a warning on stderr and is not an error.
pub(crate) async fn logout(raw: Option<&str>, credentials: &Path) -> anyhow::Result<()> {
    let mut file = load(credentials)?;
    let server = chosen_server(raw, &file)?;
    let Some(saved) = file.servers.remove(&server) else {
        anyhow::bail!("no saved token for that server");
    };
    if let Err(message) = revoke(&server, &saved.token).await {
        warn(&message);
    }
    if file.servers.is_empty() {
        if credentials.exists() {
            fs::remove_file(credentials).context("removing credentials")?;
        }
        return Ok(());
    }
    save(credentials, &file)
}

fn new_state() -> anyhow::Result<String> {
    let mut bytes = [0u8; 32];
    rand::rngs::SysRng
        .try_fill_bytes(&mut bytes)
        .map_err(|_| anyhow::anyhow!("the random source failed"))?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn is_personal_token(token: &str) -> bool {
    let mut prefix = String::from("pat-");
    prefix.push_str(llmobs_core::names::PRODUCT);
    prefix.push('-');
    let Some(tail) = token.strip_prefix(&prefix) else {
        return false;
    };
    tail.len() == 32 && tail.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

async fn wait_for_callback(listener: TcpListener, expected_state: &str) -> anyhow::Result<String> {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            anyhow::bail!("timed out waiting for the browser");
        }
        let (socket, _) = tokio::time::timeout(remaining, listener.accept())
            .await
            .map_err(|_| anyhow::anyhow!("timed out waiting for the browser"))?
            .context("accepting the callback")?;
        if let Some(token) = handle_callback(socket, expected_state).await {
            return Ok(token);
        }
    }
}

async fn handle_callback(mut socket: TcpStream, expected_state: &str) -> Option<String> {
    let line = read_request_line(&mut socket).await?;
    drain_headers(&mut socket).await;
    match token_from_request_line(&line, expected_state) {
        Some(token) => {
            let _ = respond(&mut socket, 200, "OK", "text/html; charset=utf-8", PAGE).await;
            Some(token)
        }
        None => {
            let _ = respond(
                &mut socket,
                400,
                "Bad Request",
                "text/plain; charset=utf-8",
                "rejected\n",
            )
            .await;
            None
        }
    }
}

async fn read_request_line(socket: &mut TcpStream) -> Option<String> {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if buf.len() >= READ_LIMIT {
            return None;
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        match tokio::time::timeout(remaining, socket.read(&mut byte)).await {
            Ok(Ok(1)) => {
                buf.push(byte[0]);
                if byte[0] == b'\n' {
                    return String::from_utf8(buf).ok();
                }
            }
            _ => return None,
        }
    }
}

async fn drain_headers(socket: &mut TcpStream) {
    let mut seen_cr = false;
    let mut blank = 0;
    let mut byte = [0u8; 1];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    for _ in 0..READ_LIMIT {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return;
        }
        match tokio::time::timeout(remaining, socket.read(&mut byte)).await {
            Ok(Ok(1)) => match byte[0] {
                b'\r' => seen_cr = true,
                b'\n' if seen_cr => {
                    blank += 1;
                    seen_cr = false;
                    if blank == 2 {
                        return;
                    }
                }
                _ => {
                    seen_cr = false;
                    blank = 0;
                }
            },
            _ => return,
        }
    }
}

async fn respond(
    socket: &mut TcpStream,
    status: u16,
    reason: &str,
    content_type: &str,
    body: &str,
) -> io::Result<()> {
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    socket.write_all(header.as_bytes()).await?;
    socket.write_all(body.as_bytes()).await?;
    socket.shutdown().await
}

fn token_from_request_line(line: &str, expected_state: &str) -> Option<String> {
    let mut parts = line.split_whitespace();
    let method = parts.next()?;
    let target = parts.next()?;
    if !method.eq_ignore_ascii_case("GET") {
        return None;
    }
    let (path, query) = target.split_once('?')?;
    if path != "/callback" {
        return None;
    }
    let mut token = None;
    let mut state = None;
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=')?;
        let value = percent_decode(value)?;
        match key {
            "token" if token.is_none() => token = Some(value),
            "state" if state.is_none() => state = Some(value),
            _ => {}
        }
    }
    let token = token?;
    let state = state?;
    if state != expected_state || !is_personal_token(&token) {
        return None;
    }
    Some(token)
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                if index + 2 >= bytes.len() {
                    return None;
                }
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok()?;
                out.push(u8::from_str_radix(hex, 16).ok()?);
                index += 3;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

async fn verify(server: &str, token: &str) -> anyhow::Result<()> {
    let client = http_client()?;
    let response = client
        .get(format!("{server}/api/auth/me"))
        .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
        .send()
        .await
        .context("could not reach the server")?;
    if response.status().is_success() {
        Ok(())
    } else {
        anyhow::bail!("the server rejected the token")
    }
}

async fn revoke(server: &str, token: &str) -> Result<(), String> {
    let client = http_client().map_err(|_| "could not reach the server".to_owned())?;
    let response = client
        .delete(format!("{server}/api/v1/me/tokens/current"))
        .header(reqwest::header::AUTHORIZATION, format!("Bearer {token}"))
        .send()
        .await
        .map_err(|_| "could not reach the server".to_owned())?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!("the server answered {}", response.status()))
    }
}

fn http_client() -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("building the http client")
}

fn warn(message: &str) {
    let _ = writeln!(io::stderr(), "warning: {message}");
}

fn open_browser(url: &str) {
    let _ = browser_command(url).status();
}

fn browser_command(url: &str) -> Command {
    #[cfg(target_os = "macos")]
    {
        let mut command = Command::new("open");
        command.arg(url);
        command
    }
    #[cfg(target_os = "windows")]
    {
        let mut command = Command::new("cmd");
        command.args(["/c", "start", "", url]);
        command
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let mut command = Command::new("xdg-open");
        command.arg(url);
        command
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::normalize_server_url;

    #[test]
    fn a_personal_token_has_the_pat_shape() {
        let tail = "EXAMPLE0EXAMPLE0EXAMPLE0EXAMPLE0";
        let token = format!("pat-{}-{tail}", llmobs_core::names::PRODUCT);
        assert!(is_personal_token(&token));
        assert!(!is_personal_token(&format!(
            "sk-{}-{tail}",
            llmobs_core::names::PRODUCT
        )));
    }

    #[tokio::test]
    async fn login_refuses_a_url_with_credentials_before_printing() {
        let dir = tempfile::tempdir().unwrap();
        let credentials = dir.path().join("llmobs").join("credentials.toml");
        let mut printed = Vec::new();
        let error = login("http://user:pw@h", true, &credentials, &mut printed)
            .await
            .unwrap_err();
        let text = error.to_string();
        assert_eq!(text, "the server URL must not contain credentials");
        assert!(!text.contains("user") && !text.contains("pw"));
        assert!(printed.is_empty());
        assert!(!credentials.exists());
    }

    struct LineWriter {
        tx: tokio::sync::mpsc::UnboundedSender<String>,
        buf: Vec<u8>,
    }

    impl Write for LineWriter {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.buf.extend_from_slice(data);
            while let Some(pos) = self.buf.iter().position(|byte| *byte == b'\n') {
                let line = String::from_utf8_lossy(&self.buf[..=pos]).trim().to_owned();
                let _ = self.tx.send(line);
                self.buf.drain(..=pos);
            }
            Ok(data.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn login_saves_a_token_and_logout_revokes_it() {
        let dir = std::env::temp_dir().join(format!(
            "llmobs-login-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let data = dir.join("data");
        let config_path = dir.join("llmobs.toml");
        fs::write(
            &config_path,
            format!(
                "data_dir = \"{}\"\nhttp_addr = \"127.0.0.1:0\"\notlp_http_addr = \"\"\ngrpc_addr = \"\"\npublic_url = \"http://127.0.0.1:8080\"\n\n[init]\nadmin_email = \"ada@example.com\"\nadmin_password = \"p17-cli-login-secret\"\norg = \"Acme\"\nproject = \"App\"\n",
                data.display()
            ),
        )
        .unwrap();
        let config = llmobs_server::config::ServerConfig::load(Some(&config_path)).unwrap();
        let running = llmobs_server::serve::bind(config, None).await.unwrap();
        let server = format!("http://{}", running.local_addr);
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap();
        let logged_in = client
            .post(format!("{server}/api/auth/login"))
            .header("origin", "http://127.0.0.1:8080")
            .json(&serde_json::json!({
                "email": "ada@example.com",
                "password": "p17-cli-login-secret"
            }))
            .send()
            .await
            .unwrap();
        assert!(logged_in.status().is_success(), "{}", logged_in.status());
        let cookie = logged_in
            .headers()
            .get(reqwest::header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .split_once(';')
            .unwrap()
            .0
            .to_owned();

        let credentials = dir.join("llmobs").join("credentials.toml");
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let server_for_login = server.clone();
        let credentials_for_login = credentials.clone();
        let login_task = tokio::spawn(async move {
            login(
                &server_for_login,
                true,
                &credentials_for_login,
                LineWriter {
                    tx,
                    buf: Vec::new(),
                },
            )
            .await
        });
        let printed = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("login did not print a url")
            .expect("login closed the url channel");
        let callback_port = query_value(&printed, "port").expect("callback port");
        let state = query_value(&printed, "state").expect("state");

        let created = client
            .post(format!("{server}/api/v1/me/tokens"))
            .header("origin", "http://127.0.0.1:8080")
            .header(reqwest::header::COOKIE, &cookie)
            .json(&serde_json::json!({
                "name": "cli",
                "scopes": ["read", "write"]
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(created.status(), 201, "{}", created.status());
        let created: serde_json::Value = created.json().await.unwrap();
        let secret = created["secret"].as_str().unwrap().to_owned();

        let wrong = client
            .get(format!(
                "http://127.0.0.1:{callback_port}/callback?token={secret}&state=wrong"
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(wrong.status(), 400);
        let wrong_body = wrong.text().await.unwrap();
        assert!(!wrong_body.contains(&secret));

        let ok = client
            .get(format!(
                "http://127.0.0.1:{callback_port}/callback?token={secret}&state={state}"
            ))
            .send()
            .await
            .unwrap();
        assert_eq!(ok.status(), 200);
        let page = ok.text().await.unwrap();
        assert!(!page.contains(&secret));
        login_task.await.unwrap().unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let file_mode = fs::metadata(&credentials).unwrap().permissions().mode() & 0o777;
            assert_eq!(file_mode, 0o600);
            let dir_mode = fs::metadata(credentials.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(dir_mode, 0o700);
        }
        let saved = load(&credentials).unwrap();
        assert_eq!(saved.servers[&normalize_server_url(&server)].token, secret);

        logout(Some(&server), &credentials).await.unwrap();
        assert!(
            !credentials.exists()
                || !load(&credentials)
                    .unwrap()
                    .servers
                    .contains_key(&normalize_server_url(&server))
        );
        let me = client
            .get(format!("{server}/api/auth/me"))
            .header(reqwest::header::AUTHORIZATION, format!("Bearer {secret}"))
            .send()
            .await
            .unwrap();
        assert_eq!(me.status(), 401);

        running.shutdown().await.unwrap();
        let _ = fs::remove_dir_all(&dir);
    }

    fn query_value(url: &str, name: &str) -> Option<String> {
        let query = url.split_once('?')?.1;
        for pair in query.split('&') {
            let (key, value) = pair.split_once('=')?;
            if key == name {
                return Some(value.to_owned());
            }
        }
        None
    }
}
