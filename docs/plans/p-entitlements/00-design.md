# P · Plans and entitlements — design

Track P builds the in-binary side of the cloud and open-core decisions (D169–D179):
- the open-core split: `ee/` directories under the Elastic License 2.0, next to the MIT core (D172);
- offline licence keys for self-hosted instances (D172);
- one resolved `Entitlements` value per org, read by every check (D179);
- the signed per-org entitlements token through which a cloud cell learns an org's plan from the control plane (D170, D171);
- enforcement: monthly units and the Free cap (D173, D174), ingest rate limits, seats and projects (D176), the hidden data-access window and the retention cap (D175), rule counts and feature gates (D178);
- the usage page and API;
- per-org SSO for the Teams add-on (D177).

The control plane itself (accounts, Stripe, cells, licence issuing) is track Q. This document defines every interface Q uses (§ Interfaces for track Q).

This document holds the decisions, the architecture, the interfaces and the milestones. Each milestone gets its own plan of small sub-phases. GLM implements them one at a time, with no other context, so the plans leave nothing to guess:
- [`01-m1-open-core-license.md`](01-m1-open-core-license.md): written.
- [`02-m2-org-entitlements.md`](02-m2-org-entitlements.md): written.
- `03-m3-units-and-limits.md` … `05-m5-org-sso.md`: not written yet (§ Milestones).

**Read first:** `AGENTS.md`; `docs/mvp-decisions.md` (D9, D57, D136–D143, D169–D179, Track G, Track L, Track I, § Track P); specs 04, 05, 08, 09, 11 and 12; G's design [`../g-spine/00-design.md`](../g-spine/00-design.md); the research note [`../../research/entitlements.md`](../../research/entitlements.md).

**Spelling.** Prose says "licence" (the noun). Every identifier, file name, config key, env var and JSON field says `license`, as D172's `LLMOBS_LICENSE_KEY` does: `license_key`, `ee/license.rs`, `LICENSE`, `GET /api/v1/admin/license`.

## Decisions

The user chose each option marked "(user)" on 2026-11-14. The others follow from those, from D169–D179 or from the code. They are recorded in `docs/mvp-decisions.md` § Track P. Where a decision changes a spec or an earlier decision, it says so.

### Open core

| # | Decision |
|---|---|
| D-P-1 | **`ee` directories inside the existing crates (user).** Enterprise code lives in `crates/server/src/ee/` (module `crate::ee`), `web/src/ee/` and `tools/license/` (the dev signing tool). There is no `llmobs-ee` crate: enterprise features are glue over `AppState`, `meta.db` and the route table, so a crate below `server` could hold only pure logic, and a crate above it would need hook traits with one implementation, which `AGENTS.md` forbids. Core code calls into `ee` directly (as Langfuse's does). Everything else stays MIT. |
| D-P-2 | **Elastic License 2.0 for `ee` (user).** Each `ee` directory has a `LICENSE` file with the ELv2 text. ELv2 forbids offering the software as a managed service and moving, changing, disabling or circumventing licence-key functionality, which is what keeps enterprise features paid; it is a standard text, so there is no custom legal text to maintain. The root `LICENSE` is MIT and says which directories it does not cover. Every source file under an `ee` directory starts with an SPDX line (`// SPDX-License-Identifier: Elastic-2.0` in Rust and TypeScript); no file outside them has it. `just check-ee` checks both, and that each `ee` directory's `LICENSE` has the ELv2 text's SHA-256, and runs in `just check`. Langfuse and LangWatch wrote their own texts; Opik and Laminar have no `ee` code. Settles D9 for the code layout. |
| D-P-3 | **Workspace licence metadata and the licence check.** `[workspace.package] license = "MIT"` (the crates' own `license` field; `crates/server` says `"MIT AND Elastic-2.0"`, `tools/license` says `"Elastic-2.0"`). D57's deferred `cargo deny` licence check is turned on, with an allow-list of the permissive licences in the current dependency graph plus MPL-2.0 (file-level, `option-ext`), since D172 settled D9. Workspace crates are ignored as private (`[licenses.private] ignore = true`). |
| D-P-4 | **What is `ee`.** Licence-key verification and the enterprise features: per-project role overrides (fine-grained roles, D139), SSO enforcement, and per-org SSO (M5). Audit logs and SCIM (D178) go to `ee` when a later track builds them. Plans, entitlements, metering, quotas, the data window, the usage page, alerts and online evaluation are core: the Free plan includes them, and without a plan they impose nothing (D-K-15, D-I-33). |

### Signed tokens and licence keys

| # | Decision |
|---|---|
| D-P-5 | **One signed-token format.** A token is `llmobs1.<kid>.<payload>.<signature>`. `llmobs1` is the format tag (one constant, renamed with D46). `<kid>` names the verifying key (`[a-z0-9-]{1,32}`). `<payload>` is the base64url (no padding) of a UTF-8 JSON object. `<signature>` is the base64url (no padding) of the 64-byte ed25519 signature of the ASCII bytes `llmobs1.<kid>.<payload>`. The verifier checks the signature over the received bytes before it parses the JSON, so there is no canonical-JSON problem (LangWatch's re-serialisation pitfall). A token is at most 16 KiB. Payload parsing ignores unknown fields and unknown feature names, so a token issued for a newer binary still works on an older one. Every payload has `kind` (`license`, `org` or `control_plane`), `id` (UUIDv7) and `issued_at` (RFC 3339). |
| D-P-6 | **Built-in public keys (user).** Release builds trust only keys compiled into the binary; no config or env var adds one, because a configurable key would let anyone mint licences. The first production key is `prod-1`, whose private half the user keeps outside the repository (generated 2026-11-14). Debug builds (`cfg(debug_assertions)`) also trust `dev-1`, whose private half is checked in at `fixtures/license/dev-1.key` for tests and local development. A new key gets the next `kid`; retiring one removes it from the list. |
| D-P-7 | **Licence key (D172).** `license_key` (env `LLMOBS_LICENSE_KEY`, a `Secret<String>`, unset by default) holds a token of kind `license`: `customer` (display name, at most 200 characters), `features` (strings), `expires_at` and `grace_ends_at` (`grace_ends_at >= expires_at`; trial keys set them equal). Until `expires_at` the licence is active; until `grace_ends_at` it is in grace (features on, a warning in the log and on the admin pages); after that it is expired (features off). The status is computed from the clock at every check, so a running server crosses these instants without a restart. A key that does not verify is logged once at startup as `invalid` with the reason class (never the key) and the server runs as if no key were set: licensing never blocks startup or ingest (D172). **Licence keys carry no limits in the MVP** (D172 listed "limits"): self-hosted has no volume, seat or retention limits, and seat counts for enterprise deals are contractual. A later field can add limits; older binaries ignore it. |
| D-P-8 | **Lapses never widen access (user).** When a licence expires or an org loses a feature, restrictions it already set up keep applying; only setting them up is locked. Existing per-project overrides keep hiding or lowering projects, and may still be removed, but not created or changed. Instance-wide SSO enforcement (`auth.enforce_sso_domains`) applies whenever the configured key is a genuine licence that lists `sso_enforcement`, expired or not; without such a key the setting is ignored with a startup warning. Per-org SSO enforcement (M5) follows the same rule. |

### Entitlements

| # | Decision |
|---|---|
| D-P-9 | **The catalog lives in the control plane (user).** Plan definitions (Free, Starter, Pro, Enterprise, the Teams add-on), prices and per-org overrides live in track Q. A cell never holds a plan table: it receives each org's resolved features and limits in a signed org token (D-P-11), and its config holds the instance defaults that apply to an org without a token. Changing a plan or an org's override never needs a release. This settles D179's "per-org overrides": Q merges them into the token. |
| D-P-10 | **Resolution (D179).** `entitlements::of(&state, org_id) -> Entitlements` is synchronous, reads only memory, and is called at every check. Layers: (1) instance defaults from `[entitlements.defaults]` (M2; until then, no limits and every core feature); (2) the org's valid token, which replaces the defaults' plan, features and limits (M2); (3) the licence's features, added to every org while the licence is active or in grace. Config can grant only core features; an `ee` feature name in `[entitlements.defaults]` is a config error. `Entitlements` is a small `Copy` value: a feature bit set, the limits (each `Option<u64>`, `None` = unlimited) and its source. Self-hosted without a key: no limits, every core feature, no `ee` feature (D172). |
| D-P-11 | **Org tokens (user).** The control plane signs a token of kind `org` per org and `PUT`s it to `/api/internal/v1/orgs/{org_id}/entitlements` on the org's cell. The endpoint needs no credential: the signature is the proof, and replaying a token is harmless. A token names its cell: `audience` must equal the cell's `public_url`, so a token issued for one cell (a dedicated cell in a customer's cloud included) is refused by every other instance (400). Checks run in this order, so only a token Q signed can learn whether an org exists: signature and kind (400), `audience` (400), `org_id` equal to the path's org (400), now before `grace_ends_at` (400 when past it), org exists (404). Then: a token with the stored token's `id` answers 204 (idempotent retry); a different token whose `issued_at` is not newer than the stored one answers 409 `conflict`; otherwise it is stored in `org_entitlements` and the in-memory map is swapped. Tokens that fail verification count against the per-IP form limiter (D143, D-P-33). Accepted residual risk: someone who copies a cell's `meta.db` and runs it under the same `public_url` keeps that org's entitlements until the token's `grace_ends_at`. Tokens carry `expires_at` and `grace_ends_at` like licences; after `grace_ends_at` the org falls back to the instance defaults (Free on a cloud cell). Q re-issues tokens well before expiry (it chooses the lifetime; 35 days with 7 days of grace is the suggestion). |
| D-P-12 | **Features.** Snake-case names, append-only: `retention_management` (core; Pro on the cloud, D177, D-M-16), `project_roles` (ee; Teams; D139's overrides), `sso_enforcement` (ee; licence only: the instance-wide `auth.enforce_sso_domains`), `org_sso` (ee; Teams; M5). A variant is added by the milestone that first checks it: M1 adds `project_roles` and `sso_enforcement`, M2 `retention_management`, M5 `org_sso`. |
| D-P-13 | **Limits.** `units_included` (warning base and the usage page), `units_cap` (the monthly hard cap: Free's included units, or a paid org's spend cap converted to units by Q, D174), `ingest_spans_per_sec` and `ingest_burst` (per-org token bucket), `data_access_days` (D175), `max_seats` (D176), `max_projects`, `max_alert_rules` (D178), `max_active_online_rules` (D178). In a token, a missing key takes the instance default and `null` means unlimited. **No org judge-spend limit (user, amends D179 and spec 12):** judges run on the customer's own connection (D178) and track I's per-rule budgets protect them; judge tokens and cost stay metered in `usage_daily` and shown on the usage page. |
| D-P-14 | **Problem codes.** Three codes cover every refusal: `plan_required` (403, the org's plan lacks a feature; track M adds it, D-M-16), `plan_limit` (403, a count limit is reached: seats, projects, alert rules, online rules; track K adds it, D-K-14), and `quota_exceeded` (402, the monthly units cap, M3). The `detail` names the limit in words ("Your plan allows 1 project."), never payload. Whichever of M, K or P merges first adds a code; the others reuse it. |
| D-P-15 | **Locked features stay visible (user).** Locked features and reached limits show a lock badge, the plan that unlocks them ("Available on Pro", "Teams add-on"), and an Upgrade button. The button opens `entitlements.upgrade_url` (config; `{org}` is replaced by the org id): the control plane's billing page on a cloud cell, a pricing or licence page when self-hosted. With no URL configured the message shows without a button. `GET /api/v1/orgs/{org}/entitlements` (any org member, `OrgPerm::View`) gives the UI the features, limits, plan name, upgrade URL and (from M3) the month's usage. |

### Enforcement

| # | Decision |
|---|---|
| D-P-16 | **Units per UTC calendar month (user).** Units are spans plus scores (D173) committed for the org since 00:00 UTC on the 1st, from `usage_daily` (judge-trace spans excluded, D-I-31) plus the counts not yet flushed. A per-org in-memory counter is loaded from `usage_daily` on first use in a month and incremented where `Usage::add` is called. Q anchors Stripe billing cycles to the 1st. |
| D-P-17 | **The cap answers 402 (user, amends D174's 429).** When the org's month units are at or above `units_cap`, OTLP HTTP ingest, score writes and public feedback answer `402` `quota_exceeded` with a detail naming the cap and the reset instant (00:00 UTC on the 1st). OTLP gRPC answers `RESOURCE_EXHAUSTED` without `RetryInfo`. OTel exporters retry neither, so a capped org causes no retry storm, and a quota cannot be mistaken for an auth error (LangWatch and Opik use 402 for the same reason). A request that starts below the cap is accepted whole, so an org can overshoot by the requests in flight (§ Request paths). Score deletion (a deleted copy, spec 09 § Scores) is never refused. Human annotations are scores, so a capped Free org cannot annotate until the reset; the annotation form shows the cap message. The SDKs log the 402 once per reset period. |
| D-P-18 | **Warnings at 80 % and 100 %.** When an org crosses 80 % or 100 % of `units_included` in a month: a banner on every page for owners and admins (from the entitlements response), and one email to the org's owners per threshold per month (a `usage_notices` row makes it once only). Paid orgs get the same notices, worded as overage. |
| D-P-19 | **Ingest rate limits.** A per-org token bucket (`ingest_spans_per_sec`, `ingest_burst`) over spans accepted by OTLP ingest, taken after `accept` and before `commit` (the count exists only after decoding, § Request paths); over it, `429 rate_limited` with `Retry-After` (and gRPC `RESOURCE_EXHAUSTED` with `RetryInfo`). A rate limit, unlike the cap, is worth retrying. Replaces spec 12's per-org limits JSON. |
| D-P-20 | **Seats and projects.** Seats are org members with role owner, admin or member, plus pending invites with one of those roles (D176). The check runs on invite create, on a role change from viewer to a seat role, and again when an invite is used: accept by a signed-in user and signup with an invite token (D147). Projects count `projects` rows of the org with `state = 'active'`; the check runs on project create. Over a limit: `403 plan_limit`. Existing members and projects are never removed by a downgrade. |
| D-P-21 | **The data window lives in the storage query (D175).** `llmobs_storage::QueryOptions` gains `visible_from: Option<i64>` (ns). When set, every provider of the query hides spans whose `start_time`, traces whose `start_time`, and scores whose `created_at` is before it (the columns A2's time window uses, D101), the same way a retention watermark hides old memtable partitions. `visible_from` is `now − data_access_days × 1 day`, computed per query (sliding, not day-anchored; M's retention is day-anchored and 30 days later, so the two never meet).<br>**One place builds `QueryOptions`:** `routes/read.rs::open_query_on(storage, project, around, window, visible_from: Option<i64>)`. `open_query` and `open_query_windowed` gain an `org: OrgId` argument and pass `entitlements::visible_from(state, org, now)`; every caller passes the org it already holds from its access (`access.project.org_id`), a mechanical change at G's, H's, K's and I's call sites. Stream tasks (no `AppState`) take `visible_from` computed when the stream opens; it can be stale by at most a stream's life (D-G-34), accepted. A test fails if `QueryOptions {` appears in `crates/server/src` outside `open_query_on`.<br>**Deletion reads are exempt (M's request):** the scope reads (`filter_trace_ids`, `user_deletion_scope`, `eval_trace_ids`) and the deletion preview pass `visible_from: None`, so an erasure covers hidden-but-kept data; a test lists the only callers allowed to pass `None` with an org window in force (`deletion::resolve` and the preview handler) and checks an erasure reaches a hidden trace.<br>**Media:** `GET …/media/{sha256}` reads by hash, not through a span. M4 records, per project and blob, the newest `start_time` of a span that referenced it (written where ingest extracts media), and `get_media` answers 404 when that instant is before `visible_from`.<br>This covers the UI, the API, the SQL console, MCP, exports, alerts (D-K-16) and evaluators (D-I-34). |
| D-P-22 | **Retention cap (D175).** `Entitlements::retention_cap_days() -> Option<u32>` is `data_access_days.map(\|days\| u32::try_from(days.saturating_add(30)).unwrap_or(u32::MAX))`, in days (`None` without a window), so a token's huge value never overflows. Track M's hourly retention passes it as the cap (D-M-15), so data is physically deleted 30 days after it leaves the window, and an upgrade within those 30 days reveals it again. The ingest age check (D85) uses the same bound: `IngestContext`'s retention becomes `retention::effective_days(project.retention_days, retention_cap_days())` (M's request; `ingest/mod.rs`, both `IngestContext` builders). |
| D-P-23 | **Rule counts.** Track K's `alerts::quota::rule_limit` returns `max_alert_rules`, and track I's activation check takes `max_active_online_rules` (§ Interfaces for M, K and I). Downgrades never disable rules; only creating or enabling one more is refused. |

### Cloud and SSO

| # | Decision |
|---|---|
| D-P-24 | **Control-plane requests.** Read endpoints the control plane calls on a cell (the usage export, M3) take `Authorization: Bearer <token>` with a token of kind `control_plane`: `audience` is the cell's `public_url`, and `expires_at` is at most 5 minutes after `issued_at`. Same keys and verifier as licences (D-P-5). There is no shared secret per cell. |
| D-P-25 | **Per-org SSO is P's M5, in `ee` (user).** Teams orgs register their own OIDC provider in `meta.db`, prove their email domains with a DNS TXT record, and may enforce SSO for those domains. Login discovers the provider from the email domain. Instance-wide `[[auth.oidc]]` and `auth.enforce_sso_domains` stay for self-hosted. |

### Control-plane interface (M2, 2026-11-14)

Track Q's design (`llmobs-control/docs/00-design.md` § Requests to llm-observer) asked for these; the user chose which go into M2.

| # | Decision |
|---|---|
| D-P-26 | **The token crate (user; Q's R1, amends D-P-1).** The signed-token format, the claim types of the three kinds, the built-in keys and signing live in `crates/token` (`llmobs-token`, Elastic-2.0, covered by `just check-ee`), which depends on no workspace crate, `axum` or storage. `llmobs-server`, `tools/license` and the control plane (by git commit) use it, so the format has one implementation. M2's first phase moves M1's `ee/token.rs` there. |
| D-P-27 | **Keys bound to kinds (Q's R2, amends D-P-6).** Each built-in key lists the kinds it may sign; a token whose kind its key may not sign is refused (`KeyNotAllowed`). `prod-1` signs `license` only and stays offline. `cloud-1` signs `org` and `control_plane`; its private half lives in the control plane's secrets. `cloud-2`, with the same kinds, is an offline spare: if `cloud-1` leaks, the control plane switches to it at once and the next release removes `cloud-1`. `dev-1` signs every kind, in debug builds only. A stolen `cloud-1` therefore cannot mint licences. |
| D-P-28 | **Control-plane principal and scopes.** A bearer token starting with `llmobs1.` is verified as a `control_plane` token (audience = the cell's `public_url`, at most 5 minutes valid, 60 s of clock skew) and becomes `Principal::ControlPlane { token_id, scope }`. Routes declare `Access::ControlPlane { scope }`; scopes are `orgs` (M2), `usage` (M3), and `deletions`, `members`, `users` when those endpoints are built. Every other access kind refuses this principal. |
| D-P-29 | **Internal org creation (user; Q's R3).** `POST /api/internal/v1/orgs` (scope `orgs`) creates the org with the control plane's UUIDv7 and its first owner in one transaction: an unknown email becomes a verified user without a password; an existing user is reused unchanged; a disabled user or an instance admin is `422 owner_not_allowed`. The same id again answers `200` with the org, whatever its owners now (idempotent). No default project. Routes already accept an org id wherever they accept its slug. |
| D-P-30 | **Window history (user; Q's R12).** Each accepted org token that changes an org's data window records the window that ended (`org_window_ends`, kept 30 days). `retention_window_days(org)` is the largest window that applied in the last 30 days (current, recorded ends, and an expired token's), unlimited if any was. M4 passes it to track M's retention, so a shrinking window hides data for 30 days before deleting it (D175), whatever the reason. |
| D-P-31 | **No user-created orgs on a cloud cell (user; Q's R4).** `orgs.user_create = false` makes `POST /api/v1/orgs` answer 403; `GET /api/auth/config` tells the UI, whose "New organization" and first-org page then link to `orgs.create_url` (or show no action). Bootstrap still creates the first org. |
| D-P-32 | **A Plan page in M2 (user).** `GET /api/v1/orgs/{org}/entitlements` returns the plan (name, add-ons, source, token status and dates) and the nine limits (omitted = unlimited); org settings get a read-only **Plan** page for every member. `llmobs admin entitlements show <org>` prints the same, plus the retention window. M3 adds the month's usage to both. |
| D-P-33 | **The token push and the limiter (Q's R15, amends D-P-11).** `PUT /api/internal/v1/orgs/{org_id}/entitlements` applies the per-IP form limiter only to tokens that fail verification, because every push comes from the control plane's one address. The path parameter is `{org_id}` (a UUID), since the route is public. |

## Architecture

### Code layout (end state)

```
LICENSE                                   MIT, names the ee directories              (M1)
crates/server/src/ee/LICENSE              Elastic License 2.0                         (M1)
crates/server/src/ee/mod.rs               module list                                 (M1)
crates/token/                             llmobs-token: format, claims, keys, signing (M1, moved in M2)
crates/server/src/ee/license.rs           licence claims and status                   (M1)
crates/server/src/ee/project_roles.rs     override write routes (D139)                (M1)
crates/server/src/ee/sso_enforcement.rs   instance-wide SSO enforcement               (M1)
crates/server/src/ee/org_sso/*            per-org OIDC providers and domains          (M5)
crates/server/src/entitlements/mod.rs     Feature, Entitlements, of(), the gates      (M1, M2)
crates/server/src/entitlements/tokens.rs  org token claims, store, internal route     (M2)
crates/server/src/entitlements/units.rs   month units counter, the cap                (M3)
crates/server/src/entitlements/rate.rs    per-org ingest bucket                       (M3)
crates/server/src/entitlements/notices.rs 80 % / 100 % notices                        (M3)
crates/server/src/routes/entitlements.rs  GET /orgs/{org}/entitlements                (M1, M2, M3)
crates/server/src/routes/usage.rs         usage API and the internal export           (M3)
crates/storage/src/…                      QueryOptions::visible_from                  (M4)
tools/license/                            dev keygen and signing tool (ee)            (M1)
fixtures/license/                         dev key and sample tokens                   (M1)
web/src/ee/LICENSE, web/src/ee/*          override editor, licence card, org SSO      (M1, M5)
web/src/components/plan/*                 lock badge, upgrade button, usage banner    (M1, M3)
web/src/routes/$org/settings/usage.tsx    usage page                                  (M3)
```

### Request paths

- **Feature gate.** A handler calls `entitlements::require(&state, org, Feature::X)?`, which returns `ApiError::PlanRequired` with the feature's fixed detail. Gates sit in the handler, after the access middleware and before any write.
- **Count limit.** The handler counts in the write transaction where possible (I does; K counts just before) and calls `entitlements::check_limit(limit, count, Noun::Project)`, which returns `ApiError::PlanLimit`.
- **Ingest.** `Ingest::accept` has no `AppState`, so the checks sit in the two handlers that call it, `routes/otlp.rs` (HTTP) and `ingest/grpc.rs` (gRPC), after the key resolved the org:
  1. before decoding, `entitlements::units::check_cap(&state, org, now) -> Result<(), CapExceeded>`: HTTP `402 quota_exceeded`, gRPC `RESOURCE_EXHAUSTED` without `RetryInfo`;
  2. after `accept` returns (the span count is known) and before `commit`, `entitlements::rate::take(&state, org, spans, now) -> Result<(), RetryAfter>`: HTTP `429 rate_limited` with `Retry-After`, gRPC `RESOURCE_EXHAUSTED` with `RetryInfo`;
  3. `Ingest::commit`, where it calls `Usage::add` (`ingest/mod.rs`), also adds the committed spans to the org's month counter. Score writes (`scores::write`) do the same for scores, and check the cap first (`402`).
  Concurrent requests admitted under the cap all commit, so an org can overshoot by the requests in flight; a crash loses at most the counts not yet flushed (10 s, D-G-11), so a restart can under-count by that much. Both are accepted.
- **Reads.** Every storage read goes through `routes/read.rs::open_query_on`, which alone builds `QueryOptions`, with `visible_from` from `entitlements::visible_from(state, org, now)` (D-P-21).
- **Org token push.** `PUT /api/internal/v1/orgs/{org_id}/entitlements` → `ee::token::verify` → claims → `org_entitlements` upsert (newest wins) → in-memory map swap.

### State

`AppInner` gains `entitlements: crate::entitlements::EntitlementsState` (M1): the licence verified at startup (or its absence), the instance defaults (M2), and the per-org token map (M2), `RwLock<HashMap<OrgId, OrgGrant>>` loaded from `org_entitlements` at startup. M3 adds the month-units counters and the rate buckets beside them.

### `meta.db` (M2, M3, M5)

```sql
-- M2. One row per org that has received a token.
CREATE TABLE org_entitlements (
  org_id     TEXT PRIMARY KEY REFERENCES orgs(id) ON DELETE CASCADE,
  token      TEXT NOT NULL,        -- the verified token, as received
  token_id   TEXT NOT NULL,        -- payload id
  issued_at  INTEGER NOT NULL,     -- ns
  received_at INTEGER NOT NULL     -- ns
);
-- M3. One row per org, month and threshold, so each notice is sent once.
CREATE TABLE usage_notices (
  org_id    TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
  month     TEXT NOT NULL,         -- 'YYYY-MM'
  threshold INTEGER NOT NULL,      -- 80 or 100
  sent_at   INTEGER NOT NULL,
  PRIMARY KEY (org_id, month, threshold)
);
```

`orgs.limits` (spec 04) is dropped in M2: nothing reads it, and the token replaces it (D179).

## Interfaces for M, K and I

Those tracks are adding call sites now; P fills them. P changes these lines in their code, plus the mechanical `org` argument at each `open_query`/`open_query_windowed` call (D-P-21).

| Track | Their hook (as planned) | P's replacement | Milestone |
|---|---|---|---|
| M | `retention::retention_once`: `let cap: Option<u32> = None;` (D-M-15) | `let cap = entitlements::of(&state, org_id).retention_cap_days();` (`Option<u32>`) | M4 |
| M | `routes/projects.rs::may_manage_retention(state, org) -> bool`, returns `true` (D-M-16) | body: `entitlements::of(state, org).allows(Feature::RetentionManagement)`; M's `ApiError::PlanRequired { detail: RETENTION_PLAN }` and its UI stay | M2 |
| K | `alerts::quota::rule_limit(state, org_id) -> Option<u64>`, returns `None` (D-K-14) | body: `entitlements::of(state, org_id).limits.max_alert_rules`; K's `ApiError::PlanLimit` and its message stay | M4 |
| K | the evaluator's `open_query_on(&state.storage, project, None)` (07-evaluate) | passes the rule's org window: `open_query_on(&state.storage, project, None, None, entitlements::visible_from(&state, org_id, now))` | M4 |
| I | the create (`enabled: true`) and false→true PATCH call sites pass `limit: None` to `eval::rule_limit::check(active_after: u64, limit: Option<u64>)`, with `active_after` from `meta::online_rules::count_enabled_rules(conn, org_id)` inside the write transaction (D-I-32); refusal is I's interim `409 conflict` | pass `entitlements::of(&state, org_id).limits.max_active_online_rules`, and map I's refusal to `ApiError::PlanLimit` (`403 plan_limit`, D-P-14) with the detail "Your plan allows N active online evaluation rule(s).", replacing the interim 409; spec 09 and I's error table change with it | M4 |
| G, H, K, I reads | `open_query` / `open_query_windowed` / `open_query_on` (D-K-16, D-I-34) | the `org` argument or `visible_from`, as D-P-21 says. This replaces the `entitlements::query_options(&state, org, around)` mechanism I's D-I-34 expected; I's text is amended when it merges | M4 |
| M | ingest's D85 age check uses `project.retention_days` (`IngestContext`) | `retention::effective_days(project.retention_days, retention_cap_days())` (D-P-22) | M4 |
| M | scope reads and the deletion preview | pass `visible_from: None` (exempt, D-P-21) | M4 |
| I | `Ingest::commit` skips `llmobs-eval` spans in `usage_daily.spans`; evaluator scores count through `scores::write`; judge tokens and cost go to `usage_daily.judge_*` through `Usage::add_judge` (D-I-31) | read as is: units = `spans + scores`; judge columns shown on the usage page only | M3 |

Both tracks' problem codes are kept as D-P-14 describes; I's interim `409` becomes `403 plan_limit`.

## Interfaces for track Q

1. **Licence keys.** Q signs tokens of kind `license` with the production key (D-P-5, D-P-7). Payload:

   ```json
   {"kind": "license", "id": "0193…", "issued_at": "2026-11-14T00:00:00Z",
    "customer": "Acme Corp", "features": ["project_roles", "sso_enforcement"],
    "expires_at": "2027-11-14T00:00:00Z", "grace_ends_at": "2027-12-14T00:00:00Z"}
   ```

2. **Org tokens.** Q signs one token of kind `org` per org and `PUT`s it to the org's cell whenever the plan, an add-on, an override or the spend cap changes, and periodically before expiry (D-P-11):

   ```json
   {"kind": "org", "id": "0193…", "issued_at": "…", "expires_at": "…", "grace_ends_at": "…",
    "org_id": "0193…",
    "plan": {"id": "pro", "name": "Pro", "add_ons": ["teams"]},
    "features": ["retention_management", "project_roles", "org_sso"],
    "limits": {"units_included": 200000, "units_cap": null,
               "ingest_spans_per_sec": 20000, "ingest_burst": 100000,
               "data_access_days": 1825, "max_seats": null, "max_projects": null,
               "max_alert_rules": null, "max_active_online_rules": null}}
   ```

   The payload also carries `"audience": "https://<cell public_url>"`. `PUT /api/internal/v1/orgs/{org_id}/entitlements`, body `{"token": "llmobs1.…"}`, in D-P-11's check order: `400 bad_request` (does not verify, wrong kind, wrong `audience`, `org_id` mismatch, past `grace_ends_at`), `404` (no such org), `204` (stored, or the same token `id` already stored: Q's retries are idempotent), `409 conflict` (another token with an equal or newer `issued_at` is stored; Q re-signs with a newer `issued_at`), `429` (per-IP limit, only after failed verifications, D-P-33). A new org has the instance defaults (Free on a cloud cell) until its first token, so signup needs no synchronous call; how the org gets created on its cell is Q's.

3. **Usage export (M3).** `GET /api/internal/v1/usage?from=YYYY-MM-DD&to=YYYY-MM-DD` with a `control_plane` bearer token (D-P-24) returns per org and day `{org_id, day, spans, scores, units, bytes, judge_input_tokens, judge_output_tokens, judge_cost_usd}`; `from` and `to` are inclusive UTC days (at most 92 days apart), rows are ordered by `(day, org_id)`, and pages use `?limit=` (default 1000, at most 10 000) and the opaque `?cursor=` of spec 09. The cell verifies the `control_plane` token: signature, kind, `audience` equal to its `public_url`, `expires_at − issued_at ≤ 5 min`, and now within `[issued_at − 60 s, expires_at]`. Q sums units per billing month (calendar month, D-P-16) and reports overage to Stripe. Rows of the current day keep growing until the day ends; Q bills a month after its last day plus one flush interval.

4. **Upgrade links.** A cloud cell sets `entitlements.upgrade_url` to Q's billing page for an org, for example `https://app.<domain>/orgs/{org}/billing`.

5. **Not P's.** Org → cell routing, signup, `deletion::on_dropped` (track M's hook for Q, D-M-17), and Stripe itself.

## Milestones

| # | Plan | Scope | Depends on |
|---|---|---|---|
| **M1** | [`01-m1-open-core-license.md`](01-m1-open-core-license.md) | MIT and ELv2 licence files, SPDX headers, `just check-ee`, the `cargo deny` licence check; the signed-token module and built-in keys; the dev signing tool and fixtures; `license_key` config and status; the `Entitlements` core with `project_roles` and `sso_enforcement`; `plan_required`; `GET /api/v1/orgs/{org}/entitlements` and `GET /api/v1/admin/license`; overrides' write routes and SSO enforcement moved to `ee` and gated (D-P-8); web lock badge and upgrade button, the licence card, the override editor moved to `web/src/ee` | G, L |
| **M2** | [`02-m2-org-entitlements.md`](02-m2-org-entitlements.md) | the `llmobs-token` crate and keys bound to kinds (D-P-26, D-P-27); `[entitlements.defaults]`; `org_entitlements` and the window history (D-P-30); org tokens, resolution and expiry; `PUT /api/internal/v1/orgs/{org_id}/entitlements` (D-P-33); `orgs.limits` dropped; `control_plane` tokens and `Access::ControlPlane` (D-P-28); `POST /api/internal/v1/orgs` (D-P-29); `orgs.user_create` (D-P-31); plan and limits in the entitlements response, the Plan page and `llmobs admin entitlements show` (D-P-32); `retention_management`; the `cloud-1` and `cloud-2` keys | M1 |
| **M3** | `03-m3-units-and-limits.md` | month-units counter, the `402 quota_exceeded` cap on OTLP HTTP and gRPC, scores and feedback; per-org rate bucket; `usage_notices`, banners and owner emails; seats and projects checks in L's flows (D-P-20) with their UI states; `GET /api/v1/orgs/{org}/usage` and the month's usage on the Plan page (any org member, `OrgPerm::View`); the internal usage export (`control_plane` scope `usage`); SDK handling of 402 | M2 |
| **M4** | `04-m4-window-and-gates.md` | `QueryOptions::visible_from` in storage; `routes/read.rs::open_query_on` alone builds `QueryOptions`, with `visible_from` from `entitlements::visible_from` (D-P-21), and a test that nothing else builds `QueryOptions`; `entitlements::visible_from`, the deletion exemption and the media check; the retention cap into M and the D85 ingest bound; K's and I's rule limits; upgrade states on the retention, alerts and online-rules pages | M3, K, I's M3, M's M6 |
| **M5** | `05-m5-org-sso.md` | per-org OIDC providers (sealed client secret, D-I-2), DNS TXT domain verification, per-org enforcement, login discovery by email domain, the org SSO settings page (all `ee`, feature `org_sso`) | M2 |

M1 can start now. M4 waits for K and I's M3, whose hooks it fills.

## Open items

- **Copyright holder** in the `LICENSE` files: M1 writes "Copyright (c) 2026 the llmobs authors". The user confirms the legal name before the first release.
- **Audit logs and SCIM** (D178, Enterprise): no track owns them yet. They go in `ee` and add their features to D-P-12 when built.
- **SDK messages** for `402`: M3 changes both SDKs' exporters to log the cap once; track F's owners review.
- **Q's token lifetime** and the plan values (units, rates, windows) are Q's to choose; the instance defaults of a cloud cell are its Free plan.
