//! HTTP tests: a real `meta.db` and storage, a manual clock, `oneshot` (D151).
#![cfg(test)]

pub(crate) mod mock_idp;

use std::future::Future;
use std::io::{self, Write};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::AtomicI64;
use std::sync::{Arc, Mutex};

use tracing_subscriber::layer::SubscriberExt;

use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode};
use llmobs_core::ids::{OrgId, ProjectId};
use llmobs_core::names::PRODUCT;
use llmobs_storage::Storage;
use llmobs_storage::config::{
    AnchorConfig, FlushConfig, MemtableConfig, QueryConfig, S3Config, StorageConfig, WalConfig,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use sqlx::SqliteConnection;
use tempfile::TempDir;
use tower::ServiceExt;
use uuid::Uuid;

use crate::api::common::Problem;
use crate::auth::Limits;
use crate::auth::roles::{KeyScope, OrgRole};
use crate::auth::secret::{self, KeyKind, NewKey};
use crate::auth::session::start_session;
use crate::clock::Clock;
use crate::config::{
    AuthSettings, IngestConfig, InitConfig, LogConfig, MediaConfig, PublicUrl, ServerConfig,
};
use crate::http::access::RouteDef;
use crate::http::{AppInner, AppState, router, router_with, serve_router};
use crate::meta::api_keys::{self, NewApiKey};
use crate::meta::members::add_org_member;
use crate::meta::orgs::insert_org;
use crate::meta::projects::{ProjectRow, insert_project};
use crate::meta::users::{self, NewUser, insert_user};
use crate::meta::{MetaDb, MetaError};

/// `2026-01-01T00:00:00Z`.
const MANUAL_NOW_NS: i64 = 1_767_225_600_000_000_000;

/// One process's data directory and router.
pub(crate) struct TestApp {
    /// Kept so the directory outlives the open files.
    _dir: TempDir,
    pub(crate) state: AppState,
    router: Router,
}

impl TestApp {
    /// Defaults, `data_dir` in a temp directory, `public_url` `http://localhost:8080`.
    pub(crate) async fn new() -> Self {
        Self::open(|_| {}, None, None, false, None, None).await
    }

    /// `tweak` runs before storage and `meta.db` open.
    ///
    /// `storage.data_dir` is then copied from the top-level `data_dir`.
    pub(crate) async fn with_config(tweak: impl FnOnce(&mut ServerConfig)) -> Self {
        Self::open(tweak, None, None, false, None, None).await
    }

    /// Same as [`Self::new`] with extra routes inside the shared layers.
    pub(crate) async fn with_routes(routes: Router<AppState>) -> Self {
        Self::open(|_| {}, Some(routes), None, false, None, None).await
    }

    /// [`Self::with_config`] and [`Self::with_routes`] together.
    pub(crate) async fn with(
        tweak: impl FnOnce(&mut ServerConfig),
        routes: Router<AppState>,
    ) -> Self {
        Self::open(tweak, Some(routes), None, false, None, None).await
    }

    /// The given route table instead of the server's, with the same layers.
    pub(crate) async fn with_route_table(routes: Vec<RouteDef>) -> Self {
        Self::open(|_| {}, None, Some(routes), false, None, None).await
    }

    /// The stub mailer. [`Self::sent`] reads what it accepted.
    pub(crate) async fn with_mailer() -> Self {
        Self::open(|_| {}, None, None, true, None, None).await
    }

    /// [`Self::with_config`] with the stub mailer.
    pub(crate) async fn with_mailer_config(tweak: impl FnOnce(&mut ServerConfig)) -> Self {
        Self::open(tweak, None, None, true, None, None).await
    }

    /// [`Self::with_config`] with GitHub's endpoints pointed at `base`.
    pub(crate) async fn with_github(base: &str, tweak: impl FnOnce(&mut ServerConfig)) -> Self {
        Self::open(tweak, None, None, false, Some(base), None).await
    }

    /// [`Self::with_config`] with Google's issuer pointed at `issuer`.
    pub(crate) async fn with_google(issuer: &str, tweak: impl FnOnce(&mut ServerConfig)) -> Self {
        Self::open(tweak, None, None, false, None, Some(issuer)).await
    }

    /// Messages the stub mailer accepted. Empty when the stub is not installed.
    pub(crate) fn sent(&self) -> Vec<crate::email::SentMail> {
        self.state
            .mailer
            .as_ref()
            .map(|mailer| mailer.sent())
            .unwrap_or_default()
    }

    async fn open(
        tweak: impl FnOnce(&mut ServerConfig),
        routes: Option<Router<AppState>>,
        table: Option<Vec<RouteDef>>,
        stub_mailer: bool,
        github_base: Option<&str>,
        google_issuer: Option<&str>,
    ) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut config = default_config(dir.path());
        tweak(&mut config);
        config.storage.data_dir.clone_from(&config.data_dir);
        let storage = Storage::open(config.storage_config())
            .await
            .expect("storage opens");
        let meta = MetaDb::open(&config.data_dir).await.expect("meta.db opens");
        let mailer = if stub_mailer {
            let from = format!("{PRODUCT} <noreply@example.com>");
            Some(Arc::new(crate::email::Mailer::stub(&from)))
        } else {
            crate::email::Mailer::from_config(&config)
                .expect("smtp settings build a mailer")
                .map(Arc::new)
        };
        let mut providers = {
            let http = crate::auth::oidc::identity_client().expect("identity client");
            crate::auth::oidc::Providers::from_config(&config.auth, &http)
                .expect("identity providers")
        };
        if let Some(base) = github_base {
            providers.retarget_github(base);
        }
        if let Some(issuer) = google_issuer {
            providers.retarget_google(issuer);
        }
        let state = AppState::new(AppInner {
            config,
            meta,
            storage: Arc::new(storage),
            clock: Clock::Manual(Arc::new(AtomicI64::new(MANUAL_NOW_NS))),
            limits: Limits::new(),
            key_cache: crate::auth::principal::ApiKeyCache::new(),
            pat_cache: crate::auth::principal::PatCache::new(),
            last_used: crate::auth::last_used::LastUsed::new(),
            token_last_used: crate::auth::last_used::LastUsed::tokens(),
            setup: crate::bootstrap::SetupState::new(),
            password_hasher: tokio::sync::OnceCell::new(),
            mailer,
            providers,
            oauth_key: crate::auth::oidc::random_key().expect("oauth key"),
        });
        let router = if let Some(table) = table {
            serve_router(state.clone(), table)
        } else if let Some(routes) = routes {
            router_with(state.clone(), routes)
        } else {
            router(state.clone())
        };
        Self {
            _dir: dir,
            state,
            router,
        }
    }

    pub(crate) fn request(&self, method: Method, path: &str) -> TestRequest {
        TestRequest {
            router: self.router.clone(),
            origin_default: self.state.config.public_url.origin().to_owned(),
            method,
            path: path.to_owned(),
            headers: HeaderMap::new(),
            body: Bytes::new(),
            peer: SocketAddr::from(([127, 0, 0, 1], 50_000)),
            origin: OriginChoice::Default,
        }
    }
}

/// A user (`ada@example.com`, instance admin), an org `acme` they own, a
/// project `support-bot`, a session cookie, and one `sk` key per scope set.
pub(crate) struct Seeded {
    pub(crate) org_id: OrgId,
    pub(crate) project: ProjectRow,
    pub(crate) cookie: String,
    /// `sk-llmobs-…` with `["ingest"]`.
    pub(crate) ingest_key: String,
    /// `sk-llmobs-…` with `["read"]`.
    pub(crate) read_key: String,
    /// `pk-llmobs-…` with `["feedback"]`.
    pub(crate) public_key: String,
}

impl TestApp {
    /// Writes the [`Seeded`] rows directly, without HTTP.
    pub(crate) async fn seed(&self) -> Seeded {
        let now = self.state.clock.now_ns();
        let ingest = secret::new_key(KeyKind::Secret).unwrap();
        let read = secret::new_key(KeyKind::Secret).unwrap();
        let public = secret::new_key(KeyKind::Public).unwrap();
        let ingest_key = ingest.plaintext.clone();
        let read_key = read.plaintext.clone();
        let public_key = public.plaintext.clone();
        let (user_id, org_id, project) = self
            .state
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
                    let scoped = [
                        (ingest, "secret", vec![KeyScope::Ingest]),
                        (read, "secret", vec![KeyScope::Read]),
                        (public, "public", vec![KeyScope::Feedback]),
                    ];
                    for (key, kind, scopes) in scoped {
                        insert_key(conn, project.id, key, kind, scopes, user.id, now).await?;
                    }
                    Ok((user.id, org.id, project))
                })
            })
            .await
            .unwrap();
        let cookie = session_cookie(self, user_id).await;
        Seeded {
            org_id,
            project,
            cookie,
            ingest_key,
            read_key,
            public_key,
        }
    }

    /// Another project in `seeded`'s org, with optional retention, and one `sk`
    /// key with `["ingest"]` for it. The key is returned as its plaintext.
    pub(crate) async fn add_project(
        &self,
        seeded: &Seeded,
        slug: &str,
        retention_days: Option<u32>,
    ) -> (ProjectRow, String) {
        let now = self.state.clock.now_ns();
        let ingest = secret::new_key(KeyKind::Secret).unwrap();
        let plaintext = ingest.plaintext.clone();
        let org_id = seeded.org_id;
        let slug = slug.to_owned();
        let user = users::user_by_email(self.state.meta.reader(), "ada@example.com")
            .await
            .unwrap()
            .expect("seeded user exists");
        let user_id = user.id;
        let project = self
            .state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    let project =
                        insert_project(conn, org_id, &slug, &slug, retention_days, now).await?;
                    insert_key(
                        conn,
                        project.id,
                        ingest,
                        "secret",
                        vec![KeyScope::Ingest],
                        user_id,
                        now,
                    )
                    .await?;
                    Ok(project)
                })
            })
            .await
            .unwrap();
        (project, plaintext)
    }
}

async fn insert_key(
    conn: &mut SqliteConnection,
    project_id: ProjectId,
    key: NewKey,
    kind: &'static str,
    scopes: Vec<KeyScope>,
    created_by: Uuid,
    now: i64,
) -> Result<(), MetaError> {
    api_keys::insert_api_key(
        conn,
        NewApiKey {
            project_id,
            kind,
            scopes,
            name: None,
            prefix: key.prefix,
            hash: key.hash,
            created_by,
            expires_at: None,
        },
        now,
    )
    .await?;
    Ok(())
}

async fn session_cookie(app: &TestApp, user_id: Uuid) -> String {
    let header = start_session(&app.state, user_id, None, None)
        .await
        .unwrap();
    header
        .to_str()
        .unwrap()
        .split_once(';')
        .unwrap()
        .0
        .to_owned()
}

fn default_config(data_dir: &std::path::Path) -> ServerConfig {
    ServerConfig {
        data_dir: data_dir.to_path_buf(),
        public_url: PublicUrl::default(),
        http_addr: SocketAddr::from(([0, 0, 0, 0], 8080)),
        trusted_proxies: Vec::new(),
        // Tests must not bind the OTLP port (4318).
        otlp_http_addr: None,
        storage: StorageConfig {
            data_dir: data_dir.to_path_buf(),
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
        auth: AuthSettings::default(),
        smtp_url: None,
        email_from: None,
        init: InitConfig::default(),
        log: LogConfig::default(),
    }
}

enum OriginChoice {
    Default,
    Override(Option<String>),
}

/// A request against [`TestApp`]. `Origin` defaults to the public origin on
/// methods other than `GET` and `HEAD`.
pub(crate) struct TestRequest {
    router: Router,
    origin_default: String,
    method: Method,
    path: String,
    headers: HeaderMap,
    body: Bytes,
    peer: SocketAddr,
    origin: OriginChoice,
}

impl TestRequest {
    pub(crate) fn json(mut self, value: &impl Serialize) -> Self {
        self.body = Bytes::from(serde_json::to_vec(value).unwrap());
        self.headers.insert(
            axum::http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        self
    }

    pub(crate) fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.insert(
            HeaderName::from_bytes(name.as_bytes()).unwrap(),
            HeaderValue::from_str(value).unwrap(),
        );
        self
    }

    pub(crate) fn body(mut self, body: impl Into<Bytes>) -> Self {
        self.body = body.into();
        self
    }

    pub(crate) fn peer(mut self, ip: IpAddr) -> Self {
        self.peer.set_ip(ip);
        self
    }

    pub(crate) fn origin(mut self, origin: Option<&str>) -> Self {
        self.origin = OriginChoice::Override(origin.map(str::to_owned));
        self
    }

    pub(crate) async fn send(self) -> TestResponse {
        let mut builder = Request::builder()
            .method(self.method.clone())
            .uri(&self.path);
        if let Some(origin) = self.origin_value() {
            builder = builder.header(axum::http::header::ORIGIN, origin);
        }
        for (name, value) in &self.headers {
            builder = builder.header(name, value);
        }
        let mut request = builder.body(Body::from(self.body)).unwrap();
        request.extensions_mut().insert(ConnectInfo(self.peer));
        let response = self.router.oneshot(request).await.unwrap();
        let (parts, body) = response.into_parts();
        let body = axum::body::to_bytes(body, 8 * 1024 * 1024)
            .await
            .expect("response body");
        TestResponse {
            status: parts.status,
            headers: parts.headers,
            body,
        }
    }

    fn origin_value(&self) -> Option<String> {
        match &self.origin {
            OriginChoice::Override(value) => value.clone(),
            OriginChoice::Default if self.method == Method::GET || self.method == Method::HEAD => {
                None
            }
            OriginChoice::Default => Some(self.origin_default.clone()),
        }
    }
}

/// Status, headers, and the raw body of one response.
pub(crate) struct TestResponse {
    pub(crate) status: StatusCode,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Bytes,
}

impl TestResponse {
    pub(crate) fn json<T: DeserializeOwned>(&self) -> T {
        serde_json::from_slice(&self.body).unwrap_or_else(|error| {
            panic!(
                "json body: {error}: {}",
                String::from_utf8_lossy(&self.body)
            )
        })
    }

    pub(crate) fn problem(&self) -> Problem {
        self.json()
    }

    /// The header value, when it is a single valid string.
    pub(crate) fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }
}

/// Runs `future` with a capturing subscriber and fails if any needle appears.
///
/// The subscriber stays the thread default until `future` finishes, so a task
/// spawned on this thread is captured too. HTTP tests use the current-thread
/// runtime.
pub(crate) async fn assert_logs_clean<T>(needles: &[&str], future: impl Future<Output = T>) -> T {
    let (value, logs) = capture_logs(future).await;
    for needle in needles {
        assert!(!logs.contains(needle), "log contains {needle}: {logs}");
    }
    value
}

/// The formatted tracing output produced while `future` is polled.
pub(crate) async fn capture_logs<T>(future: impl Future<Output = T>) -> (T, String) {
    let buffer = Arc::new(Mutex::new(Vec::<u8>::new()));
    let writer = Arc::clone(&buffer);
    let layer = tracing_subscriber::fmt::layer()
        .with_writer(move || LogBuffer(Arc::clone(&writer)))
        .with_ansi(false)
        .with_target(true);
    let dispatch = tracing::Dispatch::new(tracing_subscriber::registry().with(layer));
    let value = {
        let _guard = tracing::dispatcher::set_default(&dispatch);
        future.await
    };
    drop(dispatch);
    let bytes = buffer.lock().expect("log buffer").clone();
    let logs = String::from_utf8_lossy(&bytes).into_owned();
    (value, logs)
}

/// `io::Write` over the shared log buffer. The layer clones the `Arc` per event.
struct LogBuffer(Arc<Mutex<Vec<u8>>>);

impl Write for LogBuffer {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().expect("log buffer").write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
