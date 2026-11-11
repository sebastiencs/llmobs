# Auth research: synthesis for spec 08 / 04

Nov 2026. Per-product notes: [langfuse.md](langfuse.md), [phoenix.md](phoenix.md), [langwatch.md](langwatch.md), [opik.md](opik.md). Advisories: [advisories.md](advisories.md). Checked against `docs/spec/08-auth.md`, `04-metadata-db.md`, and the auth parts of `09-api.md` and `12-operations.md`.

## Where we already do better than the competitors

- Auth is always on (Phoenix and Opik default to no auth; that default made their advisories unauthenticated).
- Sign-up defaults to `invite_only` (Langfuse and LangWatch default to open).
- DB-backed opaque sessions, so they can be revoked (Langfuse uses stateless JWTs).
- Password reset works without SMTP, through the CLI or an admin link (Langfuse needs a manual DB procedure).
- PKCE, `state` and `nonce` are always on (Langfuse CVE-2025-65107, Phoenix PKCE opt-in).
- Per-install encryption key with a check value and AAD (Opik ships a hard-coded default key; Langfuse uses no AAD).
- Prefixed keys that can be scanned for. API keys and PATs are hashed (LangWatch still has plaintext legacy keys).

## Adopt

1. **Credentials cannot mint credentials** (Phoenix 19). API keys and PATs can't be created, listed or revoked with a PAT or an API key; only a web session can. `llmobs login` already gets its PAT through the browser.
2. **Assign roles ≤ your own** (Langfuse). An admin can't invite or promote anyone to owner.
3. **Org role `none`** (Langfuse). A user can be limited to a few projects, and new projects stay hidden by default.
4. **Per-provider trust for email linking** (LangWatch domain verification, Phoenix's refusal to link). See gap G1.
5. **Separate trust for operator-configured IdP URLs** (LangWatch `SSO_TRUSTED_IDP_ORIGINS`). An internal Keycloak shouldn't require `allow_private_network_egress`, which would also open webhooks to the private network for every member.
6. **Refuse multi-tenant issuers** for generic OIDC (Entra `common`/`organizations`/`consumers`). The `iss` must equal the configured issuer exactly.
7. **IdP group/role mapping with a strict mode** (Phoenix): post-MVP, but keep `auto_provision` shaped so it can grow into this.
8. **Rotation with a "previous key" window** (LangWatch `CREDENTIALS_SECRET_PREVIOUS`). Our single-transaction re-encrypt is simpler and fine for SQLite. Keep it.
9. **Fallback admin rule** (LangWatch). When `LLMOBS_AUTH_DISABLE_PASSWORD` is set, `llmobs admin reset-password` and an instance admin still work if the IdP is down. The spec already says the CLI works; state it in the UI too.

## Avoid

- Any "auth disabled" or "trusted network" mode (Opik, Phoenix).
- Default credentials (Phoenix `admin/admin`), placeholder secrets in example config (LangWatch), or a static admin bearer secret in env (Phoenix `PHOENIX_ADMIN_SECRET`).
- Signed-JWT API keys or sessions under one master secret (Phoenix GHSA-gfv3: leaking the secret allowed forging tokens).
- Opt-in CSRF (Phoenix's `PHOENIX_CSRF_TRUSTED_ORIGINS`) and opt-in Secure cookies.
- Linking an IdP identity by email alone, or to the currently signed-in user from a callback (Langfuse).
- Credentials in URL query strings (LangWatch MCP).
- Test endpoints that reuse a stored secret against new settings, or return the remote response body (Langfuse, Opik).

## Spec gaps and contradictions

**G1. Account linking is too permissive (08 § OAuth/OIDC).** "Link if `email_verified = true`" trusts every generic OIDC provider to verify emails. Okta, Auth0 or Keycloak tenants with self-registration, misconfigured mappers, or Entra (nOAuth) can assert `email_verified` for any address, including the instance admin's. Proposal:
- Add a per-provider `link_existing_by_email` setting: default `true` for Google (which verifies emails) and GitHub (primary verified email), default `false` for `[[auth.oidc]]`.
- Treat a missing `email_verified` claim as false.
- Never auto-link onto an account with `is_instance_admin`.

**G2. Account pre-hijacking.** With `open` sign-up and `REQUIRE_EMAIL_VERIFICATION=false`, an attacker can register the victim's email with a password. A later verified IdP login then links to that account, and the attacker still knows its password. Rule: when linking onto a user whose `email_verified_at` is NULL, clear `password_hash`, revoke the user's sessions and PATs, and set `email_verified_at`.

**G3. The callback must never link to the session user.** The spec doesn't say what happens when `/callback` arrives while a session cookie is present (the Langfuse CVE). Rule: the callback ignores the existing session. A "connect GitHub to my account" flow, if it exists, starts from an authenticated POST that binds `state` to the session id.

**G4. `allowed_domains` on generic OIDC** must be checked on a verified email, or on a configured claim. For Google, check the `hd` claim (the spec says so), not the email suffix.

**G5. `auto_provision` with no `allowed_domains`** lets anyone who can register at the IdP into the org as `member`. Either require `allowed_domains` (or a group claim) with `auto_provision`, or log a startup warning.

**G6. `LLMOBS_AUTH_ENFORCE_SSO_DOMAINS` doesn't name a provider.** Is a GitHub login with a matching email acceptable? It should map each domain to one provider: `domain → provider id`.

**G7. Setup page race.** On an internet-exposed fresh instance, the first visitor becomes instance admin (the same issue exists in LangWatch and Langfuse). Proposal: `POST /api/auth/setup` requires a one-time setup token printed to the log at startup (Jenkins-style), or listening on loopback only until setup is done. The headless `LLMOBS_INIT_*` path bypasses it.

**G8. Login CSRF and unauthenticated endpoints.** The Origin check applies only to "cookie-authenticated requests", so `/login`, `/signup`, `/setup`, `/password/*` and `/invites/*/accept` are not covered (login CSRF). Apply the Origin check to every state-changing `/api/auth/*` request.

**G9. Cookie plus bearer on the same request.** Define precedence: if `Authorization` is present, the cookie is ignored. Requests with a session cookie and no `Origin` are rejected, even if a bearer is present.

**G10. Disabling a user.** `users.disabled_at` exists but nothing says what it revokes. Rule: disabling deletes web sessions and makes PATs fail at once (bypassing or invalidating the 60 s cache). Project API keys the user created keep working; say so explicitly, as Phoenix's "system keys" do.

**G11. The 60 s auth cache holds roles.** A PAT resolves to a user whose role may be lowered or removed. Either cache only `hash → user_id` and resolve roles live, or invalidate the cache on membership changes. Add a test for "removed from org → PAT denied immediately".

**G12. The API key `prefix` is useless as specified.** "First 12 chars" of `sk-llmobs-…` shows only 2 random characters. Use the type prefix plus 6–8 random characters, and say how the UI disambiguates.

**G13. `LLMOBS_INIT_SECRET_KEY`** must be validated: right prefix, right length, base62. Weak or known values must be refused. Say what happens if the env changes after first start (ignored, with a warning).

**G14. LLM connection and channel edits (Langfuse CVE-2026-41487).** 09 has `PATCH …/llm-connections/{id}` and `POST …/{id}/test`. Rules:
- Changing `base_url`, `adapter` or `extra_headers` requires resubmitting the secret.
- `test` on a saved connection uses only stored, unchanged values.
- Secrets are never returned by any read.

The same applies to `notification_channels` (members can manage channels per 08), whose webhook URL and secret are in `config_enc`.

**G15. Cross-project references in JSON or loose FKs.** These can become IDORs:
- `alert_rules.channels` is a JSON list of channel ids with no FK.
- `online_rules.evaluator_id`, `experiments.dataset_id` and `eval_jobs` aren't constrained to the same project.
- Org-wide `llm_connections` are referenced from evaluators.

Rule: every referenced id is loaded with `AND project_id = ?` (or `org_id`). Add a cross-tenant test per reference.

**G16. Project override semantics are ambiguous.** Spec:
- Can an override **raise** a role (org viewer → project admin)? Langfuse allows it.
- Can an org admin or owner be overridden down to `none`? They could undo it themselves.
- Can a project-level admin manage that project's overrides, or only org admins? 09 exposes `PUT /projects/{p}/members/{user}`.

**G17. Last owner.** Nothing prevents removing, demoting or disabling the last owner of an org. Add the invariant and its test.

**G18. Instance admin scope.** It's unclear whether an instance admin can read every org's data, and how more instance admins are granted (CLI? UI?). The `/api/v1/admin/*` routes need their own permission (Langfuse CVE-2025-59305).

**G19. PAT reach.** Can a `read,write` PAT do admin actions (member management, project deletion, GDPR deletions)? 08 implies yes ("requires admin (PAT or session)"). Proposal: PATs never manage credentials (Adopt #1). Decide explicitly for members and deletions.

**G20. Invites.**
- Must the accepting account's email equal `invites.email`? Proposal: yes for password sign-up. For SSO, require a verified email match.
- No `UNIQUE(org_id, email)` for pending invites, and no revoke column (09 has `DELETE`, so it's a hard delete, which is fine).
- No project-level invite role.
- Behaviour when the invitee already has an account isn't specified.

**G21. Email verification tokens have no table.** 04 has `password_resets` and `invites` only. Generalise to one `user_tokens(token_hash, user_id, purpose, …)` table, or add `email_verifications`. There is also no email-change flow: it needs re-verification and a notice to the old address.

**G22. Token entropy and transport.**
- State the entropy of reset, invite and verify tokens (32 random bytes).
- Tokens travel in URL paths (`GET /invites/{token}`), so request logging must redact those path segments, in line with the AGENTS.md privacy rule.
- Pages that receive a token send `Referrer-Policy: no-referrer`.
- Ban credentials in query strings (LangWatch MCP), including `?token=` for SSE.

**G23. Lockout as DoS.** "20 consecutive failures → account locked 15 min" lets anyone lock out the admin. Make the lockout per (account, IP), or let password reset and SSO bypass it. Say where the counters live (in memory, so lost on restart, which is acceptable). Specify trusted-proxy handling for client IP (`X-Forwarded-For` only from configured proxies), otherwise the per-IP limits are spoofable.

**G24. Enumeration.** Login is covered, but `/signup` ("email taken") and `/password/forgot` must also answer uniformly. Login for an unknown email must run argon2 against a dummy hash, so timing matches.

**G25. Argon2 cost and concurrency.** 19 MiB per hash. Bound concurrent hashes with a semaphore, so a login flood can't exhaust memory. Normalise passwords to NFC before hashing.

**G26. Sessions revoked by other events.** Reset revokes sessions. Password change, email change, disable, and "log out other sessions" should too. `web_sessions.id` is the hash; the UI needs a non-secret id to list and revoke sessions.

**G27. OIDC state cookie key.** "Signed cookie": with which key? Derive per-purpose keys from `LLMOBS_ENCRYPTION_KEY` with HKDF labels (Phoenix): cookie signing, encryption, check value. The cookie must be `SameSite=Lax` (not Strict, or the IdP redirect drops it) and `HttpOnly`, with a ~10 min lifetime, single use.

**G28. AAD should name the column.** AAD = row id alone allows swapping `secret_enc` and `extra_headers_enc` within a row. Use `table || column || id`.

**G29. `secret.key` next to the data.** A backup of `<data_dir>` contains both the key and the ciphertext. Document this and recommend `LLMOBS_ENCRYPTION_KEY` from a secret store in production.

**G30. SQLite `COLLATE NOCASE` is ASCII-only and not Postgres-portable** (04 claims the common subset). Store a normalised lowercase email in its own column with a plain `UNIQUE`.

**G31. Contradiction with D43.** `docs/mvp-decisions.md` D43 names `AUTH_DISABLE_SIGNUP` and "first user = owner", while 08 uses `LLMOBS_AUTH_SIGNUP=open|invite_only|disabled` and makes the first user an instance admin and org owner. Align D43.

**G32. Audit events vs the privacy rule.** Failed logins for unknown emails can't log a user id, and emails must not be logged. Log a keyed hash of the email, or nothing.

**G33. `LLMOBS_PUBLIC_URL` defaults to `http://localhost:8080`** (12). Behind a TLS proxy with the default, the Origin check rejects every browser request and cookies lack `Secure`. Fail loudly at startup, or show a clear error, when the request `Host` doesn't match the public URL.

## Test cases the plan should include

Linking and OIDC:
1. A callback with a valid code but wrong or missing `state` is rejected. A callback for a signed-in user B with attacker A's code still never links A's identity to B.
2. A generic OIDC provider returns `email_verified=true` for an existing password user's email, with `link_existing_by_email=false`: a new user is refused or created, never linked.
3. Pre-hijack: an unverified password account plus a verified IdP login gives a linked account, the password cleared, and old sessions revoked.
4. A missing `email_verified` claim is treated as false. An `iss` mismatch and Entra `common` issuer are rejected. A wrong `aud`, expired token, or replayed `nonce` are rejected.
5. Google `allowed_domains`: an email in the domain but a wrong `hd` is rejected.

Authorization:
6. Cross-tenant matrix: for every route in the router, a principal from org B gets 404/403 on org A's ids. Generate the matrix from the route table, so new routes are covered automatically.
7. Every route declares a permission. A test fails on any route without one, including `/api/v1/admin/*` (instance admin only).
8. Cross-project references: an alert rule pointing at another project's channel, a rule pointing at another project's evaluator, or an experiment using another project's dataset → 404.
9. Role ceiling: an admin can't invite an owner or promote to owner. The last owner can't be removed, demoted or disabled.
10. Override semantics: per G16 once decided, plus a `none` override hiding the project from lists, search, SQL and SSE.
11. A viewer gets 403 on every mutating route. A member gets 403 on API keys, LLM connections, retention and GDPR deletions.

Credentials:
12. A PAT or API key can't create, list or revoke API keys or PATs. A PAT with only `read` is denied on writes.
13. Revocation and disable: a revoked key works ≤ 60 s. A disabled user's PAT and session fail immediately. A removed member's PAT fails immediately.
14. Keys in query strings are rejected on `/api`, `/mcp` and SSE. Bearer plus cookie: the cookie is ignored.
15. Hashes only: after creation, no API response, log line or DB column contains the plaintext key. Read the DB in the test.
16. The `LLMOBS_INIT_SECRET_KEY` value is validated. Init runs once, in one transaction: a crash mid-init leaves an empty DB.

Web:
17. The Origin check covers `/login`, `/signup`, `/setup`, `/password/*` and invite accept. A cookie request with a foreign or absent `Origin`, or a non-JSON content type, is rejected.
18. Session cookie flags (`HttpOnly`, `SameSite=Lax`, `Secure` with an https URL). Sliding expiry is refreshed at most hourly. Logout deletes the row. Reset and password change revoke all sessions.
19. The setup endpoint refuses without the setup token, and refuses once any user exists, including during concurrent setup attempts.

Abuse:
20. Uniform responses and similar timing for login, sign-up and forgot-password with known vs unknown emails. Rate limits trigger. Lockout can't be triggered from a different IP (per G23).
21. Reset, invite and verify tokens are single-use, expire, are hashed in the DB, and are redacted from request logs.

Outbound and secrets:
22. An LLM connection PATCH changing `base_url` without a new secret is rejected. The test endpoint never sends the stored key to a new URL. Webhook and connection tests go through the SSRF guard (loopback, link-local, 169.254.169.254, DNS rebinding, redirect to private) and never return the remote body.
23. Encryption: a wrong key at startup fails clearly. Ciphertext swapped between rows or columns fails to decrypt (AAD). Rotation re-encrypts everything, and a crash mid-rotation leaves the old key valid.
24. Judge and prompt templates can't reach attributes, env or files (Phoenix and Opik template advisories).
