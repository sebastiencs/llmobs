# 08 — Authentication, authorization, email

## Bootstrapping

- On first start with an empty `users` table, the UI shows a **setup page**: the first user signs up with email/password (or via a configured OIDC provider), becomes `is_instance_admin`, and creates the first org + project (owner role). An API key is shown at the end with copy-paste OTel/SDK snippets.
- Headless alternative: `LLMOBS_INIT_ADMIN_EMAIL`, `LLMOBS_INIT_ADMIN_PASSWORD`, `LLMOBS_INIT_ORG`, `LLMOBS_INIT_PROJECT`, `LLMOBS_INIT_SECRET_KEY` (optional: a pre-chosen `sk` value, for IaC) — applied once when the DB is empty.

## Sign-up policy

| Setting | Default | Meaning |
|---|---|---|
| `LLMOBS_AUTH_SIGNUP` | `invite_only` | `open` \| `invite_only` \| `disabled`. After bootstrap, `invite_only` lets only invited emails register. |
| `LLMOBS_AUTH_SIGNUP_DOMAINS` | empty | With `open`, restrict to these email domains. |
| `LLMOBS_AUTH_DISABLE_PASSWORD` | `false` | Hide and refuse password login/sign-up (SSO only). The CLI admin commands still work. |
| `LLMOBS_AUTH_REQUIRE_EMAIL_VERIFICATION` | `false` | Requires SMTP. |
| `LLMOBS_AUTH_ENFORCE_SSO_DOMAINS` | empty | Users with these email domains must log in through OIDC (password login refused). |

The default is `invite_only` (stricter than Langfuse's open default) because a self-hosted instance reachable on the internet should not accept strangers.

## Passwords

- **Hashing:** argon2id with the OWASP parameters (m = 19 MiB, t = 2, p = 1), stored as a PHC string. Parameters can be raised; hashes are upgraded on the next successful login.
- **Policy:** 10 to 128 characters, not equal to the email, and not one of the 10 000 most common passwords (an embedded list).
- **Rate limiting:** 10 login attempts per minute per (IP, email). After 20 consecutive failures, the account is locked for 15 minutes. The error response is identical whether or not the account exists.
- **Reset with SMTP:** an email with a single-use token valid for 1 h (`password_resets`, stored hashed). Resetting revokes all of the user's web sessions.
- **Reset without SMTP:**
  - `llmobs admin reset-password --email x` prints a reset URL.
  - Instance admins can also generate a reset link in the UI (admin → users) and send it to the user themselves.

## Web sessions

- **Cookie:** `llmobs_session`, an opaque random token of 32 bytes. The DB stores its SHA-256 (`web_sessions`).
- **Cookie flags:** `HttpOnly`, `SameSite=Lax`, `Secure` when the public URL is https.
- **Lifetime:** 30 days, sliding, refreshed at most once per hour. Logout deletes the row.
- **CSRF protection:** state-changing requests must be `application/json`, and their `Origin` must match `LLMOBS_PUBLIC_URL` (or be absent for non-browser clients authenticated by bearer token). Cookie-authenticated requests without a matching `Origin` are rejected.

## OAuth / OIDC

Configured in `llmobs.toml` or via env.

```toml
[auth.github]            # OAuth2 (not OIDC); uses the primary *verified* email from /user/emails
client_id = "…"
client_secret = "…"

[auth.google]            # OIDC, issuer https://accounts.google.com
client_id = "…"
client_secret = "…"
allowed_domains = ["acme.com"]   # optional `hd` check

[[auth.oidc]]            # any number of generic OIDC providers (Okta, Entra ID, Keycloak, Auth0, Authentik, …)
name = "okta"            # provider id → identities.provider = "oidc:okta"
display_name = "Okta"
issuer = "https://acme.okta.com"
client_id = "…"
client_secret = "…"
scopes = ["openid", "email", "profile"]
allowed_domains = []
auto_provision = { org = "acme", role = "member" }   # optional: auto-join org on first login
```

- Flow: authorization code + PKCE (S256), `state` and `nonce` kept in a short-lived signed cookie. Callback URL: `<public_url>/api/auth/callback/<provider>`.
- Account linking: if an identity is new and the provider asserts `email_verified = true`, it is linked to the existing user with that email; otherwise a new user is created (subject to the sign-up policy — an IdP login counts as invited if `auto_provision` is set for that provider).
- `id_token` is validated (`openidconnect` crate: signature via JWKS with caching, `iss`, `aud`, `exp`, `nonce`).
- SAML: not in MVP.

## Authorization (RBAC)

Effective role on a project = `project_members.role` if present, else the org role (`owner` behaves as `admin` at project level). `none` in `project_members` hides the project from that user.

| Action | viewer | member | admin | owner |
|---|:-:|:-:|:-:|:-:|
| View traces, sessions, dashboards, datasets, experiments, SQL console | ✓ | ✓ | ✓ | ✓ |
| Annotate (human scores), add to dataset, edit datasets, run SQL exports | | ✓ | ✓ | ✓ |
| Manage evaluators, online rules, alert rules, channels, score definitions | | ✓ | ✓ | ✓ |
| Manage API keys, LLM connections, project settings (retention), GDPR deletions | | | ✓ | ✓ |
| Create/delete projects, manage members & invites, org limits | | | ✓ | ✓ |
| Delete org, transfer ownership, change owners | | | | ✓ |
| Instance settings (instance admins only) | — | — | — | — |

Enforced in one place: every handler declares `require(ProjectPerm::X)` / `require(OrgPerm::Y)`; the extractor resolves the principal (session user, PAT user, or API key) and checks. API keys are project-bound and their scopes map onto permissions: `ingest` (OTLP + scores write), `read` (all read endpoints, SQL), `write` (datasets, experiments, scores, deletions by trace), `feedback` (public feedback only). Deletion by user/project still requires `admin` (PAT or session), not an API key.

## API keys

| Kind | Format | Scopes | Use |
|---|---|---|---|
| Secret | `sk-llmobs-` + 32 base62 chars | default `ingest, read, write`; can be `ingest` only | OTel exporters, SDK, CI |
| Public | `pk-llmobs-` + 24 base62 chars | `feedback` | browsers (end-user feedback) |
| Personal access token | `pat-llmobs-` + 32 base62 chars | `read` or `read, write`; acts as the user | CLI, MCP |

- Stored as SHA-256 hex (high-entropy keys don't need a slow hash); the first 12 chars kept as `prefix` for display. Shown once at creation.
- Optional `expires_at`; `last_used_at` updated at most once per minute (in-memory coalescing).
- Header: `Authorization: Bearer <key>`. The auth layer caches `hash → principal` for 60 s (revocation takes effect within 60 s).
- CLI login: `llmobs login --url https://…` starts a localhost callback server, opens the browser at `/cli-auth?port=…&state=…`; after the user confirms, the UI creates a PAT and redirects to the callback. The token is saved in `~/.config/llmobs/credentials.toml` (0600).

## Email

- **Transport:** SMTP only, through `lettre` (async, tokio, rustls).
  - `LLMOBS_SMTP_URL`: `smtp://user:pass@host:587` (STARTTLS required), `smtps://…:465` (implicit TLS), or `smtp://host:25?tls=none` for local relays.
  - `LLMOBS_EMAIL_FROM`: e.g. `"llmobs <noreply@acme.com>"`.
  - Resend, SES, SendGrid and Postmark all work through their SMTP relays.
- **Templates:** `minijinja` templates embedded in the binary (`llmobs-mail/templates/<name>.{html,txt}.j2`), sent as multipart/alternative. The HTML is a simple inline-styled layout with no external assets.
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
- **Delivery:** transactional emails are sent inline with 3 retries; alert emails go through `notification_deliveries` (durable). Recipient addresses and contents are never logged; only template name, message ID and status are.

## Auth events log

Not a full audit log (enterprise, later). Logins, failed logins, key creation/revocation, role changes, deletions are emitted as structured `tracing` events at `info` with `target = "audit"`, so operators can ship them.
