# P · M1 — Open core and licence keys

Milestone 1 of track P ([`00-design.md`](00-design.md)). It splits the code into the MIT core and the `ee` directories (Elastic License 2.0), adds offline licence keys, the first `Entitlements` value, and moves the two enterprise features that exist today (per-project role overrides and SSO enforcement) into `ee`, behind the licence.

When M1 is done:
- the repository has an MIT `LICENSE`, three `ee` directories with the ELv2 text, and `just check-ee` keeps them honest;
- `cargo deny` checks dependency licences;
- `LLMOBS_LICENSE_KEY` takes a signed licence; release builds trust only the built-in production key;
- `GET /api/v1/orgs/{org}/entitlements` and `GET /api/v1/admin/license` exist;
- creating or changing a per-project override needs the `project_roles` feature (`403 plan_required` otherwise); removing one never does;
- `auth.enforce_sso_domains` applies only with a genuine licence listing `sso_enforcement` (expired or not);
- the UI shows the licence on the admin settings page and a lock with an Upgrade button on the project members page.

**Read first, in this order:**
1. `AGENTS.md` (rules, commands, privacy).
2. [`00-design.md`](00-design.md) § Decisions D-P-1 to D-P-8, D-P-10, D-P-12, D-P-14, D-P-15.
3. `docs/spec/08-auth.md` § Sign-up policy and § Authorization (RBAC); `docs/spec/12-operations.md` § Configuration and § Licence keys, plans and entitlements.
4. The code named in each sub-phase before you change it.

## Conventions

- **One commit per sub-phase**, with the subject given in the sub-phase. No attribution lines.
- **Spelling**: identifiers, files, config keys and JSON fields say `license`; prose and messages to users say "licence".
- **`ee` files**: every `.rs`, `.ts` and `.tsx` file under `crates/server/src/ee/`, `web/src/ee/` or `tools/license/` starts with exactly this first line:

  ```
  // SPDX-License-Identifier: Elastic-2.0
  ```

  and no other source file contains that string.
- **Errors**: server code returns `ApiError` (`crates/server/src/http/error.rs`). No `unwrap()`/`expect()` outside tests unless a comment states the invariant.
- **Privacy**: never log or return a licence key, a token, a payload field other than `id`, `kid`, `kind` and the status, or an email address. `customer` is shown on the admin API and page only.
- **Time**: `i64` ns; `state.clock.now_ns()`. JSON instants use `llmobs_core::time_serde` (and the `optional_time` helper of `crate::api::keys` for `Option<i64>`).
- **Tests**: Rust unit tests next to the code (`#[cfg(test)] mod tests`). Server tests use `crate::test_support::TestApp` (manual clock at `2026-01-01T00:00:00Z`). Web tests use Vitest + Testing Library on the MSW server; override an endpoint with `server.use(...)`, never `vi.mock`.
- **Contract**: when the OpenAPI document changes, run `just openapi` (it rewrites `crates/server/openapi.json` and `web/src/api/schema.gen.ts`; commit both). When the API examples change, run `just api-examples`.

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

`just check` must pass before every commit. Quick loops: `cargo test -p llmobs-server <name>`, `cargo test -p llmobs-license-tool`, `cd web && pnpm vitest run <file>`. P1 edits `.github/`, so it also runs `just check-workflows` (needs `actionlint` and `zizmor`).

## Sub-phases

| # | Subject | What |
|---|---|---|
| P1 | `Add the MIT and Elastic licences and the ee check` | `LICENSE`, ELv2 files, crate `license` fields, `cargo deny` licences, `just check-ee`, CI |
| P2 | `Add signed licence tokens` | `ed25519-dalek`, `crate::ee`, `ee/token.rs`, the dev key, the test signer |
| P3 | `Add the licence signing tool and fixtures` | `tools/license`, `fixtures/license/`, fixture tokens checked by the server |
| P4 | `Add licence claims and the entitlements core` | `api/entitlements.rs` (`Feature`), `entitlements/mod.rs`, `ee/license.rs` |
| P5 | `Read the licence key at startup` | `license_key` config, `AppInner.entitlements`, startup log, `TestApp::with_license` |
| P6 | `Add org entitlements and plan_required` | `ProblemCode::PlanRequired`, `entitlements.upgrade_url`, `GET /api/v1/orgs/{org}/entitlements` |
| P7 | `Add the admin licence endpoint` | `GET /api/v1/admin/license` |
| P8 | `Move project role overrides to ee` | `ee/project_roles.rs`, the gate on PUT |
| P9 | `Move SSO enforcement to ee` | `ee/sso_enforcement.rs`, the licence condition, the startup warning |
| P10 | `Show the licence and plan locks in the UI` | `web/src/components/plan/`, `web/src/ee/license-card.tsx`, admin settings |
| P11 | `Move the override editor to web ee` | `web/src/ee/project-roles/`, the members page lock, roadmap |

Run them in this order.

---

## P1 — Licences and `just check-ee`

### Files

**`LICENSE`** (new, repository root), exactly:

```
The llmobs source code is licensed under the MIT License below, except for the
contents of these directories, which are licensed under the Elastic License 2.0
(see the LICENSE file in each of them):

- crates/server/src/ee/
- web/src/ee/
- tools/license/

MIT License

Copyright (c) 2026 the llmobs authors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

**`crates/server/src/ee/LICENSE`** and **`web/src/ee/LICENSE`** (new): the Elastic License 2.0 text, byte for byte, from SPDX:

```bash
curl -sSfL https://raw.githubusercontent.com/spdx/license-list-data/main/text/Elastic-2.0.txt -o crates/server/src/ee/LICENSE
```

```bash
cp crates/server/src/ee/LICENSE web/src/ee/LICENSE
```

```bash
sha256sum crates/server/src/ee/LICENSE web/src/ee/LICENSE
```

Both must print `6448026f2e06ab3da9ed8ddda686694bba37179e88db92dea6d4aab516c957ea` (93 lines, first line `Elastic License 2.0`). If the download is unavailable or the hash differs, stop and report; do not type the text by hand. These two directories hold only `LICENSE` until P2 and P10 add code.

**`Cargo.toml`** (root), `[workspace.package]`: replace the line `# license: intentionally omitted until D9 is decided` with

```toml
# D172, D-P-2: MIT core; the `ee` directories are Elastic-2.0 (see LICENSE).
license = "MIT"
```

**Crate manifests**: add `license.workspace = true` after `publish…` in the `[package]` of `crates/core`, `crates/otel`, `crates/storage`, `crates/eval`, `crates/cli`, `crates/ui` and `tools/capture`. In `crates/server/Cargo.toml` add `license = "MIT AND Elastic-2.0"` instead (it compiles `src/ee`).

**`deny.toml`**, `[licenses]`: replace the section (keep `version = 2` and `confidence-threshold = 0.8`) with

```toml
[licenses]
# D172 settled D9: the core is MIT, `ee/` directories are Elastic-2.0 (D-P-2, D-P-3).
# The allow-list is the set of licences in the dependency graph on 2026-11-14.
# Adding a licence here is a review decision; copyleft licences other than MPL-2.0
# (file-level, used by `option-ext`) are not allowed.
version = 2
allow = [
    "0BSD",
    "Apache-2.0",
    "Apache-2.0 WITH LLVM-exception",
    "BSD-1-Clause",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "BSL-1.0",
    "CC0-1.0",
    "CDLA-Permissive-2.0",
    "ISC",
    "MIT",
    "MIT-0",
    "MPL-2.0",
    "Unicode-3.0",
    "Unlicense",
    "Zlib",
]
confidence-threshold = 0.8

[licenses.private]
# Workspace crates are unpublished (`publish = false`); their own licences are in LICENSE.
ignore = true
```

**`justfile`**:
- `deny` recipe: comment `# Supply-chain checks: advisories, bans, licences, sources (D-P-3)` and body `cargo deny check advisories bans licenses sources`.
- `check-rust: fmt-check lint test deny check-ee`.
- New recipe, after `deny-advisories`:

```just
# Check the ee licence files and SPDX headers (D-P-2)
check-ee:
    #!/usr/bin/env bash
    set -euo pipefail
    expected=6448026f2e06ab3da9ed8ddda686694bba37179e88db92dea6d4aab516c957ea
    header='// SPDX-License-Identifier: Elastic-2.0'
    status=0
    for dir in crates/server/src/ee web/src/ee; do
        if [ ! -f "$dir/LICENSE" ]; then
            echo "$dir/LICENSE is missing"
            status=1
        elif [ "$(sha256sum "$dir/LICENSE" | cut -d' ' -f1)" != "$expected" ]; then
            echo "$dir/LICENSE is not the Elastic License 2.0 text"
            status=1
        fi
        while IFS= read -r -d '' file; do
            if [ "$(head -n 1 "$file")" != "$header" ]; then
                echo "$file: the first line must be '$header'"
                status=1
            fi
        done < <(find "$dir" -type f \( -name '*.rs' -o -name '*.ts' -o -name '*.tsx' \) -print0)
    done
    while IFS= read -r file; do
        case "$file" in
            crates/server/src/ee/*|web/src/ee/*) ;;
            *) echo "$file: Elastic-2.0 header outside an ee directory"; status=1 ;;
        esac
    done < <(git grep -l -F 'SPDX-License-Identifier: Elastic-2.0' -- '*.rs' '*.ts' '*.tsx' || true)
    exit "$status"
```

P3 adds `tools/license` to both lists.

**`AGENTS.md`**, § Commands table: the `just deny` row becomes `| \`just deny\` | supply-chain checks (advisories, bans, licences, sources; D-P-3) |`, and add a row `| \`just check-ee\` | the \`ee\` licence files and SPDX headers (D-P-2) |` after it.

**`.github/workflows/ci.yml`**: in the job that runs `- run: just deny`, change that line to `- run: just deny check-ee`. Nothing else in the file changes.

### Checks

- `just deny` passes. If it reports a licence not in the list, stop and report it (do not add it).
- `just check-ee` passes. Then, as a manual negative check (not committed): put the header line at the top of `crates/core/src/lib.rs` → `just check-ee` fails naming that file; revert. Delete `web/src/ee/LICENSE` → fails with "is missing"; restore it.
- `just check-workflows` passes.
- `just check` passes.

---

## P2 — Signed tokens

### Dependency

Root `Cargo.toml`, `[workspace.dependencies]`, after `sha2`:

```toml
# Licence keys and org entitlement tokens (D-P-5). Already in Cargo.lock through openidconnect.
ed25519-dalek = "3.0.0"
```

`crates/server/Cargo.toml` `[dependencies]`: `ed25519-dalek.workspace = true`. `Cargo.lock` must not gain a second `ed25519-dalek`.

### Files

**`crates/server/src/lib.rs`**: add `pub(crate) mod ee;` after `pub(crate) mod crypto;`.

**`crates/server/src/ee/mod.rs`** (new):

```rust
// SPDX-License-Identifier: Elastic-2.0
//! Enterprise code (D172, D-P-1). Licensed under the Elastic License 2.0 (`LICENSE` here);
//! the rest of the crate is MIT. Core code calls these modules directly.

pub(crate) mod token;
```

**`crates/server/src/ee/token.rs`** (new):

```rust
// SPDX-License-Identifier: Elastic-2.0
//! Signed tokens: licence keys, org entitlement tokens and control-plane requests (D-P-5).
//!
//! A token is `llmobs1.<kid>.<payload>.<signature>`. `<payload>` and `<signature>` are
//! base64url without padding. The signature is ed25519 over the ASCII bytes of
//! `llmobs1.<kid>.<payload>`, checked before the payload is decoded as JSON, so the JSON
//! is never re-serialised. Release builds trust only [`RELEASE_KEYS`]; debug builds also
//! trust [`DEV_KEYS`] (D-P-6). No config can add a key.

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use ed25519_dalek::{Signature, VerifyingKey};

/// Format tag. Renamed with the product (D46).
pub(crate) const TOKEN_TAG: &str = "llmobs1";

/// Largest accepted token, in bytes.
pub(crate) const MAX_TOKEN_BYTES: usize = 16 * 1024;

/// Keys every build trusts: `(kid, ed25519 public key in standard base64)`.
/// `prod-1`'s private half is kept outside the repository (generated 2026-11-14).
const RELEASE_KEYS: &[(&str, &str)] = &[("prod-1", "AvIxTEduiC/amifl8QFxLYjXjPYON8LsreI7sL7UDWE=")];

/// Keys only debug builds trust. The private half is `fixtures/license/dev-1.key`.
const DEV_KEYS: &[(&str, &str)] = &[("dev-1", "kMTjOGaR2ry6zgwZ2p2nk0kMsOoyisgt3qfRm4JH36Y=")];

/// Why a token was refused. The text never quotes the token.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum TokenError {
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
}

/// A token whose signature verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Verified {
    /// The key that signed it.
    pub(crate) kid: String,
    /// The decoded payload: UTF-8 JSON, not parsed yet.
    pub(crate) payload: Vec<u8>,
}

/// Verifies `token` (surrounding whitespace ignored) and returns its payload bytes.
///
/// # Errors
///
/// Returns [`TokenError`] when the token is too large, malformed, signed by an unknown key,
/// or its signature does not verify.
pub(crate) fn verify(token: &str) -> Result<Verified, TokenError> {
    let token = token.trim();
    if token.len() > MAX_TOKEN_BYTES {
        return Err(TokenError::TooLarge);
    }
    let mut parts = token.split('.');
    let (Some(tag), Some(kid), Some(payload), Some(signature), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return Err(TokenError::Malformed);
    };
    if tag != TOKEN_TAG || !valid_kid(kid) {
        return Err(TokenError::Malformed);
    }
    let key = trusted_key(kid).ok_or(TokenError::UnknownKey)?;
    let payload_bytes = URL_SAFE_NO_PAD
        .decode(payload)
        .map_err(|_| TokenError::Malformed)?;
    let signature_bytes = URL_SAFE_NO_PAD
        .decode(signature)
        .map_err(|_| TokenError::Malformed)?;
    let signature = Signature::from_slice(&signature_bytes).map_err(|_| TokenError::Malformed)?;
    let signed_len = tag.len() + 1 + kid.len() + 1 + payload.len();
    let signed = token
        .as_bytes()
        .get(..signed_len)
        .ok_or(TokenError::Malformed)?;
    key.verify_strict(signed, &signature)
        .map_err(|_| TokenError::BadSignature)?;
    Ok(Verified {
        kid: kid.to_owned(),
        payload: payload_bytes,
    })
}

/// `[a-z0-9-]{1,32}`.
fn valid_kid(kid: &str) -> bool {
    (1..=32).contains(&kid.len())
        && kid
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// The public key named `kid`, if this build trusts it.
fn trusted_key(kid: &str) -> Option<VerifyingKey> {
    let dev: &[(&str, &str)] = if cfg!(debug_assertions) { DEV_KEYS } else { &[] };
    RELEASE_KEYS
        .iter()
        .chain(dev)
        .find(|(name, _)| *name == kid)
        .and_then(|(_, key)| decode_key(key))
}

/// A standard-base64 ed25519 public key. `None` when it is not 32 valid bytes; the
/// `keys_decode` test proves every built-in key decodes.
fn decode_key(key: &str) -> Option<VerifyingKey> {
    let bytes = STANDARD.decode(key).ok()?;
    let bytes: [u8; 32] = bytes.try_into().ok()?;
    VerifyingKey::from_bytes(&bytes).ok()
}
```

`clippy` may ask for `unreachable_pub`-style or `doc_markdown` tweaks; follow it without changing behaviour. If `cfg!(debug_assertions)` with an unused-in-release constant triggers `dead_code` in `--release` builds, it does not, because `cfg!` keeps both branches compiled.

**`fixtures/license/dev-1.key`** (new): one line, the dev private key seed in standard base64, then a newline:

```
Mc43yiMNElJJ35mngBOSJw/v0zOVDZ07ssoN2uyU3rA=
```

**`fixtures/license/README.md`** (new):

```markdown
# Licence fixtures

`dev-1.key` is the **development** ed25519 private key (32-byte seed, standard base64). It is
public on purpose: only debug builds trust its public half (`DEV_KEYS` in
`crates/server/src/ee/token.rs`, D-P-6). Release builds trust only `prod-1`, whose private
key is never in this repository.

`payloads/*.json` are token payloads; `tokens/*.token` are those payloads signed with
`dev-1` by `just license-fixtures` (P3). Do not edit `tokens/` by hand.
```

**`crates/server/src/test_support/mod.rs`**: add, after `TEST_KEY`:

```rust
/// The development signing seed (`fixtures/license/dev-1.key`, D-P-6).
const DEV_SEED: &str = include_str!("../../../../fixtures/license/dev-1.key");

/// Signs `payload` as a `dev-1` token (D-P-5). Debug builds trust it.
pub(crate) fn sign_token(payload: &serde_json::Value) -> String {
    use base64::Engine;
    use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
    use ed25519_dalek::Signer;

    let seed: [u8; 32] = STANDARD
        .decode(DEV_SEED.trim())
        .expect("dev seed is base64")
        .try_into()
        .expect("dev seed is 32 bytes");
    let key = ed25519_dalek::SigningKey::from_bytes(&seed);
    let body = URL_SAFE_NO_PAD.encode(serde_json::to_vec(payload).expect("payload serializes"));
    let signed = format!("{}.dev-1.{body}", crate::ee::token::TOKEN_TAG);
    let signature = URL_SAFE_NO_PAD.encode(key.sign(signed.as_bytes()).to_bytes());
    format!("{signed}.{signature}")
}
```

Count the `../` in `include_str!` from `crates/server/src/test_support/mod.rs` to the repository root: four.

### Tests (`ee::token::tests`)

1. `keys_decode`: every entry of `RELEASE_KEYS` and `DEV_KEYS` gives `Some` from `decode_key`, and every `kid` passes `valid_kid`.
2. `dev_seed_matches_dev_key`: the public key of the seed in `fixtures/license/dev-1.key` (`SigningKey::from_bytes(..).verifying_key().to_bytes()`, standard base64) equals `DEV_KEYS[0].1`.
3. `round_trip`: `verify(&sign_token(&json!({"kind": "license"})))` is `Ok` with `kid == "dev-1"` and `payload` equal to `serde_json::to_vec(&json!({"kind": "license"}))`. With surrounding whitespace (`format!("  {token}\n")`) it is still `Ok`.
4. `tampering_is_refused`: changing one character of the payload part (re-encode a different JSON body but keep the old signature) → `BadSignature`; changing the signature's last character to another base64url character that decodes to different bytes → `BadSignature` or `Malformed` (assert it is one of the two).
5. `malformed_tokens`: `""`, `"llmobs1.dev-1.abc"` (three parts), five parts, tag `llmobs2`, kid `Dev-1` (uppercase), kid of 33 characters, payload `"!!"` (bad base64), signature of 10 bytes → `Malformed`; a 16 KiB + 1 byte string → `TooLarge`.
6. `unknown_key`: a token built like `sign_token` but with kid `nope-1` → `UnknownKey`.
7. `release_key_needs_prod_signature`: a dev-signed token whose kid is rewritten to `prod-1` → `BadSignature`.
8. `error_text_never_quotes_the_token`: for each `TokenError`, `to_string()` does not contain `llmobs1.`.

### Done

`just check` passes; `crate::ee` holds only `mod.rs`, `token.rs` and `LICENSE`.

---

## P3 — The licence signing tool and fixtures

A small Elastic-2.0 tool that signs payloads with a seed file. It is for development fixtures and trial licences until track Q issues keys; it is never shipped.

### Files

**`tools/license/LICENSE`**: copy `crates/server/src/ee/LICENSE` (same hash).

**`tools/license/Cargo.toml`** (new):

```toml
[package]
name = "llmobs-license-tool"
description = "Sign llmobs licence keys and entitlement tokens with a seed file (development and trials)"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
publish = false
license = "Elastic-2.0"

[[bin]]
name = "llmobs-license"
path = "src/main.rs"

[dependencies]
base64 = { workspace = true }
clap = { workspace = true }
ed25519-dalek = { workspace = true }
serde_json = { workspace = true }
thiserror = { workspace = true }

[lints]
workspace = true
```

Root `Cargo.toml` `[workspace] members`: `["crates/*", "tools/capture", "tools/license"]`.

**`tools/license/src/main.rs`** (new; first line the SPDX header). Behaviour:

```
llmobs-license sign --key <seed-file> --kid <kid> --payload <json-file>
    Prints the token and a newline. The payload file must be a JSON object; it is
    re-encoded compactly with serde_json (key order preserved) before signing.
llmobs-license public-key --key <seed-file>
    Prints the public key in standard base64 and a newline.
llmobs-license fixtures [--check]
    Signs every fixtures/license/payloads/<name>.json with fixtures/license/dev-1.key
    and kid dev-1 into fixtures/license/tokens/<name>.token (token + newline).
    --check writes nothing and exits 1 listing every token file that differs or is
    missing, and every token file without a payload.
```

Structure:
- `fn sign(seed: &[u8; 32], kid: &str, payload: &serde_json::Value) -> Result<String, ToolError>`: refuses a kid that is not `[a-z0-9-]{1,32}` and a payload that is not an object; builds `llmobs1.<kid>.<base64url(serde_json::to_vec(payload))>`, signs those bytes, appends `.<base64url(signature)>`. The tag is the literal `"llmobs1"` (the tool does not depend on `llmobs-server`).
- `fn read_seed(path) -> Result<[u8; 32], ToolError>`: trims the file, standard-base64 decodes, requires 32 bytes. The error text names the path, never the content.
- `ToolError` is a `thiserror` enum (`Io { path: String }`, `BadSeed { path: String }`, `BadPayload { path: String }`, `BadKid`, `Differs { files: Vec<String> }`). `main` returns `std::process::ExitCode`, prints errors with `eprintln!` (allowed here: add `#![allow(clippy::print_stdout, clippy::print_stderr)]` at the top of `main.rs` with a comment "a CLI tool prints its result").
- `fn fixtures(check: bool) -> Result<(), ToolError>`: lists `payloads/*.json` sorted by name, signs each with `read_seed(dev-1.key)` and kid `dev-1`, and either writes `tokens/<name>.token` (`check == false`) or collects the names whose token file is missing or differs, plus token files without a payload, into `ToolError::Differs { files }` (`check == true`). `main` calls it for the `fixtures` subcommand; test 1 calls `fixtures(true)`.
- Paths for `fixtures` are relative to the workspace root: `concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/license")`.
- clap derive with subcommands `Sign { key, kid, payload }`, `PublicKey { key }`, `Fixtures { check: bool }`.

To make a production licence (documented, not run by you): convert the PEM to a seed with `openssl pkey -in llmobs-signing-1.pem -outform DER | tail -c 32 | base64 > seed`, then `llmobs-license sign --key seed --kid prod-1 --payload licence.json`, then delete `seed`.

**`fixtures/license/payloads/`** (new), three files:

`active.json`:
```json
{"kind": "license", "id": "01900000-0000-7000-8000-000000000500", "issued_at": "2026-01-01T00:00:00Z", "customer": "Example Corp", "features": ["project_roles", "sso_enforcement"], "expires_at": "2099-01-01T00:00:00Z", "grace_ends_at": "2099-02-01T00:00:00Z"}
```

`future-feature.json` (an unknown feature and an unknown field, both ignored):
```json
{"kind": "license", "id": "01900000-0000-7000-8000-000000000501", "issued_at": "2026-01-01T00:00:00Z", "customer": "Example Corp", "features": ["project_roles", "audit_logs"], "seats": 50, "expires_at": "2099-01-01T00:00:00Z", "grace_ends_at": "2099-01-01T00:00:00Z"}
```

`wrong-kind.json`:
```json
{"kind": "org", "id": "01900000-0000-7000-8000-000000000502", "issued_at": "2026-01-01T00:00:00Z"}
```

**`fixtures/license/tokens/*.token`**: generated with `just license-fixtures`, committed.

**`justfile`**, after `check-ee`:

```just
# Re-sign fixtures/license/payloads with the dev key (D-P-6)
license-fixtures:
    cargo run -q -p llmobs-license-tool -- fixtures
```

and in `check-ee`, after the `while … done` over SPDX headers, add `tools/license` to the `for dir in` list and the `case` pattern (`crates/server/src/ee/*|web/src/ee/*|tools/license/*`), so it checks `tools/license/LICENSE` and `tools/license/src/*.rs`.

### Tests

In `tools/license/src/main.rs` `#[cfg(test)] mod tests`:
1. `fixtures_are_up_to_date`: runs the `fixtures --check` logic in-process (call the function, not the binary) and asserts no differences. Ed25519 signatures are deterministic, so a re-sign produces the same bytes.
2. `sign_refuses_bad_input`: kid `Prod-1` and a payload `[1]` are errors.
3. `public_key_of_dev_seed`: `public-key` of `fixtures/license/dev-1.key` is `kMTjOGaR2ry6zgwZ2p2nk0kMsOoyisgt3qfRm4JH36Y=`.

In `crates/server/src/ee/token.rs` tests:
9. `fixture_tokens_verify`: each of the three files under `fixtures/license/tokens/` (read with `include_str!`, path `../../../../fixtures/license/tokens/<name>.token`) verifies with kid `dev-1`, and its payload equals `serde_json::to_vec` of the parsed `payloads/<name>.json` (`include_str!` too).

`just test` runs the tool's tests (it is a workspace member).

### Done

`just license-fixtures` then `git status` shows no change; `just check` passes.

---

## P4 — Licence claims and the entitlements core

Pure types and functions; nothing reads config or `AppState` yet.

### `crates/server/src/api/entitlements.rs` (new; `pub mod entitlements;` in `api/mod.rs`, after `connections`)

```rust
//! Entitlement types in the API (D179, D-P-12, D-P-15).

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// A feature an org may be entitled to. Names are append-only (D-P-12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Feature {
    /// Per-project role overrides (D139): Teams add-on or an enterprise licence.
    ProjectRoles,
    /// Instance-wide `auth.enforce_sso_domains`: an enterprise licence.
    SsoEnforcement,
}

impl Feature {
    /// Every feature, in declaration order.
    pub const ALL: [Self; 2] = [Self::ProjectRoles, Self::SsoEnforcement];

    /// The wire name.
    pub fn name(self) -> &'static str {
        match self {
            Self::ProjectRoles => "project_roles",
            Self::SsoEnforcement => "sso_enforcement",
        }
    }

    /// The feature named `name`; `None` for a name this build does not know.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|feature| feature.name() == name)
    }

    /// The problem `detail` when an org lacks the feature (D-P-14). No payload.
    pub fn locked_detail(self) -> &'static str {
        match self {
            Self::ProjectRoles => {
                "Per-project roles need the Teams add-on or an enterprise licence."
            }
            Self::SsoEnforcement => "SSO enforcement needs an enterprise licence.",
        }
    }
}
```

(The `OrgEntitlements` and `AdminLicense` response types are added in P6 and P7.)

### `crates/server/src/entitlements/mod.rs` (new, MIT; `pub(crate) mod entitlements;` in `lib.rs` after `pub(crate) mod email;`)

```rust
//! One resolved `Entitlements` value per org (D179, D-P-10).
//!
//! M1 has one layer: the instance licence's features, while it is active or in grace.
//! M2 adds the instance defaults and the org's token.

pub(crate) use crate::api::entitlements::Feature;

/// A set of [`Feature`]s.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct FeatureSet(u32);

impl FeatureSet {
    /// No feature.
    pub(crate) const EMPTY: Self = Self(0);

    fn bit(feature: Feature) -> u32 {
        1 << (feature as u32)
    }

    /// Adds `feature`.
    pub(crate) fn insert(&mut self, feature: Feature) {
        self.0 |= Self::bit(feature);
    }

    /// Whether `feature` is in the set.
    pub(crate) fn contains(self, feature: Feature) -> bool {
        self.0 & Self::bit(feature) != 0
    }

    /// The known features named in `names`; unknown names are skipped (D-P-5).
    pub(crate) fn from_names<'a>(names: impl IntoIterator<Item = &'a str>) -> Self {
        let mut set = Self::EMPTY;
        for feature in names.into_iter().filter_map(Feature::from_name) {
            set.insert(feature);
        }
        set
    }

    /// The features in the set, in [`Feature::ALL`] order.
    pub(crate) fn to_vec(self) -> Vec<Feature> {
        Feature::ALL
            .into_iter()
            .filter(|feature| self.contains(*feature))
            .collect()
    }
}

/// What one org may do now (D179).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Entitlements {
    features: FeatureSet,
}

impl Entitlements {
    /// Entitlements with exactly `features`.
    pub(crate) fn with_features(features: FeatureSet) -> Self {
        Self { features }
    }

    /// Whether the org may use `feature`.
    pub(crate) fn allows(self, feature: Feature) -> bool {
        self.features.contains(feature)
    }

    /// Every allowed feature, in [`Feature::ALL`] order.
    pub(crate) fn features(self) -> Vec<Feature> {
        self.features.to_vec()
    }
}
```

`Feature as u32` works because `Feature` is a fieldless enum; add `#[repr(u32)]`? No: `as u32` on a fieldless enum is allowed without `repr`. Clippy's `as_conversions` is not enabled. Variants stay below 32 (a test checks `Feature::ALL.len() <= 32`).

### `crates/server/src/ee/license.rs` (new; `pub(crate) mod license;` in `ee/mod.rs`)

```rust
// SPDX-License-Identifier: Elastic-2.0
//! Licence keys (D172, D-P-7): claims, the status over time, and the features they grant.

use serde::Deserialize;
use uuid::Uuid;

use crate::ee::token::{self, TokenError};
use crate::entitlements::FeatureSet;

/// The payload of a `license` token, after [`Kind`] matched. Unknown fields are ignored (D-P-5).
#[derive(Debug, Deserialize)]
struct Claims {
    id: Uuid,
    #[serde(with = "llmobs_core::time_serde")]
    issued_at: i64,
    customer: String,
    features: Vec<String>,
    #[serde(with = "llmobs_core::time_serde")]
    expires_at: i64,
    #[serde(with = "llmobs_core::time_serde")]
    grace_ends_at: i64,
}

/// A licence whose signature and claims are valid. Its dates may be past.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct License {
    /// Payload `id`.
    pub(crate) id: Uuid,
    /// The key that signed it.
    pub(crate) kid: String,
    /// Display name of the customer. Shown to instance admins only; never logged.
    pub(crate) customer: String,
    /// The known features it lists.
    pub(crate) features: FeatureSet,
    /// ns.
    pub(crate) issued_at: i64,
    /// ns. Active before this instant.
    pub(crate) expires_at: i64,
    /// ns. In grace before this instant, expired from it on.
    pub(crate) grace_ends_at: i64,
}

/// Why a configured key was refused. Shown to instance admins and logged; never the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum InvalidLicense {
    /// The token itself was refused.
    #[error(transparent)]
    Token(#[from] TokenError),
    /// The payload is not a licence JSON object with every required field.
    #[error("the licence payload is invalid")]
    Payload,
    /// The token is not of kind `license`.
    #[error("the token is not a licence key")]
    WrongKind,
    /// `grace_ends_at` is before `expires_at`, or `customer` is empty or over 200 characters.
    #[error("the licence fields are inconsistent")]
    Fields,
}

/// The configured licence, read once at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LicenseState {
    /// `license_key` is unset.
    Absent,
    /// A key is set and was refused.
    Invalid(InvalidLicense),
    /// A genuine licence.
    Valid(License),
}

/// Where a licence stands at one instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LicenseStatus {
    /// No key.
    Absent,
    /// The key was refused.
    Invalid,
    /// Before `expires_at`.
    Active,
    /// From `expires_at` to before `grace_ends_at`: features on, warnings shown.
    Grace,
    /// From `grace_ends_at` on: features off.
    Expired,
}

impl LicenseState {
    /// Verifies and parses `key`. `None` is [`Self::Absent`].
    pub(crate) fn from_key(key: Option<&str>) -> Self {
        match key {
            None => Self::Absent,
            Some(key) => match parse(key) {
                Ok(license) => Self::Valid(license),
                Err(reason) => Self::Invalid(reason),
            },
        }
    }

    /// The status at `now_ns`.
    pub(crate) fn status(&self, now_ns: i64) -> LicenseStatus {
        match self {
            Self::Absent => LicenseStatus::Absent,
            Self::Invalid(_) => LicenseStatus::Invalid,
            Self::Valid(license) if now_ns < license.expires_at => LicenseStatus::Active,
            Self::Valid(license) if now_ns < license.grace_ends_at => LicenseStatus::Grace,
            Self::Valid(_) => LicenseStatus::Expired,
        }
    }

    /// Features in force at `now_ns`: the licence's while active or in grace, else none.
    pub(crate) fn active_features(&self, now_ns: i64) -> FeatureSet {
        match (self, self.status(now_ns)) {
            (Self::Valid(license), LicenseStatus::Active | LicenseStatus::Grace) => license.features,
            _ => FeatureSet::EMPTY,
        }
    }

    /// Features a genuine licence lists, expired or not (D-P-8: restrictions keep applying).
    pub(crate) fn listed_features(&self) -> FeatureSet {
        match self {
            Self::Valid(license) => license.features,
            Self::Absent | Self::Invalid(_) => FeatureSet::EMPTY,
        }
    }
}

/// The `kind` field alone, read before the full claims.
#[derive(Debug, Deserialize)]
struct Kind {
    kind: String,
}

fn parse(key: &str) -> Result<License, InvalidLicense> {
    let verified = token::verify(key)?;
    let kind: Kind =
        serde_json::from_slice(&verified.payload).map_err(|_| InvalidLicense::Payload)?;
    if kind.kind != "license" {
        return Err(InvalidLicense::WrongKind);
    }
    let claims: Claims =
        serde_json::from_slice(&verified.payload).map_err(|_| InvalidLicense::Payload)?;
    let customer = claims.customer.trim();
    if claims.grace_ends_at < claims.expires_at
        || customer.is_empty()
        || customer.chars().count() > 200
    {
        return Err(InvalidLicense::Fields);
    }
    Ok(License {
        id: claims.id,
        kid: verified.kid,
        customer: customer.to_owned(),
        features: FeatureSet::from_names(claims.features.iter().map(String::as_str)),
        issued_at: claims.issued_at,
        expires_at: claims.expires_at,
        grace_ends_at: claims.grace_ends_at,
    })
}
```

`Claims` has no `kind` field (serde ignores it). Reading `kind` first makes a token of another kind `WrongKind` even when it lacks the licence fields.

### Tests

`entitlements::tests`:
1. `feature_names_round_trip`: for each of `Feature::ALL`, `Feature::from_name(f.name()) == Some(f)`; serde JSON of `Feature::ProjectRoles` is `"project_roles"`; `from_name("audit_logs")` is `None`; `Feature::ALL.len() <= 32`.
2. `feature_set_ignores_unknown_names`: `FeatureSet::from_names(["project_roles", "audit_logs", "project_roles"]).to_vec() == vec![Feature::ProjectRoles]`.
3. `entitlements_allow_listed_features`: `Entitlements::with_features(set).allows(..)` true for members, false otherwise; `features()` is in `ALL` order.

`ee::license::tests` (use `crate::test_support::sign_token` and `serde_json::json!`; times as RFC 3339 strings):
4. `absent_and_status_over_time`: `from_key(None)` is `Absent`, status `Absent`, `active_features` empty. A licence with `expires_at` 2026-02-01 and `grace_ends_at` 2026-03-01: status `Active` at 2026-01-15, `Grace` exactly at 2026-02-01 and at 2026-02-15, `Expired` exactly at 2026-03-01; `active_features` holds `project_roles` while active and in grace and is empty when expired; `listed_features` holds it in all three.
5. `fixture_licences`: `from_key` of `fixtures/license/tokens/active.token` (via `include_str!`) is `Valid` with customer `Example Corp`, id `01900000-0000-7000-8000-000000000500`, kid `dev-1`, both features; `future-feature.token` is `Valid` with only `project_roles`; `wrong-kind.token` is `Invalid(WrongKind)`.
6. `invalid_keys`: `"garbage"` → `Invalid(Token(Malformed))`; a signed payload `{"kind": "license"}` (fields missing) → `Invalid(Payload)`; `grace_ends_at` before `expires_at` → `Invalid(Fields)`; empty `customer` (`"  "`) → `Invalid(Fields)`; a customer of 201 characters → `Invalid(Fields)`; an `expires_at` that is not RFC 3339 → `Invalid(Payload)`.
7. `invalid_text_is_generic`: no `InvalidLicense` `to_string()` contains `llmobs1.` or `Example`.

### Done

`just check` passes. Nothing calls `LicenseState` outside tests yet: mark the two items that are only used from P5 on with `#[cfg_attr(not(test), allow(dead_code))]` at module level in `ee/license.rs` and `entitlements/mod.rs`, with a comment `// P5 wires the licence into AppState.`; P5 removes both attributes.

---

## P5 — Read the licence key at startup

### Config (`crates/server/src/config.rs`)

- `ServerConfig` gains, after `encryption_key`:

  ```rust
  /// Self-hosted licence key, a signed token (D172, D-P-7). Unset: no licence.
  #[serde(default)]
  pub license_key: Option<Secret<String>>,
  ```

- `ALIASES`: add `("LLMOBS_LICENSE_KEY", "license_key"),` after the `LLMOBS_ENCRYPTION_KEY` entry.
- `secret_key`: add the arm `[key] if key == "license_key" => Some("license_key"),` after the `encryption_key` arm.
- `is_secret_path`: add `"license_key"` to the `matches!` list.
- `validate`: an empty or whitespace-only `license_key` becomes `None` (set `config.license_key = None` when `key.expose().trim().is_empty()`), so `LLMOBS_LICENSE_KEY=` means "no licence".
- Every literal `ServerConfig { … }` (`test_support/mod.rs` `default_config`, `ingest/fixtures_tests.rs` `serve_config`, `serve.rs` tests' `config`) gains `license_key: None,`; fix any other the compiler lists.

Config tests (next to `encryption_key_comes_from_the_environment`):
1. `license_key_comes_from_the_environment`: `LLMOBS_LICENSE_KEY=abc` → `Some("abc")`.
2. `empty_license_key_is_unset`: `LLMOBS_LICENSE_KEY=` and `"   "` → `None`.
3. `license_key_type_error_does_not_quote_the_value`: TOML `license_key = 424242` → `ConfigError::Invalid { key: "license_key", .. }` whose text does not contain `424242`.
4. Extend the existing `Debug`-redaction test that sets `encryption_key = Some(Secret::new("topsecretvalue"…))` to also set `license_key` to `"licensevalue"` and assert `format!("{config:?}")` does not contain `licensevalue`.

### State (`crates/server/src/entitlements/mod.rs`)

```rust
use llmobs_core::ids::OrgId;

use crate::ee::license::{LicenseState, LicenseStatus};
use crate::http::AppState;
use crate::http::error::ApiError;

/// Entitlement sources held by the process (D-P-10).
#[derive(Debug)]
pub(crate) struct EntitlementsState {
    license: LicenseState,
}

impl EntitlementsState {
    /// From the licence read at startup.
    pub(crate) fn new(license: LicenseState) -> Self {
        Self { license }
    }

    /// The instance licence.
    pub(crate) fn license(&self) -> &LicenseState {
        &self.license
    }
}

/// What `org` may do now. Synchronous and in memory; call it at every check (D-P-10).
///
/// M1: the licence's features while it is active or in grace, for every org. `_org`
/// is used from M2 on, when the org's own token joins the resolution.
pub(crate) fn of(state: &AppState, _org: OrgId) -> Entitlements {
    Entitlements::with_features(state.entitlements.license().active_features(state.clock.now_ns()))
}

/// `Err(ApiError::PlanRequired)` when `org` may not use `feature` (D-P-14).
pub(crate) fn require(state: &AppState, org: OrgId, feature: Feature) -> Result<(), ApiError> {
    if of(state, org).allows(feature) {
        Ok(())
    } else {
        Err(ApiError::PlanRequired {
            detail: feature.locked_detail(),
        })
    }
}

/// Logs the licence status once at startup: status, licence id and key id only (D-P-7).
pub(crate) fn log_license(license: &LicenseState, now_ns: i64) {
    match (license, license.status(now_ns)) {
        (LicenseState::Absent, _) => tracing::info!("no licence key: enterprise features are locked"),
        (LicenseState::Invalid(reason), _) => tracing::error!(
            reason = %reason,
            "the licence key was refused: running without a licence"
        ),
        (LicenseState::Valid(license), LicenseStatus::Active) => tracing::info!(
            license_id = %license.id,
            kid = %license.kid,
            "licence active"
        ),
        (LicenseState::Valid(license), LicenseStatus::Grace) => tracing::warn!(
            license_id = %license.id,
            kid = %license.kid,
            "licence expired: enterprise features stop at the end of the grace period"
        ),
        (LicenseState::Valid(license), _) => tracing::warn!(
            license_id = %license.id,
            kid = %license.kid,
            "licence expired: enterprise features are locked"
        ),
    }
}
```

`require` refers to `ApiError::PlanRequired`, which P6 adds. To keep P5 compiling, **add `require` in P6, not here**; P5 adds `EntitlementsState`, `of` and `log_license` only.

### Wiring

- `crates/server/src/http/mod.rs` `AppInner`: add after `connections`:

  ```rust
  /// Licence and (from M2) org entitlements (D179, D-P-10).
  pub(crate) entitlements: crate::entitlements::EntitlementsState,
  ```

- `crates/server/src/serve.rs`: right before `let storage = Arc::new(storage);` (the line before `AppState::new(AppInner {`):

  ```rust
  let license = crate::ee::license::LicenseState::from_key(
      config.license_key.as_ref().map(|key| key.expose().as_str()),
  );
  crate::entitlements::log_license(&license, Clock::System.now_ns());
  ```

  and in `AppInner { … }`: `entitlements: crate::entitlements::EntitlementsState::new(license),`. A refused key never fails startup (D-P-7).
- `crates/server/src/test_support/mod.rs` `open`: build `LicenseState::from_key(config.license_key.as_ref().map(|key| key.expose().as_str()))` the same way (no log) and set `entitlements` in `AppInner`.
- Remove the `dead_code` attributes P4 added.

### Test helpers (`crates/server/src/test_support/mod.rs`)

```rust
/// A `dev-1` licence listing `features`, active from 2025-01-01 to 2099-01-01.
pub(crate) fn license_key(features: &[&str]) -> String {
    license_key_until(features, "2099-01-01T00:00:00Z", "2099-01-01T00:00:00Z")
}

/// A `dev-1` licence listing `features` with the given RFC 3339 `expires_at` and
/// `grace_ends_at`. The manual clock is `2026-01-01T00:00:00Z`.
pub(crate) fn license_key_until(features: &[&str], expires_at: &str, grace_ends_at: &str) -> String {
    sign_token(&serde_json::json!({
        "kind": "license",
        "id": "01900000-0000-7000-8000-000000000510",
        "issued_at": "2025-01-01T00:00:00Z",
        "customer": "Test Customer",
        "features": features,
        "expires_at": expires_at,
        "grace_ends_at": grace_ends_at,
    }))
}

impl TestApp {
    /// An app whose licence lists `features`.
    pub(crate) async fn with_license(features: &[&str]) -> Self {
        let key = license_key(features);
        Self::with_config(move |config| {
            config.license_key = Some(crate::config::Secret::new(key));
        })
        .await
    }
}
```

`Secret::new` must be callable from `test_support`; if it is `#[cfg(test)]`-only or private, make it `pub(crate) fn new` (check `config.rs`; the existing test at line ~1954 already calls it).

### Tests

`entitlements::tests` (async, `TestApp`):
5. `no_license_allows_nothing`: `TestApp::new()` → `of(&app.state, any_org).features()` is empty.
6. `license_features_apply_to_every_org`: `TestApp::with_license(&["project_roles"])` → `allows(Feature::ProjectRoles)` for two different `OrgId`s; `allows(Feature::SsoEnforcement)` is false.
7. `expired_license_allows_nothing`: config with `license_key_until(&["project_roles"], "2025-12-01T00:00:00Z", "2025-12-31T00:00:00Z")` (grace ended before the manual clock) → no feature; `state.entitlements.license().listed_features()` still holds it.
8. `refused_key_runs_without_license`: config `license_key = "garbage"` → `TestApp` starts, `license()` is `Invalid(Token(Malformed))`, no feature.
9. `log_license_never_prints_the_customer`: with `crate::test_support::capture_logs(future) -> (T, String)`, `log_license` of a valid licence logs `licence active` and the id, and not `Test Customer` nor `llmobs1.`; of an invalid one logs `refused` and not the key.

### Done

`just check` passes. Starting `just run serve` with `LLMOBS_LICENSE_KEY=$(cat fixtures/license/tokens/active.token)` logs `licence active` (debug build).

---

## P6 — `plan_required`, `upgrade_url` and org entitlements

### Problem code (shared with track M, D-P-14)

If `ProblemCode::PlanRequired` and `ApiError::PlanRequired` already exist (track M's M6 merged first), keep them and skip this block. Otherwise add exactly what M's plan specifies:

- `crates/server/src/api/common.rs`, `ProblemCode`: add `PlanRequired` after `EmailNotVerified`, doc `/// The organization's plan does not include this feature.`; `status` → 403 (join the `Self::Forbidden | Self::SsoRequired | Self::EmailNotVerified` arm); `reason_phrase` → `"Forbidden"` (same arm). Add `assert_eq!(ProblemCode::PlanRequired.status(), 403);` to the existing status test.
- `crates/server/src/http/error.rs`, `ApiError`, after `Forbidden`:

  ```rust
  /// The org's plan does not include the feature (D177, D-P-14). `detail` names it.
  #[error("plan required")]
  PlanRequired {
      /// Static text shown as the problem `detail`.
      detail: &'static str,
  },
  ```

  and in `problem()`: `Self::PlanRequired { detail } => Problem::new(ProblemCode::PlanRequired, Some((*detail).to_owned())),`. Fix every other exhaustive `match` the compiler reports. Add `assert_eq!(status_of(ApiError::PlanRequired { detail: "x" }), 403);` to `error_statuses` in `http/mod.rs` tests.
- `crates/server/src/api/openapi.rs` `problem(code)`: the `403` text becomes `"The credentials do not allow this operation, or the organization's plan does not include it (code plan_required)."`.
- `docs/spec/09-api.md` already lists `plan_required` (planning commit).

Then add `require` to `entitlements/mod.rs` (the function shown in P5), with test 10 below.

### Config: `[entitlements]`

`config.rs`:

```rust
/// `[entitlements]` (D-P-15). M2 adds `defaults`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct EntitlementsConfig {
    /// Where Upgrade buttons go. `{org}` is replaced by the org id. Unset: no button.
    pub upgrade_url: Option<String>,
}
```

`ServerConfig` gains `#[serde(default)] pub entitlements: EntitlementsConfig,` after `security`; every literal `ServerConfig { … }` gains `entitlements: EntitlementsConfig::default(),`. `validate` calls `validate_entitlements(&config.entitlements)?`:

```rust
fn validate_entitlements(entitlements: &EntitlementsConfig) -> Result<(), ConfigError> {
    let Some(template) = entitlements.upgrade_url.as_deref() else {
        return Ok(());
    };
    let invalid = ConfigError::Invalid {
        key: "entitlements.upgrade_url",
        reason: "must be an http or https URL; {org} is the only placeholder",
    };
    let sample = template.replace("{org}", "00000000-0000-7000-8000-000000000000");
    if sample.contains('{') || sample.contains('}') {
        return Err(invalid);
    }
    match url::Url::parse(&sample) {
        Ok(url) if matches!(url.scheme(), "http" | "https") && url.host().is_some() => Ok(()),
        _ => Err(invalid),
    }
}
```

The env form is `LLMOBS_ENTITLEMENTS__UPGRADE_URL` (the split provider; no alias). Config tests:
1. `upgrade_url_accepts_the_org_placeholder`: TOML `[entitlements]\nupgrade_url = "https://app.example.com/orgs/{org}/billing"` loads.
2. `upgrade_url_rejects_other_placeholders_and_schemes`: `"https://x.example/{plan}"`, `"ftp://x.example"`, `"not a url"`, `"https:///nohost"` → `Invalid { key: "entitlements.upgrade_url", .. }`.
3. `upgrade_url_from_env`: `LLMOBS_ENTITLEMENTS__UPGRADE_URL=https://example.com/up` → set.

`entitlements/mod.rs`:

```rust
/// `entitlements.upgrade_url` for `org`, or `None` when unset (D-P-15).
pub(crate) fn upgrade_url(state: &AppState, org: OrgId) -> Option<String> {
    state
        .config
        .entitlements
        .upgrade_url
        .as_deref()
        .map(|template| template.replace("{org}", &org.to_string()))
}
```

### API type (`api/entitlements.rs`)

```rust
/// `GET /api/v1/orgs/{org}/entitlements` (D-P-15). M2 adds the plan and limits; M3 the usage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OrgEntitlements {
    /// Features the org may use now, in a fixed order.
    pub features: Vec<Feature>,
    /// Where an Upgrade button goes. Omitted when the instance sets none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upgrade_url: Option<String>,
}
```

Register both in `collect_schemas` (`api/openapi.rs`, next to `set.insert::<AdminSettings>();`): `set.insert::<OrgEntitlements>();` and `set.insert::<Feature>();`. Both are required: `OrgEntitlements.features` refers to `#/components/schemas/Feature`, and the lint fails on a missing reference target.

### Route (`crates/server/src/routes/entitlements.rs`, new; `pub(crate) mod entitlements;` in `routes/mod.rs`; `routes.extend(crate::routes::entitlements::routes());` in `http/access.rs` after `members`)

```rust
//! Org entitlements (D179, D-P-15).

use axum::extract::{Extension, State};
use axum::routing::get;

use crate::api::entitlements::OrgEntitlements;
use crate::auth::roles::OrgPerm;
use crate::http::AppState;
use crate::http::access::{Access, OrgAccess, RouteDef};
use crate::http::error::{self, ApiError};

/// `GET /api/v1/orgs/{org}/entitlements`.
pub(crate) fn routes() -> Vec<RouteDef> {
    vec![RouteDef {
        method: axum::http::Method::GET,
        path: "/api/v1/orgs/{org}/entitlements",
        access: Access::Org {
            perm: OrgPerm::View,
            session_only: false,
        },
        documented: true,
        handler: get(get_entitlements),
    }]
}

async fn get_entitlements(
    State(state): State<AppState>,
    Extension(access): Extension<OrgAccess>,
) -> Result<error::Json<OrgEntitlements>, ApiError> {
    let org = access.org.id;
    Ok(error::Json(OrgEntitlements {
        features: crate::entitlements::of(&state, org).features(),
        upgrade_url: crate::entitlements::upgrade_url(&state, org),
    }))
}
```

`OrgAccess` is `{ org: OrgRow, role: OrgRole }` (`http/access.rs`), so the org id is `access.org.id`.

### OpenAPI (`api/openapi.rs`)

In the function that builds the org paths (find where `/api/v1/orgs/{org}/members` is added), add:

```rust
let entitlements = operation("get_org_entitlements", "orgs", "Read the org's entitlements")
    .description(Some(
        "Features the org may use now and where Upgrade buttons go (D179). Any org member.",
    ))
    .parameters(Some(vec![/* the same `org` path parameter `get_org` uses */]))
    .responses(responses(
        "200",
        json_of::<OrgEntitlements>("The org's entitlements."),
        AUTH_ERRORS,
    ))
    .build();
```

and `.path("/api/v1/orgs/{org}/entitlements", PathItem::new(HttpMethod::Get, entitlements))`. There is no `.securities(…)` call: like `get_org`, the operation uses the document's default security (an operation with its own security must be session-only in the lint). `AUTH_ERRORS` is the `[401, 403, 404, 429]` constant `get_org` uses; copy its `org` path parameter.

`crates/server/tests/openapi.rs`: add `("get", "/api/v1/orgs/{org}/entitlements", "get_org_entitlements")` to `OPERATIONS`. No `expected_errors` branch is needed: the lint's default for an unlisted id is `["401", "403", "404", "429"]`, which is `AUTH_ERRORS`. Run `just openapi`.

### Example and mocks

- `fixtures/api/static/org-entitlements.json` (new):

  ```json
  {
    "features": ["project_roles", "sso_enforcement"],
    "upgrade_url": "https://app.example.com/orgs/01900000-0000-7000-8000-000000000010/billing"
  }
  ```

- `crates/server/src/api_examples.rs`, static entries: `example("get_org_entitlements", params(&[("org", "acme")]), BTreeMap::new(), "static/org-entitlements.json", "application/json"),`.
- `crates/server/tests/api_examples.rs`: `roundtrip::<OrgEntitlements>("static/org-entitlements.json");` in `static_files_match_their_types` (import it), and `| "get_org_entitlements"` in the `=> {}` arm of `generated_files_roundtrip`.
- Run `just api-examples` (rewrites `fixtures/api/manifest.json`). The web mocks serve it from the manifest; no web change.

### Tests (`routes::entitlements::tests`)

Use `TestApp::seed()` (owner session and org `acme`, see `test_support`) and `add_member`.
1. `members_read_the_licence_features`: `TestApp::with_license(&["project_roles"])`, seed, a viewer of the org → `GET /api/v1/orgs/acme/entitlements` = 200 `{"features": ["project_roles"]}` with no `upgrade_url` key.
2. `no_license_no_features`: `TestApp::new()` → `{"features": []}`.
3. `upgrade_url_names_the_org`: config `entitlements.upgrade_url = Some("https://up.example/{org}/billing")` → `upgrade_url` is `https://up.example/<org id>/billing`.
4. `outsiders_get_404`: a signed-in user of another org → 404.

`entitlements::tests`:
10. `require_maps_to_plan_required`: no licence → `require(.., Feature::ProjectRoles)` is `Err(ApiError::PlanRequired { detail })` with `detail == Feature::ProjectRoles.locked_detail()`; with the licence → `Ok`.

`access_suite` runs every documented route automatically; if it needs a per-route expectation for the new GET (look at how `get_org` is treated), add the same.

### Done

`just openapi` and `just api-examples` leave no diff on a second run; `just check` passes.

---

## P7 — `GET /api/v1/admin/license`

### API types (`api/admin.rs`)

```rust
/// Where the instance licence stands (D-P-7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LicenseStatus {
    /// No key is configured.
    None,
    /// A key is configured and was refused.
    Invalid,
    /// Before `expires_at`.
    Active,
    /// Expired, before `grace_ends_at`: features still on.
    Grace,
    /// After `grace_ends_at`: enterprise features are locked.
    Expired,
}

/// `GET /api/v1/admin/license`. Never contains the key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AdminLicense {
    /// The status now.
    pub status: LicenseStatus,
    /// Why the key was refused. Only with `invalid`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Licence id. Omitted with `none` and `invalid`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Uuid>,
    /// Customer name. Omitted with `none` and `invalid`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub customer: Option<String>,
    /// Features the licence lists, known to this build, expired or not. Empty with `none` and `invalid`.
    pub features: Vec<Feature>,
    /// Omitted with `none` and `invalid`.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional_time")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub expires_at: Option<i64>,
    /// Omitted with `none` and `invalid`.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "optional_time")]
    #[schema(value_type = Option<String>, format = DateTime)]
    pub grace_ends_at: Option<i64>,
}
```

Import `optional_time` as `api/keys.rs` exposes it (`use super::keys::optional_time;` — it is `pub(crate) mod` there; if it is private, make it `pub(crate)`). Register `AdminLicense` in `collect_schemas`.

### Route (`routes/admin.rs`)

Add a `RouteDef` after `/api/v1/admin/settings`: `GET /api/v1/admin/license`, `Access::InstanceAdmin`, `documented: true`, `handler: get(get_license)`:

```rust
async fn get_license(State(state): State<AppState>) -> error::Json<AdminLicense> {
    let license = state.entitlements.license();
    let status = match license.status(state.clock.now_ns()) {
        crate::ee::license::LicenseStatus::Absent => LicenseStatus::None,
        crate::ee::license::LicenseStatus::Invalid => LicenseStatus::Invalid,
        crate::ee::license::LicenseStatus::Active => LicenseStatus::Active,
        crate::ee::license::LicenseStatus::Grace => LicenseStatus::Grace,
        crate::ee::license::LicenseStatus::Expired => LicenseStatus::Expired,
    };
    error::Json(match license {
        LicenseState::Absent => AdminLicense { status, reason: None, id: None, customer: None, features: Vec::new(), expires_at: None, grace_ends_at: None },
        LicenseState::Invalid(reason) => AdminLicense { status, reason: Some(reason.to_string()), ..(same Nones and empty) },
        LicenseState::Valid(valid) => AdminLicense {
            status,
            reason: None,
            id: Some(valid.id),
            customer: Some(valid.customer.clone()),
            features: valid.features.to_vec(),
            expires_at: Some(valid.expires_at),
            grace_ends_at: Some(valid.grace_ends_at),
        },
    })
}
```

(Write the three struct literals in full; `..` with a base value is fine if you build a `none()` helper.)

### OpenAPI

In `admin_paths`, build `get_admin_license` like `settings` (tag `admin`, summary `Read the instance licence`, `session` security, 200 `json_of::<AdminLicense>("The licence status. The key itself is never returned.")`, errors `[401, 403, 429]`) and `.path("/api/v1/admin/license", PathItem::new(HttpMethod::Get, license))`. `tests/openapi.rs`: add `("get", "/api/v1/admin/license", "get_admin_license")` to `OPERATIONS`; join it to the `get_admin_settings` expected-errors branch (`|| *id == "get_admin_license"`); and add `"get_admin_license"` next to `"get_admin_settings"` in the `matches!` list of session-only operations in `document_lint` (an operation with its own `security` must be in that list, or the lint fails with "overrides security"). `just openapi`.

### Example

`fixtures/api/static/admin-license.json`:

```json
{
  "status": "active",
  "id": "01900000-0000-7000-8000-000000000500",
  "customer": "Example Corp",
  "features": ["project_roles", "sso_enforcement"],
  "expires_at": "2099-01-01T00:00:00.000000000Z",
  "grace_ends_at": "2099-02-01T00:00:00.000000000Z"
}
```

`api_examples.rs` static entry `example("get_admin_license", BTreeMap::new(), BTreeMap::new(), "static/admin-license.json", "application/json")`; `tests/api_examples.rs`: `roundtrip::<AdminLicense>("static/admin-license.json");` and `| "get_admin_license"` in the `=> {}` arm. `just api-examples`.

### Tests (`routes::admin::tests`)

Reuse the module's `admin()` helper (it returns an instance-admin app and cookie); for licence variants build the app with `TestApp::with_config` and the same setup steps the helper uses (factor a `admin_with(tweak)` helper out of `admin()` if needed).
1. `license_needs_an_instance_admin`: a non-admin session → 403; no cookie → 401.
2. `license_none`: no key → `{"status": "none", "features": []}` and no other key.
3. `license_active`: `license_key(&["project_roles"])` → status `active`, customer `Test Customer`, features `["project_roles"]`, `expires_at` `2099-01-01T00:00:00.000000000Z`.
4. `license_grace_and_expired`: `license_key_until(&["project_roles"], "2025-12-01T00:00:00Z", "2026-02-01T00:00:00Z")` → `grace`; `license_key_until(&["project_roles"], "2025-11-01T00:00:00Z", "2025-12-01T00:00:00Z")` → `expired`, features still listed.
5. `license_invalid`: `license_key = "garbage"` → `{"status": "invalid", "reason": "the token is malformed", "features": []}`; the body does not contain `garbage`.

### Done

`just openapi`, `just api-examples` clean; `just check` passes.

---

## P8 — Move project role overrides to `ee`

The read path (effective roles, hiding projects) stays core and is not gated: existing overrides keep applying whatever the licence (D-P-8). Only the two write routes move, and only `PUT` is gated.

### Files

**`crates/server/src/ee/project_roles.rs`** (new; `pub(crate) mod project_roles;` in `ee/mod.rs`): move from `crates/server/src/routes/members.rs`:
- the two `RouteDef`s for `PUT` and `DELETE /api/v1/projects/{project}/members/{user}` (into `pub(crate) fn routes() -> Vec<RouteDef>`);
- `set_project_override`, `remove_project_override`, `stored_override`;
- the tests of `routes/members.rs` that call those two routes (`overrides_hide_projects_and_respect_the_ceiling` and any other test whose requests use `PUT` or `DELETE` on `/members/{user}` of a project), together with the helpers they need (copy the helper if `members.rs` tests still use it too).

`routes/members.rs` keeps `list_project_members` and makes these `pub(crate)` for `ee::project_roles`: `UserPath`, `MemberChange`, `project_member`. `http/access.rs` route table: add `routes.extend(crate::ee::project_roles::routes());` right after `crate::routes::members::routes()`.

The module doc:

```rust
// SPDX-License-Identifier: Elastic-2.0
//! Per-project role overrides (D139): the write routes (D-P-4, D-P-8).
//!
//! Setting an override needs `Feature::ProjectRoles`. Removing one never does, and stored
//! overrides keep applying through the core read path whatever the licence, so a lapse
//! never widens access.
```

### The gate

At the top of `set_project_override`, before the write:

```rust
crate::entitlements::require(&state, access.project.org_id, Feature::ProjectRoles)?;
```

`remove_project_override` has no gate.

### Tests that now need a licence

Every test that `PUT`s an override through HTTP must run on an app with the feature:
- the moved tests: `TestApp::with_license(&["project_roles"])` instead of `TestApp::new()`;
- `crates/server/src/http/access_suite.rs`: the suite's app (`TestApp::with_config(|config| config.auth.signup = SignupPolicy::Open)`) also sets `config.license_key = Some(Secret::new(crate::test_support::license_key(&["project_roles"])))`, so `put_override` keeps answering 200;
- search for other `PUT` calls on `/members/` in `crates/server/src` and `crates/server/tests` (`grep -rn 'Method::PUT' crates/server/src | grep members`) and give their apps the licence.

Tests that call `meta::members::set_project_override` directly (MCP, SQL, projects) need nothing: the store is not gated.

### New tests (`ee::project_roles::tests`)

1. `setting_an_override_needs_project_roles`: `TestApp::new()` (no licence), owner seeds org and project, adds a member → `PUT …/members/{member}` `{"role": "viewer"}` → 403, `problem().code == ProblemCode::PlanRequired`, detail `Per-project roles need the Teams add-on or an enterprise licence.`; nothing stored (`meta::members::project_override` is `None`).
2. `removing_an_override_needs_nothing`: no licence; store an override with `meta::members::set_project_override` → `DELETE …/members/{member}` → 204, the override is gone.
3. `stored_overrides_keep_applying_without_a_license`: no licence; store `ProjectOverride::None` for a member → that member's `GET /api/v1/projects/{project}` is 404 and `GET /api/v1/orgs/{org}/projects` omits it.
4. `expired_license_locks_setting`: `license_key_until(&["project_roles"], "2025-12-01T00:00:00Z", "2025-12-31T00:00:00Z")` → PUT is 403 `plan_required`.
5. The moved tests pass with the licence.
6. `override_writes_are_session_only`: for every route of `ee::project_roles::routes()`, `access` is `Access::Project { session_only: true, .. }` (the moved routes leave `routes/members.rs`'s `member_writes_are_session_only` loop, so this re-asserts it).

`access_suite`: still green with the licence; add one assertion run on a second app without a licence that the owner's PUT is 403 `plan_required` (or leave that to test 1 if the suite's structure does not allow a second app; say which in the commit message).

### Done

`grep -n 'set_project_override\|remove_project_override' crates/server/src/routes/members.rs` finds only `members::` store calls used by other handlers (for example `clear_project_overrides`), not the route handlers; `just check` passes.

---

## P9 — Move SSO enforcement to `ee`

### Files

**`crates/server/src/ee/sso_enforcement.rs`** (new; `pub(crate) mod sso_enforcement;` in `ee/mod.rs`):

```rust
// SPDX-License-Identifier: Elastic-2.0
//! Instance-wide SSO enforcement, `auth.enforce_sso_domains` (spec 08, D-P-4, D-P-8).
//!
//! It applies when the configured key is a genuine licence that lists `sso_enforcement`,
//! expired or not: a lapse never re-opens password login for those domains.

use crate::entitlements::Feature;
use crate::http::AppState;

/// Whether `email` (already normalized) must sign in through SSO.
pub(crate) fn requires_sso(state: &AppState, email: &str) -> bool {
    applies(state) && crate::routes::auth::domain_listed(&state.config.auth.enforce_sso_domains, email)
}

/// Whether the configured domains are enforced at all.
pub(crate) fn applies(state: &AppState) -> bool {
    state
        .entitlements
        .license()
        .listed_features()
        .contains(Feature::SsoEnforcement)
}

/// Warns once at startup when domains are configured but not enforced. No domain is logged.
pub(crate) fn warn_if_unlicensed(state: &AppState) {
    if !state.config.auth.enforce_sso_domains.is_empty() && !applies(state) {
        tracing::warn!(
            domains = state.config.auth.enforce_sso_domains.len(),
            "auth.enforce_sso_domains is set but not enforced: it needs an enterprise licence"
        );
    }
}
```

`routes/auth.rs`: make `domain_listed` `pub(crate)`; replace the three `if domain_listed(&state.config.auth.enforce_sso_domains, …)` conditions (login, `pending_signup_invite`, open signup) with `if crate::ee::sso_enforcement::requires_sso(&state, …)` (pass `&email` or `email` as each site has it). The `signup_domains` check keeps calling `domain_listed` directly.

`serve.rs`: call `crate::ee::sso_enforcement::warn_if_unlicensed(&state);` right after `AppState::new(AppInner { … })`.

Config validation is unchanged (`enforce_sso_domains` still needs a provider).

### API and UI data

`AdminSettings` (`api/admin.rs`) gains, after `enforce_sso_domains`:

```rust
/// Whether `enforce_sso_domains` is applied: it needs a licence listing `sso_enforcement`.
pub enforce_sso_applied: bool,
```

`get_settings` sets it to `crate::ee::sso_enforcement::applies(&state)`. Update `fixtures/api/static/admin-settings.json` (add `"enforce_sso_applied": false` after `enforce_sso_domains`) and the `SETTINGS` literal at the top of `web/src/routes/admin/settings.test.tsx` (the same field, same place), then `just openapi`, `just api-examples` (nothing should change in the manifest).

### Tests

Existing SSO tests in `routes/auth.rs` (around the three `config.auth.enforce_sso_domains = vec![…]` tweaks, near lines 1250, 1430, 1765) set the domains; they now also set `config.license_key = Some(Secret::new(crate::test_support::license_key(&["sso_enforcement"])))` in the same closure, and keep expecting `sso_required`. Add in `ee::sso_enforcement::tests`:
1. `domains_without_a_license_are_not_enforced`: domains `["sso.example"]` and a GitHub provider configured as the existing tests do, no licence → password login of `x@sso.example` is not `sso_required` (it answers what an unknown account answers, 401); `applies` is false.
2. `expired_license_keeps_enforcing`: `license_key_until(&["sso_enforcement"], "2025-12-01T00:00:00Z", "2025-12-31T00:00:00Z")` → login answers 403 `sso_required`.
3. `license_without_the_feature_is_not_enough`: a licence listing only `project_roles` → not enforced.
4. `warning_is_logged_without_domains`: `capture_logs` + `warn_if_unlicensed` → the warning appears with `domains=1` and the text does not contain `sso.example`.
5. `admin_settings_report_enforcement`: `GET /api/v1/admin/settings` → `enforce_sso_applied` false without a licence, true with.

### Done

`grep -rn 'enforce_sso_domains' crates/server/src/routes/auth.rs` matches no condition (only test setup); `just check` passes.

---

## P10 — Licence and plan locks in the UI

### Queries (`web/src/api/queries-settings.ts`, `web/src/api/queries-admin.ts`)

```ts
// queries-settings.ts
export function orgEntitlementsQuery(org: string) {
  return $api.queryOptions("get", "/api/v1/orgs/{org}/entitlements", orgInit(org));
}
```

```ts
// queries-admin.ts
export const adminLicenseQuery = $api.queryOptions("get", "/api/v1/admin/license");
```

### Core plan components (`web/src/components/plan/`, MIT)

**`feature-label.ts`**:

```ts
import type { Schema } from "@/api/types";

export type Feature = Schema<"Feature">;

/** What unlocks a feature, as shown next to a lock (D-P-15). */
export function featureOffer(feature: Feature): string {
  switch (feature) {
    case "project_roles":
      return "Available with the Teams add-on or an enterprise licence";
    case "sso_enforcement":
      return "Available with an enterprise licence";
  }
}
```

**`upgrade-button.tsx`**: `UpgradeButton({ url }: { url: string | undefined })` renders nothing when `url` is undefined; otherwise `<Button asChild size="sm" variant="outline"><a href={url} target="_blank" rel="noopener noreferrer">Upgrade<ExternalLink aria-hidden="true" /></a></Button>` (`ExternalLink` from `lucide-react`).

**`locked-feature.tsx`**:

```tsx
/** A lock line for a feature the org cannot use: the offer and an Upgrade button (D-P-15). */
export function LockedFeature({ feature, upgradeUrl }: { feature: Feature; upgradeUrl: string | undefined }) {
  return (
    <div role="note" aria-label="Locked feature" className="flex flex-wrap items-center gap-2 rounded-md border px-3 py-2 text-sm">
      <Lock className="size-4 text-muted-foreground" aria-hidden="true" />
      <span>{featureOffer(feature)}.</span>
      <UpgradeButton url={upgradeUrl} />
    </div>
  );
}
```

(`Lock` from `lucide-react`.)

**`use-org-entitlements.ts`**:

```ts
/** The org's entitlements; `allows(feature)` is false while loading or on error. */
export function useOrgEntitlements(org: string) {
  const query = useQuery(orgEntitlementsQuery(org));
  return {
    query,
    allows: (feature: Feature) => query.data?.features.includes(feature) ?? false,
    upgradeUrl: query.data?.upgrade_url,
  };
}
```

### Licence card (`web/src/ee/license-card.tsx`, Elastic-2.0)

`LicenseCard()` reads `useQuery(adminLicenseQuery)`; while pending, one `Skeleton`; on error, throw. It renders a section `<section aria-label="Licence">` with an `h3` "Licence" and a `dl`:
- Status: `none` → "No licence key. Enterprise features are locked."; `invalid` → "The licence key was refused: {reason}." (`reason` as returned); `active` → "Active until {date}"; `grace` → "Expired on {expires date}. Enterprise features stop on {grace date}." with the `TriangleAlert` icon; `expired` → "Expired on {expires date}. Enterprise features are locked.".
- Customer (when present), Licence id (when present, mono font), Features: the labels `Per-project roles` and `SSO enforcement` joined by ", ", or "None".
- Dates are the UTC day of the RFC 3339 value: `iso.slice(0, 10)` (for example `2099-01-01`). Licence instants are whole days in UTC, so no time zone conversion is wanted.
- A paragraph: "Set the key with LLMOBS_LICENSE_KEY and restart the server."

First line of the file: the SPDX header.

### Admin settings page (`web/src/routes/admin/settings.tsx`)

- Render `<LicenseCard />` (import from `@/ee/license-card`) after the settings `dl` and before "Test email".
- The "Single sign-on required for" `dd`: when `data.enforce_sso_domains.length > 0 && !data.enforce_sso_applied`, append ` — not enforced: needs an enterprise licence` after the domains.

### Tests

`web/src/components/plan/locked-feature.test.tsx`: renders the offer text for `project_roles`; with `upgradeUrl` a link named "Upgrade" whose `href` is the URL and `target="_blank"`; without it no link.

`web/src/routes/admin/settings.test.tsx` (extend):
1. The default mock shows "Licence", "Active until", "Example Corp", "Per-project roles, SSO enforcement".
2. `server.use(http.get("/api/v1/admin/license", () => HttpResponse.json({ status: "none", features: [] })))` → "No licence key. Enterprise features are locked.".
3. `invalid` with `reason: "the token is malformed"` → that sentence.
4. `grace` → "Enterprise features stop on".
5. Settings with `enforce_sso_domains: ["sso.example"]`, `enforce_sso_applied: false` (override `/api/v1/admin/settings` with `{...fixture, …}`, importing `fixtures/api/static/admin-settings.json` as the members test imports `project.json`) → "not enforced: needs an enterprise licence".

`pnpm --dir web gen:routes` is not needed (no new route file).

### Done

`just check` passes (it runs `just check-ee`, which now finds `web/src/ee/license-card.tsx`).

---

## P11 — The override editor in `web/src/ee`

### Files

**`web/src/ee/project-roles/project-access-select.tsx`** (new, Elastic-2.0): move the access `Select` cell of `web/src/routes/$org/$project/settings/members.tsx` into

```tsx
export function ProjectAccessSelect(props: {
  member: Schema<"ProjectMember">;
  canEdit: boolean;
  locked: boolean;
  pending: boolean;
  onChange: (value: "inherit" | "admin" | "member" | "viewer" | "none") => void;
})
```

- It renders the same `DisabledReason` + `Select` as today (trigger `aria-label` `Project access of {email}`, options "inherit" and the four overrides with `overrideLabel`).
- `disabled = !canEdit || pending`.
- When `locked` is true: every option except `inherit` and the member's current value is `disabled` on its `SelectItem`, so an admin can still remove an override (D-P-8) but not set a new one; the `DisabledReason` stays as today for `!canEdit`.
- `OVERRIDES` moves into this file and is exported.

**`web/src/routes/$org/$project/settings/members.tsx`**:
- `const entitlements = useOrgEntitlements(org);` and `const locked = !entitlements.allows("project_roles");`.
- Uses `<ProjectAccessSelect … locked={locked} />` in the access cell; `changeAccess` keeps calling the same mutations.
- When `locked && canEdit`, renders `<LockedFeature feature="project_roles" upgradeUrl={entitlements.upgradeUrl} />` above the table.
- `onFailed`: a `403` whose code is `plan_required` shows the problem's detail (use `problemOf(error)?.code === "plan_required"` → `problemMessage(error, "")`), before the existing 403 branch.

### Tests (`web/src/routes/$org/$project/settings/members.test.tsx`, extend; the default mock has both features, so existing tests stay as they are)

1. "shows the lock and keeps removal when project roles are locked": `server.use(http.get("/api/v1/orgs/:org/entitlements", () => HttpResponse.json({ features: [], upgrade_url: "https://up.example/billing" })))` → the note "Available with the Teams add-on or an enterprise licence." and an "Upgrade" link to `https://up.example/billing`; open Bob's select (current "Viewer"): the options "Admin", "Member" and "No access" are disabled (`aria-disabled="true"`), "Inherit org role" (the `overrideLabel("inherit")` text) and "Viewer" are enabled; choosing "Inherit org role" sends the `DELETE`.
2. "no lock with the feature": default mock → no "Locked feature" note.
3. "shows the plan message from the server": `server.use` a `PUT` handler answering `problemResponse(403, "plan_required", { detail: "Per-project roles need the Teams add-on or an enterprise licence." })` (`web/src/test/problem.ts`) → choosing "Admin" shows that detail.
4. Viewers (non-admin) see no lock note even when locked.

### Roadmap and close-out

`docs/roadmap.md`, row **P**: Status becomes `**In progress.** Design and M1 plan written; M1 implemented (open core split, licence keys, entitlements core, override and SSO gates). M2–M5 plans to follow.` and Plan `[`plans/p-entitlements/`](plans/p-entitlements/): design doc, then one plan per milestone (M1 written)`.

### Done

`just check` passes; `just e2e` passes (the e2e suite does not set overrides through the API; if it does, give its server the dev licence with `LLMOBS_LICENSE_KEY`, read from `fixtures/license/tokens/active.token` in `tools/e2e/run.sh`, and say so in the commit message).

## Report

When M1 is done, report: what was done per sub-phase; any deviation and why; that no new dependency version was added (`ed25519-dalek` 3.0.0 was already in `Cargo.lock`); whether `ProblemCode::PlanRequired` was added here or came from track M; anything left undone or unverified.
