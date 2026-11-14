# 08 — Authentication, authorization, email

## Bootstrapping

- On first start with an empty `users` table, the UI shows a **setup page** at `/setup?token=…`. While no user exists, `llmobs serve` generates a random setup token at startup and logs that URL once at `warn` (D144). This is the only secret that is ever logged. `POST /api/auth/setup` requires the token. The first user signs up with email/password (or via a configured OIDC provider), becomes `is_instance_admin`, and creates the first org + project (owner role). An API key is shown at the end with copy-paste OTel/SDK snippets.
- Headless alternative: `LLMOBS_INIT_ADMIN_EMAIL`, `LLMOBS_INIT_ADMIN_PASSWORD`, `LLMOBS_INIT_ORG`, `LLMOBS_INIT_PROJECT`, `LLMOBS_INIT_SECRET_KEY` (optional: a pre-chosen `sk` value, for IaC). It runs once, in one transaction, only when `users` is empty, and does not need the setup token. When `LLMOBS_INIT_SECRET_KEY` is set, it must be `sk-llmobs-` plus 32 base62 characters, or startup fails.

## Sign-up policy

| Setting | Default | Meaning |
|---|---|---|
| `LLMOBS_AUTH_SIGNUP` | `invite_only` | `open` \| `invite_only` \| `disabled`. After bootstrap, `invite_only` lets only invited emails register. |
| `LLMOBS_AUTH_SIGNUP_DOMAINS` | empty | With `open`, restrict to these email domains. |
| `LLMOBS_AUTH_DISABLE_PASSWORD` | `false` | Hide and refuse password login/sign-up (SSO only). The CLI admin commands still work. |
| `LLMOBS_AUTH_REQUIRE_EMAIL_VERIFICATION` | `false` | Requires SMTP. |
| `LLMOBS_AUTH_ENFORCE_SSO_DOMAINS` | empty | Users with these email domains must log in through an OAuth/OIDC provider (password login refused). Any configured provider satisfies it. |

The default is `invite_only` (stricter than Langfuse's open default) because a self-hosted instance reachable on the internet should not accept strangers.

An invite token admits its own email only (D147). Signup under `open` with an email already registered answers 409 `conflict` (D145).

## Passwords

- **Hashing:** argon2id, m = 19 456 KiB, t = 2, p = 1, stored as a PHC string. Hashing runs in `spawn_blocking` behind a semaphore of `max(2, cores / 2)` permits (D143). Parameters can be raised; hashes are upgraded on the next successful login.
- **Policy:** NFC-normalized, then 10 to 128 characters counted in Unicode scalar values, not equal to the email, and not in the embedded common-password list (SecLists `10k-most-common.txt`, MIT; entries of 10 characters or more are kept).
- **Unknown email:** login verifies the password against a fixed dummy hash, so its timing matches a real user (D143).
- **Rate limiting and lockout** are in memory; a restart clears them (D143). Login: 10 per minute per (client IP, email). Signup, setup, password forgot and verification resend: 5 per minute per client IP; password forgot and resend also 3 per hour per email. After 20 consecutive failed password logins for one email, password login for that email is locked for 15 minutes. A success or a password reset clears the lockout. Lockout does not block OAuth/OIDC login or password reset.
- **Uniform answers (D145):** login answers the same 401 `unauthorized` for an unknown email, a wrong password, a disabled user, a user without a password and a locked email. Password forgot and verification resend always answer 204.
- **Change:** `POST /api/v1/me/password` takes the current password and the new one, and revokes every other web session and all personal access tokens.
- **Reset with SMTP:** an email with a single-use token valid for 1 h (`password_resets`, stored hashed). Resetting revokes all of the user's web sessions.
- **Reset without SMTP:**
  - `llmobs admin reset-password --email x` prints a reset URL.
  - Instance admins can also generate a reset link in the UI (admin → users) and send it to the user themselves.

## Web sessions

- **Cookie:** `llmobs_session`, an opaque random token of 32 bytes. The DB stores its SHA-256 hex (`web_sessions`).
- **Cookie flags:** `HttpOnly`, `SameSite=Lax`, `Secure` when the public URL is https.
- **Lifetime:** 30 days, sliding, refreshed at most once per hour. Logout deletes the row.
- **Bearer wins (D140):** a bearer token wins over the cookie. When a request has both, the cookie is ignored.
- **CSRF (D141):** every state-changing request (not GET, HEAD, OPTIONS) authenticated by the session cookie, and every state-changing request under `/api/auth/` whatever its credentials (login, signup, setup, password reset, invite accept, logout), must carry an `Origin` equal to the origin of `public_url`. Requests authenticated by a bearer token skip the check. JSON bodies require `Content-Type: application/json`.
- Disabling a user deletes their web sessions. Their personal access tokens fail at once, because every request checks `disabled_at` (D149).

## OAuth / OIDC

Configured in `llmobs.toml` or via env.

```toml
[auth.github]            # OAuth2 (not OIDC); uses the primary *verified* email from /user/emails
client_id = "…"
client_secret = "…"
link_existing_by_email = true    # default true

[auth.google]            # OIDC, issuer https://accounts.google.com
client_id = "…"
client_secret = "…"
link_existing_by_email = true    # default true
allowed_domains = ["acme.com"]   # verified email domain; Google also checks the `hd` claim

[[auth.oidc]]            # any number of generic OIDC providers (Okta, Entra ID, Keycloak, Auth0, Authentik, …)
name = "okta"            # provider id → identities.provider = "oidc:okta"
display_name = "Okta"
issuer = "https://acme.okta.com"
client_id = "…"
client_secret = "…"
scopes = ["openid", "email", "profile"]
link_existing_by_email = false   # default false
allowed_domains = []             # verified email domain
auto_provision = { org = "acme", role = "member" }   # optional: auto-join org on first login
```

- Flow: authorization code + PKCE (S256). `state`, the PKCE verifier and `nonce` travel in a cookie signed with a per-process random key, lasting 10 minutes, `SameSite=Lax`, `Path=/api/auth/callback`. Callback URL: `<public_url>/api/auth/callback/<provider>`. The callback ignores any existing session. It redirects to the relative path given to `GET /authorize/{provider}?redirect=` (the path must start with `/` and not `//`); errors redirect to `/login?error=<code>`. The code is one of `invalid_state`, `provider_error`, `provider_unavailable`, `email_not_verified`, `domain_not_allowed`, `signup_closed`, `account_disabled`, `link_refused`, `setup_required`.
- Account linking (D148): a provider login with a known `(provider, subject)` signs that user in. Otherwise, when a user with the same email exists, the identity is linked only if the provider's `link_existing_by_email` is on, the provider asserts the email verified (a missing claim is false), and the user is not an instance admin. Linking revokes the user's web sessions and personal access tokens, and clears the password when the email was never verified. Otherwise a new user is created, subject to the sign-up policy — an IdP login counts as invited if `auto_provision` is set for that provider.
- `allowed_domains` checks the verified email's domain, and for Google also the `hd` claim.
- Multi-tenant Entra issuers (`common`, `organizations`, `consumers`) are refused at config load. Provider metadata is discovered lazily and cached for one hour.
- `id_token` is validated (`openidconnect` crate: signature via JWKS with caching, `iss`, `aud`, `exp`, `nonce`).
- SAML: not in MVP.

## Authorization (RBAC)

Effective role on a project = `project_members.role` if present, else the org role (`owner` behaves as `admin` at project level). `none` in `project_members` hides the project from that user.

Overrides apply only to org `member`s and `viewer`s (D139). They can raise the project role to `admin` or lower it to `viewer` or `none`. Org owners and admins are never overridden; setting an override for one is a 400. Only a caller with effective role `admin` on the project edits overrides. Nobody grants a role above their own, and only owners grant or change `owner`. An org always keeps at least one owner. Any signed-in user may create an org and becomes its owner. Instance admins get the `/api/v1/admin/*` endpoints only, with no implicit access to org data.

| Action | viewer | member | admin | owner |
|---|:-:|:-:|:-:|:-:|
| View traces, sessions, dashboards, datasets, experiments, SQL console | ✓ | ✓ | ✓ | ✓ |
| Annotate (human scores), add to dataset, edit datasets, run SQL exports (an `sk` key with `read` may export, D-H-5), share saved views (D-H-6) | | ✓ | ✓ | ✓ |
| Manage evaluators, online rules, alert rules, channels, score definitions | | ✓ | ✓ | ✓ |
| Manage API keys, LLM connections, project settings (retention), GDPR deletions | | | ✓ | ✓ |
| Create/delete projects, manage members & invites, org limits | | | ✓ | ✓ |
| Delete org, transfer ownership, change owners | | | | ✓ |
| Instance settings (instance admins only) | — | — | — | — |

LLM connections belong to the org. Org admins and owners manage and test them, and every org member can list them without their keys (D-I-4, D-I-24).

A resource outside the caller's reach answers **404**, never 403, so ids cannot be probed. A visible resource with too low a role answers **403** (D138).

Enforced in one place: every route is declared with its access (public, any signed-in user, session-only, an org permission, a project permission, or instance admin). One middleware enforces it before the handler, resolves `{org}` and `{project}`, and hands the handler the resolved access. The principal is a session user, a PAT user, or an API key.

**Session-only** (a session cookie; a PAT or an API key is refused): creating, listing or revoking API keys and personal access tokens, changing members, roles, overrides or invites, the password and the account name. Exception: a PAT may revoke itself with `DELETE /api/v1/me/tokens/current` (D140). A PAT with scope `read` may call safe methods (GET, HEAD) only, plus the two reads that use POST: `POST /api/v1/projects/{project}/sql` and `POST /mcp` (D-H-4); `read, write` may call everything except those session-only routes.

API keys are project-bound and their scopes map onto permissions: `ingest` (OTLP + scores write), `read` (safe methods of project-scoped routes, and `POST …/sql`, D-H-4), `write` (datasets, experiments, scores, deletions by trace), `feedback` (public feedback only). Track D enforces `read`; later tracks map `ingest`, `write` and `feedback` (D140). Deletion by user or by filter requires project `admin` with a session or a `read, write` PAT, never an API key; deleting a project requires the **org** role `admin` or `owner` (a project-admin override is not enough), and deleting an org requires `owner` (D-M-4).

## API keys

| Kind | Format | Scopes | Use |
|---|---|---|---|
| Secret | `sk-llmobs-` + 32 base62 chars | default `ingest, read, write`; can be `ingest` only | OTel exporters, SDK, CI |
| Public | `pk-llmobs-` + 24 base62 chars | `feedback` | browsers (end-user feedback) |
| Personal access token | `pat-llmobs-` + 32 base62 chars | `read` or `read, write`; acts as the user | CLI, MCP (the only credential `/mcp` accepts, D-H-3) |

- Stored as SHA-256 hex (high-entropy keys don't need a slow hash). The display `prefix` is the type prefix plus the first 6 random characters (for example `sk-llmobs-a1B2c3`) (D140). Shown once at creation.
- Optional `expires_at`. `last_used_at` is coalesced in memory and written every 60 s and at shutdown.
- Header: `Authorization: Bearer <key>`. Credentials are read only from that header, never from a query string. The auth layer caches `hash → principal` for 60 s. Revoking or expiring a key evicts the entry at once in this process.
- CLI login: `llmobs login --url https://…` starts a localhost callback server and opens the browser at `/cli-auth?port=…&state=…`. The callback host is always `127.0.0.1`; only the port comes from the query. After the user confirms, the UI creates a PAT and redirects to the callback. The token is saved in `~/.config/llmobs/credentials.toml` (0600). `llmobs logout` revokes that PAT with `DELETE /api/v1/me/tokens/current`.

## Email

- **Transport:** SMTP only, through `lettre` (async, tokio, rustls).
  - `LLMOBS_SMTP_URL`: `smtp://user:pass@host:587` (STARTTLS required), `smtps://…:465` (implicit TLS), or `smtp://host:25?tls=none` for local relays.
  - `LLMOBS_EMAIL_FROM`: e.g. `"llmobs <noreply@acme.com>"`.
  - Resend, SES, SendGrid and Postmark all work through their SMTP relays.
- **Templates:** `minijinja` templates embedded in `llmobs-server`, sent as multipart/alternative. The HTML is a simple inline-styled layout with no external assets. Links: `verify_email` uses `<public_url>/verify?token=…`, `password_reset` uses `<public_url>/reset?token=…`, and `invite` uses `<public_url>/invite/<token>`.
- **Emails in MVP:**

  | Template | Trigger |
  |---|---|
  | `invite` | member invited (link valid 7 days) |
  | `password_reset` | reset requested |
  | `verify_email` | sign-up when verification is required |
  | `alert` | alert fired / resolved (07) |
  | `test` | "send test email" in instance settings |

- **Without SMTP:**
  - invites show a copyable link in the UI;
  - password resets go through the CLI or an admin-generated link;
  - email alert channels are disabled, with an explanation;
  - email verification cannot be enabled (the setting is refused at startup).
- **Delivery:** transactional emails are sent by a background task after the response is decided (D145), with 3 retries. Alert emails go through `notification_deliveries` (durable). Recipient addresses and contents are never logged; only template name, message ID and status are.

## Auth events log

Not a full audit log (enterprise, later). Logins, failed logins, key creation/revocation, role changes and deletions are emitted as structured `tracing` events at `info` with `target = "audit"`, so operators can ship them. Events carry user, org, project and key ids, never emails, names or token values (D150). A failed login for an unknown email logs `account = "unknown"`.

## Client IP

`trusted_proxies` is a list of CIDRs, empty by default (`LLMOBS_TRUSTED_PROXIES`, D142). The client IP is the socket peer, unless the peer is in the list: then it is the rightmost address of `X-Forwarded-For` that is not itself in the list. Rate limits, lockout keys and `web_sessions.ip` use that address.
