# Opik (Comet): self-hosted auth

Sources (Nov 2026):
- Self-host overview: https://www.comet.com/docs/opik/self-host/overview
- Local checkout `/home/sebastien/github/opik`: `apps/opik-backend/config.yml`, `apps/opik-backend/src/main/java/com/comet/opik/infrastructure/auth/`, `…/infrastructure/EncryptionUtils.java`. Nothing copied.

## Summary

Open-source Opik has **no user management**. The docs say self-hosting gives all features "without user management". Auth, users, workspaces and API keys live in Comet's closed-source platform; the OSS backend only calls out to it.

## How auth works

- `authentication.enabled: ${AUTH_ENABLED:-false}`. Default **off**.
- With auth off, every request is accepted if it names the `default` workspace (`AuthServiceImpl.authenticate`): any network client is a full user. GHSA-87xc (SSRF) was reachable unauthenticated because of this.
- With auth on, `RemoteAuthService` sends the `Authorization` header (API key) or session cookie plus the `Comet-Workspace` header to a "react service" (`REACT_SERVICE_URL`, Comet's user service), which returns the user, workspace and permissions. Results are cached (`AUTH_API_KEY_RESOLUTION_CACHE_TTL_IN_SEC`, default 5 s).
- Endpoints declare required permissions (`RequiredPermissions`), resolved per workspace. The webhook-test endpoint lacked this check (GHSA-87xc).
- No OIDC/SSO, sign-up policy, password rules, roles or PATs in OSS; all of it is in Comet Enterprise.

## API keys

- Header `Authorization: <api key>` plus `Comet-Workspace: <name>`; keys are Comet account keys, validated remotely. Hashing/rotation is not visible in OSS.

## Stored secrets

- LLM provider keys are encrypted with `OPIK_ENCRYPTION_KEY`, whose **default is a hard-coded 16-byte string in `config.yml`**. Older code path uses `Cipher.getInstance("AES")` (Java default = AES/ECB/PKCS5Padding: deterministic, no integrity); a newer AES-GCM path exists for structured auth configs.
- Every install that keeps the default shares the same key, so a DB dump decrypts anywhere.

## MCP

`MCP_OAUTH_ENABLED` (default false) for the hosted MCP server; self-hosted local MCP runs over stdio with "no browser sign-in".

## Advisories (details in advisories.md)

- GHSA-87xc-vmvv-2g6f: SSRF via alert webhook test endpoint, missing permission check, response body returned to caller.
- GHSA-2xwj-cg3g-7hxg: incomplete escaping in a ClickHouse `log_comment` string built from user input.
- GHSA-5mc6-2qfj-59cg: SDKs rendered Jinja2/Nunjucks prompt templates without a sandbox (RCE in the SDK user's process).

## Takeaways for llmobs

- An "auth disabled" mode turns every missing-authz bug into an unauthenticated one. Do not offer one; keep auth always on, even on localhost.
- Never ship a default encryption key. Generate per install (our spec already does) and refuse known/placeholder values.
- Use an AEAD with random nonce for every encrypted column; no deterministic modes.
- Test endpoints that make outbound requests (webhook test, LLM connection test) need the same permission as editing the object, must go through the SSRF guard, and must not echo the remote response body.
- Do not build SQL fragments (even comments/labels) by string concatenation from request input.
