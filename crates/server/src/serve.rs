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

use llmobs_otel::{PriceError, PriceTable, builtin_price_rows};
use llmobs_storage::Storage;
use llmobs_storage::error::OpenError;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::auth::Limits;
use crate::clock::Clock;
use crate::config::{ConfigError, ServerConfig};
use crate::crypto::{CryptoError, KeySource, SecretBox, finish_rotation, load_key};
use crate::http::{AppInner, AppState, otlp_router, router};
use crate::ingest::{Ingest, grpc};
use crate::meta::instance_settings::{ENCRYPTION_CHECK, get_setting};
use crate::meta::{MetaDb, MetaError};
use tonic::transport::server::TcpIncoming;

/// How long in-flight HTTP may finish after stop-accepting (spec 12).
const DRAIN: Duration = Duration::from_secs(20);

/// Best-effort cap on `Storage::shutdown` (spec 12).
const STORAGE_SHUTDOWN: Duration = Duration::from_secs(30);

/// How often idle rate-limit keys are dropped (D143).
const PRUNE_EVERY: Duration = Duration::from_secs(60);

/// How often coalesced `last_used_at` values are written (D140).
const LAST_USED_EVERY: Duration = Duration::from_secs(60);

/// How often usage counters are written (D-G-11).
const USAGE_EVERY: Duration = Duration::from_secs(10);

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
    /// The encryption key could not be loaded, or does not match the data directory.
    #[error(transparent)]
    Crypto(#[from] CryptoError),
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
    /// The built-in price table did not compile. A build defect, not a config value.
    #[error("built-in price table")]
    Prices(#[source] PriceError),
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
    /// The OTLP/HTTP listener's address, when `otlp_http_addr` is set (D-G-21).
    pub otlp_local_addr: Option<SocketAddr>,
    /// The OTLP/gRPC listener's address, when `grpc_addr` is set (D-G-21).
    pub grpc_local_addr: Option<SocketAddr>,
    serve: Option<JoinHandle<io::Result<()>>>,
    shutdown_tx: Option<oneshot::Sender<()>>,
    otlp_serve: Option<JoinHandle<io::Result<()>>>,
    otlp_shutdown_tx: Option<oneshot::Sender<()>>,
    grpc_serve: Option<JoinHandle<io::Result<()>>>,
    grpc_shutdown_tx: Option<oneshot::Sender<()>>,
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
        if let Some(serve) = self.otlp_serve.take() {
            serve.abort();
        }
        if let Some(serve) = self.grpc_serve.take() {
            serve.abort();
        }
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// Open the data directory, bind `config.http_addr`, and serve until `shutdown`.
/// `ui` answers the main listener's non-API paths; the OTLP listeners never get it.
///
/// # Errors
///
/// Returns a [`ServeError`] from opening storage or `meta.db`, from binding,
/// or from the accept loop.
pub async fn run(
    config: ServerConfig,
    ui: Option<axum::Router>,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> Result<(), ServeError> {
    let running = bind(config, ui).await?;
    shutdown.await;
    running.shutdown().await
}

/// Create `data_dir`, open storage and `meta.db`, and start accepting. See [`run`] for `ui`.
///
/// # Errors
///
/// Returns a [`ServeError`] when the directory, the lock, `meta.db`, or the
/// socket fails. Storage is shut down before a later step's error is returned,
/// so `LOCK` is not held after `Err`.
pub async fn bind(config: ServerConfig, ui: Option<axum::Router>) -> Result<Running, ServeError> {
    let data_dir = config.data_dir.clone();
    let http_addr = config.http_addr;
    let otlp_http_addr = config.otlp_http_addr;
    let grpc_addr = config.grpc_addr;
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
    let secrets = match open_secrets(&config, &meta).await {
        Ok(secrets) => secrets,
        Err(error) => {
            release_storage(storage).await;
            meta.close().await;
            return Err(error.into());
        }
    };
    let eval_runner = match llmobs_eval::EvalRunner::new(llmobs_eval::EgressPolicy {
        allow_private_network: config.security.allow_private_network_egress,
    }) {
        Ok(runner) => Arc::new(runner),
        Err(_error) => {
            release_storage(storage).await;
            meta.close().await;
            return Err(ServeError::Config(ConfigError::Invalid {
                key: "security",
                reason: "could not build the evaluation HTTP client",
            }));
        }
    };
    let prices = match PriceTable::new(builtin_price_rows()) {
        Ok(prices) => prices,
        Err(error) => {
            release_storage(storage).await;
            meta.close().await;
            return Err(ServeError::Prices(error));
        }
    };
    let ingest = Ingest::new(&config.ingest, &config.media, prices);
    let mailer = match crate::email::Mailer::from_config(&config) {
        Ok(mailer) => mailer.map(Arc::new),
        Err(_error) => {
            release_storage(storage).await;
            meta.close().await;
            return Err(ServeError::Config(ConfigError::Invalid {
                key: "smtp_url",
                reason: "could not build the mailer",
            }));
        }
    };
    let providers = match crate::auth::oidc::identity_client()
        .and_then(|http| crate::auth::oidc::Providers::from_config(&config.auth, &http))
    {
        Ok(providers) => providers,
        Err(error) => {
            release_storage(storage).await;
            meta.close().await;
            let reason = match error {
                crate::auth::oidc::OidcError::Client => {
                    "could not build the identity provider client"
                }
                crate::auth::oidc::OidcError::Unavailable
                | crate::auth::oidc::OidcError::Invalid => {
                    "identity provider configuration is invalid"
                }
            };
            return Err(ServeError::Config(ConfigError::Invalid {
                key: "auth",
                reason,
            }));
        }
    };
    let oauth_key = match crate::auth::oidc::random_key() {
        Ok(key) => key,
        Err(_error) => {
            release_storage(storage).await;
            meta.close().await;
            return Err(ServeError::Config(ConfigError::Invalid {
                key: "auth",
                reason: "could not generate the oauth state key",
            }));
        }
    };
    let license = crate::ee::license::LicenseState::from_key(
        config.license_key.as_ref().map(|key| key.expose().as_str()),
    );
    crate::entitlements::log_license(&license, Clock::System.now_ns());
    let storage = Arc::new(storage);
    let state = AppState::new(AppInner {
        config,
        meta,
        storage,
        clock: Clock::System,
        limits: Limits::new(),
        key_cache: crate::auth::principal::ApiKeyCache::new(),
        pat_cache: crate::auth::principal::PatCache::new(),
        last_used: crate::auth::last_used::LastUsed::new(),
        token_last_used: crate::auth::last_used::LastUsed::tokens(),
        setup: crate::bootstrap::SetupState::new(),
        password_hasher: tokio::sync::OnceCell::new(),
        mailer,
        providers,
        oauth_key,
        ingest,
        live: crate::live::LiveBus::new(crate::live::StreamTiming::DEFAULT),
        metrics_cache: crate::routes::metrics::MetricsCache::default(),
        usage: crate::usage::Usage::default(),
        secrets,
        eval_runner,
        connections: crate::eval::connection::ConnectionCache::default(),
        entitlements: crate::entitlements::EntitlementsState::new(license),
    });
    crate::ee::sso_enforcement::warn_if_unlicensed(&state);
    if let Err(error) = crate::bootstrap::prepare(&state).await {
        release_state(state).await;
        return Err(serve_bootstrap(error));
    }
    // Every socket is bound before any server starts. A failure drops the sockets
    // already bound and releases the state, so `LOCK` is free after `Err`.
    let (listener, local_addr) = match bind_listener(http_addr).await {
        Ok(bound) => bound,
        Err(error) => {
            release_state(state).await;
            return Err(error);
        }
    };
    let otlp = match otlp_http_addr {
        None => None,
        Some(addr) => match bind_listener(addr).await {
            Ok(bound) => Some(bound),
            Err(error) => {
                release_state(state).await;
                return Err(error);
            }
        },
    };
    let grpc = match grpc_addr {
        None => None,
        Some(addr) => match bind_listener(addr).await {
            Ok(bound) => Some(bound),
            Err(error) => {
                release_state(state).await;
                return Err(error);
            }
        },
    };
    tracing::info!(%local_addr, "listening");
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let app = router(state.clone(), ui).into_make_service_with_connect_info::<SocketAddr>();
    let serve = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await
    });
    let (otlp_local_addr, otlp_serve, otlp_shutdown_tx) = match otlp {
        None => (None, None, None),
        Some((listener, addr)) => {
            tracing::info!(otlp_http_local_addr = %addr, "listening");
            let (tx, rx) = oneshot::channel();
            let app =
                otlp_router(state.clone()).into_make_service_with_connect_info::<SocketAddr>();
            let serve = tokio::spawn(async move {
                axum::serve(listener, app)
                    .with_graceful_shutdown(async move {
                        let _ = rx.await;
                    })
                    .await
            });
            (Some(addr), Some(serve), Some(tx))
        }
    };
    let (grpc_local_addr, grpc_serve, grpc_shutdown_tx) = match grpc {
        None => (None, None, None),
        Some((listener, addr)) => {
            tracing::info!(grpc_local_addr = %addr, "listening");
            let (tx, rx) = oneshot::channel();
            let service = grpc::service(state.clone());
            let serve = tokio::spawn(async move {
                tonic::transport::Server::builder()
                    .add_service(service)
                    .serve_with_incoming_shutdown(TcpIncoming::from(listener), async move {
                        let _ = rx.await;
                    })
                    .await
                    .map_err(io::Error::other)
            });
            (Some(addr), Some(serve), Some(tx))
        }
    };
    Ok(Running {
        local_addr,
        otlp_local_addr,
        grpc_local_addr,
        serve: Some(serve),
        shutdown_tx: Some(shutdown_tx),
        otlp_serve,
        otlp_shutdown_tx,
        grpc_serve,
        grpc_shutdown_tx,
        state: Some(state.clone()),
        tasks: vec![
            spawn_prune(state.clone()),
            spawn_purge(state.clone()),
            spawn_last_used(state.clone()),
            spawn_usage(state),
        ],
    })
}

/// Finishes an interrupted rotation, loads the key, then checks it (D-I-1, D-I-3).
async fn open_secrets(config: &ServerConfig, meta: &MetaDb) -> Result<SecretBox, CryptoError> {
    let check = get_setting(meta.reader(), ENCRYPTION_CHECK).await?;
    let env = config
        .encryption_key
        .as_ref()
        .map(|key| key.expose().as_str());
    if env.is_none() {
        finish_rotation(&config.data_dir, check.as_deref())?;
    }
    let loaded = load_key(env, &config.data_dir, check.is_some())?;
    let secrets = SecretBox::new(&loaded.key);
    crate::crypto::verify_or_record(meta, &secrets, &loaded.path).await?;
    if loaded.source == KeySource::Generated {
        tracing::warn!(
            path = %loaded.path.display(),
            "generated an encryption key; back this file up with the data directory"
        );
    }
    Ok(secrets)
}

/// Binds `addr` and returns the listener with the address it actually got.
async fn bind_listener(addr: SocketAddr) -> Result<(TcpListener, SocketAddr), ServeError> {
    let bind_error = |source| ServeError::Bind { addr, source };
    let listener = TcpListener::bind(addr).await.map_err(bind_error)?;
    let local_addr = listener.local_addr().map_err(bind_error)?;
    Ok((listener, local_addr))
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
            if let Err(error) = state.token_last_used.flush(&state.meta).await {
                tracing::error!(error = %error, "last_used flush failed");
            }
        }
    })
}

fn spawn_usage(state: AppState) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(USAGE_EVERY);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        interval.tick().await;
        loop {
            interval.tick().await;
            if let Err(error) = state.usage.flush(&state.meta).await {
                tracing::error!(error = %error, "usage flush failed");
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
        if let Some(state) = &self.state {
            state.live.close();
        }
        if let Some(sender) = self.shutdown_tx.take() {
            let _ = sender.send(());
        }
        if let Some(sender) = self.otlp_shutdown_tx.take() {
            let _ = sender.send(());
        }
        if let Some(sender) = self.grpc_shutdown_tx.take() {
            let _ = sender.send(());
        }
        let (serve_result, otlp_result, grpc_result) = tokio::join!(
            drain_optional(self.serve.take()),
            drain_optional(self.otlp_serve.take()),
            drain_optional(self.grpc_serve.take()),
        );
        let serve_error = serve_result.or(otlp_result).or(grpc_result);
        for task in self.tasks.drain(..) {
            task.abort();
            let _ = task.await;
        }
        if let Some(state) = self.state.take() {
            // Commits whose request was dropped still run (D-G-38): they append and count
            // before the final flushes.
            if tokio::time::timeout(STORAGE_SHUTDOWN, state.ingest.wait_for_commits())
                .await
                .is_err()
            {
                tracing::error!("ingest commits did not finish before shutdown");
            }
            if let Err(error) = state.last_used.flush(&state.meta).await {
                tracing::error!(error = %error, "last_used flush failed");
            }
            if let Err(error) = state.token_last_used.flush(&state.meta).await {
                tracing::error!(error = %error, "last_used flush failed");
            }
            if let Err(error) = state.usage.flush(&state.meta).await {
                tracing::error!(error = %error, "usage flush failed");
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

async fn drain_optional(serve: Option<JoinHandle<io::Result<()>>>) -> Option<ServeError> {
    match serve {
        Some(serve) => drain(serve).await,
        None => None,
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
        AnchorConfig, FlushConfig, MemtableConfig, QueryConfig, S3Config, StorageConfig, WalConfig,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    use bytesize::ByteSize;
    use llmobs_otel::decode_json;
    use opentelemetry_proto::tonic::collector::trace::v1::ExportTraceServiceRequest;
    use opentelemetry_proto::tonic::collector::trace::v1::trace_service_client::TraceServiceClient;
    use opentelemetry_proto::tonic::common::v1::{AnyValue, KeyValue, any_value};
    use tonic::codec::CompressionEncoding;

    use super::*;
    use crate::config::{
        AuthSettings, EntitlementsConfig, EvalConfig, IngestConfig, InitConfig, LogConfig,
        MediaConfig, PublicUrl, Secret, SecurityConfig,
    };
    use crate::crypto::{decode_key, encode_key, random_key, write_key_file};
    use crate::meta::instance_settings::put_setting;

    #[tokio::test]
    async fn an_empty_database_logs_the_setup_url_once() {
        let dir = tempfile::tempdir().unwrap();
        let (running, logs) =
            crate::test_support::capture_logs(Box::pin(bind(config(dir.path()), None))).await;
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
        let error = bind(config, None).await.unwrap_err();
        let text = error.to_string();
        assert!(matches!(error, ServeError::Init { .. }), "{text}");
        assert!(text.contains("LLMOBS_INIT_SECRET_KEY"), "{text}");
        assert!(!text.contains("not-a-secret-key"), "{text}");
    }

    #[tokio::test]
    async fn a_wrong_encryption_key_fails_startup() {
        let dir = tempfile::tempdir().unwrap();
        let mut first = config(dir.path());
        first.encryption_key = Some(Secret::new(encode_key(&[1; 32])));
        let running = bind(first, None).await.unwrap();
        running.shutdown().await.unwrap();

        let mut second = config(dir.path());
        second.encryption_key = Some(Secret::new(encode_key(&[2; 32])));
        let error = bind(second, None).await.unwrap_err();
        assert!(matches!(error, ServeError::Crypto(_)), "{error}");

        // The failed start released the lock, and the first key still works.
        let mut third = config(dir.path());
        third.encryption_key = Some(Secret::new(encode_key(&[1; 32])));
        let running = bind(third, None).await.unwrap();
        running.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn startup_finishes_an_interrupted_rotation() {
        let dir = tempfile::tempdir().unwrap();
        let running = bind(config(dir.path()), None).await.unwrap();
        running.shutdown().await.unwrap();

        // An interrupted rotation: the database already uses the new key.
        let new = random_key().unwrap();
        write_key_file(&dir.path().join("secret.key.new"), &new).unwrap();
        let check = SecretBox::new(&new).check_value().to_owned();
        let meta = MetaDb::open(dir.path()).await.unwrap();
        meta.write(move |conn| {
            Box::pin(async move { put_setting(conn, ENCRYPTION_CHECK, &check).await })
        })
        .await
        .unwrap();
        meta.close().await;

        let running = bind(config(dir.path()), None).await.unwrap();
        running.shutdown().await.unwrap();
        let rotated = std::fs::read_to_string(dir.path().join("secret.key")).unwrap();
        assert_eq!(decode_key(rotated.trim_end(), "test").unwrap(), new);
        assert!(!dir.path().join("secret.key.new").exists());
    }

    #[tokio::test]
    async fn startup_removes_a_stale_new_key() {
        let dir = tempfile::tempdir().unwrap();
        let running = bind(config(dir.path()), None).await.unwrap();
        running.shutdown().await.unwrap();
        let before = std::fs::read_to_string(dir.path().join("secret.key")).unwrap();
        write_key_file(&dir.path().join("secret.key.new"), &random_key().unwrap()).unwrap();

        let running = bind(config(dir.path()), None).await.unwrap();
        running.shutdown().await.unwrap();
        assert!(!dir.path().join("secret.key.new").exists());
        let after = std::fs::read_to_string(dir.path().join("secret.key")).unwrap();
        assert_eq!(before, after);
    }

    #[tokio::test]
    async fn a_lost_key_file_fails_startup() {
        let dir = tempfile::tempdir().unwrap();
        let running = bind(config(dir.path()), None).await.unwrap();
        running.shutdown().await.unwrap();
        std::fs::remove_file(dir.path().join("secret.key")).unwrap();

        let error = bind(config(dir.path()), None).await.unwrap_err();
        assert!(
            matches!(error, ServeError::Crypto(CryptoError::Missing { .. })),
            "{error}"
        );
    }

    #[tokio::test]
    async fn healthz_over_tcp_and_shutdown_releases_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let running = bind(config(dir.path()), None).await.unwrap();
        let body = http_get(running.local_addr, "/healthz", "close").await;
        let text = String::from_utf8_lossy(&body);
        assert!(text.starts_with("HTTP/1.1 200"), "{text}");
        assert!(text.contains("\"status\":\"ok\""), "{text}");
        running.shutdown().await.unwrap();

        let again = bind(config(dir.path()), None).await.unwrap();
        again.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn a_second_bind_stops_on_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let running = bind(config(dir.path()), None).await.unwrap();
        let error = bind(config(dir.path()), None).await.unwrap_err();
        let text = error.to_string();
        assert!(matches!(error, ServeError::Storage(_)), "{text}");
        assert!(text.contains("in use"), "{text}");
        assert!(text.contains(&dir.path().display().to_string()), "{text}");
        running.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn idle_keepalive_shuts_down_within_the_drain() {
        let dir = tempfile::tempdir().unwrap();
        let running = bind(config(dir.path()), None).await.unwrap();
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
    async fn shutdown_flushes_usage() {
        use crate::meta::MetaDb;
        use llmobs_core::ids::{OrgId, ProjectId};

        let dir = tempfile::tempdir().unwrap();
        let running = bind(config(dir.path()), None).await.unwrap();
        let state = running.state.as_ref().unwrap().clone();
        let project = ProjectId::new_v7();
        // Counted less than 10 s before shutdown, so only the shutdown flush can write it.
        state
            .usage
            .add(OrgId::new_v7(), project, state.clock.now_ns(), 3, 0, 0);
        running.shutdown().await.unwrap();
        let meta = MetaDb::open(dir.path()).await.unwrap();
        let spans: i64 = sqlx::query_scalar("SELECT spans FROM usage_daily WHERE project_id = ?")
            .bind(project.to_string())
            .fetch_one(meta.reader())
            .await
            .unwrap();
        assert_eq!(spans, 3);
    }

    #[tokio::test]
    async fn an_ingest_request_over_tcp_is_counted_by_the_shutdown_flush() {
        use crate::meta::MetaDb;

        let dir = tempfile::tempdir().unwrap();
        let running = bind(with_init(config(dir.path())), None).await.unwrap();
        let accepted = post_traces(running.local_addr, OTLP_KEY).await;
        assert!(accepted.starts_with("HTTP/1.1 200"), "{accepted}");
        running.shutdown().await.unwrap();
        let meta = MetaDb::open(dir.path()).await.unwrap();
        let spans: i64 = sqlx::query_scalar("SELECT spans FROM usage_daily")
            .fetch_one(meta.reader())
            .await
            .unwrap();
        assert!(spans > 0, "{spans}");
        meta.close().await;
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
        let running = bind(config(dir.path()), None).await.unwrap();
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

    const OTLP_KEY: &str = "sk-llmobs-0123456789ABCDEFGHIJKLMNOPQRSTUV";
    const TRACES: &str =
        include_str!("../../../fixtures/otlp/genai/finish-reasons-fill/input.json");

    fn with_init(mut config: ServerConfig) -> ServerConfig {
        config.init = serde_json::from_value(serde_json::json!({
            "admin_email": "ada@example.com",
            "admin_password": "llmobs-ok1",
            "org": "Acme",
            "project": "Support bot",
            "secret_key": OTLP_KEY,
        }))
        .unwrap();
        config
    }

    async fn send_raw(addr: SocketAddr, request: &[u8]) -> String {
        let mut stream = TcpStream::connect(addr).await.unwrap();
        stream.write_all(request).await.unwrap();
        String::from_utf8_lossy(&read_response(&mut stream).await).into_owned()
    }

    async fn post_traces(addr: SocketAddr, key: &str) -> String {
        let request = format!(
            "POST /v1/traces HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nAuthorization: Bearer {key}\r\nConnection: close\r\n\r\n{TRACES}",
            TRACES.len()
        );
        send_raw(addr, request.as_bytes()).await
    }

    #[tokio::test]
    async fn otlp_port_takes_traces_only() {
        let dir = tempfile::tempdir().unwrap();
        let mut both = with_init(config(dir.path()));
        both.otlp_http_addr = Some(SocketAddr::from(([127, 0, 0, 1], 0)));
        let running = bind(both, None).await.unwrap();
        let otlp = running.otlp_local_addr.expect("the OTLP listener is bound");
        assert_ne!(otlp, running.local_addr);

        let accepted = post_traces(otlp, OTLP_KEY).await;
        assert!(accepted.starts_with("HTTP/1.1 200"), "{accepted}");

        let health = send_raw(
            otlp,
            b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(health.starts_with("HTTP/1.1 404"), "{health}");

        let on_main = post_traces(running.local_addr, OTLP_KEY).await;
        assert!(on_main.starts_with("HTTP/1.1 200"), "{on_main}");
        running.shutdown().await.unwrap();
    }

    fn test_ui() -> axum::Router {
        axum::Router::new().fallback(|| async { "ui" })
    }

    #[tokio::test]
    async fn the_ui_answers_the_main_listener_but_not_api_paths() {
        let dir = tempfile::tempdir().unwrap();
        let mut both = config(dir.path());
        both.otlp_http_addr = Some(SocketAddr::from(([127, 0, 0, 1], 0)));
        let running = bind(both, Some(test_ui())).await.unwrap();

        let page = http_get(running.local_addr, "/some/page", "close").await;
        let text = String::from_utf8_lossy(&page);
        assert!(text.starts_with("HTTP/1.1 200"), "{text}");
        assert!(text.ends_with("ui"), "{text}");

        for path in ["/apiary", "/v1x"] {
            let page = http_get(running.local_addr, path, "close").await;
            let text = String::from_utf8_lossy(&page);
            assert!(text.starts_with("HTTP/1.1 200"), "{path}: {text}");
            assert!(text.ends_with("ui"), "{path}: {text}");
        }

        for path in ["/api", "/api/v1/nope", "/v1/nope", "/mcp/x"] {
            let missing = http_get(running.local_addr, path, "close").await;
            let text = String::from_utf8_lossy(&missing);
            assert!(text.starts_with("HTTP/1.1 404"), "{path}: {text}");
            assert!(text.contains("application/problem+json"), "{path}: {text}");
        }

        let health = http_get(running.local_addr, "/healthz", "close").await;
        assert!(String::from_utf8_lossy(&health).starts_with("HTTP/1.1 200"));

        let otlp = running.otlp_local_addr.expect("the OTLP listener is bound");
        let on_otlp = send_raw(
            otlp,
            b"GET /some/page HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(on_otlp.starts_with("HTTP/1.1 404"), "{on_otlp}");
        running.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn without_a_ui_unknown_paths_are_problems() {
        let dir = tempfile::tempdir().unwrap();
        let running = bind(config(dir.path()), None).await.unwrap();
        let missing = http_get(running.local_addr, "/some/page", "close").await;
        let text = String::from_utf8_lossy(&missing);
        assert!(text.starts_with("HTTP/1.1 404"), "{text}");
        assert!(text.contains("application/problem+json"), "{text}");
        running.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn no_otlp_address_binds_one_listener() {
        let dir = tempfile::tempdir().unwrap();
        let running = bind(config(dir.path()), None).await.unwrap();
        assert!(running.otlp_local_addr.is_none());
        assert!(running.grpc_local_addr.is_none());
        running.shutdown().await.unwrap();
    }

    fn with_grpc(data_dir: &Path, tweak: impl FnOnce(&mut ServerConfig)) -> ServerConfig {
        let mut config = with_init(config(data_dir));
        config.grpc_addr = Some(SocketAddr::from(([127, 0, 0, 1], 0)));
        tweak(&mut config);
        config
    }

    /// `TRACES` as an export request, with the ingest key in `authorization`.
    fn authorized(message: ExportTraceServiceRequest) -> tonic::Request<ExportTraceServiceRequest> {
        let mut request = tonic::Request::new(message);
        request.metadata_mut().insert(
            "authorization",
            format!("Bearer {OTLP_KEY}").parse().expect("ASCII token"),
        );
        request
    }

    #[tokio::test]
    async fn grpc_export_over_tcp_plain_and_gzip() {
        let dir = tempfile::tempdir().unwrap();
        let running = bind(with_grpc(dir.path(), |_| {}), None).await.unwrap();
        let addr = running.grpc_local_addr.expect("the gRPC listener is bound");
        let message = decode_json(TRACES.as_bytes()).expect("fixture decodes");

        let mut plain = TraceServiceClient::connect(format!("http://{addr}"))
            .await
            .unwrap();
        plain
            .export(authorized(message.clone()))
            .await
            .expect("plain export is accepted");

        let mut gzip = TraceServiceClient::connect(format!("http://{addr}"))
            .await
            .unwrap()
            .send_compressed(CompressionEncoding::Gzip);
        gzip.export(authorized(message))
            .await
            .expect("gzip export is accepted");
        drop(plain);
        drop(gzip);
        running.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn grpc_refuses_a_message_over_the_configured_size() {
        let dir = tempfile::tempdir().unwrap();
        let running = bind(
            with_grpc(dir.path(), |config| {
                config.ingest.max_grpc_message_bytes = ByteSize::kib(1);
            }),
            None,
        )
        .await
        .unwrap();
        let addr = running.grpc_local_addr.expect("the gRPC listener is bound");
        let mut message = decode_json(TRACES.as_bytes()).expect("fixture decodes");
        let padding = KeyValue {
            key: "pad".to_owned(),
            value: Some(AnyValue {
                value: Some(any_value::Value::StringValue("x".repeat(4096))),
            }),
            ..Default::default()
        };
        message.resource_spans[0].scope_spans[0].spans[0]
            .attributes
            .push(padding);

        let mut client = TraceServiceClient::connect(format!("http://{addr}"))
            .await
            .unwrap();
        let status = client
            .export(authorized(message))
            .await
            .expect_err("the message is over the limit");
        assert_eq!(status.code(), tonic::Code::OutOfRange, "{status:?}");
        drop(client);
        running.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn empty_grpc_address_binds_no_grpc_listener() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config(dir.path());
        config.grpc_addr = None;
        let running = bind(config, None).await.unwrap();
        assert!(running.grpc_local_addr.is_none());
        running.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn empty_otlp_address_in_toml_binds_one_listener() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("llmobs.toml");
        std::fs::write(
            &path,
            format!(
                "data_dir = \"{}\"\nhttp_addr = \"127.0.0.1:0\"\notlp_http_addr = \"\"\ngrpc_addr = \"\"\n",
                dir.path().display()
            ),
        )
        .unwrap();
        let loaded = ServerConfig::load(Some(&path)).unwrap();
        assert_eq!(loaded.otlp_http_addr, None);
        let running = bind(loaded, None).await.unwrap();
        assert!(running.otlp_local_addr.is_none());
        running.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn an_occupied_otlp_port_fails_bind_and_releases_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let taken = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let taken_addr = taken.local_addr().unwrap();
        let mut clash = config(dir.path());
        clash.otlp_http_addr = Some(taken_addr);
        let error = bind(clash, None).await.unwrap_err();
        let text = error.to_string();
        assert!(
            matches!(error, ServeError::Bind { addr, .. } if addr == taken_addr),
            "{text}"
        );
        assert!(text.contains(&taken_addr.to_string()), "{text}");
        drop(taken);

        let again = bind(config(dir.path()), None).await.unwrap();
        again.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn shutdown_with_both_listeners_releases_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let mut both = config(dir.path());
        both.otlp_http_addr = Some(SocketAddr::from(([127, 0, 0, 1], 0)));
        both.grpc_addr = Some(SocketAddr::from(([127, 0, 0, 1], 0)));
        let running = bind(both, None).await.unwrap();
        assert!(running.otlp_local_addr.is_some());
        assert!(running.grpc_local_addr.is_some());
        running.shutdown().await.unwrap();

        let again = bind(config(dir.path()), None).await.unwrap();
        again.shutdown().await.unwrap();
    }

    fn config(data_dir: &Path) -> ServerConfig {
        ServerConfig {
            data_dir: data_dir.to_path_buf(),
            public_url: PublicUrl::default(),
            http_addr: SocketAddr::from(([127, 0, 0, 1], 0)),
            trusted_proxies: Vec::new(),
            otlp_http_addr: None,
            grpc_addr: None,
            storage: StorageConfig {
                data_dir: data_dir.to_path_buf(),
                min_free_bytes: ByteSize::b(0),
                object_store: None,
                s3: S3Config::default(),
                wal: WalConfig::default(),
                flush: FlushConfig::default(),
                memtable: MemtableConfig::default(),
                query: QueryConfig::default(),
                anchor: AnchorConfig::default(),
                gc: Default::default(),
                compaction: Default::default(),
                deletion: Default::default(),
            },
            ingest: IngestConfig::default(),
            media: MediaConfig::default(),
            eval: EvalConfig::default(),
            security: SecurityConfig::default(),
            entitlements: EntitlementsConfig::default(),
            encryption_key: None,
            license_key: None,
            auth: AuthSettings::default(),
            smtp_url: None,
            email_from: None,
            init: InitConfig::default(),
            log: LogConfig::default(),
        }
    }
}
