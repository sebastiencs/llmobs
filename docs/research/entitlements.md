# Plans, entitlements and licence keys in competitors

Read on 2026-11-14 for track P (`plans/p-entitlements/00-design.md`). Design facts only; no code was copied.

## Langfuse

- **Open core.** The root licence is MIT except `ee/`, `web/src/ee/`, `worker/src/ee/` and `packages/shared/src/server/ee/`, which use Langfuse's own enterprise licence: free for development and testing, production needs a licence, no redistribution. Core imports `ee` modules directly.
- **Licence key.** An opaque string recognised by its prefix (`langfuse_ee_`, `langfuse_pro_`, …) in `LANGFUSE_EE_LICENSE_KEY`. No signature, no expiry, no phone-home: anyone can forge one.
- **Plans.** Code constants: plan names (`oss`, `cloud:hobby`, `cloud:pro`, `self-hosted:enterprise`, …) map to features (`rbac-project-roles`, `audit-logs`, `data-retention`, `cloud-multi-tenant-sso`, …) and limits (`organization-member-count`, `data-access-days`, …). On the cloud the plan comes from a manual override, then the billing product, then the Stripe product, written to the org by the Stripe webhook. The plan is copied into the session JWT, so it goes stale until the session refreshes.
- **Enforcement.** A worker counts events per billing cycle, emails at thresholds and blocks the free tier at 5× its allowance; ingest then answers 403 "usage threshold exceeded". The block reaches the API through a 60 s API-key cache. Seats are checked at invite. The data-access window clamps query start dates (hidden, not deleted); deletion is a separate `data-retention` feature. The UI shows a lock icon and "limit reached (x/y)".

## LangWatch

- **Open core.** Apache-2.0 core; `enterprise/` under LangWatch's own licence: production allowed while the licence checks stay intact, no SaaS offering, swapping the public key or circumventing checks voids the grant. Core reaches enterprise modules through dependency-injected module APIs.
- **Licence key.** Base64 JSON `{data, signature}` with an RSA-SHA256 signature over the re-serialised `data`, verified offline against an embedded public key (overridable by env), with a revocation list. The key carries an org binding, an expiry and plan limits. An invalid or expired key falls back to the uncapped open-source plan: only enterprise features turn off. Re-serialising JSON for verification forces append-only optional fields.
- **Plans.** Code catalogue (members, monthly volume, automation dispatches) and enterprise gates (`rbac`, `auditLogs`, `scim`, …); the cloud plan comes from Stripe webhooks.
- **Enforcement.** Ingest over the monthly volume (UTC calendar month) answers **402** `ERR_PLAN_LIMIT`, chosen so SDKs do not retry. Usage reads go through a 30 s in-process cache and fail open. Free data older than 14 days is redacted in the UI, not deleted.

## Opik

- Whole repository Apache-2.0; commercial logic lives in a closed service reached over HTTP, and a frontend plugin folder adds billing links and upgrade buttons.
- No licence key. On auth, the remote service returns quotas (only a span count); an interceptor answers **402** when one is reached. Retention is a UI banner with an upgrade link and an 80 % warning. The Python SDK stops retrying a 429 after 60 s to avoid endless re-queueing.

## Laminar

- Whole repository Apache-2.0; gates switched on by an environment variable.
- Plans are a database table (`subscription_tiers`: retention days, byte limits, Stripe product, overage prices); the Stripe webhook sets the workspace's tier and reset time.
- Free and trial workspaces get a hard byte cap per billing cycle, read from a 24 h Redis cache (so the cap overshoots), answered with 403; usage read failures count as zero (fail open). Free gets 1 project and 1 seat. Retention is a UI date clamp.

## What track P takes from this

- **Sign keys offline with an asymmetric key, and verify the received bytes** (no re-serialisation), with an expiry and a grace period; on expiry degrade to the open-source behaviour, never lock out (LangWatch; D-P-5, D-P-7).
- **Never accept a configurable verification key** in release builds (LangWatch's override would let anyone mint keys; D-P-6).
- **402 for the monthly cap**, so exporters do not retry and it is not read as an auth error (LangWatch, Opik; D-P-17). A rate limit stays 429.
- **Hide data outside the window, delete later** (Langfuse's `data-access-days` vs `data-retention`; D175, D-P-21, D-P-22).
- **No stale entitlements in sessions or long caches:** the cell resolves entitlements in memory at every check, and an org token push swaps them at once (Langfuse's JWT, Laminar's 24 h cache; D-P-10, D-P-11).
- **A standard licence text** rather than a custom one: ELv2 has LangWatch's protections without a text of our own (D-P-2).
