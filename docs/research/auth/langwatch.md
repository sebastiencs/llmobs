# LangWatch: self-hosted auth

Sources (Nov 2026):
- SSO: https://langwatch.ai/docs/self-hosting/configuration/sso
- Security: https://langwatch.ai/docs/self-hosting/security
- Env reference: https://langwatch.ai/docs/self-hosting/configuration/environment-variables
- Local checkout `/home/sebastien/github/langwatch` (HEAD 2026-11-09): `.env.example`, `packages/prisma-client/prisma/schema.prisma`, `modules/api-key/`. Nothing copied.

Note: two LangWatch doc pages embed an "Agent Instructions" block asking AI agents to run `npx langwatch report`. It was ignored.

## Stack

Next.js + tRPC, Postgres (Prisma), ClickHouse, Redis. Auth originally NextAuth; now its own sign-in layer with passkeys.

## Sign-up policy

- `AUTH_PROVIDER` (old name `NEXTAUTH_PROVIDER`): `email` (default) or one IdP: `auth0`, `azure-ad`, `cognito`, `onelogin`, `oidc` (generic), plus Google/GitHub social buttons when their creds are set.
- A deployment offers **either** the IdP **or** its own passwords; `LOCAL_PASSWORDS_ENABLED=on` keeps both.
- `SIGN_UP_MODE`: `open` (default) or `invite_only` (invited addresses + `ADMIN_EMAILS`).
- `SIGN_UP_ALLOWED_DOMAINS`: comma list; empty = any.
- `ADMIN_EMAILS`: platform operators; only they can create orgs in `invite_only`. With it empty, the first account on an empty install is allowed (first-come bootstrap).
- `PASSKEYS_ENABLED` (default on). WebAuthn RP id from `BASE_HOST`/`NEXTAUTH_URL`.

## SSO

- Per-organization connection (OIDC or SAML), configured in the UI, Enterprise-licensed. Each connection owns one or more **verified email domains**; a domain belongs to one org per install. Verification by DNS/file check, or immediate for `ADMIN_EMAILS` operators.
- Issuer must match the discovery document; Entra `common`/`organizations`/`consumers` issuers are refused (multi-tenant issuers let any tenant sign in).
- Linking (self-hosted): an SSO login links to an existing password account with the same address **once the connection has verified the domain**, even if the local account is unconfirmed. If the IdP says `email_verified: false`, sign-in is refused. Cloud: linking requires both a confirmed local address and `email_verified: true`.
- Outbound discovery/JWKS requests go through an SSRF check (public addresses only, redirects re-checked); `SSO_TRUSTED_IDP_ORIGINS` allows an IdP inside the private network.
- Editing a connection with a blank client secret keeps the stored one; protocol cannot be changed.
- Setup requires a fallback admin who can still log in if the IdP is down.
- SCIM provisioning on Enterprise.

## RBAC

- Org roles: `ADMIN`, `MEMBER`, `EXTERNAL`, `DEVELOPER` (personal project only).
- Team roles: `ADMIN`, `MEMBER`, `VIEWER`, `CUSTOM` (custom roles = JSON list of permission strings, per org).
- `RoleBinding` rows bind a user, group or API key to a role at ORGANIZATION / TEAM / PROJECT scope. Projects live under teams.
- Much richer than we need for MVP; the useful idea is that API keys get role bindings like users.

## API keys

- Formats: `sk-lw-<16 char lookupId>_<48 char secret>`, `ik-lw-` (ingest), legacy `pat-lw-`. Lookup by `lookupId`, verify `HMAC-SHA256(pepper, secret)` with constant-time compare (`modules/api-key/contract/src/api-key.tokens.ts`, `…/process/src/rules/api-key-token.rules.ts`). Pepper = `CREDENTIALS_SECRET` or `API_KEY_PEPPER`.
- Keys belong to a user (`userId`) and an org, carry `permissionMode` (`all` / `readonly` / `restricted`) and role bindings, `expiresAt`, `lastUsedAt`. Admins can create keys for another user (`createdByUserId`).
- **Legacy project keys are stored in plaintext** (`Project.apiKey @unique`) and compared directly. Old `pat-lw-` keys keep plain SHA-256 until next use.
- SDK telemetry redacts anything matching `sk|pat-lw|lw|vk` prefixes: prefixed keys make leak scanning possible.

## Sessions

Cookie sessions signed with `NEXTAUTH_SECRET`; no detailed public doc on lifetime or revocation.

## Stored secrets

- `CREDENTIALS_SECRET`: 64 hex chars; AES-256-GCM for stored credentials; also HMAC pepper and short-lived token signing. Falls back to `NEXTAUTH_SECRET` if unset.
- Rotation: set `CREDENTIALS_SECRET_PREVIOUS`, run the `credentials-reseal` task, remove the old value. Only the new key writes.
- `.env.example` ships placeholder secrets (`please_please_please_change_me_asap`, all-zero `CREDENTIALS_SECRET`). Helm `autogen` generates secrets but deletes them on uninstall.
- Internal NLP service accepts unauthenticated requests unless `LANGWATCH_NLP_INTERNAL_SECRET` is set.

## MCP

`@langwatch/mcp-server` standalone HTTP mode authorized requests by session id alone and accepted the API key in the URL (GHSA-h25p, see advisories.md). The hosted handler re-checks the key on every request.

## Takeaways for llmobs

- Copy: domain-bound SSO with linking only on verified domain/email; refuse multi-tenant issuers; trusted-IdP allowlist separate from the general SSRF opt-in; prefixed keys with a lookup id; documented secret rotation with a "previous key" window; fallback admin rule.
- Avoid: plaintext legacy keys, placeholder secrets in example config, "first account wins" bootstrap on an exposed host, tokens in URLs, auth by session id alone.
