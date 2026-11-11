# Part 6 · OAuth and OIDC (P23–P25)

GitHub (OAuth 2, not OIDC), Google (OIDC) and any number of generic OIDC providers (spec 08 § OAuth / OIDC), with the linking rules of D148. P24 lands the flow with the strictest behaviour (a provider login whose email belongs to an existing account is refused); P25 adds conditional linking, provisioning and domain rules.

---

## P23 — Provider config and the mock IdP

**Goal:** provider configuration with its validation, lazy discovery through an HTTP client without redirects, and an in-process mock IdP for tests (D151).

**Edit only:** `crates/server/src/config.rs`, `crates/server/src/auth/oidc/{mod.rs,provider.rs,github.rs}` (new), `crates/server/src/test_support/mock_idp.rs` (new; turn `test_support.rs` into `test_support/mod.rs`), `crates/server/src/http/mod.rs` (`providers` in `AppInner`), `crates/server/src/routes/auth.rs` (`GET /api/auth/config` lists providers), `crates/server/Cargo.toml`, root `Cargo.toml` (`openidconnect`, `oauth2`, `hmac`, dev `rsa`).

**Commit:** `server: configure OAuth and OIDC providers`

### Config (`[auth]`)

```toml
[auth.github]
client_id = "…"
client_secret = "…"                  # Secret<String>
link_existing_by_email = true        # default true (D148)

[auth.google]
client_id = "…"
client_secret = "…"
allowed_domains = []                 # checked against the verified email and the `hd` claim
link_existing_by_email = true        # default true

[[auth.oidc]]
name = "okta"                        # [a-z0-9-]{1,32}, unique; provider id "oidc:okta"
display_name = "Okta"
issuer = "https://acme.okta.com"
client_id = "…"
client_secret = "…"
scopes = ["openid", "email", "profile"]   # default; must contain "openid"
allowed_domains = []
auto_provision = { org = "acme", role = "member" }   # optional; role ∈ admin | member | viewer
link_existing_by_email = false       # default false (D148)
```

Validation at load (errors name the key and the provider, never a secret):
- `issuer` is `https://…`, or `http://` with a loopback host (`127.0.0.1`, `::1`, `localhost`) for development and the mock IdP.
- Multi-tenant Microsoft issuers are refused: host `login.microsoftonline.com` with first path segment `common`, `organizations` or `consumers`.
- `auto_provision.org` has slug syntax; its role is not `owner`. Whether the org exists is checked at login.
- `auth.disable_password = true` without any provider → error. `auth.enforce_sso_domains` non-empty without any provider → error.
- Env: GitHub and Google work through the split form (`LLMOBS_AUTH__GITHUB__CLIENT_ID`); `[[auth.oidc]]` arrays are TOML only (document it in spec 12 and the config doc comment).

### Runtime (`auth/oidc/`)

```rust
pub(crate) struct Providers { github: Option<GithubProvider>, oidc: Vec<OidcProvider> }   // Google is an OidcProvider with `kind: Google`
impl Providers {
    pub(crate) fn from_config(cfg: &AuthSettings, http: reqwest::Client) -> Self;
    pub(crate) fn get(&self, id: &str) -> Option<ProviderRef<'_>>;   // "github", "google", "oidc:<name>"
    pub(crate) fn list(&self) -> Vec<AuthProvider>;                    // config order: github, google, oidc…
}
pub(crate) struct OidcProvider {
    id: String, kind: OidcKind /* Google | Generic */, display_name: String, issuer: IssuerUrl,
    client_id: ClientId, client_secret: Secret<String>, scopes: Vec<String>, allowed_domains: Vec<String>,
    auto_provision: Option<AutoProvision>, link_existing_by_email: bool,
    metadata: tokio::sync::RwLock<Option<(CoreProviderMetadata, i64 /*fetched_ns*/)>>,
}
```

- The HTTP client: `reqwest::Client` with `redirect(Policy::none())` (required by `openidconnect`: following redirects there enables SSRF), a 10 s timeout and rustls. Issuers come from the operator's config, so the SSRF guard of spec 12 is not applied to them; note this in spec 12's checklist ("identity provider URLs are operator configuration").
- `metadata(now)`: discovery on first use and when older than 1 hour; on failure, keep using stale metadata if there is any, else `OidcError::Unavailable`. Discovery never runs at startup (an IdP outage must not block the server).
- `GithubProvider` holds `GithubEndpoints { authorize, token, api }` with the production URLs; a `#[cfg(test)]` constructor points them at the mock.
- `GET /api/auth/config` now returns `providers: Providers::list()`.

### Mock IdP (`test_support/mock_idp.rs`, `#[cfg(test)]`)

An axum app on `127.0.0.1:0` serving:
- `/.well-known/openid-configuration` (issuer = its own base URL, `authorization_endpoint`, `token_endpoint`, `jwks_uri`, `response_types_supported: ["code"]`, `subject_types_supported: ["public"]`, `id_token_signing_alg_values_supported: ["RS256"]`);
- `/jwks` with the public part of an RSA-2048 key generated once per test process (`rsa` 0.10; check its `rand_core` compatibility with the workspace `rand` and pick the RNG accordingly);
- `POST /token`: looks up the `code` registered by the test, checks `code_verifier` against the registered S256 challenge, `redirect_uri` and client credentials (basic or post), and returns `{access_token, token_type, expires_in, id_token}`. The id_token is a compact JWS signed RS256 with `rsa::pkcs1v15::SigningKey<Sha256>` over the claims the test chose (`iss`, `aud`, `sub`, `exp`, `iat`, `nonce`, `email`, `email_verified`, `hd`, `name`).
- GitHub-shaped endpoints for the `GithubProvider`: `POST /login/oauth/access_token`, `GET /user` (`{id, login, name}`), `GET /user/emails` (`[{email, primary, verified}]`).
- Test API: `MockIdp::start().await`, `issuer()`, `issue_code(Grant { claims, code_challenge, redirect_uri }) -> String`, knobs to sign with a foreign key, to return a wrong `aud`, a wrong `iss`, an expired token, to rotate the signing key, and to fail discovery.

### Tests

- Config: valid GitHub/Google/OIDC; bad name; duplicate names; `http://` non-loopback issuer; Entra `common`; `auto_provision.role = owner`; password disabled without providers; SSO domains without providers; the `Debug` of the config does not contain any client secret.
- `GET /api/auth/config` lists providers in order with display names.
- Discovery against the mock: cached for an hour (count requests); stale metadata used when the mock fails; first discovery failing → `Unavailable`.
- The HTTP client does not follow a redirect (mock route answering 302 to another mock route: the second is never hit).

### Report

The `rsa`/`rand_core` combination used, and the openidconnect 4.1 types for metadata and the async HTTP client.

---

## P24 — Authorize and callback

**Goal:** the authorization-code flow with `state`, PKCE and `nonce` (D148), id_token validation, GitHub's OAuth flow, sign-in of a known identity, and creation of new users under the sign-up policy. An email that matches an existing account without an identity is refused (`link_refused`) until P25.

**Edit only:** `crates/server/src/auth/oidc/{mod.rs,flow.rs,state_cookie.rs,github.rs}` (`flow.rs`, `state_cookie.rs` new), `crates/server/src/meta/identities.rs` (new), `crates/server/src/meta/mod.rs`, `crates/server/src/routes/oauth.rs` (new), `crates/server/src/routes/mod.rs`, `crates/server/src/http/{mod.rs,access.rs}` (`oauth_key` in `AppInner`), `crates/server/src/api/openapi.rs`, `crates/server/openapi.json`, `web/src/api/schema.gen.ts`, `crates/server/src/test_support/`.

**Commit:** `server: add OAuth and OIDC login`

### State cookie (`state_cookie.rs`)

- `AppInner.oauth_key: [u8; 32]`, random per process (D148): a restart invalidates logins in flight, which is acceptable for a 10-minute cookie.
- Value: `base64url(json(OAuthState { provider, state, nonce, pkce_verifier, redirect, created_ns }))` + `.` + `base64url(HMAC-SHA256(oauth_key, first part))`. Verification is constant-time (`hmac`'s `verify_slice`).
- Cookie `llmobs_oauth` (name from the constant): `HttpOnly; SameSite=Lax; Path=/api/auth/callback; Max-Age=600`, plus `Secure` with an https `public_url`.

### Routes

| Route | Access | Behaviour |
|---|---|---|
| `GET /api/auth/authorize/{provider}` | `Public` | `?redirect=` optional: kept only if it starts with `/`, does not start with `//` or `/\`, and is ≤ 1 024 bytes; else `/`. Unknown provider → 404. Metadata unavailable → 302 `/login?error=provider_unavailable`. New `state` and `nonce` (opaque tokens), PKCE S256 verifier; set the cookie; 302 to the provider's authorization URL with `redirect_uri = <public_url>/api/auth/callback/<provider>` and the provider's scopes (GitHub: `read:user user:email`). |
| `GET /api/auth/callback/{provider}` | `Public` | Always clears the cookie. Success → session cookie and 302 to the saved `redirect`. Any failure → 302 to `/login?error=<code>`. |

Error codes (closed set, documented in spec 08 and the OpenAPI description): `invalid_state`, `provider_error`, `provider_unavailable`, `email_not_verified`, `domain_not_allowed`, `signup_closed`, `account_disabled`, `link_refused`, `setup_required`. P25 uses the last three more widely.

Both routes are GET, so the CSRF check of D141 does not apply; `state` bound to the cookie is the CSRF defence (Langfuse CVE-2025-65107).

### Callback steps (`flow.rs`)

1. Cookie present, signature valid, `provider` equal to the path, age ≤ 600 s, `state` query equal (constant time) → else `invalid_state`.
2. `?error=` present (user denied) → `provider_error`.
3. **OIDC:** exchange the code with the PKCE verifier; verify the id_token with openidconnect: signature against the JWKS, `iss`, `aud = client_id`, `exp`, `nonce`. On a signature failure because the key id is unknown, refresh the metadata once and retry (key rotation). Claims: `sub`, `email`, `email_verified` (absent → false), `hd`, `name`. **GitHub:** exchange the code (with PKCE), `GET /user` → `id` as the subject (string), `GET /user/emails` → the primary address that is `verified` (none → `email_not_verified`).
4. `resolve_login(state, provider, Claims) -> Result<Uuid, LoginError>`:
   - an identity `(provider, subject)` exists → its user; disabled → `account_disabled`;
   - no email, or `email_verified = false` → `email_not_verified` (never create an account from an unverified email);
   - `normalize_email`; a user with that email exists → `link_refused` (P25 refines);
   - no user at all in the database → `setup_required` (P25 adds setup through SSO);
   - else sign-up policy: `open` → create; `invite_only` → create only if a pending invite exists for the email; `disabled` → `signup_closed`. A created user is verified (`email_verified_at = now`), has no password, and gets an `identities` row.
5. Start a session (P9); audit `login` with the provider id and the user id.

**The callback never looks at an existing session cookie** (research G3): a signed-in user B who opens a callback carrying attacker A's code ends up signed in as whoever the code belongs to, and no identity is ever attached to B.

### Store (`meta/identities.rs`)

`identity_user(pool, provider, subject) -> Option<Uuid>`; `insert_identity(conn, user_id, provider, subject, email, now)`; `user_identities(pool, user_id) -> Vec<String /*provider*/>` (P22's `providers` field now gets real values: update the admin handler in this sub-phase).

### Tests (mock IdP)

- Generic OIDC under `open`: authorize → callback with a code issued for the recorded challenge and nonce → 302 to the redirect with a session; `/me` works; a second login uses the identity.
- `state` missing, wrong, from another provider, older than 10 minutes, cookie tampered → `invalid_state`.
- Wrong `nonce`, wrong `aud`, wrong `iss`, expired, signed with a foreign key → `provider_error` (no session).
- Key rotation: the mock rotates keys after discovery; the login still succeeds after one metadata refresh.
- `email_verified` false or missing → `email_not_verified`; GitHub with no verified primary → `email_not_verified`.
- Existing password user with the same email → `link_refused`, no identity row.
- Callback with a valid code while signed in as another user → the response signs in the code's user; no identity attached to the other user (check `identities`).
- `invite_only` without invite → `signup_closed`; with a pending invite → created; `disabled` → `signup_closed`.
- Redirect sanitizing: `//evil.example`, `/\evil`, `https://evil.example` → `/`.
- Disabled user with an identity → `account_disabled`.
- `assert_logs_clean` with the code, the id_token, the client secret and the email.

### Report

Standard.

---

## P25 — Linking, provisioning, domains and setup through SSO

**Goal:** the rest of D148: conditional linking with pre-hijack protection, `allowed_domains` and Google's `hd`, `auto_provision`, SSO-enforced domains, and first-run setup through an identity provider.

**Edit only:** `crates/server/src/auth/oidc/{flow.rs,mod.rs}`, `crates/server/src/routes/oauth.rs`, `crates/server/src/bootstrap.rs`, `crates/server/src/meta/{identities.rs,users.rs,members.rs}`, `crates/server/src/api/openapi.rs`, `crates/server/openapi.json`, `web/src/api/schema.gen.ts`, `docs/spec/08-auth.md` (only if the error codes changed).

**Commit:** `server: link identities and provision OAuth users`

### `resolve_login` additions (in this order)

1. **Domain rules** (Google and generic OIDC with non-empty `allowed_domains`): the verified email's domain is in the list, and for Google the `hd` claim is present and in the list → else `domain_not_allowed`. Checked before any lookup.
2. **Setup through SSO:** no user in the database → allowed only when the authorize request carried `?setup_token=` equal (constant time) to the current setup token (P11), stored in the state cookie. Then create the user as instance admin (verified), clear the setup token, audit `setup completed`; the UI then creates the first org with `POST /api/v1/orgs`. Otherwise `setup_required`. `GET /api/auth/authorize/{provider}` accepts `setup_token` and puts it in `OAuthState`.
3. **Linking** (identity unknown, a user with the email exists): link only if the provider's `link_existing_by_email` is true, `email_verified` is true (already required), and the user is not an instance admin; else `link_refused`. Disabled user → `account_disabled`. Linking, in one transaction: insert the identity; delete all the user's web sessions; revoke all their PATs; if `email_verified_at` is null, clear `password_hash` and set `email_verified_at = now` (pre-hijacking: an attacker who registered the victim's email with a password loses that password). Audit `identity linked` (user id, provider).
4. **Sign-up policy** for new users: `auto_provision` on the provider counts as an invitation under `invite_only` (spec 08). `signup_domains` applies to `open` sign-ups through providers too.
5. **Provisioning** (new and existing users, after login): `auto_provision` with an existing org whose slug matches → add the membership with the configured role if the user is not a member yet. A missing org logs a `warn` (org slug only) and provisioning is skipped.

### Password login and SSO domains

P12 already answers 403 `sso_required` for password login in `enforce_sso_domains`. Here: signup with such an email (P12/P20) also → `sso_required`, if not done already; any configured provider satisfies the rule (P1).

### Tests (mock IdP)

- Google with `allowed_domains = ["acme.com"]`: `ada@acme.com` with `hd = acme.com` → ok; `hd` missing or `other.com` → `domain_not_allowed`; generic OIDC with a domain list and an outside email → `domain_not_allowed`.
- Linking: GitHub (default on) links to an existing verified password user: identity created, old sessions and PATs dead, password kept. Generic OIDC (default off) → `link_refused`. Instance admin → `link_refused` whatever the flag.
- Pre-hijack: an unverified password account plus a verified Google login → linked, `password_hash` null, verified; the old password no longer logs in.
- Setup through SSO: empty DB, authorize without `setup_token` → callback `setup_required`; with the logged token → instance admin created, setup token cleared; a later login carrying the old token follows the normal rules (users exist, the token is ignored).
- `auto_provision` → membership with the role; under `invite_only` a new user is created through that provider; a missing org → login succeeds, no membership, a `warn` with the slug.
- `assert_logs_clean` as in P24.

### Report

Standard.
