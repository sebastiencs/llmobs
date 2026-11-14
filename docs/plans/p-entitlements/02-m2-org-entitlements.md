# P · M2 — Org entitlements and the control-plane interface

Milestone 2 of track P ([`00-design.md`](00-design.md)). It builds on M1 ([`01-m1-open-core-license.md`](01-m1-open-core-license.md), merged first) and gives track Q (the cloud control plane, private repository `llmobs-control`) what its M2 needs.

When M2 is done:
- the signed-token code lives in its own Elastic-2.0 crate, `crates/token` (`llmobs-token`), which the server, `tools/license` and Q share; each built-in key names the token kinds it may sign (D-P-26, D-P-27);
- `[entitlements.defaults]` sets the instance defaults: plan name, core features and the nine limits (D-P-10, D-P-13);
- a cloud cell accepts each org's signed token on `PUT /api/internal/v1/orgs/{org_id}/entitlements`, stores it in `meta.db`, and resolves the org's entitlements from it until its grace period ends (D-P-11);
- the cell records every shrink of an org's data window, so M4's retention can use the largest window of the last 30 days (D-P-30);
- `control_plane` bearer tokens with a `scope` authenticate internal endpoints (D-P-24, D-P-28); `POST /api/internal/v1/orgs` creates an org with Q's id and its owner (D-P-29);
- `orgs.user_create = false` stops users from creating orgs on a cloud cell (D-P-31);
- `GET /api/v1/orgs/{org}/entitlements` returns the plan, limits and token status, shown on a new org settings **Plan** page (D-P-32);
- `llmobs admin entitlements show <org>` prints an org's resolved entitlements.

**Read first, in this order:**
1. `AGENTS.md` (rules, commands, privacy).
2. [`00-design.md`](00-design.md) § Decisions D-P-5–D-P-15, D-P-22, D-P-24, D-P-26–D-P-32, and § Interfaces for track Q.
3. `docs/spec/12-operations.md` § Licence keys, plans and entitlements; `docs/spec/09-api.md` § Orgs and the internal endpoints row.
4. M1's code: `crates/server/src/ee/token.rs`, `ee/license.rs`, `entitlements/mod.rs`, `routes/entitlements.rs`, `tools/license/src/main.rs`, the helpers `sign_token`, `sign_raw`, `license_key`, `license_key_until` in `crates/server/src/test_support/mod.rs`.
5. The code named in each sub-phase before you change it.

## Conventions

The same as M1 (`01-m1-open-core-license.md` § Conventions): one commit per sub-phase with the subject given; identifiers say `license`, prose says "licence"; every source file under an `ee` directory **and under `crates/token/`** starts with `// SPDX-License-Identifier: Elastic-2.0`; server code returns `ApiError`; no `unwrap()`/`expect()` outside tests without an invariant comment; never log or return a token, a payload field other than ids, `kid`, `kind` and status, an email, or a customer name; time is `i64` ns from `state.clock.now_ns()`; Rust tests next to the code, server tests on `TestApp` (manual clock `2026-01-01T00:00:00Z`), web tests with Vitest + Testing Library on MSW with `server.use`, never `vi.mock`; run `just openapi` and `just api-examples` when the contract or examples change.

**Dead code between phases.** Items a phase adds that have no non-test caller until a later phase would fail `just lint`. Such a module gets `#![cfg_attr(not(test), allow(dead_code))]` at its top with the comment `// Track P M2: <phase> uses these outside tests.`; P9 removes every attribute M2 added (after P9 clippy must be clean without them). Never put it on a module whose items all have callers. P2 puts it on `crates/server/src/entitlements/mod.rs` (its new `Limits` fields and `Defaults::plan_name` are read only from P9 on), P3 on `meta/entitlements.rs`, P4 on `entitlements/tokens.rs`.

**Generated files.** `crates/server/openapi.json`, `web/src/api/schema.gen.ts`, `fixtures/api/manifest.json` and `fixtures/license/tokens/*` are regenerated (`just openapi`, `just api-examples`, `just license-fixtures`), never hand-merged.

## Commands

```bash
export PATH=$HOME/.local/share/nvm/v26.13.0/bin:$PATH
```

```bash
just fmt
```

```bash
just check
```

`just check` must pass before every commit; it takes over 20 minutes (storage crash tests). While iterating use `cargo test -p llmobs-token`, `cargo test -p llmobs-server <filter>`, `cargo test -p llmobs-license-tool`, `just lint`, `just check-ee`, `just check-web`, `cd web && pnpm vitest run <file>`.

## Sub-phases

| # | Subject | What | After |
|---|---|---|---|
| P1 | `Move signed tokens into llmobs-token` | `crates/token`, claim types, keys bound to kinds; server, tool and fixtures use it | M1 |
| P2 | `Add instance entitlement defaults` | `[entitlements.defaults]`, `Feature::RetentionManagement`, `Limits`, defaults layer | P1 |
| P3 | `Store org entitlement tokens` | migration (`org_entitlements`, `org_window_ends`, drop `orgs.limits`), `meta::entitlements` | P2 |
| P4 | `Resolve entitlements from org tokens` | `OrgGrant`, load at startup, `of` with tokens, `resolve`, `plan_at`, `retention_window_at` | P3 |
| P5 | `Accept org tokens from the control plane` | `PUT /api/internal/v1/orgs/{org_id}/entitlements` | P4 |
| P6 | `Add control-plane bearer tokens` | `Principal::ControlPlane`, `Access::ControlPlane { scope }` | P5 |
| P7 | `Let the control plane create orgs` | `POST /api/internal/v1/orgs`, `insert_org` takes the id, `owner_not_allowed` | P6 |
| P8 | `Add orgs.user_create` | config, `403` on `POST /api/v1/orgs`, `AuthConfig`, web "New organization" link | P7 |
| P9 | `Return the plan and limits of an org` | `OrgEntitlements` v2, fixtures, `llmobs admin entitlements show` | P8 |
| P10 | `Add the org Plan page` | `/$org/settings/plan`; roadmap | P9 |
| P11 | `Add the cloud signing keys` | `cloud-1`, `cloud-2` in `RELEASE_KEYS`; roadmap | P10 |

Run P1–P10 in order, one at a time: most phases edit the same files (`config.rs`, `test_support/mod.rs`, `api/openapi.rs` and the generated contract). P11 runs last, after P10 (both edit the roadmap row).

---

## P1 — The token crate

Track Q depends on this crate by git commit, so it has no dependency on another workspace crate, `axum` or storage.

### Files

**`crates/token/LICENSE`**: copy `crates/server/src/ee/LICENSE` (same SHA-256).

**`crates/token/Cargo.toml`**:

```toml
[package]
name = "llmobs-token"
description = "Signed licence, org and control-plane tokens (D-P-5, D-P-26)"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
publish.workspace = true
license = "Elastic-2.0"

[dependencies]
base64.workspace = true
chrono.workspace = true
ed25519-dalek.workspace = true
serde.workspace = true
serde_json.workspace = true
thiserror.workspace = true
uuid.workspace = true

[lints]
workspace = true
```

Root `Cargo.toml`: `llmobs-token = { path = "crates/token" }` in `[workspace.dependencies]` under `# workspace crates` (`crates/*` already makes it a member). `crates/server/Cargo.toml` and `tools/license/Cargo.toml` add `llmobs-token.workspace = true`; `tools/license` drops `ed25519-dalek` and `base64` if it no longer uses them directly (keep what the compiler needs).

**`crates/token/src/lib.rs`** (SPDX first line). Modules `mod keys; mod rfc3339;` and the public API below. Move the M1 code of `crates/server/src/ee/token.rs` here (format, `verify`, key decoding) and extend it.

```rust
// SPDX-License-Identifier: Elastic-2.0
//! Signed tokens: licence keys, org entitlement tokens and control-plane requests
//! (D-P-5, D-P-26, D-P-27). Shared by `llmobs-server`, `tools/license` and the
//! control plane. Depends on no other workspace crate.
//!
//! A token is `llmobs1.<kid>.<payload>.<signature>`: base64url (no padding) of a
//! UTF-8 JSON object, then of the ed25519 signature of the ASCII bytes
//! `llmobs1.<kid>.<payload>`. The signature is checked before the JSON is parsed.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signature, Signer, SigningKey};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

mod keys;
mod rfc3339;

pub use keys::{BuiltInKey, built_in_keys};

/// Format tag. Renamed with the product (D46).
pub const TOKEN_TAG: &str = "llmobs1";

/// Largest accepted token, in bytes.
pub const MAX_TOKEN_BYTES: usize = 16 * 1024;

/// What a token is for. The JSON `kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A self-hosted licence key (D-P-7).
    License,
    /// An org's entitlements, pushed to its cell (D-P-11).
    Org,
    /// A control-plane request to a cell (D-P-24).
    ControlPlane,
}

/// Why a token was refused. The text never quotes the token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TokenError {
    /// More than [`MAX_TOKEN_BYTES`].
    #[error("the token is longer than 16 KiB")]
    TooLarge,
    /// Not four dot-separated parts, a wrong tag, a bad key id, or bad base64.
    #[error("the token is malformed")]
    Malformed,
    /// The key id is not trusted by this build.
    #[error("the token is signed with a key this build does not trust")]
    UnknownKey,
    /// The signature does not match.
    #[error("the token signature does not verify")]
    BadSignature,
    /// The key may not sign this kind of token (D-P-27).
    #[error("the token's key may not sign this kind of token")]
    KeyNotAllowed,
    /// The payload is not a JSON object of a known kind with every required field.
    #[error("the token payload is invalid")]
    Payload,
    /// A valid token of another kind than the caller expects.
    #[error("the token is not of the expected kind")]
    WrongKind,
}
```

Claim types (all `Debug, Clone, PartialEq, Eq, Serialize, Deserialize`; no `deny_unknown_fields`, so unknown fields are ignored, D-P-5; every instant is `i64` ns with `#[serde(with = "crate::rfc3339")]`):

```rust
/// `kind: "license"` (D-P-7).
pub struct LicenseClaims {
    pub id: Uuid,
    #[serde(with = "crate::rfc3339")] pub issued_at: i64,
    pub customer: String,
    pub features: Vec<String>,
    #[serde(with = "crate::rfc3339")] pub expires_at: i64,
    #[serde(with = "crate::rfc3339")] pub grace_ends_at: i64,
}

/// `kind: "org"` (D-P-11). `audience` is the cell's `public_url`.
pub struct OrgClaims {
    pub id: Uuid,
    #[serde(with = "crate::rfc3339")] pub issued_at: i64,
    #[serde(with = "crate::rfc3339")] pub expires_at: i64,
    #[serde(with = "crate::rfc3339")] pub grace_ends_at: i64,
    pub audience: String,
    pub org_id: Uuid,
    pub plan: PlanClaim,
    /// Missing: no feature.
    #[serde(default)]
    pub features: Vec<String>,
    /// Missing: every limit takes the cell's default (D-P-13).
    #[serde(default)]
    pub limits: LimitsClaim,
}

/// The plan an org token names.
pub struct PlanClaim {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub add_ons: Vec<String>,
}

/// The nine limits of D-P-13. Outer `None`: the key is missing (the cell's default applies);
/// `Some(None)`: JSON `null` (unlimited); `Some(Some(n))`: the limit.
#[derive(Default)]
pub struct LimitsClaim {
    #[serde(default, with = "crate::double_option", skip_serializing_if = "Option::is_none")]
    pub units_included: Option<Option<u64>>,
    // … the same attribute on: units_cap, ingest_spans_per_sec, ingest_burst,
    // data_access_days, max_seats, max_projects, max_alert_rules, max_active_online_rules
}

/// `kind: "control_plane"` (D-P-24, D-P-28).
pub struct ControlPlaneClaims {
    pub id: Uuid,
    #[serde(with = "crate::rfc3339")] pub issued_at: i64,
    #[serde(with = "crate::rfc3339")] pub expires_at: i64,
    pub audience: String,
    /// One of `orgs`, `usage`, `deletions`, `members`, `users`; the cell matches it per route.
    pub scope: String,
}

/// A payload, tagged by `kind`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Claims {
    License(LicenseClaims),
    Org(OrgClaims),
    ControlPlane(ControlPlaneClaims),
}

impl Claims {
    /// The payload's kind.
    pub fn kind(&self) -> Kind { /* match */ }
}

/// A token whose signature, key binding and payload are valid. Its dates are not checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    /// The key that signed it.
    pub kid: String,
    /// The payload.
    pub claims: Claims,
}
```

`double_option` is a small `mod double_option` in `lib.rs` with `pub(crate) fn serialize` (outer `None` → `serializer.serialize_none()`; `Some(inner)` → `inner.serialize(serializer)`, i.e. a number or `null`) and `pub(crate) fn deserialize` (`Option::<u64>::deserialize(d).map(Some)`); with `#[serde(default)]` a missing key stays `None` and `skip_serializing_if` omits it. Functions inside private modules are `pub(crate)`, not `pub` (`unreachable_pub` warns). Write a doc comment on every field.

Functions:

```rust
/// Verifies `token` (surrounding whitespace ignored), in this order: size and shape
/// (`TooLarge`, `Malformed`), a trusted `kid` (`UnknownKey`), the signature
/// (`BadSignature`), the payload's `kind` (`Payload` when absent or unknown), the key's
/// binding to that kind (`KeyNotAllowed`), the full payload (`Payload`).
pub fn verify(token: &str) -> Result<Verified, TokenError>;

/// Signs `claims` with `key` under `kid` (compact JSON from `serde_json::to_vec`).
///
/// # Errors
/// `Malformed` when `kid` is not `[a-z0-9-]{1,32}`; `Payload` when serialization fails.
pub fn sign(key: &SigningKey, kid: &str, claims: &Claims) -> Result<String, TokenError>;

/// Signs raw payload bytes (tests and the dev tool). Same errors as [`sign`], without `Payload`.
pub fn sign_raw(key: &SigningKey, kid: &str, payload: &[u8]) -> Result<String, TokenError>;
```

`verify`'s first steps are M1's `ee::token::verify` (move it, including `valid_kid` and the `.get(..signed_len)` slicing). Then:

```rust
#[derive(Deserialize)]
struct Probe { kind: Kind }
let probe: Probe = serde_json::from_slice(&payload_bytes).map_err(|_| TokenError::Payload)?;
if !key.kinds.contains(&probe.kind) { return Err(TokenError::KeyNotAllowed); }
let claims: Claims = serde_json::from_slice(&payload_bytes).map_err(|_| TokenError::Payload)?;
```

**`crates/token/src/keys.rs`** (SPDX first line):

```rust
//! Built-in public keys and the kinds each may sign (D-P-6, D-P-27).

use crate::Kind;

/// One trusted public key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltInKey {
    /// Key id, `[a-z0-9-]{1,32}`.
    pub kid: &'static str,
    /// ed25519 public key, standard base64.
    pub public: &'static str,
    /// The token kinds it may sign.
    pub kinds: &'static [Kind],
}

/// Keys every build trusts. `prod-1` signs licences only and stays offline (D-P-27).
/// P11 adds `cloud-1` and `cloud-2` for org and control-plane tokens.
const RELEASE_KEYS: &[BuiltInKey] = &[BuiltInKey {
    kid: "prod-1",
    public: "AvIxTEduiC/amifl8QFxLYjXjPYON8LsreI7sL7UDWE=",
    kinds: &[Kind::License],
}];

/// Keys only debug builds trust. The private half is `fixtures/license/dev-1.key`.
const DEV_KEYS: &[BuiltInKey] = &[BuiltInKey {
    kid: "dev-1",
    public: "kMTjOGaR2ry6zgwZ2p2nk0kMsOoyisgt3qfRm4JH36Y=",
    kinds: &[Kind::License, Kind::Org, Kind::ControlPlane],
}];

/// The keys this build trusts: [`RELEASE_KEYS`], plus [`DEV_KEYS`] when built with debug
/// assertions (`cfg!` is evaluated in this crate's profile).
pub fn built_in_keys() -> impl Iterator<Item = &'static BuiltInKey> {
    let dev: &'static [BuiltInKey] = if cfg!(debug_assertions) { DEV_KEYS } else { &[] };
    RELEASE_KEYS.iter().chain(dev)
}
```

plus a crate-private `fn find(kid: &str) -> Option<(&'static BuiltInKey, VerifyingKey)>` that `verify` uses (M1's `decode_key`).

**`crates/token/src/rfc3339.rs`** (SPDX first line), deliberately as lax as `chrono`'s RFC 3339 parser (tokens are produced by our own signers): `pub(crate) fn serialize(ns: &i64, s)` writes `chrono::DateTime::from_timestamp_nanos(*ns).to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)`; `deserialize` parses with `chrono::DateTime::parse_from_rfc3339`, converts to UTC, and `timestamp_nanos_opt()` (`pub(crate) fn deserialize`); a parse failure or an out-of-range instant is `serde::de::Error::custom("invalid RFC 3339 instant")` (no input in the message).

### Moves and adaptations

- **Delete** `crates/server/src/ee/token.rs` and the `pub(crate) mod token;` line in `ee/mod.rs`; move its tests into `crates/token/src/lib.rs` `#[cfg(test)] mod tests`, adapted: `Verified` has no `payload` field any more, so `round_trip` and `tampering_is_refused` sign a **complete** licence payload (the JSON of `fixtures/license/payloads/active.json`) and compare `verified.claims`; `fixture_tokens_verify` reads each token and payload with `std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/license/…"))` and compares `verified.claims` with `serde_json::from_str::<Claims>(payload)`; the "error texts never quote the token" test iterates all seven `TokenError` variants. The test-only signer in the crate tests builds the dev key from `fixtures/license/dev-1.key` the same way.
- **Other references** to the removed module: `entitlements/mod.rs` imports `crate::ee::token::TokenError` (use `llmobs_token::TokenError`); `test_support/mod.rs` uses `crate::ee::token::TOKEN_TAG` (use `llmobs_token::TOKEN_TAG` or remove the use with the new `sign_raw`). `grep -rn 'ee::token' crates/` must be empty. Also update the text that names the old file: `fixtures/license/README.md` ("`DEV_KEYS` in `crates/token/src/keys.rs`"), the module doc of `tools/license/src/main.rs`, and `docs/plans/p-entitlements/00-design.md` § Request paths (`llmobs_token::verify` instead of `ee::token::verify`).
- **Server, `ee/license.rs`**: parse with `llmobs_token::verify`, then `match verified.claims { Claims::License(claims) => …, _ => return Err(InvalidLicense::Token(TokenError::WrongKind)) }`. `InvalidLicense` becomes two variants: `Token(#[from] llmobs_token::TokenError)` and `Fields` (the customer and date checks stay in the server). Remove the `Kind`/`Claims` structs M1 had there. Update the tests: what was `InvalidLicense::WrongKind` is now `InvalidLicense::Token(TokenError::WrongKind)`, `InvalidLicense::Payload` is `InvalidLicense::Token(TokenError::Payload)`; the admin licence tests that compare `reason` texts use the new `Display` strings above.
- **Server, `test_support/mod.rs`**: `sign_raw(payload: &[u8])` calls `llmobs_token::sign_raw(&dev_signing_key(), "dev-1", payload)` (`.expect("dev key signs")` is fine in test code); `sign_token` keeps its signature. Add `pub(crate) fn dev_signing_key() -> ed25519_dalek::SigningKey` built from `DEV_SEED` (the server keeps `ed25519-dalek` as a dev-dependency only if the build needs it there; move it from `[dependencies]` to `[dev-dependencies]` when nothing else uses it).
- **`tools/license/src/main.rs`**: `sign` calls `llmobs_token::sign_raw`; delete its own `TAG` and `valid_kid` copies (map `TokenError::Malformed` to its `BadKid` error).
- **`fixtures/license/payloads/wrong-kind.json`** becomes a complete control-plane payload, so that it parses and is refused as the wrong kind by the licence parser:

  ```json
  {"kind": "control_plane", "id": "01900000-0000-7000-8000-000000000502", "issued_at": "2026-01-01T00:00:00Z", "expires_at": "2026-01-01T00:05:00Z", "audience": "http://localhost:8080", "scope": "orgs"}
  ```

  Add `fixtures/license/payloads/org-pro.json`:

  ```json
  {"kind": "org", "id": "01900000-0000-7000-8000-000000000520", "issued_at": "2026-01-01T00:00:00Z", "expires_at": "2099-01-01T00:00:00Z", "grace_ends_at": "2099-02-01T00:00:00Z", "audience": "http://localhost:8080", "org_id": "01900000-0000-7000-8000-000000000010", "plan": {"id": "pro", "name": "Pro", "add_ons": ["teams"]}, "features": ["retention_management", "project_roles"], "limits": {"units_included": 200000, "units_cap": null, "ingest_spans_per_sec": 20000, "ingest_burst": 100000, "data_access_days": 1825, "max_seats": null, "max_projects": null, "max_alert_rules": null, "max_active_online_rules": null}}
  ```

  The tool scans `payloads/*.json`, so it signs the new file by itself; add `org-pro` to the crate's `fixture_tokens_verify` list. Run `just license-fixtures` (tokens are regenerated and committed).
- **`justfile` `check-ee`**: add `crates/token` to the `for dir in` list and to the `case` allow-list.
- **Root `LICENSE`**: add `- crates/token/` to the list of Elastic-2.0 directories. **`docs/spec/12-operations.md`** § Licence keys, plans and entitlements, "Open core": add `crates/token/` to the list.
- **`AGENTS.md`** § Layout: add `  token/     llmobs-token    signed licence, org and control-plane tokens (Elastic-2.0, D-P-26)`; § Crate dependency graph: "`token` depends on no workspace crate; `server` depends on it, and so do `tools/license` and the control plane (by git)".

### Tests (`llmobs_token` tests)

1. All of M1's token tests, moved (keys decode; dev seed matches dev key; round trip; tampering; malformed; unknown key; release key needs prod signature; error texts never quote the token; fixture tokens verify).
2. `keys_are_bound_to_kinds`: a dev-signed token whose kid is rewritten to `prod-1` and whose payload is `kind: org` is `BadSignature` (signature first); a token signed with the dev seed under kid `dev-1` of each kind verifies; build a test-only key table is not possible for `KeyNotAllowed` with real keys, so test `KeyNotAllowed` through a crate-private `verify_with(token, keys: &[(BuiltInKey, VerifyingKey)])` that `verify` calls with the built-in keys: a dev-seed key bound only to `License` refuses an `Org` payload with `KeyNotAllowed`.
3. `payload_errors`: signed `b"not json"` → `Payload`; signed `{"kind": "nope"}` → `Payload`; signed `{"kind": "org"}` → `Payload` (missing fields).
4. `org_claims_round_trip`: build an `OrgClaims` with every limit `Some(None)` except `data_access_days: Some(Some(30))`, sign with `sign`, `verify` → equal claims; the payload JSON (decode the middle part) contains `"units_cap":null` and `"data_access_days":30`; a `LimitsClaim::default()` serializes to `{}` and parses back to all `None`.
5. `rfc3339_accepts_offsets_and_writes_nanos`: `"2026-01-01T01:00:00+01:00"` parses to 2026-01-01T00:00:00Z; serialization writes `"2026-01-01T00:00:00.000000000Z"`; `"2026-01-01"` is an error whose text does not contain `2026`.

Server tests keep passing with the adaptations above; `cargo test -p llmobs-license-tool` passes with regenerated fixtures.

### Done

`just check-ee`, `just lint`, `cargo test -p llmobs-token`, `cargo test -p llmobs-server ee::`, `cargo test -p llmobs-server entitlements::`, `cargo test -p llmobs-server routes::admin`, `cargo test -p llmobs-license-tool`, then `just check`. `Cargo.lock` gains the `llmobs-token` package and the dependency-list changes of `llmobs-server` and `llmobs-license-tool`; no new third-party version.

---

## P2 — Instance entitlement defaults

### `Feature` (`crates/server/src/api/entitlements.rs`)

Add `RetentionManagement` **first** in declaration order (core features before `ee` ones), with name `retention_management` and `locked_detail` `"Data retention management is part of the Pro plan."`. `Feature::ALL` lists the three in order `RetentionManagement, ProjectRoles, SsoEnforcement`. Add:

```rust
/// Enterprise features live in `ee` and are never granted by config (D-P-10).
pub fn is_ee(self) -> bool {
    matches!(self, Self::ProjectRoles | Self::SsoEnforcement)
}
```

Web, so the exhaustive switches still compile after `just openapi`: `web/src/components/plan/feature-label.ts` `featureOffer` gains `case "retention_management": return "Available on the Pro plan";`; `web/src/ee/license-card.tsx` `FEATURE_NAMES` gains `retention_management: "Data retention management"`.

### Config (`crates/server/src/config.rs`)

```rust
/// `[entitlements.defaults]`: what an org without a valid token gets (D-P-10, D-P-13).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct EntitlementDefaults {
    /// Shown as the org's plan. Default `"Self-hosted"`.
    pub plan_name: String,
    /// Core features granted. `None` (the key absent): every core feature.
    pub features: Option<Vec<String>>,
    /// Limits; unset means unlimited. Same names as D-P-13.
    pub units_included: Option<u64>,
    pub units_cap: Option<u64>,
    pub ingest_spans_per_sec: Option<u64>,
    pub ingest_burst: Option<u64>,
    pub data_access_days: Option<u64>,
    pub max_seats: Option<u64>,
    pub max_projects: Option<u64>,
    pub max_alert_rules: Option<u64>,
    pub max_active_online_rules: Option<u64>,
}

impl Default for EntitlementDefaults {
    fn default() -> Self {
        Self { plan_name: "Self-hosted".to_owned(), features: None, /* every limit None */ }
    }
}
```

`EntitlementsConfig` gains `pub defaults: EntitlementDefaults` (it already has `#[serde(deny_unknown_fields, default)]`). `validate_entitlements` returns early when `upgrade_url` is `None`; restructure it so the `upgrade_url` check (when set) and the defaults checks always both run. It also checks:
- `plan_name.trim()` is 1–64 characters → else `Invalid { key: "entitlements.defaults.plan_name", reason: "must be 1 to 64 characters" }`; store it trimmed;
- every name in `features` is a known non-ee feature (`Feature::from_name(name).is_some_and(|f| !f.is_ee())`) → else `Invalid { key: "entitlements.defaults.features", reason: "must list core features only: retention_management" }`.

`validate_entitlements` may need `&mut EntitlementsConfig` to store the trimmed name; change its signature and the call.

Config tests: `defaults_are_self_hosted_and_unlimited` (no section → plan "Self-hosted", features `None`, every limit `None`); `cloud_defaults_load` (TOML with `plan_name = "Free"`, `features = []`, `units_included = 100000`, `units_cap = 100000`, `max_seats = 1`, `max_projects = 1` → those values); `defaults_refuse_ee_and_unknown_features` (`["project_roles"]`, `["nope"]` → the error above); `defaults_refuse_an_empty_plan_name` (`"  "`); `defaults_refuse_unknown_keys` (`max_users = 1` → an unknown-key error).

### Entitlements (`crates/server/src/entitlements/mod.rs`)

```rust
/// The nine limits of D-P-13 for one org. `None` is unlimited.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub(crate) struct Limits {
    pub(crate) units_included: Option<u64>,
    pub(crate) units_cap: Option<u64>,
    pub(crate) ingest_spans_per_sec: Option<u64>,
    pub(crate) ingest_burst: Option<u64>,
    pub(crate) data_access_days: Option<u64>,
    pub(crate) max_seats: Option<u64>,
    pub(crate) max_projects: Option<u64>,
    pub(crate) max_alert_rules: Option<u64>,
    pub(crate) max_active_online_rules: Option<u64>,
}

/// The instance defaults, resolved from config once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Defaults {
    pub(crate) plan_name: String,
    pub(crate) features: FeatureSet,
    pub(crate) limits: Limits,
}

impl Defaults {
    /// From config. `features: None` is every core feature.
    pub(crate) fn from_config(config: &EntitlementDefaults) -> Self { … }
}
```

(Drop the `serde::Serialize` derive on `Limits` if nothing serializes it; P9 maps it to the API type.)

`Entitlements` becomes `{ features: FeatureSet, pub(crate) limits: Limits }`; replace `with_features` by `pub(crate) fn new(features: FeatureSet, limits: Limits) -> Self` and update its callers and tests. (Where the plan came from is display only: P4's `PlanView.source`.) `FeatureSet` gains `pub(crate) fn union(self, other: Self) -> Self`.

`EntitlementsState` gains `defaults: Defaults`; `EntitlementsState::new(license, defaults)`; `pub(crate) fn defaults(&self) -> &Defaults`. `serve.rs` and `test_support::open` build `Defaults::from_config(&config.entitlements.defaults)`.

`of(state, org)` (still ignoring `org` until P4):

```rust
let now = state.clock.now_ns();
let defaults = state.entitlements.defaults();
Entitlements::new(
    defaults.features.union(state.entitlements.license().active_features(now)),
    defaults.limits,
)
```

Self-hosted without config: every core feature (`retention_management`) and no limits, as before for `ee` features (none without a licence).

### Tests (`entitlements::tests`)

- `defaults_grant_every_core_feature`: `TestApp::new()` → `allows(Feature::RetentionManagement)` true, `allows(ProjectRoles)` false, `limits == Limits::default()`.
- `configured_defaults_apply`: config with `features = Some(vec![])`, `units_cap = Some(100_000)`, `max_seats = Some(1)` → `RetentionManagement` not allowed, those limits set.
- `licence_adds_ee_features_to_defaults`: `features = Some(vec![])` and `license_key(&["project_roles"])` → `ProjectRoles` allowed, `RetentionManagement` not.
- Update M1's tests for `Entitlements::new` and the `Feature::ALL` order (`feature_names_round_trip` and `features()` order).
- M1's tests that assert an empty feature list now see `retention_management`, granted by default: `no_license_allows_nothing`, `expired_license_allows_nothing` and `refused_key_runs_without_license` in `entitlements/mod.rs` assert instead that `ProjectRoles` and `SsoEnforcement` are not allowed and `RetentionManagement` is.

`GET /api/v1/orgs/{org}/entitlements` now lists `retention_management` too: update `routes::entitlements` tests (`["retention_management", "project_roles"]` with the licence, `["retention_management"]` without). `fixtures/api/static/org-entitlements.json` becomes `["retention_management", "project_roles", "sso_enforcement"]`. `web/src/routes/$org/$project/settings/members.test.tsx` and other web tests that override `features` keep working (they list what they need). Run `just openapi`, `just api-examples`.

### Done

`just check`.

---

## P3 — Storing org tokens

### Migration

`crates/server/migrations/meta/NNNN_entitlements.sql`, where `NNNN` is the highest four-digit prefix in that directory plus one (`0005` on this branch; tracks M, I and K may have added migrations first — then take the next number):

```sql
-- Track P M2 (D-P-11, D-P-30). orgs.limits was never read; org tokens replace it (D179).
ALTER TABLE orgs DROP COLUMN limits;

-- The newest accepted org token, as received (D-P-11).
CREATE TABLE org_entitlements (
    org_id TEXT PRIMARY KEY REFERENCES orgs(id) ON DELETE CASCADE,
    token TEXT NOT NULL,
    token_id TEXT NOT NULL,
    issued_at INTEGER NOT NULL,
    received_at INTEGER NOT NULL
);

-- An org's data window that stopped applying, and when (D-P-30). NULL days = unlimited.
CREATE TABLE org_window_ends (
    org_id TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
    ended_at INTEGER NOT NULL,
    data_access_days INTEGER,
    PRIMARY KEY (org_id, ended_at)
);
```

`crates/server/src/meta/orgs.rs`: change the `OrgRow` doc line "`limits` is not read in D." to describe the row only. Nothing selects `limits` today; if the compiler or a test still names it, remove that use.

`crates/server/src/meta/mod.rs`: make `expected_version` `pub(crate)`. Tests that hard-code the migration count or versions must derive them: `admin.rs` `schema_mismatch_names_the_versions` (insert `expected_version() + 1`, assert `format!("this binary expects schema {e}, found {}", e + 1)` with `e = crate::meta::expected_version()`), and the tests in `meta/mod.rs` that assert the `_sqlx_migrations` row count is 4 and use versions 4/5 (use `expected_version()` and `expected_version() + 1`). The insta snapshot of the schema (`schema_matches_the_snapshot`, `crates/server/tests/snapshots/meta_schema.snap` or wherever it lives) changes: run `INSTA_UPDATE=always cargo test -p llmobs-server schema_matches_the_snapshot`, review the diff (two new tables, no `orgs.limits`), and commit the `.snap`.

### Store (`crates/server/src/meta/entitlements.rs`, new; `pub(crate) mod entitlements;` in `meta/mod.rs`)

```rust
//! `org_entitlements` and `org_window_ends` (D-P-11, D-P-30).

/// A stored org token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StoredToken {
    pub(crate) org_id: OrgId,
    /// The token as received. Never logged.
    pub(crate) token: String,
    pub(crate) token_id: Uuid,
    pub(crate) issued_at: i64,
    pub(crate) received_at: i64,
}

/// Every stored token, ordered by org id. Used at startup.
pub(crate) async fn all_tokens(pool: &SqlitePool) -> Result<Vec<StoredToken>, MetaError>;

/// The org's stored token, in the write transaction.
pub(crate) async fn token_of_tx(conn: &mut SqliteConnection, org_id: OrgId) -> Result<Option<StoredToken>, MetaError>;

/// Inserts or replaces the org's token.
pub(crate) async fn put_token(conn: &mut SqliteConnection, token: &StoredToken) -> Result<(), MetaError>;

/// Records that `days` (None = unlimited) stopped applying to the org at `ended_at`, and
/// deletes the org's rows that ended before `ended_at − 30 days`. An existing row with the
/// same `(org_id, ended_at)` is replaced.
pub(crate) async fn record_window_end(conn: &mut SqliteConnection, org_id: OrgId, ended_at: i64, days: Option<u64>) -> Result<(), MetaError>;

/// Windows of the org that ended at or after `since`, newest first.
pub(crate) async fn window_ends_since(pool: &SqlitePool, org_id: OrgId, since: i64) -> Result<Vec<(i64, Option<u64>)>, MetaError>;
```

Ids are stored as text (`to_string()`), parsed back with `Uuid::parse_str` / `OrgId::from_uuid`; a row that does not parse is `MetaError::Corrupt { table: "org_entitlements" | "org_window_ends", column: … }` (follow the existing `Corrupt` usage in `meta/members.rs`). `days` binds as `Option<i64>` (`i64::try_from(d).unwrap_or(i64::MAX)`), and reads back with `u64::try_from` (a negative value is `Corrupt`).

### Tests (`meta::entitlements::tests`, with an org from `meta::orgs::insert_org`)

1. `put_and_read_back`: put, read with `token_of_tx` and `all_tokens`; a second put for the same org replaces it.
2. `cascade_on_org_delete`: deleting the `orgs` row removes both tables' rows (`PRAGMA foreign_keys` is on in `MetaDb`).
3. `window_ends_prune_after_30_days`: record at day 0 (`Some(30)`), day 20 (`None`), day 45 (`Some(120)`) → `window_ends_since(…, day 45 − 30 days)` returns day 45 then day 20; day 0 was pruned by the day-45 insert (it ended before day 15).
4. `orgs_limits_is_gone`: `SELECT limits FROM orgs` fails.

### Done

`just check`.

---

## P4 — Resolving entitlements from org tokens

### `crates/server/src/entitlements/tokens.rs` (new, MIT; `pub(crate) mod tokens;` in `entitlements/mod.rs`)

```rust
//! Org tokens (D-P-11): validation, the in-memory grants, the window history (D-P-30).

/// What a valid org token grants. Limits are already merged over the instance defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OrgGrant {
    pub(crate) token_id: Uuid,
    pub(crate) issued_at: i64,
    pub(crate) expires_at: i64,
    pub(crate) grace_ends_at: i64,
    pub(crate) plan: PlanInfo,
    pub(crate) features: FeatureSet,
    pub(crate) limits: Limits,
}

/// The plan a token names, for display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlanInfo {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) add_ons: Vec<String>,
}

/// Why an org token was refused. Never quotes the token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum OrgTokenError {
    #[error(transparent)]
    Token(#[from] llmobs_token::TokenError),
    #[error("the token is for another cell")]
    Audience,
    #[error("the token is for another org")]
    OrgMismatch,
    #[error("the token fields are inconsistent")]
    Fields,
    #[error("the token has expired")]
    Expired,
}

/// Verifies `token` as an org token for this cell and returns its org and grant.
/// Checks, in order: `llmobs_token::verify` (a non-org kind is `Token(WrongKind)`),
/// `audience == public_url` (`Audience`), then `issued_at <= expires_at <= grace_ends_at`,
/// a plan id and name of 1–64 characters (`Fields`). Dates against now are the caller's.
pub(crate) fn grant_from_token(token: &str, public_url: &str, defaults: &Limits) -> Result<(OrgId, OrgGrant), OrgTokenError>;
```

`features` is `FeatureSet::from_names(claims.features…)` — `ee` names are allowed in tokens (D-P-11: audience-bound). Limits merge field by field: `None` (missing) → the default's value, `Some(None)` → `None`, `Some(Some(n))` → `Some(n)`.

`public_url` is the cell's `PublicUrl` as its origin string (look at `crate::config::PublicUrl`; use the method that returns the origin, e.g. `as_str()`/`origin()`, without a trailing slash). Q sets `audience` to exactly that.

### State (`entitlements/mod.rs`)

`EntitlementsState` gains `orgs: std::sync::RwLock<HashMap<OrgId, Arc<OrgGrant>>>`. The cell's audience is always `state.config.public_url.origin()` (`crate::config::PublicUrl::origin`), never a copy:

```rust
impl EntitlementsState {
    /// Loads the stored tokens. A token that no longer verifies (a retired key, a changed
    /// `public_url`) is skipped with a `warn` naming the org id and the reason class.
    pub(crate) async fn load(meta: &MetaDb, license: LicenseState, defaults: Defaults, public_url: &str) -> Result<Self, MetaError>;

    /// The org's grant, if one is held (valid or not at `now`).
    pub(crate) fn grant(&self, org: OrgId) -> Option<Arc<OrgGrant>>;

    /// Replaces the org's grant if `grant.issued_at` is newer than the held one.
    pub(crate) fn put_grant(&self, org: OrgId, grant: OrgGrant);
}
```

Use `read()`/`write()` with `.unwrap_or_else(std::sync::PoisonError::into_inner)` (as `usage.rs` does). `serve.rs`: `let entitlements = EntitlementsState::load(&meta, license, defaults, config.public_url.origin()).await` before `AppState::new`; a `MetaError` aborts startup like the other startup reads there (release storage and meta as the surrounding error paths do). `test_support::open` does the same with `.expect`.

`of(state, org)` (now uses `org`):

```rust
let now = state.clock.now_ns();
let license = state.entitlements.license().active_features(now);
match state.entitlements.grant(org) {
    Some(grant) if now < grant.grace_ends_at => {
        Entitlements::new(grant.features.union(license), grant.limits)
    }
    _ => { /* the defaults, as in P2 */ }
}
```

**One implementation, two callers.** Write the logic once on `&EntitlementsState` so the offline admin command (P9) reuses it without an `AppState`:

```rust
impl EntitlementsState {
    /// What `org` may do at `now` (the body of `of`).
    pub(crate) fn resolve(&self, org: OrgId, now: i64) -> Entitlements;
    /// The plan view at `now` (D-P-32).
    pub(crate) fn plan_at(&self, org: OrgId, now: i64) -> PlanView;
    /// The largest data window (days; `None` = unlimited) that applied to the org during
    /// the last 30 days (D-P-30): the current one, every recorded end since `now − 30 days`,
    /// and the held token's window if it expired since then. Track P's M4 passes it to
    /// retention; P9's admin command shows it.
    pub(crate) async fn retention_window_at(&self, meta: &MetaDb, org: OrgId, now: i64) -> Result<Option<u64>, MetaError>;
}
```

`of` is a one-line caller of `resolve` with `state.clock.now_ns()`. Callers that need several values at one instant (P9) read `now` once and call `resolve` and `plan_at` with it. There is no `AppState` wrapper for `plan_at` or `retention_window_at`.

```rust
/// Where an org's plan and limits come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Source {
    /// `[entitlements.defaults]`: no valid org token.
    Defaults,
    /// The org's token, until its grace period ends.
    OrgToken,
}
```

```rust
/// The org's plan as shown to users (D-P-32).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlanView {
    pub(crate) id: Option<String>,   // None with the defaults
    pub(crate) name: String,         // the token's plan name, or `defaults.plan_name`
    pub(crate) add_ons: Vec<String>,
    pub(crate) source: Source,
    /// Token status at `now`: `Active` before `expires_at`, `Grace` before `grace_ends_at`.
    pub(crate) status: Option<TokenStatus>,
    pub(crate) expires_at: Option<i64>,
    pub(crate) grace_ends_at: Option<i64>,
}

/// An org token's state at one instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenStatus { Active, Grace }

```

An expired token (now ≥ `grace_ends_at`) gives the defaults' view (`source: Defaults`, `status: None`).

In `retention_window_at`, `None` anywhere in the set wins (unlimited); otherwise the maximum.

### Tests (`entitlements::tokens::tests` and `entitlements::tests`)

Add two helpers to `crate::test_support` (later phases use them too):

```rust
/// A dev-signed org token for `org_id`, audience the default test `public_url`'s origin.
/// Times are RFC 3339; `limits` is the JSON object of the `limits` claim.
pub(crate) fn org_token(org_id: OrgId, issued_at: &str, expires_at: &str, grace_ends_at: &str, features: &[&str], limits: serde_json::Value) -> String;
/// A dev-signed control-plane token for the default test `public_url`, issued at the
/// manual clock (2026-01-01T00:00:00Z), valid 5 minutes, with this scope.
pub(crate) fn control_plane_token(scope: &str) -> String;
```

(plan `{"id": "pro", "name": "Pro", "add_ons": ["teams"]}`, a fresh `Uuid::now_v7()` id; read the default `PublicUrl` in `test_support::default_config` and use its `origin()`.)

1. `grant_merges_limits_over_defaults`: defaults `max_seats = Some(1)`, `units_cap = Some(100)`; token limits `{"units_cap": null, "data_access_days": 120}` → `units_cap None`, `data_access_days Some(120)`, `max_seats Some(1)`.
2. `grant_refusals`: wrong audience → `Audience`; a licence token → `Token(WrongKind)`; `grace_ends_at < expires_at` → `Fields`; empty plan name → `Fields`; garbage → `Token(Malformed)`.
3. `token_applies_until_grace_ends`: put a grant (expires 2026-02-01, grace 2026-03-01); at the manual clock (2026-01-01) `plan_at(..).source == Source::OrgToken` and the token's features apply; set the clock to 2026-02-15 → still the token, `plan_at(..).status == Some(TokenStatus::Grace)`; to 2026-03-01 → `plan_at(..).source == Source::Defaults` and the defaults' features apply. (Use the manual clock's setter from `crate::clock`; look at how other tests advance it.)
4. `older_grant_does_not_replace_newer`: `put_grant` with `issued_at` 10 then 5 → the held one has 10.
5. `ee_features_from_a_token_need_no_licence`: token features `["project_roles"]`, no licence → `allows(ProjectRoles)`.
6. `load_skips_tokens_that_no_longer_verify`: store (through `meta::entitlements::put_token`) one valid token and one with another audience; `EntitlementsState::load` holds only the valid one; `capture_logs` shows a `warn` with the org id and no token text.
7. `retention_window_takes_the_largest_of_30_days` (through `state.entitlements.retention_window_at(&state.meta, org, now)`): current token `data_access_days 30`; a recorded end 10 days ago of `1825` → `Some(1825)`; an end 40 days ago of `None` is ignored (pruned or older than 30 days) → still `Some(1825)`; an end 5 days ago of `None` → `None`.

### Done

`just check`.

---

## P5 — The org-token PUT

### Route (`crates/server/src/routes/entitlements.rs`)

```rust
RouteDef {
    method: axum::http::Method::PUT,
    path: "/api/internal/v1/orgs/{org_id}/entitlements",
    access: Access::Public,
    documented: true,
    handler: put(put_org_token),
}
```

The path parameter is `{org_id}`, not `{org}`: the route is public (the signature is the credential, D-P-11) and the route-table test requires `{org}` only on `Access::Org` routes. It takes a UUID only.

Body (`crates/server/src/api/entitlements.rs`):

```rust
/// `PUT /api/internal/v1/orgs/{org_id}/entitlements` (D-P-11).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PutOrgTokenRequest {
    /// A signed `kind: org` token. Never logged.
    pub token: String,
}
```

Handler, in this order (every refusal is a problem response; details are the error `Display` texts; nothing quotes the token):

1. `ClientIp(ip)` extractor (as `routes/auth.rs` uses it), `error::Path(OrgIdPath { org_id })` with `#[derive(Debug, Deserialize)] struct OrgIdPath { org_id: Uuid }` (as `UserPath` in `routes/admin.rs`), `error::Json<PutOrgTokenRequest>`.
2. `grant_from_token(&body.token, state.config.public_url.origin(), &state.entitlements.defaults().limits)`. On `Err(error)`: first `state.limits.ip_forms.check(&ip, now)` → on `Err(retry)` return `ApiError::RateLimited { retry_after_s: retry.0.as_secs().max(1) }`; otherwise `ApiError::BadRequest { detail: Some(error.to_string()), errors: Vec::new() }`. Verified tokens never touch the limiter (D-P-11 as amended by D-P-33).
3. Token `org_id` ≠ path `org_id` → 400 `OrgTokenError::OrgMismatch` text.
4. `now >= grant.grace_ends_at` → 400 `OrgTokenError::Expired` text.
5. `orgs::org_by_id` is `None` or its `state` is not `active` → `ApiError::NotFound` (checked again inside the transaction with `org_by_id_tx`, which P7 also needs — add it here next to `org_by_id`: a concurrent drop then answers 404, not a foreign-key 500).
6. In one `state.meta.write` transaction: `token_of_tx`; if its `token_id` equals the new one → outcome `Same`; else if its `issued_at >= grant.issued_at` → outcome `Stale`; else record the windows that end now (re-derive the stored token's grant with `grant_from_token` on its text; if it no longer verifies, treat it as absent):
   - stored token still valid (`now < its grace_ends_at`): its window ended now → `record_window_end(conn, org, now, stored_window)` when it differs from the new grant's;
   - stored token expired (`its grace_ends_at <= now`): its own window ended at its `grace_ends_at` → `record_window_end(conn, org, stored.grace_ends_at, stored_window)` (the row is pruned if older than 30 days); then the defaults' window ended now → `record_window_end(conn, org, now, defaults_window)` when it differs from the new grant's;
   - no stored token: the defaults' window ended now, recorded when it differs.
   Any difference is recorded, growth included; only the maximum matters. Then `put_token`. Outcome `Stored`.
7. After the commit: `Stored` → `state.entitlements.put_grant(org, grant)` and `tracing::info!(target: "audit", org_id = %org, token_id = %grant.token_id, "org entitlements updated")` (no plan or limit values: payload); `Same` → nothing; both answer `204 No Content`. `Stale` → `ApiError::Conflict` with detail `"a token with the same or a newer issued_at is stored"` (use the existing conflict constructor; look at `ApiError` for its shape).

### OpenAPI

A new tag `internal` ("Endpoints the cloud control plane calls on a cell"): add it to the tags list in `api/openapi.rs` and to `TAGS` in `crates/server/tests/openapi.rs`. Operation `put_org_entitlements_token`, tag `internal`, `.securities(Some(Vec::new()))` (public, like the auth routes), path parameter `org_id` (UUID), request body `PutOrgTokenRequest`, responses `204` (no body), `400, 404, 409, 429`. Register `PutOrgTokenRequest` in `collect_schemas`. `tests/openapi.rs`: the `OPERATIONS` entry, an expected-errors branch `["400", "404", "409", "429"]`, and whatever the lint needs for a public operation (copy how `post_login` is treated). `just openapi`. No API example (the response has no body).

### Tests (`routes::entitlements::tests`)

Seed an org (`app.seed()`), make an org token for `seeded.org_id` with `test_support::org_token`.
1. `stores_a_token_and_applies_it`: PUT → 204; `state.entitlements.plan_at(org, now).source == Source::OrgToken`; `meta::entitlements::token_of_tx` has it; the same PUT again → 204 and nothing changes (`received_at` unchanged).
2. `newer_replaces_and_older_conflicts`: issued 10:00 then 11:00 → both 204; then 10:30 → 409; the held grant is 11:00's.
3. `refusals`: garbage token → 400 detail `the token is malformed`; licence token → 400 `the token is not of the expected kind`; wrong audience → 400 `the token is for another cell`; org mismatch → 400; expired (grace before the clock) → 400 `the token has expired`; unknown org id (a fresh v7 uuid) → 404; a `deny_unknown_fields` body `{"token": "…", "x": 1}` → 400.
4. `failed_tokens_hit_the_limiter_and_good_ones_do_not`: 6 PUTs with a garbage token from one peer IP (`TestRequest::peer`) → the 6th is 429 with `Retry-After`; then a valid token from the same IP → 204; 10 valid PUTs in a row (fresh tokens with increasing `issued_at`) → all 204.
5. `window_shrink_is_recorded`: a token with `data_access_days 1825`, then a newer one with `30` → `window_ends_since` has one row with `Some(1825)` at the clock's time; a third with `30` again records nothing more.
6. `audit_log_has_no_token`: `capture_logs` around a PUT: contains `org entitlements updated` and the org id, not `llmobs1.`.
7. `expired_token_window_is_kept_after_replacement`: a token with `data_access_days 1825` and grace ending 2025-12-20 stored directly (`meta::entitlements::put_token` + `put_grant`), defaults without a window; PUT a new token with `30` at the clock (2026-01-01) → `window_ends_since` holds `(2025-12-20, Some(1825))` and `(now, None)`; `retention_window_at` is `None` (unlimited, from the defaults that applied since 2025-12-20).

The `access_suite` drives documented routes automatically; if it needs an entry for a public PUT with a path parameter, follow how it treats `post_login`.

### Done

`just openapi` twice with no change; `just check`.

---

## P6 — Control-plane bearer tokens

### Principal (`crates/server/src/auth/principal.rs`)

```rust
/// The cloud control plane, by a `kind: control_plane` token (D-P-24, D-P-28).
ControlPlane {
    /// Payload `id`, for audit logs.
    token_id: Uuid,
    /// The token's scope.
    scope: String,
},
```

In `bearer_principal`, right after reading the token and before `secret::parse_key` (compute `let now = state.clock.now_ns();` first; it is defined later in the function today, move it up): if `token.starts_with("llmobs1.")` (`format!("{}.", llmobs_token::TOKEN_TAG)`), return `control_plane_principal(state, token, now)`:

```rust
fn control_plane_principal(state: &AppState, token: &str, now: i64) -> Result<Principal, ApiError> {
    let Ok(verified) = llmobs_token::verify(token) else { return Err(ApiError::Unauthorized) };
    let llmobs_token::Claims::ControlPlane(claims) = verified.claims else { return Err(ApiError::Unauthorized) };
    let five_minutes = 300 * 1_000_000_000;
    let skew = 60 * 1_000_000_000;
    if claims.audience != state.config.public_url.origin()
        || claims.expires_at.saturating_sub(claims.issued_at) > five_minutes
        || now < claims.issued_at.saturating_sub(skew)
        || now >= claims.expires_at
    {
        return Err(ApiError::Unauthorized);
    }
    Ok(Principal::ControlPlane { token_id: claims.id, scope: claims.scope })
}
```

A control-plane token is not single-use: a captured one works until it expires (at most 5 minutes). The internal endpoints are idempotent, so this is accepted (D-P-28).

Every other `match` on `Principal` gains the arm (the compiler lists them, e.g. in `routes/me.rs`, `routes/saved_views.rs`, `routes/scores.rs`, `routes/streams.rs`). `session_user` and every user-needing path: `ApiError::Forbidden { code: ProblemCode::Forbidden }`. Logs and audit lines that print a principal print `control_plane` and the `token_id`.

### Access (`crates/server/src/http/access.rs`)

```rust
/// The control plane, with a `control_plane` token of this scope (D-P-28).
ControlPlane {
    /// The scope the token must carry.
    scope: &'static str,
},
```

In `prepare`: `Access::ControlPlane { scope } => match &principal { Principal::ControlPlane { scope: held, .. } if held == scope => {}, Principal::ControlPlane { .. } => return Err(forbidden()), _ => return Err(forbidden()) }`. The other variants refuse a control-plane principal. Two checks in `prepare` are `matches!`, not exhaustive, so the compiler will not point at them: the `Access::User` arm (`if matches!(principal, Principal::ApiKey { .. })`) becomes `if matches!(principal, Principal::ApiKey { .. } | Principal::ControlPlane { .. })` → `forbidden()`, and the early check of the `Access::Org` arm becomes the same pattern → `ApiError::NotFound`. `InstanceAdmin`, `Pat` and `Key` already refuse anything else. `Project`: in `grant_project`, a `ControlPlane` principal → `ApiError::NotFound` (add the arm where it matches on the principal). `route_table_matches_parameters_and_openapi_security`: `Access::ControlPlane { .. }` → `Some(vec![SecurityRequirement::new("bearer", …)])`. `access_suite.rs`: add a caller `Who::ControlPlane` whose credential is `test_support::control_plane_token("orgs")`, and the variant to its `match access`: a `Who::ControlPlane` caller is allowed only on `Access::ControlPlane { scope: "orgs" }` routes, gets 404 on `Access::Org` and `Access::Project` routes, and 403 everywhere else; every other caller gets 403 on `Access::ControlPlane` routes (401 without credentials, as the suite already expects). In `access_suite.rs` (in `crates/server/src/http/`) this means: `expected()`'s `Access::User` arm returns 403 for `Who::ControlPlane`; its `Access::Org` arm returns 404 for it before the `session_only` check; its `Access::Project` arm returns 404 for it before the key and role checks; `org_membership` and `project_role` get a `Who::ControlPlane` arm (no membership, no role); `assert_every_route_declares_access` gets the `Access::ControlPlane` arm; and `concrete_path` replaces `{org_id}` with the suite org's id (so P5's PUT is exercised with a real id).

No route uses `Access::ControlPlane` until P7.

### Tests

`auth::principal::tests`:
1. `control_plane_token_resolves`: a dev-signed `{"kind": "control_plane", "id": …, "issued_at": "2026-01-01T00:00:00Z", "expires_at": "2026-01-01T00:05:00Z", "audience": "<test public_url>", "scope": "orgs"}` as `Authorization: Bearer …` → `Principal::ControlPlane { scope: "orgs", .. }`.
2. `control_plane_refusals` → `Unauthorized`: lifetime 6 minutes; `now` before `issued_at − 60 s` (issued 2026-01-01T00:02:00Z); expired (`expires_at` = the clock); another audience; an org token; a licence token; a token with a bad signature.

`http::access::tests`: with a test route `Access::ControlPlane { scope: "orgs" }` (use `TestApp::with_route_table` as the module's other tests do): an `orgs` token → allowed; a `usage` token → 403; a session → 403; no credentials → 401; an `orgs` token on an `Access::Org` route → 404; an `orgs` token on an `Access::User` route → 403.

### Done

`just check`.

---

## P7 — Internal org creation

### `insert_org` takes the id

`crates/server/src/meta/orgs.rs`: `insert_org(conn, id: OrgId, name, slug, now)` and `insert_org_with_derived_slug(conn, id: OrgId, name, now)` (it passes `id` to each attempt). Every caller passes `OrgId::new_v7()` (the compiler lists them, about twenty: routes, bootstrap, serve, mcp and many tests). No wrapper keeps the old signature (`AGENTS.md`). Comment on `insert_org_with_derived_slug`: a primary-key collision also maps to `Conflict` and would be retried as a slug collision; callers with a chosen id check `org_by_id_tx` first in the same transaction.

### Problem code

`ProblemCode::OwnerNotAllowed` (`owner_not_allowed`, status 422, reason `"Unprocessable Content"`; doc `/// The org's owner cannot be that user (disabled or instance admin).`), `ApiError::OwnerNotAllowed` (no fields; problem without detail). Add to the status test and `error_statuses`. `api/openapi.rs` `problem(422)` text becomes `"The query took longer than its time limit or needed more memory than allowed, or the owner cannot be that user (code owner_not_allowed)."`.

### API types (`crates/server/src/api/internal.rs`, new; `pub mod internal;` in `api/mod.rs`)

```rust
/// `POST /api/internal/v1/orgs` (D-P-29).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateInternalOrgRequest {
    /// The org id the control plane chose. A UUIDv7.
    pub id: Uuid,
    /// Display name, 1–100 characters after trimming (as `POST /api/v1/orgs`).
    pub name: String,
    /// The first owner.
    pub owner: InternalOwner,
}

/// The owner of a new org.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct InternalOwner {
    /// Email; normalized like every email (D136).
    pub email: String,
    /// Display name for a new user. Ignored when the user exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// The created or existing org.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct InternalOrg {
    pub id: Uuid,
    pub slug: String,
}
```

### Route (`crates/server/src/routes/internal_orgs.rs`, new; `pub(crate) mod internal_orgs;`; wired in `http/access.rs`)

`POST /api/internal/v1/orgs`, `Access::ControlPlane { scope: "orgs" }`, `documented: true`. Handler:
1. `body.id.get_version_num() != 7` → `field_error("id", "must be a UUIDv7")` (use the module-local helper pattern of `routes/orgs.rs`).
2. Name: the same trimming and length check as `routes/orgs.rs`: make `trimmed_name` and `field_error` in `routes/orgs.rs` `pub(crate)` and reuse them. The owner's `name`, when present: trimmed; empty → `None`; over 100 characters → `field_error("owner.name", …)` with the message `trimmed_name` uses.
3. Email: `normalize_email` → `field_error("owner.email", "invalid email")`.
4. One `state.meta.write` transaction:
   - `orgs::org_by_id_tx(conn, id)` (added in P5) → `Some(org)` → outcome `Existing(org)` (whatever its owners; an idempotent retry).
   - `users::user_by_email_tx` → `Some(user)`: if `disabled_at.is_some() || is_instance_admin` → outcome `OwnerNotAllowed`; else reuse it unchanged. `None` → `users::insert_user` with `NewUser { email, name: owner.name (trimmed, empty → None), password_hash: None, email_verified_at: Some(now), is_instance_admin: false }` (check `NewUser`'s fields).
   - `orgs::insert_org_with_derived_slug(conn, OrgId::from_uuid(id), &name, now)`, `add_org_member(conn, org.id, user_id, OrgRole::Owner, now)` → outcome `Created(org)`. No project is created.
5. `Created` → `201` `InternalOrg`, audit `tracing::info!(target: "audit", org_id = %org.id, actor = "control_plane", token_id = %token_id, "org created")` (the token id from the `Principal::ControlPlane` extension); `Existing` → `200`; `OwnerNotAllowed` → `ApiError::OwnerNotAllowed`.

Never log the email or the name.

### OpenAPI

Operation `post_internal_org`, tag `internal`, bearer security (`.securities(Some(vec![bearer]))` — look at how key-only routes declare bearer-only security), body `CreateInternalOrgRequest`, `201` and `200` `InternalOrg`, errors `400, 401, 403, 422, 429`. Register the three types. `tests/openapi.rs`: `OPERATIONS`, expected-errors branch, and the session/bearer security lint (bearer-only, like the key routes). `just openapi`.

### Tests (`routes::internal_orgs::tests`)

Use `test_support::control_plane_token(scope)`.
1. `creates_org_and_new_owner`: `POST` with a fresh v7 id, name "Acme Cloud", owner `new@example.com` / "New Owner" → 201 `{id, slug: "acme-cloud"}`; the user exists with `email_verified_at` set and no password hash; they are the owner; the org has no projects.
2. `reuses_an_existing_user`: insert a non-admin user with `meta::users::insert_user` (`existing@example.com`, name "Existing") → 201; that user is owner; their name is unchanged. (`TestApp::seed`'s `ada@example.com` is an instance admin: use her in test 4.)
3. `same_id_is_idempotent`: the same request twice → 201 then 200 with the same slug; after transferring ownership in the DB, a third call → 200.
4. `refuses_disabled_and_admin_owners`: a disabled user (`insert_user` then `set_disabled`) → 422 `owner_not_allowed`; `ada@example.com` from `TestApp::seed` (an instance admin) → 422; no org created.
5. `validation`: a v4 id → 400 field `id`; blank name → 400; bad email → 400; unknown field → 400.
6. `needs_the_orgs_scope`: a `usage` token → 403; a session cookie → 403; no credentials → 401.
7. `slug_collision_gets_a_suffix`: an existing org `acme-cloud` → the new org's slug is `acme-cloud-2`.

### Done

`just openapi` twice with no change; `just check`.

---

## P8 — `orgs.user_create`

### Config

```rust
/// `[orgs]` (D-P-31).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct OrgsConfig {
    /// Users may create orgs. `false` on a cloud cell, where the control plane creates them.
    pub user_create: bool,
    /// Where "New organization" sends users when `user_create` is false. Unset: no link.
    pub create_url: Option<String>,
}
// Default: user_create true, create_url None.
```

`ServerConfig.orgs: OrgsConfig` (`#[serde(default)]`) after `entitlements`; every literal `ServerConfig { … }` gains `orgs: OrgsConfig::default(),`. Validate `create_url` like `upgrade_url` but with no placeholder (`http`/`https` with a host) → `Invalid { key: "orgs.create_url", reason: "must be an http or https URL" }`. Env: `LLMOBS_ORGS__USER_CREATE=false` works through the split provider. Config tests: default, `user_create = false` with a URL, a bad URL.

### Server

`routes/orgs.rs` `create_org`: right after the principal check, `if !state.config.orgs.user_create { return Err(ApiError::Forbidden { code: ProblemCode::Forbidden }); }`. `crate::bootstrap`'s setup creates the first org regardless (it is the operator's own bootstrap; leave it).

`AuthConfig` (`api/account.rs`) gains:

```rust
/// Users may create orgs (D-P-31).
pub org_create_allowed: bool,
/// Where "New organization" goes when users may not create orgs. Omitted when unset.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub org_create_url: Option<String>,
```

set in the `GET /api/auth/config` handler. Document both fields in `docs/spec/09-api.md` § Auth, on the `GET /config` row.

`api/openapi.rs`: the `post_org` operation's errors gain `403` (`[400, 401, 403, 409, 429]`); `tests/openapi.rs`: the `post_org` expected-errors branch gains `"403"`. Update `fixtures/api/static/auth-config.json` (`"org_create_allowed": true`), `just openapi`, `just api-examples`.

Tests: `routes::orgs::tests::user_create_off_refuses_new_orgs` (config `user_create = false` → `POST /api/v1/orgs` 403; the internal route of P7 still creates); `routes::auth` config test: the two fields are returned.

### Web

- `web/src/components/org-switcher.tsx`: read `useQuery(authConfigQuery)` (from `@/api/auth`; not suspense, so the switcher renders while it loads); while it is pending, behave as allowed; when `org_create_allowed` is false, the "New organization" item becomes a link (`<a href={org_create_url} target="_blank" rel="noopener noreferrer">`) when `org_create_url` is set, and is not rendered otherwise.
- `web/src/routes/setup/workspace.tsx` (the first-org page for a signed-in user with no org): read the config the same way (pending → allowed); when `org_create_allowed` is false, render instead a short text "Organizations are created from your account." with a link "Create an organization" to `org_create_url` (no link when unset), and no form.
- Tests: `org-switcher.test.tsx` (allowed → dialog item as today; not allowed with URL → a link with that `href`; not allowed without URL → no item); `setup/workspace.test.tsx` (not allowed → the text and link, no form). Override `/api/auth/config` with `server.use` spreading `fixtures/api/static/auth-config.json`, as `login.test.tsx` does.

### Done

`just check`.

---

## P9 — Plan and limits in the entitlements response; `admin entitlements show`

### API types (`api/entitlements.rs`)

```rust
/// Where the org's plan comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanSource {
    /// The instance defaults: no valid org token.
    Defaults,
    /// The org's token from the control plane.
    Token,
}

/// An org token's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    /// Before `expires_at`.
    Active,
    /// After `expires_at`, before `grace_ends_at`: the plan still applies.
    Grace,
}

/// The org's plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OrgPlan {
    /// Plan id from the token. Omitted with the defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Display name.
    pub name: String,
    /// Add-ons, e.g. `teams`.
    pub add_ons: Vec<String>,
    pub source: PlanSource,
    /// Omitted with the defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<PlanStatus>,
    /// Omitted with the defaults.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "crate::api::keys::optional_time")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub expires_at: Option<i64>,
    /// Omitted with the defaults.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "crate::api::keys::optional_time")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub grace_ends_at: Option<i64>,
}

/// The org's limits (D-P-13). An omitted limit is unlimited.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OrgLimits {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub units_included: Option<u64>,
    // … the same for units_cap, ingest_spans_per_sec, ingest_burst, data_access_days,
    // max_seats, max_projects, max_alert_rules, max_active_online_rules
}
```

`OrgEntitlements` gains `pub plan: OrgPlan` and `pub limits: OrgLimits` (after `features`). Map `Source::Defaults → PlanSource::Defaults`, `Source::OrgToken → PlanSource::Token`, `TokenStatus::Active/Grace → PlanStatus::Active/Grace`, and `Limits` field by field to `OrgLimits`; write these as `From` impls next to the API types. Register the new types in `collect_schemas`. `get_entitlements` reads `let now = state.clock.now_ns();` once and fills the response from `state.entitlements.resolve(org, now)` and `state.entitlements.plan_at(org, now)` (so both use the same instant). `fixtures/api/static/org-entitlements.json` becomes (pretty JSON, one array element per line, as the canonical check wants):

```json
{
  "features": [
    "retention_management",
    "project_roles"
  ],
  "plan": {
    "id": "pro",
    "name": "Pro",
    "add_ons": [
      "teams"
    ],
    "source": "token",
    "status": "active",
    "expires_at": "2026-02-05T00:00:00.000000000Z",
    "grace_ends_at": "2026-02-12T00:00:00.000000000Z"
  },
  "limits": {
    "units_included": 200000,
    "ingest_spans_per_sec": 20000,
    "ingest_burst": 100000,
    "data_access_days": 1825
  },
  "upgrade_url": "https://app.example.com/orgs/01900000-0000-7000-8000-000000000010/billing"
}
```

`just openapi`, `just api-examples`.

Tests (`routes::entitlements::tests`): the tests that compare the whole response body (P2's `features` checks) switch to checking `body["features"]` only, since the body now has more fields. Defaults → `plan: {name: "Self-hosted", add_ons: [], source: "defaults"}` and `limits: {}`; after a PUT of an org token → `plan` with id, status `active`, dates, and the token's limits (an unlimited one omitted).

### Admin command

`crates/server/src/admin.rs`:

```rust
/// One org's resolved entitlements, for `llmobs admin entitlements show` (D-P-32).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EntitlementsReport {
    pub org_id: Uuid,
    pub org_slug: String,
    pub plan: crate::api::entitlements::OrgPlan,
    pub features: Vec<crate::api::entitlements::Feature>,
    pub limits: crate::api::entitlements::OrgLimits,
    /// The largest data window of the last 30 days (D-P-30); omitted when unlimited.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retention_window_days: Option<u64>,
}

/// Resolves `org` (id or slug) offline from `meta.db`, the config and the licence.
///
/// # Errors
/// [`AdminError`] when `meta.db` is missing, or `AdminError::OrgNotFound` (a new variant,
/// displayed "no such org") when the org does not exist.
pub async fn entitlements(config: &ServerConfig, org: &str) -> Result<EntitlementsReport, AdminError>;
```

It opens `meta.db` like `list_users`, builds the licence from `config.license_key`, `Defaults::from_config`, `EntitlementsState::load(.., config.public_url.origin())`, takes the system clock, and calls P4's `resolve`, `plan_at` and `retention_window_at` — no duplicate logic.

`crates/cli/src/main.rs`: `AdminCommand::Entitlements { #[command(subcommand)] command: EntitlementsCommand }` with `EntitlementsCommand::Show { org: String, #[arg(long, value_enum, default_value = "table")] format: output::ListFormat, #[arg(long)] config: Option<PathBuf> }`. `table` prints `key: value` lines (org, plan, source, status, expires, grace ends, features, each limit or `unlimited`, retention window); enum values print as their JSON names (`serde_json::to_value(x)` then `as_str()`), instants as RFC 3339; `json` prints `serde_json::to_string_pretty(&report)`.

Remove every `allow(dead_code)` attribute M2 added (see § Conventions); `just lint` must pass.

Tests: `admin::tests::entitlements_show_reads_the_token` (a stored org token → the report's plan and limits; an unknown org → error); a CLI test following the existing `ListUsers` CLI tests.

### Done

`just check`.

---

## P10 — The org Plan page

- `web/src/components/settings/settings-nav.ts`: `orgSettingsNav` gains `{ to: "/$org/settings/plan", label: "Plan" }` after "General". (`settings-nav.test.ts` tests only `isProjectSettingsPath`; no change there.)
- `web/src/routes/$org/settings/plan.tsx` (new, MIT): `SettingsPage` titled "Plan", description "What this organization's plan includes." It reads `useOrgEntitlements(org)` (P10 of M1); pending → skeletons; error → throw. Content:
  - the plan name and add-ons joined with " + ", each add-on id with its first letter capitalised (`teams` → `Pro + Teams`); when `plan.source === "defaults"`, the line "Instance defaults apply." under it; when `status === "grace"`, a warning line "This plan expired on {expires date}. It stays in effect until {grace date}." (dates `iso.slice(0, 10)`);
  - "Features": one row per `Feature` in this order (`retention_management`, `project_roles`, `sso_enforcement`), with "Included" or a lock and `featureOffer(feature)`;
  - "Limits": a table with rows "Units per month (included)", "Monthly cap", "Ingest rate (spans/s)", "Ingest burst", "Data access", "Seats", "Projects", "Alert rules", "Active online evaluation rules"; values formatted with `new Intl.NumberFormat("en-US").format(n)` ("200,000") and "unlimited" when omitted; "Data access" shows `N days`;
  - `<UpgradeButton url={entitlements.upgradeUrl} />` at the bottom.
  - Any org member may view it (no admin gate).
- The route tree is generated by `pnpm test` / `just check-web` (`gen:routes`); no manual step.
- Tests `web/src/routes/$org/settings/plan.test.tsx`: the default fixture → "Pro + Teams", "Included" for project roles, a lock for SSO enforcement, "200,000" (or the formatter's output) for included units, "unlimited" for seats, "1,825 days"; `server.use` with `plan.source: "defaults"`, `name: "Self-hosted"`, `limits: {}` → "Instance defaults apply." and "unlimited" everywhere; `status: "grace"` → the grace warning; no `upgrade_url` → no Upgrade link.

- `docs/roadmap.md` row **P** Status: `**In progress.** M1 merged (open core, licence keys, entitlements core). M2 implemented: the llmobs-token crate, org tokens from the control plane, instance defaults, control-plane tokens, internal org creation, orgs.user_create, the Plan page. M3–M5 plans to follow.`

### Done

`just check-web`, then `just check`.

---

## P11 — The cloud signing keys

The user generated both keys on 2026-11-14 and keeps the private halves outside the repository: `cloud-1`'s for the control plane's secrets, `cloud-2`'s offline (D-P-27). The public keys are:

- `cloud-1` public key: `9Dd2255sYAaeYOUWa2tJOlDMFCDXBT0csdSVVrU4zYY=`
- `cloud-2` public key: `hhDDxpReDC1t83KURe1Yp09DXXmupihoMUm7YYqKWZY=`

Never generate or replace keys yourself.

`crates/token/src/keys.rs` `RELEASE_KEYS` gains, after `prod-1`:

```rust
BuiltInKey { kid: "cloud-1", public: "9Dd2255sYAaeYOUWa2tJOlDMFCDXBT0csdSVVrU4zYY=", kinds: &[Kind::Org, Kind::ControlPlane] },
// Offline spare (D-P-27): the control plane switches to it if `cloud-1` leaks; the next
// release then removes `cloud-1`.
BuiltInKey { kid: "cloud-2", public: "hhDDxpReDC1t83KURe1Yp09DXXmupihoMUm7YYqKWZY=", kinds: &[Kind::Org, Kind::ControlPlane] },
```

The existing `keys_decode` test covers them. Add `cloud_keys_cannot_sign_licences`: for `cloud-1` and `cloud-2`, `kinds` does not contain `Kind::License`, and `prod-1`'s `kinds` is exactly `[Kind::License]`.

`docs/roadmap.md` row **P** Status: add "the cloud-1 and cloud-2 keys" to the list of what M2 implemented.

### Done

`just check`.

## Report

When M2 is done, report: what was done per sub-phase; deviations and why; whether track M, I or K migrations changed the migration number; whether P11 ran (keys present) or is pending; anything left undone or unverified.
