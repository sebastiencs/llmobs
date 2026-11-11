# Langfuse: self-hosted auth

Sources: public docs (Nov 2026) and a glance at `/home/sebastien/github/langfuse` (HEAD 2026-11-09) to confirm hashing and session details. Nothing copied.

- Auth & SSO: https://langfuse.com/self-hosting/security/authentication-and-sso
- Headless init: https://langfuse.com/self-hosting/administration/headless-initialization
- RBAC: https://langfuse.com/docs/administration/rbac
- Configuration: https://langfuse.com/self-hosting/configuration

## Stack

Next.js app; auth through NextAuth/Auth.js. Postgres holds users, orgs, projects, keys.

## Sign-up policy

| Env var | Effect |
|---|---|
| `AUTH_DISABLE_SIGNUP=true` | No new accounts. Note: this also blocks invited users who have no account yet. |
| `AUTH_DISABLE_USERNAME_PASSWORD=true` | Password login and sign-up off; SSO only. |
| `AUTH_DOMAINS_WITH_SSO_ENFORCEMENT` | Comma list of email domains that must use SSO. |
| `AUTH_EMAIL_VERIFICATION_REQUIRED=true` | One-time code by email before sign-up (needs SMTP); direct `POST /api/auth/signup` returns 403. |
| `LANGFUSE_ALLOWED_ORGANIZATION_CREATORS` | Only these emails may create orgs. |
| `LANGFUSE_DEFAULT_ORG_ID` / `_ORG_ROLE` / `_PROJECT_ID` / `_PROJECT_ROLE` | Every new user is auto-added to this org/project with this role. |

Default is **open** sign-up: anyone who reaches the instance can register.

## Passwords

- bcrypt (PBKDF2-HMAC-SHA256 in FIPS mode), min 8 chars plus a "letters, numbers, special chars" rule (`web/src/features/auth/lib/signupSchema.ts`).
- Reset needs transactional email. Without it, the documented recovery is a manual DB procedure (change email, re-sign-up, move memberships, delete old row). No CLI or admin-generated link.

## SSO / OIDC

- Per provider: `AUTH_<PROVIDER>_CLIENT_ID`, `_CLIENT_SECRET`, `_ISSUER` (Okta, Auth0, Keycloak, Authentik, Cognito, JumpCloud, OneLogin), `AUTH_AZURE_AD_TENANT_ID`, `AUTH_GITHUB_ENTERPRISE_BASE_URL`, `AUTH_GITLAB_ISSUER`, generic `AUTH_CUSTOM_*`. One instance of each named provider; one "custom".
- `AUTH_GOOGLE_ALLOWED_DOMAINS`: domain restriction for Google only.
- `AUTH_<PROVIDER>_CHECKS` = subset of `pkce,state,nonce,none`. **Before Nov 2025 no check was forced by default**, which caused CVE-2025-65107 (login CSRF / account linking; see advisories.md).
- `AUTH_<PROVIDER>_ALLOW_ACCOUNT_LINKING=true` maps to Auth.js `allowDangerousEmailAccountLinking`: an IdP login with an email equal to an existing user's email is merged into that user. The docs warn this enables takeover when the IdP does not verify emails. There is no `email_verified` check in the documented behaviour; linking is all-or-nothing per provider.
- Users are identified by email; the IdP must return an email claim.
- IdP-initiated SSO via `/auth/sso-initiate?provider=…`.
- Enterprise (cloud/EE) has per-org SSO configs stored in DB (`allowDangerousEmailAccountLinking` per config, default false).
- No SAML in OSS (only through an OIDC bridge such as WorkOS).

## RBAC

- Roles: `OWNER`, `ADMIN`, `MEMBER`, `VIEWER`, `NONE` (+ `AI Gateway` org-only role).
- Org role applies to all projects; a project role overrides it for one project. Project-level roles are an EE/paid feature.
- `NONE` at org level + explicit project roles = "access only these projects". Projects without `project:read` are hidden.
- A user can only assign roles ≤ their own.
- Member: create scores, datasets, prompts, evaluators, alerts; **sees the API key list but cannot manage keys**.
- Admin: API keys, LLM connections, integrations, deleting traces. Owner: delete project/org, org API keys, billing.
- Instance admin: `admin` flag on the user row; EE "Instance Management API" authenticated with `ADMIN_API_KEY`.

## API keys

- Pair per project: public `pk-lf-…` + secret `sk-lf-…`, sent as HTTP Basic (pk:sk). The public key is an identifier, not a browser-safe key.
- Permission set chosen at creation, immutable: full access, viewer (read), ingestion (write traces/scores/media, no read), scores-only ingestion.
- Organization keys exist for the org management API (owner only).
- Storage: SHA-256 of `secret || sha256(SALT)` (`fastHashedSecretKey`, `packages/shared/src/server/auth/apiKeys.ts`), plus a legacy bcrypt hash backfilled on first use. Display form `sk-lf-...xxxx`. Shown once.
- Optional expiry, "last used" column. Rotation = create new, switch, delete old; delete is immediate. Redis caches the resolved key (`LANGFUSE_CACHE_API_KEY_ENABLED`), never the plaintext.
- No personal access tokens; the CLI/MCP use project keys.

## Sessions

- Auth.js **JWT sessions** (`strategy: "jwt"`) signed with `NEXTAUTH_SECRET`; `AUTH_SESSION_MAX_AGE` minutes (default 43 200 = 30 days). Stateless: no server-side revocation list, so logout/reset cannot kill other devices' sessions before expiry.
- `NEXTAUTH_URL` must equal the public URL (callbacks and email links).

## Stored secrets

- `ENCRYPTION_KEY`: 64 hex chars (256 bits), AES-256-GCM with random 12-byte IV, stored as `iv:ciphertext:tag` (`packages/shared/src/encryption/encryption.ts`). No associated data: a ciphertext can be moved between rows/columns. Used for LLM API keys, integration secrets, etc. No documented rotation command.
- `SALT` for API key hashes; `NEXTAUTH_SECRET` for session JWTs. Three independent operator secrets.

## Bootstrap

- No setup page: the first person to sign up creates an org. With open sign-up an exposed fresh instance can be claimed by anyone.
- Headless: `LANGFUSE_INIT_ORG_ID`, `_ORG_NAME`, `_PROJECT_ID`, `_PROJECT_NAME`, `_PROJECT_RETENTION`, `_PROJECT_PUBLIC_KEY`, `_PROJECT_SECRET_KEY`, `_USER_EMAIL`, `_USER_NAME`, `_USER_PASSWORD`. Created on startup "if they don't already exist"; behaviour when values change later is undocumented.

## Takeaways for llmobs

- Copy: immutable scope set per key; ingestion-only key; role ≤ own when assigning; org role `none` + project grants; headless init with pre-chosen keys.
- Do better: DB-backed sessions (revocable), state+nonce+PKCE always on, linking gated on `email_verified` and per-provider trust, password reset without SMTP (CLI), AAD on encrypted columns, secure default sign-up.
