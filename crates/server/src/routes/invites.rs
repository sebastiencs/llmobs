//! Org invitations: create, list, delete, preview, and accept (spec 08, D147).
#![cfg_attr(not(test), allow(dead_code))]

use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use serde::Deserialize;
use uuid::Uuid;

use crate::api::account::OrgMembership;
use crate::api::common::{Page, PageParams, Problem, ProblemCode};
use crate::api::invites::{CreateInviteRequest, CreatedInvite, Invite, InviteOrg, InvitePreview};
use crate::api::members::MemberUser;
use crate::auth::principal::{self, Principal};
use crate::auth::roles::{self, OrgPerm, OrgRole};
use crate::auth::secret::{hash_token, is_opaque_token, new_opaque_token};
use crate::email::Email;
use crate::http::AppState;
use crate::http::access::{Access, OrgAccess, RouteDef};
use crate::http::client_ip::ClientIp;
use crate::http::error::{self, ApiError};
use crate::meta::MetaError;
use crate::meta::invites::{self, INVITE_TTL_NS, InviteRow, Inviter};
use crate::meta::members;
use crate::meta::users::{self, normalize_email};

use super::orgs::page_named;

/// Invitation routes. Accept and preview sit under `/api/auth/` so CSRF applies
/// to accept (D141).
pub(crate) fn routes() -> Vec<RouteDef> {
    vec![
        RouteDef {
            method: axum::http::Method::GET,
            path: "/api/v1/orgs/{org}/invites",
            access: Access::Org {
                perm: OrgPerm::ManageMembers,
                session_only: false,
            },
            documented: true,
            handler: get(list_invites),
        },
        RouteDef {
            method: axum::http::Method::POST,
            path: "/api/v1/orgs/{org}/invites",
            access: Access::Org {
                perm: OrgPerm::ManageMembers,
                session_only: true,
            },
            documented: true,
            handler: post(create_invite),
        },
        RouteDef {
            method: axum::http::Method::DELETE,
            path: "/api/v1/orgs/{org}/invites/{id}",
            access: Access::Org {
                perm: OrgPerm::ManageMembers,
                session_only: true,
            },
            documented: true,
            handler: delete(delete_invite),
        },
        RouteDef {
            method: axum::http::Method::GET,
            path: "/api/auth/invites/{token}",
            access: Access::Public,
            documented: true,
            handler: get(preview_invite),
        },
        RouteDef {
            method: axum::http::Method::POST,
            path: "/api/auth/invites/{token}/accept",
            access: Access::User { session_only: true },
            documented: true,
            handler: post(accept_invite),
        },
    ]
}

#[derive(Deserialize)]
struct IdPath {
    id: Uuid,
}

#[derive(Deserialize)]
struct TokenPath {
    token: String,
}

enum Created {
    Invite(InviteRow),
    Forbidden,
}

enum Accepted {
    Done {
        membership: OrgMembership,
        invite_id: Uuid,
    },
    Missing,
    WrongEmail,
}

async fn list_invites(
    State(state): State<AppState>,
    Extension(access): Extension<OrgAccess>,
    error::Query(params): error::Query<PageParams>,
) -> Result<error::Json<Page<Invite>>, ApiError> {
    let pending =
        invites::pending_invites(state.meta.reader(), access.org.id, state.clock.now_ns()).await?;
    let rows = pending
        .into_iter()
        .map(|item| {
            let email = item.invite.email.clone();
            let id = item.invite.id.to_string();
            (email, id, invite_json(item.invite, item.invited_by))
        })
        .collect();
    Ok(error::Json(page_named(
        rows,
        params.cursor.as_deref(),
        params.limit.get(),
    )?))
}

async fn create_invite(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    Extension(access): Extension<OrgAccess>,
    error::Json(body): error::Json<CreateInviteRequest>,
) -> Result<(StatusCode, error::Json<CreatedInvite>), ApiError> {
    let user = principal::session_user(&principal)?;
    let email = normalize_email(&body.email).map_err(|_| field_error("email", "invalid email"))?;
    let role = body.role;
    let actor = user.id;
    let org_id = access.org.id;
    let org_name = access.org.name.clone();
    let minted = new_opaque_token().map_err(|error| ApiError::Internal(Box::new(error)))?;
    let now = state.clock.now_ns();
    let expires_at = now
        .checked_add(INVITE_TTL_NS)
        .ok_or_else(|| ApiError::Internal(Box::new(ExpiryOverflow)))?;
    let hash = minted.hash.clone();
    let email_for_db = email.clone();
    let inviter = users::user_by_id(state.meta.reader(), actor).await?;
    let invited_by = inviter.as_ref().map(member_user);
    let inviter_name = inviter.and_then(|user| user.name);
    let created = state
        .meta
        .write(move |conn| {
            Box::pin(async move {
                let Some(actor_role) = members::org_role_tx(conn, org_id, actor).await? else {
                    return Ok(Created::Forbidden);
                };
                if !may_grant(actor_role, role) {
                    return Ok(Created::Forbidden);
                }
                let existing: Option<String> = sqlx::query_scalar(
                    "SELECT users.id FROM users \
                     JOIN org_members ON org_members.user_id = users.id \
                     WHERE users.email = ? AND org_members.org_id = ?",
                )
                .bind(&email_for_db)
                .bind(org_id.to_string())
                .fetch_optional(&mut *conn)
                .await?;
                if existing.is_some() {
                    return Err(MetaError::Conflict("already a member"));
                }
                let row = invites::replace_invite(
                    conn,
                    org_id,
                    &email_for_db,
                    role,
                    &hash,
                    actor,
                    now,
                    expires_at,
                )
                .await?;
                Ok(Created::Invite(row))
            })
        })
        .await?;
    let Created::Invite(row) = created else {
        return Err(ApiError::Forbidden {
            code: ProblemCode::Forbidden,
        });
    };
    let url = state
        .config
        .public_url
        .join(&format!("/invite/{}", minted.plaintext));
    tracing::info!(
        target: "audit",
        org_id = %org_id,
        actor_id = %actor,
        invite_id = %row.id,
        "invite created"
    );
    if let Some(mailer) = &state.mailer {
        mailer.send_in_background(
            email,
            Email::Invite {
                org_name,
                inviter_name,
                role,
                url: url.clone(),
            },
        );
    }
    Ok((
        StatusCode::CREATED,
        error::Json(CreatedInvite {
            invite: invite_json(row, invited_by),
            accept_url: url,
        }),
    ))
}

async fn delete_invite(
    State(state): State<AppState>,
    Extension(access): Extension<OrgAccess>,
    error::Path(IdPath { id }): error::Path<IdPath>,
) -> Result<StatusCode, ApiError> {
    let org_id = access.org.id;
    let removed = state
        .meta
        .write(move |conn| Box::pin(async move { invites::delete_invite(conn, org_id, id).await }))
        .await?;
    if !removed {
        return Err(ApiError::NotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn preview_invite(
    State(state): State<AppState>,
    ClientIp(ip): ClientIp,
    error::Path(TokenPath { token }): error::Path<TokenPath>,
) -> Result<error::Json<InvitePreview>, ApiError> {
    limit_forms(&state, ip)?;
    let Some((invite, org)) = pending_by_token(&state, &token).await? else {
        return Err(ApiError::NotFound);
    };
    let account_exists = users::user_by_email(state.meta.reader(), &invite.email)
        .await?
        .is_some();
    Ok(error::Json(InvitePreview {
        org: InviteOrg {
            name: org.name,
            slug: org.slug,
        },
        email: invite.email,
        role: invite.role,
        expires_at: invite.expires_at,
        account_exists,
    }))
}

async fn accept_invite(
    State(state): State<AppState>,
    Extension(principal): Extension<Principal>,
    ClientIp(ip): ClientIp,
    error::Path(TokenPath { token }): error::Path<TokenPath>,
) -> Result<Response, ApiError> {
    limit_forms(&state, ip)?;
    let user = principal::session_user(&principal)?;
    if !is_opaque_token(&token) {
        return Err(ApiError::NotFound);
    }
    let hash = hash_token(&token);
    let now = state.clock.now_ns();
    let caller_id = user.id;
    let caller_email = user.email.clone();
    let outcome = state
        .meta
        .write(move |conn| {
            Box::pin(async move {
                let Some((invite, org)) = invites::unexpired_invite_tx(conn, &hash, now).await?
                else {
                    return Ok(Accepted::Missing);
                };
                if invite.email != caller_email {
                    return Ok(Accepted::WrongEmail);
                }
                let role = if let Some(role) = members::org_role_tx(conn, org.id, caller_id).await?
                {
                    role
                } else if invite.accepted_at.is_some() {
                    return Ok(Accepted::Missing);
                } else {
                    match members::add_org_member(conn, org.id, caller_id, invite.role, now).await {
                        Ok(()) => invite.role,
                        Err(MetaError::Conflict("already a member")) => {
                            match members::org_role_tx(conn, org.id, caller_id).await? {
                                Some(role) => role,
                                None => return Err(MetaError::Conflict("already a member")),
                            }
                        }
                        Err(error) => return Err(error),
                    }
                };
                invites::mark_accepted(conn, invite.id, now).await?;
                let verified: Option<i64> =
                    sqlx::query_scalar("SELECT email_verified_at FROM users WHERE id = ?")
                        .bind(caller_id.to_string())
                        .fetch_one(&mut *conn)
                        .await?;
                if verified.is_none() {
                    users::set_email_verified(conn, caller_id, now).await?;
                }
                Ok(Accepted::Done {
                    membership: OrgMembership {
                        id: org.id,
                        slug: org.slug,
                        name: org.name,
                        role,
                    },
                    invite_id: invite.id,
                })
            })
        })
        .await?;
    match outcome {
        Accepted::Done {
            membership,
            invite_id,
        } => {
            tracing::info!(
                target: "audit",
                org_id = %membership.id,
                actor_id = %caller_id,
                invite_id = %invite_id,
                "invite accepted"
            );
            Ok(error::Json(membership).into_response())
        }
        Accepted::Missing => Err(ApiError::NotFound),
        Accepted::WrongEmail => Ok(error::problem_response(&Problem::new(
            ProblemCode::Forbidden,
            Some("this invitation is for another email address".to_owned()),
        ))),
    }
}

/// No role above the caller's. `owner` only by an owner (D139).
fn may_grant(actor: OrgRole, role: OrgRole) -> bool {
    if role == OrgRole::Owner && actor != OrgRole::Owner {
        return false;
    }
    roles::org_rank(role) <= roles::org_rank(actor)
}

fn invite_json(row: InviteRow, invited_by: Option<Inviter>) -> Invite {
    Invite {
        id: row.id,
        email: row.email,
        role: row.role,
        invited_by: invited_by.map(|user| MemberUser {
            id: user.id,
            email: user.email,
            name: user.name,
        }),
        created_at: row.created_at,
        expires_at: row.expires_at,
    }
}

fn member_user(user: &users::UserRow) -> Inviter {
    Inviter {
        id: user.id,
        email: user.email.clone(),
        name: user.name.clone(),
    }
}

async fn pending_by_token(
    state: &AppState,
    token: &str,
) -> Result<Option<(InviteRow, crate::meta::orgs::OrgRow)>, ApiError> {
    if !is_opaque_token(token) {
        return Ok(None);
    }
    let hash = hash_token(token);
    Ok(invites::invite_by_hash(state.meta.reader(), &hash, state.clock.now_ns()).await?)
}

fn limit_forms(state: &AppState, ip: std::net::IpAddr) -> Result<(), ApiError> {
    state
        .limits
        .ip_forms
        .check(&ip, state.clock.now_ns())
        .map_err(|retry| ApiError::RateLimited {
            retry_after_s: retry.0.as_secs().max(1),
        })
}

fn field_error(field: &str, message: &str) -> ApiError {
    use crate::api::common::FieldError;
    ApiError::BadRequest {
        detail: None,
        errors: vec![FieldError {
            field: field.to_owned(),
            message: message.to_owned(),
            position: None,
        }],
    }
}

#[derive(Debug, thiserror::Error)]
#[error("invite expiry overflow")]
struct ExpiryOverflow;

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::http::{Method, header};
    use llmobs_core::ids::OrgId;
    use uuid::Uuid;

    use super::*;
    use crate::api::account::OrgRole;
    use crate::api::common::ProblemCode;
    use crate::auth::secret::hash_token;
    use crate::auth::session::start_session;
    use crate::meta::members::add_org_member;
    use crate::meta::users::{NewUser, insert_user};
    use crate::test_support::{TestApp, TestResponse, capture_logs};

    const SEVEN_DAYS_AND_A_SECOND: Duration = Duration::from_secs(7 * 24 * 60 * 60 + 1);

    struct Posted {
        token: String,
        url: String,
        invite: Invite,
    }

    fn cookie_pair(set_cookie: &axum::http::HeaderValue) -> String {
        set_cookie
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned()
    }

    async fn signed_in(app: &TestApp, email: &str) -> (Uuid, String) {
        let email = email.to_owned();
        let now = app.state.clock.now_ns();
        let user = app
            .state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    insert_user(
                        conn,
                        NewUser {
                            email,
                            name: None,
                            password_hash: None,
                            email_verified_at: Some(now),
                            is_instance_admin: false,
                        },
                        now,
                    )
                    .await
                })
            })
            .await
            .unwrap();
        let cookie = start_session(&app.state, user.id, None, None)
            .await
            .unwrap();
        (user.id, cookie_pair(&cookie))
    }

    async fn insert_unverified(app: &TestApp, email: &str) -> Uuid {
        let email = email.to_owned();
        let now = app.state.clock.now_ns();
        let user = app
            .state
            .meta
            .write(move |conn| {
                Box::pin(async move {
                    insert_user(
                        conn,
                        NewUser {
                            email,
                            name: None,
                            password_hash: None,
                            email_verified_at: None,
                            is_instance_admin: false,
                        },
                        now,
                    )
                    .await
                })
            })
            .await
            .unwrap();
        user.id
    }

    async fn add_member(app: &TestApp, org: OrgId, user: Uuid, role: OrgRole) {
        let now = app.state.clock.now_ns();
        app.state
            .meta
            .write(move |conn| {
                Box::pin(async move { add_org_member(conn, org, user, role, now).await })
            })
            .await
            .unwrap();
    }

    async fn create_org(app: &TestApp, cookie: &str) -> OrgMembership {
        let response = app
            .request(Method::POST, "/api/v1/orgs")
            .header(header::COOKIE.as_str(), cookie)
            .json(&serde_json::json!({ "name": "Acme" }))
            .send()
            .await;
        assert_eq!(response.status, 201, "{}", body(&response));
        response.json()
    }

    async fn send_invite(
        app: &TestApp,
        cookie: &str,
        slug: &str,
        email: &str,
        role: &str,
    ) -> TestResponse {
        app.request(Method::POST, &format!("/api/v1/orgs/{slug}/invites"))
            .header(header::COOKIE.as_str(), cookie)
            .json(&serde_json::json!({ "email": email, "role": role }))
            .send()
            .await
    }

    async fn post_invite(
        app: &TestApp,
        cookie: &str,
        slug: &str,
        email: &str,
        role: &str,
    ) -> Posted {
        let response = send_invite(app, cookie, slug, email, role).await;
        assert_eq!(response.status, 201, "{}", body(&response));
        let created: CreatedInvite = response.json();
        let token = created.accept_url.rsplit('/').next().unwrap().to_owned();
        Posted {
            url: created.accept_url,
            invite: created.invite,
            token,
        }
    }

    async fn wait_for_mail(app: &TestApp, count: usize) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while app.sent().len() < count && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    fn body(response: &TestResponse) -> String {
        String::from_utf8_lossy(&response.body).into_owned()
    }

    #[tokio::test(flavor = "current_thread")]
    async fn invite_stores_the_hash_and_replaces_the_previous() {
        let app = TestApp::with_mailer().await;
        let (ada_id, ada) = signed_in(&app, "ada@example.com").await;
        let org = create_org(&app, &ada).await;
        let email = "guest@example.com";
        let ((first, second), logs) = capture_logs(async {
            let first = post_invite(&app, &ada, &org.slug, email, "member").await;
            let second = post_invite(&app, &ada, &org.slug, email, "member").await;
            let old = app
                .request(Method::GET, &format!("/api/auth/invites/{}", first.token))
                .send()
                .await;
            let current = app
                .request(Method::GET, &format!("/api/auth/invites/{}", second.token))
                .send()
                .await;
            assert_eq!(old.status, 404);
            assert_eq!(current.status, 200);
            wait_for_mail(&app, 2).await;
            (first, second)
        })
        .await;
        assert!(!logs.contains(&first.token), "{logs}");
        assert!(!logs.contains(&second.token), "{logs}");
        assert!(!logs.contains(email), "{logs}");
        assert!(second.url.starts_with("http://localhost:8080/invite/"));
        assert!(second.url.ends_with(&second.token));
        assert_eq!(second.invite.email, email);
        assert_eq!(second.invite.role, OrgRole::Member);
        assert_eq!(second.invite.invited_by.unwrap().id, ada_id);
        let sent = app.sent();
        assert_eq!(sent.len(), 2, "{logs}");
        assert_eq!(sent[0].to, email);
        assert_eq!(sent[1].to, email);
        assert!(sent[0].text.contains(&first.url), "{}", sent[0].text);
        assert!(sent[1].text.contains(&second.url), "{}", sent[1].text);
        let hashes: Vec<String> = sqlx::query_scalar("SELECT token_hash FROM invites")
            .fetch_all(app.state.meta.reader())
            .await
            .unwrap();
        assert_eq!(hashes, vec![hash_token(&second.token)]);
        assert!(!hashes[0].contains(&second.token));
        assert_ne!(hashes[0], second.token);

        let listed: Page<Invite> = app
            .request(Method::GET, &format!("/api/v1/orgs/{}/invites", org.slug))
            .header(header::COOKIE.as_str(), &ada)
            .send()
            .await
            .json();
        assert_eq!(listed.data.len(), 1);
        assert_eq!(listed.data[0].id, second.invite.id);
        assert!(listed.next_cursor.is_none());
    }

    #[tokio::test]
    async fn an_admin_cannot_grant_owner_and_a_member_conflicts() {
        let app = TestApp::new().await;
        let (_, ada) = signed_in(&app, "ada@example.com").await;
        let (admin_id, admin) = signed_in(&app, "admin@example.com").await;
        let org = create_org(&app, &ada).await;
        add_member(&app, org.id, admin_id, OrgRole::Admin).await;

        let denied = send_invite(&app, &admin, &org.slug, "guest@example.com", "owner").await;
        assert_eq!(denied.status, 403);
        assert!(denied.problem().detail.is_none());

        let granted = post_invite(&app, &ada, &org.slug, "guest@example.com", "owner").await;
        assert_eq!(granted.invite.role, OrgRole::Owner);

        let member = send_invite(&app, &admin, &org.slug, "Ada@Example.com", "member").await;
        assert_eq!(member.status, 409);
        assert_eq!(member.problem().detail.as_deref(), Some("already a member"));

        let also = post_invite(&app, &admin, &org.slug, "bea@example.com", "member").await;
        let first: Page<Invite> = app
            .request(
                Method::GET,
                &format!("/api/v1/orgs/{}/invites?limit=1", org.slug),
            )
            .header(header::COOKIE.as_str(), &ada)
            .send()
            .await
            .json();
        assert_eq!(first.data.len(), 1);
        assert_eq!(first.data[0].id, also.invite.id);
        let cursor = first.next_cursor.unwrap();
        let second: Page<Invite> = app
            .request(
                Method::GET,
                &format!("/api/v1/orgs/{}/invites?limit=1&cursor={cursor}", org.slug),
            )
            .header(header::COOKIE.as_str(), &ada)
            .send()
            .await
            .json();
        assert_eq!(second.data.len(), 1);
        assert_eq!(second.data[0].id, granted.invite.id);
        assert!(second.next_cursor.is_none());
    }

    #[tokio::test]
    async fn preview_hides_expired_and_deleted_invites() {
        let app = TestApp::new().await;
        let (_, ada) = signed_in(&app, "ada@example.com").await;
        let org = create_org(&app, &ada).await;
        insert_unverified(&app, "other@example.com").await;
        let guest = post_invite(&app, &ada, &org.slug, "guest@example.com", "member").await;
        let other = post_invite(&app, &ada, &org.slug, "other@example.com", "viewer").await;

        let pending = app
            .request(Method::GET, &format!("/api/auth/invites/{}", guest.token))
            .send()
            .await;
        assert_eq!(pending.status, 200);
        let preview: InvitePreview = pending.json();
        assert!(!preview.account_exists);
        assert_eq!(preview.org.slug, org.slug);
        assert_eq!(preview.org.name, org.name);
        assert_eq!(preview.email, "guest@example.com");
        assert_eq!(preview.role, OrgRole::Member);

        let exists = app
            .request(Method::GET, &format!("/api/auth/invites/{}", other.token))
            .send()
            .await;
        assert_eq!(exists.status, 200);
        let preview: InvitePreview = exists.json();
        assert!(preview.account_exists);
        assert_eq!(preview.role, OrgRole::Viewer);

        let gone = app
            .request(
                Method::DELETE,
                &format!("/api/v1/orgs/{}/invites/{}", org.slug, guest.invite.id),
            )
            .header(header::COOKIE.as_str(), &ada)
            .send()
            .await;
        assert_eq!(gone.status, 204);
        let missing = app
            .request(Method::GET, &format!("/api/auth/invites/{}", guest.token))
            .send()
            .await;
        assert_eq!(missing.status, 404);
        let unknown = app
            .request(
                Method::DELETE,
                &format!("/api/v1/orgs/{}/invites/{}", org.slug, Uuid::now_v7()),
            )
            .header(header::COOKIE.as_str(), &ada)
            .send()
            .await;
        assert_eq!(unknown.status, 404);

        app.state.clock.advance(SEVEN_DAYS_AND_A_SECOND);
        let expired = app
            .request(Method::GET, &format!("/api/auth/invites/{}", other.token))
            .send()
            .await;
        assert_eq!(expired.status, 404);
        let listed: Page<Invite> = app
            .request(Method::GET, &format!("/api/v1/orgs/{}/invites", org.slug))
            .header(header::COOKIE.as_str(), &ada)
            .send()
            .await
            .json();
        assert!(listed.data.is_empty());
        assert!(listed.next_cursor.is_none());
    }

    #[tokio::test]
    async fn accept_checks_the_email_and_is_idempotent() {
        let app = TestApp::new().await;
        let (_, ada) = signed_in(&app, "ada@example.com").await;
        let (_, other) = signed_in(&app, "other@example.com").await;
        let org = create_org(&app, &ada).await;
        let guest_id = insert_unverified(&app, "guest@example.com").await;
        let guest_cookie = cookie_pair(
            &start_session(&app.state, guest_id, None, None)
                .await
                .unwrap(),
        );
        let invite = post_invite(&app, &ada, &org.slug, "guest@example.com", "member").await;

        let wrong = app
            .request(
                Method::POST,
                &format!("/api/auth/invites/{}/accept", invite.token),
            )
            .header(header::COOKIE.as_str(), &other)
            .send()
            .await;
        assert_eq!(wrong.status, 403);
        assert_eq!(wrong.problem().code, ProblemCode::Forbidden);
        assert_eq!(
            wrong.problem().detail.as_deref(),
            Some("this invitation is for another email address")
        );

        let created = app
            .request(Method::POST, "/api/v1/me/tokens")
            .header(header::COOKIE.as_str(), &guest_cookie)
            .json(&serde_json::json!({"name": "ci", "scopes": ["read", "write"]}))
            .send()
            .await;
        assert_eq!(created.status, 201, "{}", body(&created));
        let secret = created.json::<serde_json::Value>()["secret"]
            .as_str()
            .unwrap()
            .to_owned();
        let pat = app
            .request(
                Method::POST,
                &format!("/api/auth/invites/{}/accept", invite.token),
            )
            .header(header::AUTHORIZATION.as_str(), &format!("Bearer {secret}"))
            .send()
            .await;
        assert_eq!(pat.status, 403);

        let (accepted, logs) = capture_logs(async {
            let accepted = app
                .request(
                    Method::POST,
                    &format!("/api/auth/invites/{}/accept", invite.token),
                )
                .header(header::COOKIE.as_str(), &guest_cookie)
                .send()
                .await;
            let again = app
                .request(
                    Method::POST,
                    &format!("/api/auth/invites/{}/accept", invite.token),
                )
                .header(header::COOKIE.as_str(), &guest_cookie)
                .send()
                .await;
            assert_eq!(again.status, 200, "{}", body(&again));
            accepted
        })
        .await;
        assert!(!logs.contains(&invite.token), "{logs}");
        assert!(!logs.contains("guest@example.com"), "{logs}");
        assert_eq!(accepted.status, 200, "{}", body(&accepted));
        let membership: OrgMembership = accepted.json();
        assert_eq!(membership.id, org.id);
        assert_eq!(membership.slug, org.slug);
        assert_eq!(membership.role, OrgRole::Member);
        let again: OrgMembership = app
            .request(
                Method::POST,
                &format!("/api/auth/invites/{}/accept", invite.token),
            )
            .header(header::COOKIE.as_str(), &guest_cookie)
            .send()
            .await
            .json();
        assert_eq!(again.role, OrgRole::Member);
        let members: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM org_members WHERE org_id = ? AND user_id = ?")
                .bind(org.id.to_string())
                .bind(guest_id.to_string())
                .fetch_one(app.state.meta.reader())
                .await
                .unwrap();
        assert_eq!(members, 1);
        let verified: Option<i64> =
            sqlx::query_scalar("SELECT email_verified_at FROM users WHERE id = ?")
                .bind(guest_id.to_string())
                .fetch_one(app.state.meta.reader())
                .await
                .unwrap();
        assert!(verified.is_some());
        let hidden = app
            .request(Method::GET, &format!("/api/auth/invites/{}", invite.token))
            .send()
            .await;
        assert_eq!(hidden.status, 404);
        app.state.clock.advance(Duration::from_secs(61));

        let existing = post_invite(&app, &ada, &org.slug, "viewer@example.com", "admin").await;
        let viewer_id = insert_unverified(&app, "viewer@example.com").await;
        add_member(&app, org.id, viewer_id, OrgRole::Viewer).await;
        let viewer_cookie = cookie_pair(
            &start_session(&app.state, viewer_id, None, None)
                .await
                .unwrap(),
        );
        let kept = app
            .request(
                Method::POST,
                &format!("/api/auth/invites/{}/accept", existing.token),
            )
            .header(header::COOKIE.as_str(), &viewer_cookie)
            .send()
            .await;
        assert_eq!(kept.status, 200, "{}", body(&kept));
        let kept: OrgMembership = kept.json();
        assert_eq!(kept.role, OrgRole::Viewer);
    }
}
