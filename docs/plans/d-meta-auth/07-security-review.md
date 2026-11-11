# Part 7 · Independent security review (P26)

## P26 — Security review

**Goal:** a model that did not implement P1–P25 reviews the auth code against spec 08, the decisions D133–D149 and the checklist below, writes the findings, and fixes only what is small and certain.

**Who:** a different model (or a different vendor's model) from the one that implemented P1–P25. Start from a fresh session with no memory of the implementation.

**Edit only:** `docs/research/auth/review-<date>.md` (new; the findings), and, for fixes, the files the finding names inside `crates/server/` and `crates/cli/`. A fix that changes behaviour, a spec, or more than about 50 lines is **not** made here: it is written as a finding with a proposed sub-phase.

**Commit:** `server: fix findings of the auth security review` (or `docs: record the auth security review` when nothing was fixed).

### How to review

1. Read `AGENTS.md`, spec 04 § Identity & tenancy and § Keys & credentials, spec 08, spec 09 § Conventions and the auth/org/admin endpoints, D133–D149, and `docs/research/auth/README.md` and `advisories.md`.
2. Read `crates/server/src/{auth,http,routes,meta,email,bootstrap.rs,admin.rs,serve.rs,config.rs}` and `crates/cli/src/login.rs` in full. Do not rely on test names; read the code paths.
3. Go through every checklist item below. For each, record **OK** with the file and line that shows it, or a **finding**.
4. Try to break it: write throw-away tests (kept only if they reveal a bug; then they stay as regression tests with the fix or the finding).
5. Run `just check`.

### Checklist

**Authentication**
1. Every credential source: cookie, `Bearer` sk/pk/pat. No other header, no query parameter, no form field authenticates (`?token=`, `?api_key=`, `X-Api-Key`).
2. Bearer present → cookie ignored entirely (including CSRF decisions and session refresh).
3. Session tokens, API keys, PATs, reset, verification, invite and setup tokens: ≥ 128 bits from the OS CSPRNG; only SHA-256 hashes stored; compared by hash lookup or constant-time equality.
4. Disabled users: sessions deleted, PATs rejected on the next request, login refused with the uniform 401, OAuth refused.
5. Expiry checked on every use (keys and PATs even when cached); revocation through the API evicts caches immediately.
6. Password hashing parameters, rehash on login, NFC, policy, dummy hash for unknown users: one argon2 verification per login attempt in every branch.
7. Login, forgot and resend answers do not depend on whether the account exists (status, body, headers, and no SMTP on the request path).
8. Rate limits and lockout keyed as D141; lockout cannot lock out OAuth login or password reset; memory bounded.
9. Setup: token required, single use, constant-time compare, concurrency-safe (one transaction re-checking `users`), refused once a user exists; headless init runs once and validates `LLMOBS_INIT_SECRET_KEY`.

**Authorization**
10. Every route in the router comes from `routes()`; nothing is mounted outside the table (search for `.route(`, `.nest(`, `.merge(` outside `build_router`).
11. `enforce` runs before body extraction for every route; handlers never re-derive the org or project from the body or query instead of the path.
12. Every handler that takes an id in the path (`{id}`, `{user}`) checks that the row belongs to the org or project resolved by `enforce` (IDOR: API key ids, invite ids, member ids, override user ids, admin user ids).
13. 404 for invisible resources and 403 for visible-but-forbidden, consistently; no error message reveals another tenant's names.
14. Role ceilings, owner-only changes, last-owner protection, last-instance-admin protection, overrides never applied to org owners/admins.
15. Session-only routes reject PATs and API keys; read-only PATs reject non-safe methods; API keys only reach `Project` routes of their own project with the right scope.
16. Instance admins have no implicit org access.
17. P18's suite covers every route currently in the table (re-run it; look for routes added after P18 and missing request shapes).

**Web**
18. CSRF: Origin check on every cookie-authenticated state change and every state change under `/api/auth/`; exact origin comparison; behaviour behind a proxy (`public_url` must be set) documented.
19. Cookies: flags; `Secure` with https; the OAuth state cookie's path, lifetime and signature; sliding refresh at most hourly.
20. Redirect targets (OAuth `redirect`, CLI callback) cannot point off-site.
21. `X-Forwarded-For` is only trusted from `trusted_proxies`.

**OAuth/OIDC**
22. `state`, PKCE and `nonce` always on; the callback ignores existing sessions; id_token checks (signature, `iss`, `aud`, `exp`, `nonce`); unverified emails never create or link accounts.
23. Linking rules of D146, pre-hijack clearing, instance admin never linked, Entra multi-tenant issuers refused, `hd` checked for Google domains.
24. The HTTP client for providers does not follow redirects; timeouts set.

**Data and logs**
25. No email, name, password, token, key or SMTP credential in logs, problem details, audit events or panics (search for `tracing::` and `format!` in the reviewed files; check `Debug` impls of types holding secrets).
26. Request logs use route templates; tokens in paths (`/invites/{token}`) are not logged.
27. SQL: only bound parameters, never string-built SQL with request values.
28. Uniqueness and normalization of emails (NFC + lowercase) used on every lookup path (login, signup, invite, OAuth, admin, CLI).
29. Config: secrets wrapped in `Secret`; config errors name keys only.

**CLI**
30. `credentials.toml` mode 0600, directory 0700; the login callback validates `state`, binds 127.0.0.1 only, and never prints the token.

### Findings file

`docs/research/auth/review-<YYYY-MM-DD>.md`: one table with `#`, checklist item, severity (critical / high / medium / low / note), file:line, description, status (fixed in this commit / proposed sub-phase). Then one short section per finding: how to reproduce, the fix or the proposed change. Severities: critical = cross-tenant access, account takeover or auth bypass; high = credential exposure or privilege escalation inside a tenant; medium = defence-in-depth gaps; low = hardening.

### Report

The number of findings per severity, what was fixed, and what needs a follow-up sub-phase. Any critical or high finding that is not fixed blocks marking track D done.
