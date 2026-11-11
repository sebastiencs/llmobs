//! `llmobs serve`: open storage, open `meta.db`, bind, and shut down (D137).
//!
//! Startup creates `data_dir`, then `Storage::open` (which takes `LOCK`), then
//! `meta.db`. A second process fails on the lock before `meta.db`.
//!
//! Shutdown stops accepting, drains in-flight HTTP for at most [`DRAIN`],
//! aborts background tasks, gives storage [`STORAGE_SHUTDOWN`] (spec 12's
//! 30 s flush budget; `Storage::shutdown` also caps each flush at 30 s), then
//! closes `meta.db`.

use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use llmobs_storage::Storage;
use llmobs_storage::error::OpenError;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::auth::Limits;
use crate::clock::Clock;
use crate::config::{ConfigError, ServerConfig};
use crate::http::{AppInner, AppState, router};
use crate::meta::{MetaDb, MetaError};

/// How long in-flight HTTP may finish after stop-accepting (spec 12).
const DRAIN: Duration = Duration::from_secs(20);

/// Best-effort cap on `Storage::shutdown` (spec 12).
const STORAGE_SHUTDOWN: Duration = Duration::from_secs(30);

/// How often idle rate-limit keys are dropped (D143).
const PRUNE_EVERY: Duration = Duration::from_secs(60);

/// How often coalesced `last_used_at` values are written (D140).
const LAST_USED_EVERY: Duration = Duration::from_secs(60);

/// How often expired web sessions are deleted.
const PURGE_EVERY: Duration = Duration::from_secs(60 * 60);

/// Why `llmobs serve` stopped before it was listening, or the accept loop failed.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    /// The config file or a `LLMOBS_*` value was rejected.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// `Storage::open` failed. A held `LOCK` says the data directory is in use.
    #[error("{}", storage_message(.0))]
    Storage(#[source] OpenError),
    /// `meta.db` could not be opened or migrated.
    #[error(transparent)]
    Meta(#[from] MetaError),
    /// The HTTP socket could not be bound.
    #[error("binding {addr}")]
    Bind {
        /// The address from `http_addr`.
        addr: SocketAddr,
        /// The bind or `local_addr` error.
        #[source]
        source: io::Error,
    },
    /// Creating `data_dir`, or the accept loop, failed.
    #[error(transparent)]
    Serve(io::Error),
    /// Headless init failed a check. The reason names the rule or the variable,
    /// not the password, the key, or the email.
    #[error("init failed: {reason}")]
    Init {
        /// What was wrong.
        reason: String,
    },
}

fn serve_bootstrap(error: crate::bootstrap::BootstrapError) -> ServeError {
    match error {
        crate::bootstrap::BootstrapError::Meta(error) => ServeError::Meta(error),
        crate::bootstrap::BootstrapError::Init { reason } => ServeError::Init { reason },
        crate::bootstrap::BootstrapError::Auth(error) => ServeError::Init {
            reason: error.to_string(),
        },
        crate::bootstrap::BootstrapError::Lock => ServeError::Init {
            reason: "setup token lock poisoned".to_owned(),
        },
    }
}

impl From<OpenError> for ServeError {
    fn from(error: OpenError) -> Self {
        Self::Storage(error)
    }
}

/// `OpenError::Locked` says `data directory is locked: {path}`. The path field
/// is private to the storage crate, so the in-use wording is applied to that
/// sentence. Every other open error keeps its own message.
fn storage_message(error: &OpenError) -> String {
    let text = error.to_string();
    if let Some(rest) = text.strip_prefix("data directory is locked:") {
        format!("data directory is in use:{rest}")
    } else {
        text
    }
}

/// A bound server. [`Self::shutdown`] drains HTTP and releases `LOCK`.
pub struct Running {
    /// The address `bind` actually got, including an ephemeral port.
    pub local_addr: SocketAddr,
    serve: Option<JoinHandle<io::Result<()>>>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    state: Option<AppState>,
    /// Background work. Shutdown aborts whatever is here.
    tasks: Vec<JoinHandle<()>>,
}

impl std::fmt::Debug for Running {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Running")
            .field("local_addr", &self.local_addr)
            .finish_non_exhaustive()
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(serve) = self.serve.take() {
            serve.abort();
        }
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// Open the data directory, bind `config.http_addr`, and serve until `shutdown`.
///
/// # Errors
///
/// Returns a [`ServeError`] from opening storage or `meta.db`, from binding,
/// or from the accept loop.
pub async fn run(
    config: ServerConfig,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), ServeError> {
    let running = bind(config).await?;
    shutdown.await;
    running.shutdown().await
}

/// Create `data_dir`, open storage and `meta.db`, and start accepting.
///
/// # Errors
///
/// Returns a [`ServeError`] when the directory, the lock, `meta.db`, or the
/// socket fails. Storage is shut down before a later step's error is returned,
/// so `LOCK` is not held after `Err`.
pub async fn bind(config: ServerConfig) -> Result<Running, ServeError> {
    let data_dir = config.data_dir.clone();
    let http_addr = config.http_addr;
    if let Err(source) = tokio::fs::create_dir_all(&data_dir).await {
        return Err(directory_error(&data_dir, &source));
    }
    let storage = Storage::open(config.storage_config()).await?;
    let meta = match MetaDb::open(&data_dir).await {
        Ok(meta) => meta,
        Err(error) => {
            release_storage(storage).await;
            return Err(error.into());
        }
    };
    let storage = Arc::new(storage);
    let state = AppState::new(AppInner {
        config,
        meta,
        storage,
        clock: Clock::System,
        limits: Limits::new(),
        key_cache: crate::auth::principal::ApiKeyCache::new(),
        last_used: crate::auth::last_used::LastUsed::new(),
        setup: crate::bootstrap::SetupState::new(),
        password_hasher: tokio::sync::OnceCell::new(),
    });
    if let Err(error) = crate::bootstrap::prepare(&state).await {
        release_state(state).await;
        return Err(serve_bootstrap(error));
    }
    let listener = match TcpListener::bind(http_addr).await {
        Ok(listener) => listener,
        Err(source) => {
            release_state(state).await;
            return Err(ServeError::Bind {
                addr: http_addr,
                source,
            });
        }
    };
    let local_addr = match listener.local_addr() {
        Ok(addr) => addr,
        Err(source) => {
            release_state(state).await;
            return Err(ServeError::Bind {
                addr: http_addr,
                source,
            });
        }
    };
    tracing::info!(%local_addr, "listening");
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let app = router(state.clone()).into_make_service_with_connect_info::<SocketAddr>();
    let serve = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await
    });
    Ok(Running {
        local_addr,
        serve: Some(serve),
        shutdown_tx: Some(shutdown_tx),
        state: Some(state.clone()),
        tasks: vec![
            spawn_prune(state.clone()),
            spawn_purge(state.clone()),
            spawn_last_used(state),
        ],
    })
}

fn spawn_purge(state: AppState) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(PURGE_EVERY);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        interval.tick().await;
        loop {
            interval.tick().await;
            let now = state.clock.now_ns();
            let purged = state
                .meta
                .write(move |conn| {
                    Box::pin(async move {
                        crate::meta::web_sessions::purge_expired_sessions(conn, now).await
                    })
                })
                .await;
            match purged {
                Ok(removed) if removed > 0 => {
                    tracing::info!(removed, "purged expired sessions");
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::error!(error = %error, "session purge failed");
                }
            }
        }
    })
}

fn spawn_last_used(state: AppState) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(LAST_USED_EVERY);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        interval.tick().await;
        loop {
            interval.tick().await;
            if let Err(error) = state.last_used.flush(&state.meta).await {
                tracing::error!(error = %error, "last_used flush failed");
            }
        }
    })
}

fn spawn_prune(state: AppState) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(PRUNE_EVERY);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        interval.tick().await;
        loop {
            interval.tick().await;
            state.limits.prune(state.clock.now_ns());
        }
    })
}

impl Running {
    /// Stop accepting, drain HTTP, stop background tasks, flush storage, close `meta.db`.
    ///
    /// A storage flush error is logged. The lock is released inside
    /// `Storage::shutdown` before that error, and this method still closes
    /// `meta.db`.
    ///
    /// # Errors
    ///
    /// Returns the accept loop's error when it failed before the drain finished.
    pub async fn shutdown(mut self) -> Result<(), ServeError> {
        if let Some(sender) = self.shutdown_tx.take() {
            let _ = sender.send(());
        }
        let serve_error = match self.serve.take() {
            Some(serve) => drain(serve).await,
            None => None,
        };
        for task in self.tasks.drain(..) {
            task.abort();
            let _ = task.await;
        }
        if let Some(state) = self.state.take() {
            if let Err(error) = state.last_used.flush(&state.meta).await {
                tracing::error!(error = %error, "last_used flush failed");
            }
            match tokio::time::timeout(STORAGE_SHUTDOWN, state.storage.shutdown()).await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    tracing::error!(error = %error, "storage shutdown");
                }
                Err(_elapsed) => {
                    tracing::error!("storage shutdown timed out");
                }
            }
            close_meta(state).await;
        }
        match serve_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

async fn drain(serve: JoinHandle<io::Result<()>>) -> Option<ServeError> {
    let abort = serve.abort_handle();
    match tokio::time::timeout(DRAIN, serve).await {
        Ok(Ok(Ok(()))) => None,
        Ok(Ok(Err(error))) => Some(ServeError::Serve(error)),
        Ok(Err(_join)) => Some(ServeError::Serve(io::Error::other(
            "http server task stopped",
        ))),
        Err(_elapsed) => {
            abort.abort();
            None
        }
    }
}

async fn close_meta(state: AppState) {
    match state.into_inner() {
        Ok(inner) => inner.meta.close().await,
        Err(state) => state.meta.reader().close().await,
    }
}

async fn release_storage(storage: Storage) {
    if let Err(error) = storage.shutdown().await {
        tracing::error!(error = %error, "storage shutdown");
    }
}

async fn release_state(state: AppState) {
    if let Err(error) = state.storage.shutdown().await {
        tracing::error!(error = %error, "storage shutdown");
    }
    close_meta(state).await;
}

fn directory_error(path: &Path, source: &io::Error) -> ServeError {
    ServeError::Serve(io::Error::new(
        source.kind(),
        format!("creating data directory {}: {source}", path.display()),
    ))
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use llmobs_storage::config::{
        FlushConfig, MemtableConfig, QueryConfig, S3Config, StorageConfig, WalConfig,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    use super::*;
    use crate::config::{AuthSettings, InitConfig, LogConfig, PublicUrl};

    #[tokio::test]
    async fn an_empty_database_logs_the_setup_url_once() {
        let dir = tempfile::tempdir().unwrap();
        let (running, logs) = crate::test_support::capture_logs(bind(config(dir.path()))).await;
        let running = running.unwrap();
        assert_eq!(
            logs.matches("setup required: open http://localhost:8080/setup?token=")
                .count(),
            1,
            "{logs}"
        );
        running.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn a_bad_init_secret_fails_startup() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config(dir.path());
        config.init = serde_json::from_value(serde_json::json!({
            "admin_email": "ada@example.com",
            "admin_password": "llmobs-ok1",
            "org": "Acme",
            "project": "Support bot",
            "secret_key": "not-a-secret-key",
        }))
        .unwrap();
        let error = bind(config).await.unwrap_err();
        let text = error.to_string();
        assert!(matches!(error, ServeError::Init { .. }), "{text}");
        assert!(text.contains("LLMOBS_INIT_SECRET_KEY"), "{text}");
        assert!(!text.contains("not-a-secret-key"), "{text}");
    }

    #[tokio::test]
    async fn healthz_over_tcp_and_shutdown_releases_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let running = bind(config(dir.path())).await.unwrap();
        let body = http_get(running.local_addr, "/healthz", "close").await;
        let text = String::from_utf8_lossy(&body);
        assert!(text.starts_with("HTTP/1.1 200"), "{text}");
        assert!(text.contains("\"status\":\"ok\""), "{text}");
        running.shutdown().await.unwrap();

        let again = bind(config(dir.path())).await.unwrap();
        again.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn a_second_bind_stops_on_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let running = bind(config(dir.path())).await.unwrap();
        let error = bind(config(dir.path())).await.unwrap_err();
        let text = error.to_string();
        assert!(matches!(error, ServeError::Storage(_)), "{text}");
        assert!(text.contains("in use"), "{text}");
        assert!(text.contains(&dir.path().display().to_string()), "{text}");
        running.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn idle_keepalive_shuts_down_within_the_drain() {
        let dir = tempfile::tempdir().unwrap();
        let running = bind(config(dir.path())).await.unwrap();
        let mut stream = TcpStream::connect(running.local_addr).await.unwrap();
        let request = b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: keep-alive\r\n\r\n";
        stream.write_all(request).await.unwrap();
        let body = read_response(&mut stream).await;
        let text = String::from_utf8_lossy(&body);
        assert!(text.starts_with("HTTP/1.1 200"), "{text}");

        let started = Instant::now();
        running.shutdown().await.unwrap();
        assert!(started.elapsed() <= DRAIN + Duration::from_secs(2));
    }

    async fn http_get(addr: SocketAddr, path: &str, connection: &str) -> Vec<u8> {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        let request =
            format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: {connection}\r\n\r\n");
        stream.write_all(request.as_bytes()).await.unwrap();
        read_response(&mut stream).await
    }

    async fn read_response(stream: &mut TcpStream) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut chunk = [0; 2048];
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let read = tokio::time::timeout(remaining, stream.read(&mut chunk))
                .await
                .expect("timed out waiting for the response");
            let count = read.expect("read");
            assert!(count > 0, "connection closed");
            buf.extend_from_slice(&chunk[..count]);
            if response_complete(&buf) {
                return buf;
            }
        }
    }

    fn response_complete(buf: &[u8]) -> bool {
        let Some(end) = buf.windows(4).position(|window| window == b"\r\n\r\n") else {
            return false;
        };
        let headers = &buf[..end];
        let body = &buf[end + 4..];
        let mut length = None;
        for line in headers.split(|&byte| byte == b'\n') {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let Ok(line) = std::str::from_utf8(line) else {
                continue;
            };
            let Some((name, value)) = line.split_once(':') else {
                continue;
            };
            if name.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().ok();
            }
        }
        match length {
            Some(length) => body.len() >= length,
            None => !body.is_empty(),
        }
    }

    #[tokio::test]
    async fn shutdown_flushes_last_used() {
        use crate::auth::roles::{KeyScope, OrgRole};
        use crate::meta::MetaDb;
        use crate::meta::api_keys::{NewApiKey, insert_api_key};
        use crate::meta::members::add_org_member;
        use crate::meta::orgs::insert_org;
        use crate::meta::projects::insert_project;
        use crate::meta::users::{NewUser, insert_user};

        let dir = tempfile::tempdir().unwrap();
        let running = bind(config(dir.path())).await.unwrap();
        let state = running.state.as_ref().unwrap().clone();
        let now = state.clock.now_ns();
        let key_id = state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    let user = insert_user(
                        conn,
                        NewUser {
                            email: "ada@example.com".to_owned(),
                            name: None,
                            password_hash: None,
                            email_verified_at: None,
                            is_instance_admin: true,
                        },
                        now,
                    )
                    .await?;
                    let org = insert_org(conn, "Acme", "acme", now).await?;
                    add_org_member(conn, org.id, user.id, OrgRole::Owner, now).await?;
                    let project =
                        insert_project(conn, org.id, "Support", "support-bot", None, now).await?;
                    let key = insert_api_key(
                        conn,
                        NewApiKey {
                            project_id: project.id,
                            kind: "secret",
                            scopes: vec![KeyScope::Read],
                            name: None,
                            prefix: "sk-llmobs-abcdef".to_owned(),
                            hash: "ab".repeat(32),
                            created_by: user.id,
                            expires_at: None,
                        },
                        now,
                    )
                    .await?;
                    Ok(key.id)
                })
            })
            .await
            .unwrap();
        state.last_used.record(key_id, now);
        running.shutdown().await.unwrap();
        let meta = MetaDb::open(dir.path()).await.unwrap();
        let stored: Option<i64> =
            sqlx::query_scalar("SELECT last_used_at FROM api_keys WHERE id = ?")
                .bind(key_id.to_string())
                .fetch_one(meta.reader())
                .await
                .unwrap();
        assert_eq!(stored, Some(now));
        meta.close().await;
    }

    fn config(data_dir: &Path) -> ServerConfig {
        ServerConfig {
            data_dir: data_dir.to_path_buf(),
            public_url: PublicUrl::default(),
            http_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            trusted_proxies: Vec::new(),
            storage: StorageConfig {
                data_dir: data_dir.to_path_buf(),
                object_store: None,
                s3: S3Config::default(),
                wal: WalConfig::default(),
                flush: FlushConfig::default(),
                memtable: MemtableConfig::default(),
                query: QueryConfig::default(),
            },
            auth: AuthSettings::default(),
            smtp_url: None,
            email_from: None,
            init: InitConfig::default(),
            log: LogConfig::default(),
        }
    }
}
