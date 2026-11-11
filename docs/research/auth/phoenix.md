# Arize Phoenix: self-hosted auth

Sources: https://arize.com/docs/phoenix/self-hosting/features/authentication (Nov 2026), release note https://arize.com/docs/phoenix/release-notes/09.26.2024-authentication-and-rbac, and a glance at `/home/sebastien/github/phoenix` to confirm hashing, key derivation and OAuth matching. Nothing copied.

## Stack

Python (Starlette/FastAPI + Strawberry GraphQL), SQLite or Postgres. Auth added in Phoenix 5.0 (Sept 2024).

## Auth is off by default

- `PHOENIX_ENABLE_AUTH=True` turns it on; default is **no authentication at all** (UI, REST, GraphQL, gRPC open). Several advisories (Phoenix GHSA-gfv3, Opik GHSA-87xc) are rated worse because of this default.
- `PHOENIX_SECRET` required when enabled; signs JWTs. Recent versions also keep a random 32-byte deployment seed in the DB and derive purpose-specific keys with HKDF (labels like `phoenix/token-signing/v1`, `phoenix/oauth2-login-context/v1`; `src/phoenix/server/deployment_secret.py`). Stored secrets (LLM keys) use Fernet with a key derived from the secret.

## Bootstrap

- Default admin `admin@localhost` / password `admin`, forced change at first login. `PHOENIX_DEFAULT_ADMIN_INITIAL_PASSWORD` overrides it while the password is still the default.
- `PHOENIX_ADMINS="user=email;…"` creates extra admins at startup with random passwords to be reset; existing users untouched; startup fails on parse errors.
- `PHOENIX_ADMIN_SECRET`: a static bearer token that acts as the first admin (≥ 32 chars, must differ from `PHOENIX_SECRET`). Useful for IaC, but it is a non-expiring god token in env.

## Passwords and login

- PBKDF2-HMAC-SHA256, 10 000 iterations, per-user salt (`src/phoenix/auth.py`). Min length **4** by default; `PHOENIX_ENABLE_STRONG_PASSWORD_POLICY` = 12+ chars with character classes.
- Brute force: 5 failures → 5-minute lockout (`PHOENIX_BRUTE_FORCE_LOGIN_PROTECTION_MAX_ATTEMPTS`, `PHOENIX_DISABLE_BRUTE_FORCE_LOGIN_PROTECTION`).
- `PHOENIX_DISABLE_BASIC_AUTH`: no password login or local user creation (OAuth2/LDAP only).
- Reset via SMTP (`PHOENIX_SMTP_*`), or an admin resets it in Settings; IdP users recover at their IdP.

## Sessions and CSRF

- Access + refresh JWTs in cookies (`PHOENIX_ACCESS_TOKEN_EXPIRY_MINUTES`, `PHOENIX_REFRESH_TOKEN_EXPIRY_MINUTES`, `PHOENIX_PASSWORD_RESET_TOKEN_EXPIRY_MINUTES`). Token IDs are recorded in the DB so logout/revocation works.
- `PHOENIX_USE_SECURE_COOKIES` defaults to **false**.
- `PHOENIX_CSRF_TRUSTED_ORIGINS`: if empty, **CSRF protection is off** (Origin/Referer not checked). Opt-in CSRF is the wrong default.

## OAuth2 / OIDC and LDAP

- Per IdP: `PHOENIX_OAUTH2_<IDP>_CLIENT_ID`, `_CLIENT_SECRET`, `_OIDC_CONFIG_URL` (HTTPS except localhost), `_DISPLAY_NAME`, `_USE_PKCE` (default **false**), `_TOKEN_ENDPOINT_AUTH_METHOD`, `_SCOPES`, `_EMAIL_ATTRIBUTE_PATH`, `_CLIENT_ASSERTION_FILE`.
- `_ALLOW_SIGN_UP` (default true). When false, only users that already exist (matched by email, pre-created by an admin) can sign in.
- `_AUTO_LOGIN`: skip the login page (one provider only).
- Group gate: `_GROUPS_ATTRIBUTE_PATH` + `_ALLOWED_GROUPS`; claims must come from the ID token or UserInfo, not the access token.
- Role mapping: `_ROLE_ATTRIBUTE_PATH`, `_ROLE_MAPPING` (`IdpRole:PhoenixRole`), `_ROLE_ATTRIBUTE_STRICT` (deny when unmapped; default falls back to VIEWER), `_ROLE_RESYNC` (default true: role re-synced on every login). SYSTEM role cannot be granted by an IdP.
- **Account linking is conservative**: an OAuth identity is keyed by (client id, IdP user id). A user that has a local password is never taken over by an IdP login; sign-in is refused (`SignInNotAllowed`). A collision on email raises `EmailAlreadyInUse` (`src/phoenix/server/api/routers/oauth2.py`). The user's email is updated from the IdP on each login.
- No email-domain allowlist (groups are the mechanism).
- LDAP (12.20+): `PHOENIX_LDAP_HOST`, `_USER_SEARCH_BASE_DNS`, `_GROUP_ROLE_MAPPINGS`, `_ATTR_EMAIL`, TLS mode (`starttls` default), `_ALLOW_SIGN_UP`.

## RBAC

- Instance-wide roles only: `ADMIN`, `MEMBER`, `VIEWER` (+ internal `SYSTEM`). **No orgs, no per-project roles.** Projects are shared by everyone.
- Viewer: read-only; write requests to `/v1/` return 403 except managing their own user keys.

## API keys

- **System keys**: created by admins, survive deletion of the creator, for automation.
- **User keys**: act as the user, deleted with the user.
- Keys are JWTs signed with the server key; the key id is stored in `api_keys` so a key can be revoked. Leaking the signing secret lets an attacker mint keys (GHSA-gfv3).
- API keys cannot mint other keys. Through Phoenix 18 GraphQL allowed a key to create keys; from 19, creation requires a browser session.
- Client env var `PHOENIX_API_KEY`, header `Authorization: Bearer`.

## Takeaways for llmobs

- Copy: refuse to link an IdP identity onto an account that has a password unless explicitly allowed; role/group mapping from IdP claims with strict mode; "credentials cannot mint credentials"; HKDF purpose labels for every derived key.
- Avoid: auth off by default, default `admin/admin`, CSRF opt-in, PKCE opt-in, non-Secure cookies by default, 4-char passwords, a static admin bearer secret in env, signed-JWT API keys (master-key compromise = key forgery; opaque random keys have no such failure mode).
