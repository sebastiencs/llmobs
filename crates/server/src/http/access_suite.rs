//! Every documented route against every principal (D138–D140).
//!
//! [`expected`] predicts the header. It does not call [`super::enforce`].
#![cfg(test)]

use std::net::{IpAddr, Ipv4Addr};

use axum::http::{Method, header};
use llmobs_core::ids::OrgId;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{Access, RouteDef, routes};
use crate::api::account::{OrgRole, SignupPolicy};
use crate::auth::roles::{self, KeyScope, ProjectPerm, Role};
use crate::meta::MetaError;
use crate::meta::members::add_org_member;
use crate::meta::users::set_instance_admin;
use crate::test_support::{TestApp, TestResponse, capture_logs};

/// 43 characters of the base64url alphabet. No route uses `{token}` yet.
const TOKEN: &str = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
/// A provider id of the shape OAuth routes will take. No route uses it yet.
const PROVIDER: &str = "google";

const ROOT: Account = Account::new("root@example.com", "p18-root-password", "Root");
const ADA: Account = Account::new("ada@example.com", "p18-ada-password", "Ada");
const ADMIN: Account = Account::new("admin@example.com", "p18-admin-password", "Admin");
const MEMBER: Account = Account::new("member@example.com", "p18-member-password", "Member");
const VIEWER: Account = Account::new("viewer@example.com", "p18-viewer-password", "Viewer");
const BOB: Account = Account::new("bob@example.com", "p18-bob-password", "Bob");

#[derive(Clone, Copy)]
struct Account {
    email: &'static str,
    password: &'static str,
    name: &'static str,
}

impl Account {
    const fn new(email: &'static str, password: &'static str, name: &'static str) -> Self {
        Self {
            email,
            password,
            name,
        }
    }
}

struct SignedUp {
    cookie: String,
    id: Uuid,
    account: Account,
}

/// One caller in the matrix. This is the input to [`expected`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Who {
    Anonymous,
    Root,
    Ada,
    OrgAdmin,
    Member,
    Viewer,
    Bob,
    PatRead,
    PatWrite,
    KeyRead,
    KeyIngest,
    KeyPublic,
    KeyForeign,
}

struct Caller {
    who: Who,
    cookie: Option<String>,
    bearer: Option<String>,
    account: Option<Account>,
}

struct World {
    org_id: OrgId,
    org_slug: String,
    project_a: String,
    member_id: Uuid,
    /// Sacrificed by the first allowed API-key revoke. Not a principal.
    key_id: String,
    /// Sacrificed by the first allowed token revoke. Not a principal.
    token_id: String,
    pat_read: String,
    key_foreign: String,
    needles: Vec<String>,
    callers: Vec<Caller>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Outcome {
    Allowed,
    Denied401,
    Denied403,
    Denied404,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::Denied401 => "denied-401",
            Self::Denied403 => "denied-403",
            Self::Denied404 => "denied-404",
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn every_route_against_every_principal() {
    let ((needles, checked), logs) = capture_logs(suite()).await;
    for needle in &needles {
        assert!(!logs.contains(needle), "log contains {needle}: {logs}");
    }
    assert!(checked > 0, "the suite checked no pairs");
}

async fn suite() -> (Vec<String>, usize) {
    let app = TestApp::with_config(|config| config.auth.signup = SignupPolicy::Open).await;
    let mut world = build(&app).await;
    assert_lists_omit_secrets(&app, &world).await;
    let checked = drive(&app, &mut world).await;
    assert_query_credentials_are_ignored(&app, &world).await;
    assert_every_route_declares_access();
    (world.needles, checked)
}

async fn build(app: &TestApp) -> World {
    let mut needles = emails_and_passwords();
    let root = signup(app, &ROOT, 1).await;
    let ada = signup(app, &ADA, 2).await;
    let admin = signup(app, &ADMIN, 3).await;
    let member = signup(app, &MEMBER, 4).await;
    let viewer = signup(app, &VIEWER, 5).await;
    let bob = signup(app, &BOB, 6).await;

    let (org_id, org_slug) = create_org(app, &ada.cookie, "Org A", "org-a").await;
    let project_a = create_project(app, &ada.cookie, &org_slug, "Project A1", "a1").await;
    let org_b = create_org(app, &bob.cookie, "Org B", "org-b").await;
    let project_b = create_project(app, &bob.cookie, &org_b.1, "Project B1", "b1").await;

    let now = app.state.clock.now_ns();
    let org = org_id;
    let admin_id = admin.id;
    let member_id = member.id;
    let viewer_id = viewer.id;
    let root_id = root.id;
    app.state
        .meta
        .write(move |conn| {
            Box::pin(async move {
                add_org_member(conn, org, admin_id, OrgRole::Admin, now).await?;
                add_org_member(conn, org, member_id, OrgRole::Member, now).await?;
                add_org_member(conn, org, viewer_id, OrgRole::Viewer, now).await?;
                set_instance_admin(conn, root_id, true, now).await?;
                Ok(())
            })
        })
        .await
        .unwrap();

    put_override(app, &ada.cookie, &project_a, member.id, "none").await;
    put_override(app, &ada.cookie, &project_a, viewer.id, "admin").await;

    let (_, pat_read) = create_token(app, &member.cookie, "read", &["read"]).await;
    let (_, pat_write) = create_token(app, &member.cookie, "write", &["read", "write"]).await;
    let (token_id, token_extra) = create_token(app, &member.cookie, "extra", &["read"]).await;
    let (_, key_read) = create_key(app, &ada.cookie, &project_a, "secret", &["read"], "read").await;
    let (_, key_ingest) = create_key(
        app,
        &ada.cookie,
        &project_a,
        "secret",
        &["ingest"],
        "ingest",
    )
    .await;
    let (_, key_public) = create_key(app, &ada.cookie, &project_a, "public", &[], "public").await;
    let (key_id, key_extra) =
        create_key(app, &ada.cookie, &project_a, "secret", &["read"], "extra").await;
    let (_, key_foreign) =
        create_key(app, &bob.cookie, &project_b, "secret", &["read"], "foreign").await;
    for secret in [
        pat_read.clone(),
        pat_write.clone(),
        token_extra,
        key_read.clone(),
        key_ingest.clone(),
        key_public.clone(),
        key_extra,
        key_foreign.clone(),
    ] {
        needles.push(secret);
    }

    let me = send(
        app,
        Method::GET,
        "/api/auth/me",
        Some(&root.cookie),
        None,
        false,
    )
    .await;
    assert_eq!(me.status, 200, "{}", body_text(&me));
    let me: Value = me.json();
    assert_eq!(me["user"]["is_instance_admin"], true);
    assert_eq!(me["orgs"].as_array().map(Vec::len), Some(0));

    World {
        org_id,
        org_slug,
        project_a,
        member_id,
        key_id,
        token_id,
        pat_read: pat_read.clone(),
        key_foreign: key_foreign.clone(),
        needles,
        callers: vec![
            Caller {
                who: Who::Anonymous,
                cookie: None,
                bearer: None,
                account: None,
            },
            session(Who::Root, &root),
            session(Who::Ada, &ada),
            session(Who::OrgAdmin, &admin),
            session(Who::Member, &member),
            session(Who::Viewer, &viewer),
            session(Who::Bob, &bob),
            bearer(Who::PatRead, pat_read),
            bearer(Who::PatWrite, pat_write),
            bearer(Who::KeyRead, key_read),
            bearer(Who::KeyIngest, key_ingest),
            bearer(Who::KeyPublic, key_public),
            bearer(Who::KeyForeign, key_foreign),
        ],
    }
}

fn session(who: Who, signed: &SignedUp) -> Caller {
    Caller {
        who,
        cookie: Some(signed.cookie.clone()),
        bearer: None,
        account: Some(signed.account),
    }
}

fn bearer(who: Who, secret: String) -> Caller {
    Caller {
        who,
        cookie: None,
        bearer: Some(secret),
        account: None,
    }
}

fn emails_and_passwords() -> Vec<String> {
    [ROOT, ADA, ADMIN, MEMBER, VIEWER, BOB]
        .into_iter()
        .flat_map(|account| [account.email.to_owned(), account.password.to_owned()])
        .collect()
}

async fn drive(app: &TestApp, world: &mut World) -> usize {
    let routes = documented_routes();
    let mut checked = 0;
    for route in routes {
        let path = concrete_path(&route, world);
        for index in 0..world.callers.len() {
            let member_cookie = cookie_of(&world.callers, Who::Member);
            let ada_cookie = cookie_of(&world.callers, Who::Ada);
            let response = call(app, &route, &path, &world.callers[index]).await;
            let got = response.header("x-test-access");
            let want = expected(route.access, &route.method, world.callers[index].who);
            assert_eq!(
                got,
                Some(want.as_str()),
                "{} {} as {:?}: header {got:?}, expected {}",
                route.method,
                route.path,
                world.callers[index].who,
                want.as_str(),
            );
            if matches!(
                world.callers[index].who,
                Who::Bob | Who::Root | Who::KeyForeign
            ) && matches!(route.access, Access::Org { .. } | Access::Project { .. })
            {
                assert_eq!(
                    got,
                    Some("denied-404"),
                    "cross-tenant {} {} as {:?}",
                    route.method,
                    route.path,
                    world.callers[index].who,
                );
            }
            checked += 1;
            if route.path == "/api/auth/logout"
                && let Some(account) = world.callers[index].account
            {
                let cookie = login(app, &account).await;
                world.callers[index].cookie = Some(cookie);
            }
            if route.method == Method::DELETE
                && route.path == "/api/v1/orgs/{org}/members/{user}"
                && got == Some("allowed")
            {
                restore_member(app, world, &ada_cookie).await;
            }
            // Deleting the override drops `none`, so the member could see A1.
            if route.method == Method::DELETE
                && route.path == "/api/v1/projects/{project}/members/{user}"
                && got == Some("allowed")
            {
                put_override(app, &ada_cookie, &world.project_a, world.member_id, "none").await;
            }
            if route.path == "/api/v1/me/tokens/current"
                && world.callers[index].who == Who::PatWrite
                && got == Some("allowed")
            {
                let (_, secret) =
                    create_token(app, &member_cookie, "write", &["read", "write"]).await;
                world.needles.push(secret.clone());
                world.callers[index].bearer = Some(secret);
            }
        }
    }
    checked
}

fn cookie_of(callers: &[Caller], who: Who) -> String {
    callers
        .iter()
        .find(|caller| caller.who == who)
        .and_then(|caller| caller.cookie.clone())
        .unwrap()
}

fn documented_routes() -> Vec<RouteDef> {
    routes()
        .into_iter()
        .filter(|route| route.documented)
        .collect()
}

fn concrete_path(route: &RouteDef, world: &World) -> String {
    let id = if route.path.contains("api-keys") {
        world.key_id.as_str()
    } else if route.path.contains("tokens/{id}") {
        world.token_id.as_str()
    } else {
        "01900000-0000-7000-8000-000000009999"
    };
    route
        .path
        .replace("{org}", &world.org_slug)
        .replace("{project}", &world.project_a)
        .replace("{user}", &world.member_id.to_string())
        .replace("{id}", id)
        .replace("{token}", TOKEN)
        .replace("{provider}", PROVIDER)
}

async fn call(app: &TestApp, route: &RouteDef, path: &str, caller: &Caller) -> TestResponse {
    let body = route.method != Method::GET && route.method != Method::HEAD;
    send(
        app,
        route.method.clone(),
        path,
        caller.cookie.as_deref(),
        caller.bearer.as_deref(),
        body,
    )
    .await
}

async fn send(
    app: &TestApp,
    method: Method,
    path: &str,
    cookie: Option<&str>,
    bearer: Option<&str>,
    body: bool,
) -> TestResponse {
    let mut request = app.request(method, path);
    if body {
        request = request.json(&json!({}));
    }
    if let Some(cookie) = cookie {
        request = request.header(header::COOKIE.as_str(), cookie);
    }
    if let Some(bearer) = bearer {
        request = request.header(header::AUTHORIZATION.as_str(), &format!("Bearer {bearer}"));
    }
    request.send().await
}

/// A read PAT is denied on writes before the resource is resolved. A key that
/// does not own the org or project is 404, including on session-only routes.
/// On its own project, a missing scope or a session-only route is 403.
fn expected(access: Access, method: &Method, who: Who) -> Outcome {
    if matches!(access, Access::Public) {
        return Outcome::Allowed;
    }
    if who == Who::Anonymous {
        return Outcome::Denied401;
    }
    if who == Who::PatRead && !safe(method) {
        return Outcome::Denied403;
    }
    match access {
        Access::Public => Outcome::Allowed,
        Access::User { session_only } => {
            if is_key(who) || (session_only && !is_session(who)) {
                Outcome::Denied403
            } else {
                Outcome::Allowed
            }
        }
        Access::InstanceAdmin => {
            if who == Who::Root {
                Outcome::Allowed
            } else {
                Outcome::Denied403
            }
        }
        Access::Org { perm, session_only } => {
            if is_key(who) {
                return Outcome::Denied404;
            }
            if session_only && !is_session(who) {
                return Outcome::Denied403;
            }
            match org_membership(who) {
                None => Outcome::Denied404,
                Some(role) if roles::org_rank(role) < roles::org_rank(perm.min_role()) => {
                    Outcome::Denied403
                }
                Some(_) => Outcome::Allowed,
            }
        }
        Access::Project { perm, session_only } => {
            if is_key(who) {
                if !key_on_a1(who) {
                    return Outcome::Denied404;
                }
                if session_only {
                    return Outcome::Denied403;
                }
                return key_outcome(who, perm, method);
            }
            if session_only && !is_session(who) {
                return Outcome::Denied403;
            }
            match project_role(who) {
                None => Outcome::Denied404,
                Some(role) if role < perm.min_role() => Outcome::Denied403,
                Some(_) => Outcome::Allowed,
            }
        }
    }
}

fn org_membership(who: Who) -> Option<OrgRole> {
    match who {
        Who::Ada => Some(OrgRole::Owner),
        Who::OrgAdmin => Some(OrgRole::Admin),
        Who::Member | Who::PatRead | Who::PatWrite => Some(OrgRole::Member),
        Who::Viewer => Some(OrgRole::Viewer),
        Who::Anonymous
        | Who::Root
        | Who::Bob
        | Who::KeyRead
        | Who::KeyIngest
        | Who::KeyPublic
        | Who::KeyForeign => None,
    }
}

/// Effective role on project A1. The member's override is `none`. The viewer's is `admin`.
fn project_role(who: Who) -> Option<Role> {
    match who {
        Who::Ada | Who::OrgAdmin | Who::Viewer => Some(Role::Admin),
        Who::Anonymous
        | Who::Root
        | Who::Member
        | Who::Bob
        | Who::PatRead
        | Who::PatWrite
        | Who::KeyRead
        | Who::KeyIngest
        | Who::KeyPublic
        | Who::KeyForeign => None,
    }
}

fn key_outcome(who: Who, perm: ProjectPerm, method: &Method) -> Outcome {
    let Some(scope) = perm.key_scope() else {
        return Outcome::Denied403;
    };
    if !holds_scope(who, scope) {
        return Outcome::Denied403;
    }
    if scope == KeyScope::Read && !safe(method) {
        Outcome::Denied403
    } else {
        Outcome::Allowed
    }
}

fn holds_scope(who: Who, scope: KeyScope) -> bool {
    matches!(
        (who, scope),
        (Who::KeyRead, KeyScope::Read)
            | (Who::KeyIngest, KeyScope::Ingest)
            | (Who::KeyPublic, KeyScope::Feedback)
    )
}

fn is_session(who: Who) -> bool {
    matches!(
        who,
        Who::Root | Who::Ada | Who::OrgAdmin | Who::Member | Who::Viewer | Who::Bob
    )
}

fn is_key(who: Who) -> bool {
    matches!(
        who,
        Who::KeyRead | Who::KeyIngest | Who::KeyPublic | Who::KeyForeign
    )
}

fn key_on_a1(who: Who) -> bool {
    matches!(who, Who::KeyRead | Who::KeyIngest | Who::KeyPublic)
}

fn safe(method: &Method) -> bool {
    method == Method::GET || method == Method::HEAD
}

fn assert_every_route_declares_access() {
    for route in routes() {
        match route.access {
            Access::Public
            | Access::User { .. }
            | Access::InstanceAdmin
            | Access::Org { .. }
            | Access::Project { .. } => {}
        }
        if route.path.contains("{project}") {
            assert!(
                matches!(route.access, Access::Project { .. }),
                "{} {} has a project parameter but is not project access",
                route.method,
                route.path
            );
        }
    }
}

async fn assert_lists_omit_secrets(app: &TestApp, world: &World) {
    let ada = cookie_of(&world.callers, Who::Ada);
    for route in documented_routes() {
        if !is_list(&route) {
            continue;
        }
        let path = concrete_path(&route, world);
        let response = send(app, Method::GET, &path, Some(&ada), None, false).await;
        assert_eq!(
            response.status,
            200,
            "list {} {}",
            path,
            body_text(&response)
        );
        assert_eq!(response.header("x-test-access"), Some("allowed"));
        let text = body_text(&response);
        for needle in &world.needles {
            if needle.contains("@") || needle.starts_with("p18-") {
                continue;
            }
            assert!(!text.contains(needle), "list {path} contains a secret");
        }
        let value: Value = response.json();
        assert!(
            !json_key_ends_with_hash(&value),
            "list {path} has a hash field"
        );
    }
}

fn is_list(route: &RouteDef) -> bool {
    route.method == Method::GET
        && matches!(
            route.path.rsplit('/').next(),
            Some("orgs" | "projects" | "members" | "api-keys" | "tokens")
        )
}

fn json_key_ends_with_hash(value: &Value) -> bool {
    match value {
        Value::Object(map) => map
            .iter()
            .any(|(key, child)| key.ends_with("hash") || json_key_ends_with_hash(child)),
        Value::Array(items) => items.iter().any(json_key_ends_with_hash),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => false,
    }
}

async fn assert_query_credentials_are_ignored(app: &TestApp, world: &World) {
    for (name, secret) in [("token", &world.pat_read), ("api_key", &world.key_foreign)] {
        let path = format!("/api/auth/me?{name}={secret}");
        let response = send(app, Method::GET, &path, None, None, false).await;
        assert_eq!(response.status, 401, "{name} query");
        assert_eq!(response.header("x-test-access"), Some("denied-401"));
    }
}

async fn restore_member(app: &TestApp, world: &World, ada_cookie: &str) {
    let now = app.state.clock.now_ns();
    let org_id = world.org_id;
    let user_id = world.member_id;
    app.state
        .meta
        .write(move |conn| {
            Box::pin(async move {
                match add_org_member(conn, org_id, user_id, OrgRole::Member, now).await {
                    Ok(()) | Err(MetaError::Conflict(_)) => Ok(()),
                    Err(error) => Err(error),
                }
            })
        })
        .await
        .unwrap();
    put_override(app, ada_cookie, &world.project_a, world.member_id, "none").await;
}

async fn signup(app: &TestApp, account: &Account, host: u8) -> SignedUp {
    let response = app
        .request(Method::POST, "/api/auth/signup")
        .peer(IpAddr::V4(Ipv4Addr::new(127, 0, 0, host)))
        .json(&json!({
            "email": account.email,
            "password": account.password,
            "name": account.name,
        }))
        .send()
        .await;
    assert_eq!(response.status, 201, "{}", body_text(&response));
    let body: Value = response.json();
    SignedUp {
        cookie: cookie_pair(&response),
        id: body["me"]["user"]["id"].as_str().unwrap().parse().unwrap(),
        account: *account,
    }
}

async fn login(app: &TestApp, account: &Account) -> String {
    let response = app
        .request(Method::POST, "/api/auth/login")
        .json(&json!({
            "email": account.email,
            "password": account.password,
        }))
        .send()
        .await;
    assert_eq!(response.status, 200, "{}", body_text(&response));
    cookie_pair(&response)
}

fn cookie_pair(response: &TestResponse) -> String {
    response
        .header(header::SET_COOKIE.as_str())
        .unwrap()
        .split_once(';')
        .unwrap()
        .0
        .to_owned()
}

async fn create_org(app: &TestApp, cookie: &str, name: &str, slug: &str) -> (OrgId, String) {
    let response = app
        .request(Method::POST, "/api/v1/orgs")
        .header(header::COOKIE.as_str(), cookie)
        .json(&json!({"name": name, "slug": slug}))
        .send()
        .await;
    assert_eq!(response.status, 201, "{}", body_text(&response));
    let body: Value = response.json();
    let id = body["id"].as_str().unwrap().parse().unwrap();
    let slug = body["slug"].as_str().unwrap().to_owned();
    (id, slug)
}

async fn create_project(
    app: &TestApp,
    cookie: &str,
    org_slug: &str,
    name: &str,
    slug: &str,
) -> String {
    let path = format!("/api/v1/orgs/{org_slug}/projects");
    let response = app
        .request(Method::POST, &path)
        .header(header::COOKIE.as_str(), cookie)
        .json(&json!({"name": name, "slug": slug}))
        .send()
        .await;
    assert_eq!(response.status, 201, "{}", body_text(&response));
    let body: Value = response.json();
    body["id"].as_str().unwrap().to_owned()
}

async fn put_override(app: &TestApp, cookie: &str, project: &str, user: Uuid, role: &str) {
    let path = format!("/api/v1/projects/{project}/members/{user}");
    let response = app
        .request(Method::PUT, &path)
        .header(header::COOKIE.as_str(), cookie)
        .json(&json!({"role": role}))
        .send()
        .await;
    assert_eq!(response.status, 200, "{}", body_text(&response));
}

async fn create_token(
    app: &TestApp,
    cookie: &str,
    name: &str,
    scopes: &[&str],
) -> (String, String) {
    let response = app
        .request(Method::POST, "/api/v1/me/tokens")
        .header(header::COOKIE.as_str(), cookie)
        .json(&json!({"name": name, "scopes": scopes}))
        .send()
        .await;
    assert_eq!(response.status, 201, "{}", body_text(&response));
    let body: Value = response.json();
    (
        body["token"]["id"].as_str().unwrap().to_owned(),
        body["secret"].as_str().unwrap().to_owned(),
    )
}

async fn create_key(
    app: &TestApp,
    cookie: &str,
    project: &str,
    kind: &str,
    scopes: &[&str],
    name: &str,
) -> (String, String) {
    let path = format!("/api/v1/projects/{project}/api-keys");
    let body = if scopes.is_empty() {
        json!({"kind": kind, "name": name})
    } else {
        json!({"kind": kind, "name": name, "scopes": scopes})
    };
    let response = app
        .request(Method::POST, &path)
        .header(header::COOKIE.as_str(), cookie)
        .json(&body)
        .send()
        .await;
    assert_eq!(response.status, 201, "{}", body_text(&response));
    let body: Value = response.json();
    (
        body["key"]["id"].as_str().unwrap().to_owned(),
        body["secret"].as_str().unwrap().to_owned(),
    )
}

fn body_text(response: &TestResponse) -> String {
    String::from_utf8_lossy(&response.body).into_owned()
}
