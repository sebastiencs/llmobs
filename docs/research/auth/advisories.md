# Security advisories in competing tools (auth, authz, secrets, SSRF)

Collected Nov 2026 from the GitHub Security Advisories of each repo (`gh api repos/<repo>/security-advisories`), OSV (https://osv.dev) and NVD. At that date the repos had published: langfuse/langfuse 4, Arize-ai/phoenix 1, langwatch/langwatch 1, comet-ml/opik 3, lmnr-ai/lmnr 0, Helicone/helicone 0. Langfuse also has CVEs without a repo GHSA. Lunary (a smaller LLM-observability tool) is added because its huntr reports are typical IDORs.

URL patterns: `https://github.com/<repo>/security/advisories/<GHSA>` and `https://nvd.nist.gov/vuln/detail/<CVE>`.

## Table

| Product | ID | Published | What went wrong | Lesson for llmobs |
|---|---|---|---|---|
| Langfuse | GHSA-w9pw-c549-5m6w / CVE-2025-65107 | 2025-11-19 | SSO providers without an explicit `AUTH_<PROVIDER>_CHECKS` ran with no `state`/PKCE check. An attacker aborts their own IdP login, then makes a signed-in victim open the callback URL with the attacker's code: the attacker's IdP account gets linked to the victim's account (login CSRF → takeover). | `state` + PKCE + `nonce` always on, not configurable. The callback must verify `state` against a browser-bound cookie and **never link an identity to the currently signed-in user**; linking to a session user only happens in an explicit "connect account" flow started by that user. |
| Langfuse | GHSA-94hf-6gqq-pj69 / CVE-2025-64504 | 2025-11-09 | Membership APIs (`members.allFromProject`, `allInvitesFromProject`) checked permissions against a client-supplied `orgId` that did not have to match the project: any user could list names/emails of another org. | Derive the org/project from the resource server-side and authorize on that. Never accept a parent id from the request as the authorization scope when the child id determines it. Test every route with a principal from another org. |
| Langfuse | GHSA-pvq7-vvfj-p98x / CVE-2026-24055 | 2026-01-21 | `/api/public/slack/install` started Slack OAuth for any `projectId` without auth; the callback trusted the projectId carried through the flow. Attackers could bind their Slack workspace to any project. | Any third-party OAuth "connect" flow must require a session with the right project permission at start, and bind `state` to that session + project, re-checked at callback. |
| Langfuse | GHSA-2524-j966-gfgh / CVE-2026-41487 | 2026-04-17 | A project "member" could update an LLM connection's `baseUrl` to an attacker host; the test request reused the stored provider key and sent it there. | Changing `base_url`/adapter/headers must require re-entering the secret (or admin role). Test endpoints use the submitted config only. Stored secrets are write-only in the API. |
| Langfuse | CVE-2025-59305 | 2025-09-24 | Background-migration tRPC endpoints (`backgroundMigrations.all/status/retry`) were callable by any authenticated user. | Instance-level operations need an explicit instance-admin check. Make "no permission declared" a build/test failure: a test enumerates every route and asserts it declares a permission. |
| Langfuse | CVE-2025-9799 | 2025-09-01 | SSRF in the prompt-change webhook handler (`promptRouter.ts`). | All user-supplied outbound URLs go through the SSRF guard, checked on the resolved IP at connect time and on every redirect. |
| Phoenix | GHSA-gfv3-m769-5xm3 / CVE-2026-53998 | 2026-11-09 | Python format-string injection in the `applyChatTemplate` GraphQL query let any caller (unauthenticated with default no-auth) read `os.environ`, including `PHOENIX_SECRET`, which signs all JWTs and derives the key encrypting stored credentials. | One master secret should not both sign sessions and encrypt data; derive per-purpose keys (HKDF labels). Prefer opaque DB-backed tokens: a leaked key then cannot forge sessions. Templates (judge prompts) must not allow attribute/index access into server objects. Keep secrets out of the process env after startup where practical. |
| LangWatch | GHSA-h25p-f8f6-2ccf | 2026-08-19 | Standalone MCP server (`--http`) authorized requests by MCP session id alone (no re-check of the key), and accepted the API key in the URL query string (leaks to proxy logs, Referer, shell history). | MCP and SSE endpoints re-authenticate the bearer on every request; session ids are not credentials. Refuse credentials in query strings everywhere. |
| Opik | GHSA-87xc-vmvv-2g6f | 2026-09-18 | Alert webhook **test** endpoint had no permission check and returned the remote response body; with Opik's default no-auth, unauthenticated full-read SSRF (cloud metadata, internal services). | No "auth disabled" mode. Test endpoints carry the same permission as edit, go through the SSRF guard, and return only status/latency, never the remote body. |
| Opik | GHSA-2xwj-cg3g-7hxg | 2026-11-05 | ClickHouse `log_comment` label built from request input with incomplete escaping (`\` not escaped) → string-literal injection. | Never assemble SQL text from input, even comments/labels; use parameters or a fixed whitelist. Relevant to our SQL console and DataFusion settings. |
| Opik | GHSA-5mc6-2qfj-59cg | 2026-11-05 | SDKs rendered Jinja2/Nunjucks prompts from a shared workspace without a sandbox → code execution in the user's app. | Prompt/judge templates use a logic-less or sandboxed engine on the server and in SDKs. Anyone with `member` can edit them, so they are untrusted input. |
| Lunary | CVE-2024-1625 | 2024 | Authenticated user could delete another org's project by id. | Cross-tenant tests for every mutating route (see README test list). https://nvd.nist.gov/vuln/detail/CVE-2024-1625 |
| Lunary | CVE-2024-5128 | 2024 | IDOR (object accessed by id without ownership check). | Same. https://nvd.nist.gov/vuln/detail/CVE-2024-5128 |
| Lunary | CVE-2024-7473 | 2024 | Changing an id parameter let a user edit other users' prompts. | Load objects through queries scoped by the authorized project id (`WHERE id = ? AND project_id = ?`), not by id alone. https://nvd.nist.gov/vuln/detail/CVE-2024-7473 |

## Related public research (not advisories of these tools)

- **nOAuth** (Descope, 2023): apps that identified or linked Entra ID users by the mutable, unverified `email` claim allowed takeover. Lesson: identify by `(issuer, sub)`; only link by email when the provider is trusted to verify emails. https://www.descope.com/blog/post/noauth
- **Account pre-hijacking** (Sudhodanan & Paverd, USENIX Security 2022): attacker registers the victim's email with a password before the victim arrives via SSO; the "classic-federated merge" then gives the attacker a password on the victim's account. Lesson: when linking a verified IdP identity onto an account whose email was never verified, drop the password and sessions (or refuse). https://www.usenix.org/conference/usenixsecurity22/presentation/sudhodanan

## Patterns

1. **Authorization scoped by a client-supplied parent id** (Langfuse orgId, Lunary): 4 of 14 entries.
2. **OAuth flows without session-bound state** (Langfuse SSO and Slack): takeover or cross-project binding.
3. **Outbound requests from "test" endpoints** (Langfuse LLM test, Opik webhook test, Langfuse webhook): SSRF and secret exfiltration.
4. **Auth-off defaults** turn every bug above into an unauthenticated one (Phoenix, Opik).
5. **Template engines** that can reach server objects (Phoenix, Opik).
