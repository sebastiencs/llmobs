# I · M1 — Secrets and LLM connections

Implementation plan for milestone M1 of track I. It has 7 sub-phases, I1.1 to I1.7. Each one is done by one agent (GLM) in one go, in order, with no context beyond this plan and the files it names, and ends with `just check` green and one commit.

**Prerequisites** (checked when this plan was written, 2026-11-14):
- Track G is merged on `main` (M1–M5). This plan, the design and the Track I decisions are merged on `main` too: start a new branch from `main`.
- Node `26.13.0` is installed at `$HOME/.local/share/nvm/v26.13.0/bin`. It is not on the non-interactive `PATH`.

**Read first** (every sub-phase):
- `AGENTS.md`, all of it.
- This file's sections up to § I1.1, then your own sub-phase.
- `docs/plans/i-online-eval/00-design.md` § Decisions: D-I-1 to D-I-5, D-I-24 and D-I-29.
- `docs/mvp-decisions.md`: D136, D138, D139, D140, D150, D156, D157, D160, D166, D-L-4, D-L-5, D-L-10, D-L-11, and § Track I.
- `docs/spec/04-metadata-db.md` § Keys & credentials, § Encryption of secrets; `docs/spec/09-api.md` § Orgs, projects, members, keys and § CLI; `docs/spec/12-operations.md` § Configuration, § Process lifecycle, § Backups.
- The code each sub-phase points to, before you edit it.

## What M1 is, and what it is not

When M1 is done:
- the server has an encryption key: from `LLMOBS_ENCRYPTION_KEY`, or from `<data_dir>/secret.key`, which it generates on the first start of a new data directory. It refuses to start with a key that does not match the data directory, and it refuses to start without a key once one was used;
- `llmobs admin rotate-encryption-key` re-encrypts every sealed column under a new key, with the server stopped;
- org admins create, edit, delete and test LLM connections through `/api/v1/orgs/{org}/llm-connections`. Every org member can list them; the API key and header values never leave the server;
- the org settings area has an "LLM connections" page.

**Out of scope:** evaluators, rules, jobs, judge traces and score writes (M2–M6). M1 adds no project route. Nothing in M1 calls a provider except the connection test. The 409 for deleting a connection that evaluators use comes in M2, with evaluators.

## Decisions this plan relies on

| # | Summary |
|---|---|
| D-I-1 | Key from `LLMOBS_ENCRYPTION_KEY` (`encryption_key`), else `<data_dir>/secret.key`, else generated into that file (0600), but only while `meta.db` has no check value. A check value in `instance_settings` refuses a different key. |
| D-I-2 | Sealed value = `0x01` ‖ key id (4 bytes) ‖ nonce (12) ‖ ciphertext and tag. AAD `table:column:row_id`. Key id and check value are HMAC-SHA256 of fixed labels. |
| D-I-3 | `llmobs admin rotate-encryption-key [--new-from-stdin]`: server stopped (takes `LOCK`), one transaction, `secret.key.new` then rename (finished at startup after a crash). With an env key the new key must be given with `--new-from-stdin`; with a file key it may be generated. |
| D-I-4 | Connections are org-wide. `max_concurrency` 1–64 (default 4), `requests_per_minute` 1–10 000 (default 300). The API key and header values are write-only; `api_key_hint` is the key's last 4 characters when it has at least 16. |
| D-I-5 | `POST …/llm-connections/{id}/test` with `{model?}` runs one boolean judge call through `EvalRunner` (30 s), answering `200 {ok, model, latency_ms, error?}`. A success sets `status = 'ok'`; a pause-class error sets `'failing'` only when the test used the connection's default model, because a pause-class 404 for a typed model name says nothing about the key. |
| D-I-24 | `OrgPerm::ManageConnections` (org role `admin`) for writes and tests; `OrgPerm::View` for reads. Not session-only. |
| D-I-29 | Config keys `encryption_key`, `eval.workers`, `security.allow_private_network_egress`. |

## Versions

Add to the root `[workspace.dependencies]` in I1.2, with a one-line comment saying why. Then add `aes-gcm.workspace = true` to `crates/server/Cargo.toml` `[dependencies]`.

| Crate | Version | Features | Why |
|---|---|---|---|
| `aes-gcm` | `0.11.1` | `default-features = false, features = ["aes", "alloc"]` | AES-256-GCM for sealed columns (D-I-2). Nonces come from the server's own random source, so the `getrandom` feature is not needed |

In I1.6, `llmobs-eval` becomes a dependency of `llmobs-server`: add `llmobs-eval.workspace = true`. The root already has a `llmobs-eval = { path = "crates/eval" }` workspace entry; if it does not, add one next to the other `llmobs-*` path entries. This keeps the crate graph in `AGENTS.md` (`server` depends on `eval`).

`Cargo.lock` changes in I1.2 and I1.6; commit it. After I1.2 and I1.6, `cargo deny check` must still pass. An *error* is a STOP (§ Conventions); a duplicate-version *warning* is fine.

## Conventions for every sub-phase

- **Node.** Before any `just` recipe that runs web or TypeScript checks (`just check`, `just check-web`, `just openapi`), run `export PATH="$HOME/.local/share/nvm/v26.13.0/bin:$PATH"` in the same shell command.
- **Checks.** While iterating, use `just check-rust` or `just check-web`. Before the commit, run `just check` once, in the foreground, with a timeout of at least 20 minutes. Never start background or duplicate runs. Never commit with a failing check.
- **Contract.** A sub-phase that changes `api/openapi.rs` or an API type runs `just openapi` and commits the regenerated `crates/server/openapi.json` and `web/src/api/schema.gen.ts`. One that changes `api_examples.rs` runs `just api-examples` and commits `fixtures/api/manifest.json`.
- **Dead code.** `meta/mod.rs`, `routes/mod.rs` and `http/mod.rs` already carry `#![cfg_attr(not(test), allow(dead_code))]`, which covers their submodules. A new *top-level* module whose items get their first non-test caller in a later sub-phase (`crypto.rs` in I1.2, `eval/mod.rs` in I1.6) starts with that line plus a comment naming the sub-phase that first uses it.
- **Clippy.** The workspace denies warnings, including `unreachable_pub`, `unused_qualifications` and `needless_pass_by_value`. Make a type `pub` only when a `pub` type exposes it. Do not write `aes_gcm::Aes256Gcm` once it is imported. Functions that store a struct's strings take the struct by value and move the fields into the binds. If clippy reports `large_futures` on an `.await`, wrap that future in `Box::pin(…)`.
- **Temporary files** go in the session's scratchpad directory, not `/tmp`.
- **Errors and panics.** No `unwrap()` or `expect()` outside tests (AGENTS.md). Handlers return `ApiError`; library code returns `thiserror` enums.
- **Privacy.** Never log or put in an error message:
  - an API key, a header value, a key or key material;
  - a prompt or a provider answer.
  Logs carry ids, kinds, counts and durations. Audit events use `target: "audit"`.
- **Rust tests** live in a `#[cfg(test)] mod tests` at the bottom of the file they test, unless this plan names another place. HTTP tests use `crate::test_support::TestApp` with `#[tokio::test(flavor = "multi_thread")]`, and call `app.seed().await`. That gives org `acme` (`seeded.org_id`), a project, an owner session cookie (`seeded.cookie`) and keys.
- **Web tests** use Vitest and Testing Library over the MSW server (`@/mocks/node`). A test overrides an endpoint with `server.use(http.<method>(…))`. Never `vi.mock` a module. A test of a write asserts the request body it sent (D-L-10).
- **STOP rules.** Stop and report, without committing, when:
  - the plan is wrong about the code in a way you cannot resolve without guessing;
  - a test outside your change fails for a reason you did not cause;
  - `cargo deny check` reports an error.
- **Commit.** One commit per sub-phase, with the subject given, and no trailer lines.
- **Report.** What you did; every deviation from this plan, and why; new dependency versions or "no new dependency"; anything left unverified.

## Final layout after M1

```
crates/eval/src/judge/mod.rs        Adapter::from_name, StructuredOutput::from_name, UnknownName public (I1.1)
crates/eval/src/lib.rs              re-exports UnknownName (I1.1)
crates/server/src/
  config.rs                         encryption_key, EvalConfig, SecurityConfig, Secret::new (I1.1)
  crypto.rs                         SecretBox, Aad, CryptoError, KeySource, LoadedKey, read_key, load_key,
                                    verify_or_record, finish_rotation, ENCRYPTED_COLUMNS (I1.2)
  meta/instance_settings.rs         get_setting, get_setting_tx, put_setting (I1.2)
  meta/connections.rs               ConnectionRow, ConnectionFields, list/get/insert/update/delete/set_status (I1.4)
  admin.rs                          rotate_encryption_key, Rotated (I1.3)
  api/connections.rs                LlmConnection and its request and response types (I1.4)
  api/keys.rs                       optional_time becomes pub(crate) (I1.4)
  api/openapi.rs                    connection_paths (I1.4)
  auth/roles.rs                     OrgPerm::ManageConnections (I1.5)
  auth/limits.rs                    connection_tests limiter (I1.6)
  routes/connections.rs             list, get, create, update, delete (I1.5), test (I1.6)
  eval/mod.rs, eval/connection.rs   ConnectionCache (I1.6)
  ingest/mod.rs                     Ingest::prices (I1.6)
  http/mod.rs                       AppInner.secrets (I1.2), .eval_runner, .connections (I1.6)
  serve.rs                          open_secrets at startup (I1.2), runner (I1.6)
  test_support/mod.rs               TEST_KEY and AppInner fields (I1.2, I1.6)
  test_support/mock_llm.rs          in-process OpenAI-compatible mock (I1.6)
crates/server/migrations/meta/0003_connections.sql    (I1.2)
crates/server/tests/openapi.rs, tests/api_examples.rs (I1.4)
crates/cli/src/main.rs              ServeError label (I1.2), `admin rotate-encryption-key` (I1.3)
fixtures/api/static/llm-connections.json, llm-connection.json, llm-connection-test.json   (I1.4)
fixtures/api/requests/llm-connection-create.json      (I1.4)
web/src/routes/$org/settings/llm-connections.tsx (+ .test.tsx)   (I1.7)
web/src/components/settings/connection-dialog.tsx, connection-test.tsx   (I1.7)
```

## Shared reference

### Sealing (D-I-2)

```
sealed = [0x01] ++ key_id[0..4] ++ nonce[0..12] ++ AES-256-GCM(key, nonce, aad, plaintext)   // ciphertext ++ 16-byte tag
key_id = HMAC-SHA256(key, b"llmobs.key-id.v1")[0..4]
check  = lowercase hex of HMAC-SHA256(key, b"llmobs.encryption-check.v1")   // 64 hex characters
aad    = format!("{table}:{column}:{row_id}")                               // row_id: hyphenated lowercase UUID
```

The labels are byte strings exactly as written. They are not the product name, so the D46 rename does not touch them.

Code, for `aes-gcm` 0.11 (checked against the crate's own tests):

```rust
use aes_gcm::Aes256Gcm;
use aes_gcm::aead::array::Array;
use aes_gcm::aead::{Aead, KeyInit, Payload};

let cipher = Aes256Gcm::new(&Array(*key));                    // key: &[u8; 32]
let nonce_bytes: [u8; 12] = /* 12 random bytes, see "Random bytes" */;
let ciphertext = cipher
    .encrypt(&Array(nonce_bytes), Payload { msg: plaintext, aad: aad.as_bytes() })
    .map_err(|_| CryptoError::Corrupt)?;                       // fails only on absurd lengths
// open: nonce = sealed[5..17], ciphertext and tag = sealed[17..]
let plaintext = cipher
    .decrypt(&Array(nonce_bytes), Payload { msg: &sealed[17..], aad: aad.as_bytes() })
    .map_err(|_| CryptoError::Corrupt)?;
```

`open` checks, in order:
1. the length is at least 1 + 4 + 12 + 16 (else `Corrupt`);
2. the first byte is `0x01` (else `Corrupt`);
3. the key id equals this key's (else `WrongKey`);
4. decryption succeeds (else `Corrupt`).

**Random bytes** (keys and nonces): `crate::auth::secret::random_bytes(n)` returns `Result<Vec<u8>, AuthError>`. Map its error to `CryptoError::Rng`, then copy the bytes into the array with `<[u8; 12]>::try_from(bytes.as_slice())`. A length mismatch cannot happen; map it to `CryptoError::Rng` too.

**HMAC**: `hmac::{Hmac, Mac}` with `sha2::Sha256`, as in `crates/server/src/auth/oidc/state_cookie.rs`. Build it with `<Hmac<Sha256> as KeyInit>::new_from_slice(key)`. `KeyInit` is imported once, from `aes_gcm::aead::KeyInit` (hmac re-exports the same trait, and importing it twice is error E0252). **Constant-time compare**: `crate::auth::secret::constant_time_eq(&str, &str)`.

### Key file (D-I-1, D-I-3)

- Path: `config.data_dir.join("secret.key")`. Content: standard base64 with padding (44 characters) of the 32 key bytes, then `\n`. Mode 0600.
- **Write** (`write_key_file(path, key)`): remove a leftover `path` + `.tmp` if one exists (a crash can leave one with a wider mode), then write `path` with `.tmp` appended to its file name, opened with `create(true).truncate(true).write(true).mode(0o600)` (`std::os::unix::fs::OpenOptionsExt`). `sync_all()`, rename it over `path`, then fsync the directory (`File::open(dir)?.sync_all()`). A crash leaves either no file or a whole one.
- **Read**: the file text with one trailing `\n` (and a `\r` before it) removed, then decoded. A file whose mode allows group or other access (`metadata.permissions().mode() & 0o077 != 0`) logs `tracing::warn!(path = %…, "encryption key file is readable by other users")` and is still used.
- **Source order:**
  1. the `encryption_key` config value (`LLMOBS_ENCRYPTION_KEY`) wins, and the file is neither read nor written;
  2. otherwise the file;
  3. otherwise, generate the key, but only when `instance_settings` has no `encryption.check`. When a check value exists, startup fails with `CryptoError::Missing`.
- **Finishing a rotation**: at startup, when the env value is unset and before reading the key: if `secret.key.new` exists, decodes, and its check value equals the stored `encryption.check`, rename it over `secret.key` and log `warn` "finished an interrupted key rotation". Otherwise delete it and log `warn` "removed an unused secret.key.new". Also delete a leftover `secret.key.tmp` or `secret.key.new.tmp`.

### Connection contract

JSON field names are exact. Times are RFC 3339 through `llmobs_core::time_serde`, and optional times through `crate::api::keys::optional_time` (made `pub(crate)` in I1.4).

**`LlmConnection`** (response). Fields in this order, which is also the fixture key order:

| Field | Type | Notes |
|---|---|---|
| `id` | UUID | |
| `name` | string | |
| `adapter` | `LlmAdapter`: `"openai"` \| `"anthropic"` \| `"gemini"` | |
| `base_url` | string, omitted when unset | may carry only an `api-version` query parameter (Azure) |
| `default_model` | string, omitted when unset | |
| `structured_output` | `StructuredOutputMode`: `"native"` \| `"tool"` \| `"prompt"` | |
| `max_concurrency` | integer | 1–64 |
| `requests_per_minute` | integer | 1–10 000 |
| `api_key_hint` | string | last 4 characters, or `""` |
| `extra_header_names` | string[] | lowercase, sorted |
| `status` | `ConnectionStatus`: `"ok"` \| `"failing"` | |
| `last_error` | string, omitted when unset | |
| `last_error_at` | RFC 3339, omitted when unset | |
| `created_by` | UUID, omitted when unknown | |
| `created_at`, `updated_at` | RFC 3339 | |

**`CreateLlmConnectionRequest`** (`deny_unknown_fields`). Fields in this order: `name`, `adapter`, `api_key` (all required), `base_url?`, `default_model?`, `structured_output?`, `max_concurrency?`, `requests_per_minute?`, `extra_headers?` (object of string to string).
- Every `Option` field has `#[serde(default, skip_serializing_if = "Option::is_none")]`.
- `extra_headers` is `BTreeMap<String, String>` with `#[serde(default, skip_serializing_if = "BTreeMap::is_empty")]`.
- With these, the request fixture round-trips textually.
- Defaults applied by the handler: `structured_output` native, `max_concurrency` 4, `requests_per_minute` 300.

**`UpdateLlmConnectionRequest`** (`deny_unknown_fields`). Every field is optional and absent means unchanged, with the same `skip_serializing_if` attributes:
- `name`, `adapter`;
- `api_key` replaces the key;
- `base_url` and `default_model`: `""` clears the value;
- `structured_output`, `max_concurrency`, `requests_per_minute`;
- `extra_headers` (`Option<BTreeMap<String, String>>`) replaces the whole map, and `{}` clears it.

An empty body `{}` is valid and changes only `updated_at`. No field is nullable, so the OpenAPI null-stripping pass needs no exception.

**`TestLlmConnectionRequest`** (`deny_unknown_fields`): `model?`.

**`LlmConnectionTestResult`**: `ok` (bool), `model` (string, the model used), `latency_ms` (integer, `u64`), `error?`: `LlmConnectionTestError { class: TestErrorClass ("retryable" | "pause" | "permanent"), message: string }`.

**Validation.** Every failed rule adds a `FieldError { field, message, position: None }` (`crate::api::common::FieldError`). The handler answers `ApiError::BadRequest { detail: None, errors }` with all of them and stores nothing. On PATCH, only fields present in the body are checked.

| Field | Rule | Message |
|---|---|---|
| `name` | after `trim()`, 1–100 characters (`chars().count()`) | `name must be 1 to 100 characters` |
| `api_key` | 1–4096 bytes; no leading or trailing whitespace; no `char::is_control` character | `api_key must be 1 to 4096 bytes, without spaces at either end` |
| `base_url` | when present and not `""`: at most 2048 bytes; `url::Url::parse` succeeds; scheme `http` or `https`; has a host; `username()` is empty and `password()` is `None`; no fragment; the query, if any, has exactly one pair and its key is `api-version` (`url.query_pairs()`). Other query keys are refused because providers put keys there (`?key=`, `?code=`), and `base_url` is shown to every org member. | `base_url must be an http or https URL without credentials, fragment or query other than api-version` |
| `default_model` | when present and not `""`: after `trim()`, 1–200 bytes, and no whitespace, control character, `/`, `\`, `%`, `?` or `#` (the eval crate's own model rule, `validate_model` in `crates/eval/src/judge/mod.rs`) | `default_model must be 1 to 200 characters without spaces or / \ % ? #` |
| `max_concurrency` | 1–64 | `max_concurrency must be between 1 and 64` |
| `requests_per_minute` | 1–10000 | `requests_per_minute must be between 1 and 10000` |
| `extra_headers` | at most 20 entries | `at most 20 extra headers` |
| `extra_headers` | a name that `axum::http::HeaderName::from_bytes` refuses | `invalid header name` |
| `extra_headers.<lowercased name>` | the name is not `content-type`, `content-length`, `host` or `transfer-encoding`; two names are not equal after lowercasing; `axum::http::HeaderValue::from_str(value)` succeeds and the value is at most 4096 bytes | `invalid header` |

Names are stored trimmed. Header names are stored lowercased.

`adapter` and `structured_output` are checked by serde: an unknown value is the existing JSON 400.

OpenRouter-style model ids such as `openai/gpt-5` are refused by the eval crate's adapters today, so they are refused here too. Lifting that limit is a change to `llmobs-eval`, outside M1.

**Cap.** An org holds at most 100 connections. The 101st create is `409` with `an organization has at most 100 connections` (count inside the insert's transaction).

**Errors per route.**

| Route | Success | Errors (declared in OpenAPI) |
|---|---|---|
| `GET /api/v1/orgs/{org}/llm-connections` | `200` `LlmConnectionPage`, ordered by name then id | 400, 401, 403, 404, 429 |
| `POST /api/v1/orgs/{org}/llm-connections` | `201` `LlmConnection` | 400, 401, 403, 404, 409 (`a connection with this name already exists`, or the cap), 429 |
| `GET /api/v1/orgs/{org}/llm-connections/{id}` | `200` `LlmConnection` | 400 (id not a UUID), 401, 403, 404, 429 |
| `PATCH …/{id}` | `200` `LlmConnection` | 400, 401, 403, 404, 409, 429 |
| `DELETE …/{id}` | `204` | 400, 401, 403, 404, 429 (M2 adds 409) |
| `POST …/{id}/test` | `200` `LlmConnectionTestResult`, also when the provider failed (`ok: false`) | 400 (no model), 401, 403, 404, 429 |

A connection of another org, or an unknown id, is `404` (spec 08: never 403 for out-of-reach resources).

---

## I1.1 — Config keys and public adapter names

**Goal:** the three new config keys exist and are validated, and the server can parse stored adapter and mode names with the eval crate's own parsers.

**Edit only:** `crates/server/src/config.rs`, `crates/server/src/test_support/mod.rs`, `crates/server/src/serve.rs` (its test `ServerConfig { … }` literal only), `crates/server/src/ingest/fixtures_tests.rs` (its `ServerConfig { … }` literal only), `crates/eval/src/judge/mod.rs`, `crates/eval/src/lib.rs`, `docs/spec/12-operations.md`.

**Commit:** `Add encryption, eval and egress config keys`

### Config (`crates/server/src/config.rs`)

1. `ALIASES`: add `("LLMOBS_ENCRYPTION_KEY", "encryption_key"),` after the `LLMOBS_EMAIL_FROM` row.
2. `is_secret_path` (near line 1429): add `"encryption_key"` to the `matches!` list, so an env value made only of digits stays a string and is never echoed.
3. `secret_key` (the figment error mapper): add an arm before the `[..]` arm: `[key] if key == "encryption_key" => Some("encryption_key"),`.
4. `Secret<T>`: add a constructor after `expose`:

```rust
    /// Wraps a value read from somewhere other than the config loader.
    pub fn new(value: T) -> Self {
        Self(value)
    }
```

5. Add two structs, after `MediaConfig` and its `Default`:

```rust
/// `[eval]` (spec 06, D-I-6).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct EvalConfig {
    /// Evaluation jobs run at once by this process. Default 16, 1 to 256.
    pub workers: u32,
}

impl Default for EvalConfig {
    fn default() -> Self {
        Self { workers: 16 }
    }
}

/// `[security]` (spec 12, D166).
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct SecurityConfig {
    /// Let judges and webhooks reach private, loopback and link-local addresses. Default false.
    pub allow_private_network_egress: bool,
}
```

6. `ServerConfig`: add after `media`:

```rust
    /// `[eval]` worker settings.
    #[serde(default)]
    pub eval: EvalConfig,
    /// `[security]` settings.
    #[serde(default)]
    pub security: SecurityConfig,
    /// 32 bytes in standard base64. Unset: `<data_dir>/secret.key` (D-I-1).
    #[serde(default)]
    pub encryption_key: Option<Secret<String>>,
```

   `Secret`'s `Debug` prints `Secret(***)`, so the derived `Debug` of `ServerConfig` stays safe.
7. Remove "encryption key, evaluation workers," from the `ServerConfig` doc comment's list of keys that are not fields.
8. `validate`: add at the end, before `Ok(())`:

```rust
    if !(1..=256).contains(&config.eval.workers) {
        return Err(ConfigError::Invalid {
            key: "eval.workers",
            reason: "must be between 1 and 256",
        });
    }
```

   The key's content is checked when it is loaded (I1.2), so no config error can quote it.
9. Add `eval: EvalConfig::default(), security: SecurityConfig::default(), encryption_key: None,` to the three `ServerConfig { … }` literals: `test_support/mod.rs` `default_config` (near line 505), `serve.rs` (the test near line 1084) and `ingest/fixtures_tests.rs` (near line 433). Import the two types where needed.

### Eval crate (`crates/eval/src/judge/mod.rs`, `lib.rs`)

- `Adapter::from_name` and `StructuredOutput::from_name`: change `pub(crate) fn` to `pub fn`, and remove their `#[cfg_attr(not(test), allow(dead_code))]` lines. Their doc comment becomes "Parses the stored connection value. Unknown text is [`UnknownName`]."
- `UnknownName`: change `pub(crate) struct UnknownName;` to `pub struct UnknownName;` and remove its `cfg_attr` line. Keep its `Display` ("unknown name") and `Error` impls, and add the doc line "The rejected text is not kept."
- `lib.rs`: `pub use judge::{Adapter, JudgeConnection, JudgeUsage, StructuredOutput, UnknownName};`.

The existing `mod tests` in `judge/mod.rs` already round-trips both parsers. Add one test there, `from_name_rejects_other_text`: `"OPENAI"`, `""` and `"open ai"` are `Err(UnknownName)` for `Adapter`, and `"NATIVE"` is for `StructuredOutput`.

### Spec (`docs/spec/12-operations.md` § Configuration)

- Replace the `encryption_key` row: `` | `encryption_key` | `LLMOBS_ENCRYPTION_KEY` | unset: read `<data_dir>/secret.key`, generated on the first start of a new data directory (D-I-1) | ``.
- Replace the `eval.workers` row: `` | `eval.workers` | | `16` (1–256, D-I-6) | ``.
- Add after it: `` | `security.allow_private_network_egress` | | `false` (D166) | ``.
- § Security checklist: in the sentence "…(needed for local Ollama/vLLM — the setting is shown in the LLM connection form)…", replace "the setting is shown in the LLM connection form" with "the LLM connection form explains the setting; the UI cannot read its value (D-I-4)".

### Tests (`config.rs` `mod tests`; read the first tests there for the helper that calls `load_from(toml, env)`)

- `eval_and_security_default`: no TOML, no env → `eval.workers == 16`, `!security.allow_private_network_egress`, `encryption_key.is_none()`.
- `eval_workers_out_of_range_is_rejected`: TOML `[eval]\nworkers = 0` → `ConfigError::Invalid { key: "eval.workers", .. }`; `257` → the same.
- `encryption_key_comes_from_the_environment`: env `LLMOBS_ENCRYPTION_KEY=abc` → `encryption_key.map(|k| k.expose().clone()) == Some("abc".into())`.
- `numeric_encryption_key_stays_a_string`: env `LLMOBS_ENCRYPTION_KEY=12345678` → `Some("12345678")`.
- `encryption_key_type_error_does_not_quote_the_value`: TOML `encryption_key = 424242` → `ConfigError::Invalid { key: "encryption_key", .. }`, and its `to_string()` does not contain `424242`.
- `security_section_parses`: TOML `[security]\nallow_private_network_egress = true` → `true`.
- `server_config_debug_hides_the_key`: `format!("{config:?}")` with `encryption_key = Some(Secret::new("topsecretvalue".into()))` does not contain `topsecretvalue`.

### Done when

`just check` passes.

---

## I1.2 — Encryption key, `instance_settings` and the startup check

**Goal:** the server loads or generates its key, records or verifies the check value, finishes an interrupted rotation, and holds a `SecretBox` in `AppInner`. Migration `0003` creates `instance_settings` and `llm_connections`.

**Edit only:**
- `Cargo.toml` (root), `Cargo.lock`, `crates/server/Cargo.toml`;
- `crates/server/src/crypto.rs` (new), `crates/server/src/lib.rs`;
- `crates/server/src/meta/instance_settings.rs` (new), `crates/server/src/meta/mod.rs` (module list and tests);
- `crates/server/migrations/meta/0003_connections.sql` (new);
- `crates/server/src/admin.rs` (the test `schema_mismatch_names_the_versions` only);
- `crates/server/tests/snapshots/meta_schema.snap`;
- `crates/server/src/http/mod.rs`, `crates/server/src/serve.rs`, `crates/server/src/test_support/mod.rs`;
- `crates/cli/src/main.rs`;
- `docs/spec/04-metadata-db.md`, `docs/spec/12-operations.md`.

**Commit:** `Load the encryption key and verify it at startup`

### Migration `crates/server/migrations/meta/0003_connections.sql`

```sql
CREATE TABLE instance_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);

CREATE TABLE llm_connections (
  id TEXT PRIMARY KEY,
  org_id TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  adapter TEXT NOT NULL,
  base_url TEXT,
  secret_enc BLOB NOT NULL,
  api_key_hint TEXT NOT NULL,
  extra_headers_enc BLOB,
  extra_header_names TEXT NOT NULL DEFAULT '[]',
  default_model TEXT,
  structured_output TEXT NOT NULL DEFAULT 'native',
  max_concurrency INTEGER NOT NULL DEFAULT 4,
  requests_per_minute INTEGER NOT NULL DEFAULT 300,
  status TEXT NOT NULL DEFAULT 'ok',
  last_error TEXT,
  last_error_at INTEGER,
  created_by TEXT REFERENCES users(id) ON DELETE SET NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE(org_id, name)
);
```

`llm_connections` is created here so M1 has one migration; I1.3 and I1.4 read it. `sqlx::migrate!` picks the file up with no code change. If `main` already has a `0003_*.sql` when you start, use the next free number, adjust the numbers below by one, and say so in the report.

**Existing tests the new migration changes:**
- `meta/mod.rs` tests: the migration count assertion (`assert_eq!(count, 2)`, near line 387) becomes 3. In `open_existing_checks_the_file_and_the_version`, the inserted future version becomes `4`, and the expected values become `found == 4`, `expected == 3`.
- `admin.rs` `schema_mismatch_names_the_versions`: insert version `4`, and expect `"this binary expects schema 3, found 4"`.
- `crates/server/tests/snapshots/meta_schema.snap` (checked by `schema_matches_the_snapshot` in `meta/mod.rs`): regenerate it with `INSTA_UPDATE=always cargo test -p llmobs-server schema_matches_the_snapshot`. Read the diff: it must add exactly the two tables above and their three automatic indexes (`sqlite_autoindex_instance_settings_1`, `sqlite_autoindex_llm_connections_1`, `sqlite_autoindex_llm_connections_2`), and nothing else.

### `crates/server/src/meta/instance_settings.rs`

```rust
//! `instance_settings` rows (spec 04): one text value per key.

use sqlx::{SqliteConnection, SqlitePool};

use super::MetaError;

/// Key of the encryption check value (D-I-2).
pub(crate) const ENCRYPTION_CHECK: &str = "encryption.check";

/// The value of `key`, or `None`.
pub(crate) async fn get_setting(pool: &SqlitePool, key: &str) -> Result<Option<String>, MetaError>;

/// The same read on the writer, inside a transaction.
pub(crate) async fn get_setting_tx(conn: &mut SqliteConnection, key: &str) -> Result<Option<String>, MetaError>;

/// Inserts or replaces `key`.
pub(crate) async fn put_setting(conn: &mut SqliteConnection, key: &str, value: &str) -> Result<(), MetaError>;
```

SQL: `SELECT value FROM instance_settings WHERE key = ?` with `sqlx::query_scalar::<_, String>(…).fetch_optional(…)`; `INSERT INTO instance_settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value`. Register `pub(crate) mod instance_settings;` in `meta/mod.rs` in alphabetical order (no `cfg_attr` line of its own). Tests: get on an empty table is `None`; put then get; put twice keeps the second value.

### `crates/server/src/crypto.rs`

Register `pub(crate) mod crypto;` in `lib.rs` right after `pub mod config;`. The file starts with `#![cfg_attr(not(test), allow(dead_code))] // seal and rotation are used from I1.3 on`.

```rust
//! Sealed columns: AES-256-GCM with a key id and row-bound associated data (D-I-1, D-I-2).

use std::fmt;
use std::path::{Path, PathBuf};

use uuid::Uuid;

/// Where the key came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeySource {
    /// `LLMOBS_ENCRYPTION_KEY` or `encryption_key`.
    Env,
    /// `<data_dir>/secret.key`, read.
    File,
    /// `<data_dir>/secret.key`, created by this start.
    Generated,
}

/// A loaded key. `Debug` prints the source and the path, never the key.
pub(crate) struct LoadedKey {
    pub(crate) key: [u8; 32],
    pub(crate) source: KeySource,
    /// `<data_dir>/secret.key`, also when the source is `Env`.
    pub(crate) path: PathBuf,
}

/// Associated data: `table:column:row_id`.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Aad<'a> {
    pub(crate) table: &'static str,
    pub(crate) column: &'static str,
    pub(crate) row_id: &'a Uuid,
}

/// Seals and opens values with one key. `Debug` prints the key id only.
pub(crate) struct SecretBox {
    cipher: Aes256Gcm,
    key_id: [u8; 4],
    check: String,
}

/// Why a key could not be used, or a value could not be opened. No variant holds key bytes,
/// plaintext or ciphertext.
#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    /// The value is not 32 bytes of standard base64.
    #[error("encryption key from {origin} must be 32 bytes in standard base64")]
    BadKey { origin: String },
    /// A file could not be read or written.
    #[error("could not {action} {}", .path.display())]
    Io { action: &'static str, path: PathBuf, #[source] error: std::io::Error },
    /// The key does not match the check value.
    #[error("the encryption key does not match this data directory (check LLMOBS_ENCRYPTION_KEY or {})", .path.display())]
    Mismatch { path: PathBuf },
    /// `meta.db` has a check value and no key is available.
    #[error("no encryption key: set LLMOBS_ENCRYPTION_KEY or restore {} from the backup", .path.display())]
    Missing { path: PathBuf },
    /// A sealed value was written with another key.
    #[error("a sealed value was written with another key")]
    WrongKey,
    /// A sealed value is corrupt.
    #[error("a sealed value is corrupt")]
    Corrupt,
    /// The OS random source failed.
    #[error("random number generator failed")]
    Rng,
    /// `meta.db` failed.
    #[error(transparent)]
    Meta(#[from] crate::meta::MetaError),
}
```

`CryptoError` is `pub` because `ServeError` (public) wraps it; `MetaError` is the precedent, `pub` inside a `pub(crate)` module. `KeySource` stays `pub(crate)`: no public type exposes it, and `unreachable_pub` would warn.

The `origin` of `BadKey` is `"LLMOBS_ENCRYPTION_KEY"` for the env value, the file path (`path.display().to_string()`) for the file, and `"--new-from-stdin"` for the rotation input.

```rust
/// Every sealed column, for key rotation (D-I-3). Later milestones append rows.
pub(crate) const ENCRYPTED_COLUMNS: &[(&str, &str)] = &[
    ("llm_connections", "secret_enc"),
    ("llm_connections", "extra_headers_enc"),
];

/// `<data_dir>/secret.key`.
pub(crate) fn key_path(data_dir: &Path) -> PathBuf;

impl SecretBox {
    pub(crate) fn new(key: &[u8; 32]) -> Self;
    /// Lowercase hex HMAC-SHA256(key, "llmobs.encryption-check.v1").
    pub(crate) fn check_value(&self) -> &str;
    pub(crate) fn seal(&self, plaintext: &[u8], aad: Aad<'_>) -> Result<Vec<u8>, CryptoError>;
    pub(crate) fn open(&self, sealed: &[u8], aad: Aad<'_>) -> Result<Vec<u8>, CryptoError>;
}

/// Decodes `text` (standard base64, padding required, `base64::engine::general_purpose::STANDARD`)
/// into 32 bytes. Any other length or a decode error is `BadKey { origin }`.
pub(crate) fn decode_key(text: &str, origin: &str) -> Result<[u8; 32], CryptoError>;

/// Standard base64 of `key`.
pub(crate) fn encode_key(key: &[u8; 32]) -> String;

/// 32 random bytes (§ Random bytes).
pub(crate) fn random_key() -> Result<[u8; 32], CryptoError>;

/// § Key file, "Write".
pub(crate) fn write_key_file(path: &Path, key: &[u8; 32]) -> Result<(), CryptoError>;

/// The env value, else the file. `Ok(None)` when neither exists. Never writes.
pub(crate) fn read_key(env_value: Option<&str>, data_dir: &Path) -> Result<Option<LoadedKey>, CryptoError>;

/// § Key file, "Finishing a rotation". `check` is the stored check value, if any.
pub(crate) fn finish_rotation(data_dir: &Path, check: Option<&str>) -> Result<(), CryptoError>;

/// `read_key`; when it finds nothing, a new key file when `has_check` is false, else `Missing`.
pub(crate) fn load_key(env_value: Option<&str>, data_dir: &Path, has_check: bool) -> Result<LoadedKey, CryptoError>;

/// The outcome of [`verify_or_record`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CheckOutcome { Recorded, Matched }

/// Stores the check value when there is none; otherwise compares it. A difference is `Mismatch`.
pub(crate) async fn verify_or_record(meta: &crate::meta::MetaDb, secrets: &SecretBox, path: &Path) -> Result<CheckOutcome, CryptoError>;
```

Implementation notes:
- `verify_or_record`: `MetaDb::write`'s closure must return `MetaError`. Return an enum out of the closure, `enum Found { Recorded, Same, Different }`, and map `Different` to `CryptoError::Mismatch { path }` after the write. Inside the closure, `get_setting_tx(ENCRYPTION_CHECK)`: `None` → `put_setting`, `Recorded`; `Some(stored)` → `constant_time_eq(&stored, secrets.check_value())` gives `Same` or `Different`. The check string is copied into the closure (`let check = secrets.check_value().to_owned();`) because the closure is `Send` and borrows nothing.
- `impl fmt::Debug for LoadedKey` prints `source` and `path`. `impl fmt::Debug for SecretBox` prints the key id in hex. Neither derives `Debug`.
- `finish_rotation` decodes `secret.key.new` with `decode_key(text, &path.display().to_string())`; a decode failure counts as "does not match" (delete it, warn).

### State and startup

- `http/mod.rs` `AppInner`: add after `usage`:

```rust
    /// Seals and opens secret columns (D-I-2).
    pub(crate) secrets: crate::crypto::SecretBox,
```

- `serve.rs` `ServeError`: add

```rust
    /// The encryption key could not be loaded, or does not match the data directory.
    #[error(transparent)]
    Crypto(#[from] crate::crypto::CryptoError),
```

  and in `bind`, right after `MetaDb::open` succeeds:

```rust
    let secrets = match open_secrets(&config, &meta).await {
        Ok(secrets) => secrets,
        Err(error) => {
            release_storage(storage).await;
            meta.close().await;
            return Err(error.into());
        }
    };
```

  with a new private function in `serve.rs`:

```rust
/// Finishes an interrupted rotation, loads the key, then checks it (D-I-1, D-I-3).
async fn open_secrets(config: &ServerConfig, meta: &MetaDb) -> Result<SecretBox, CryptoError> {
    let check = get_setting(meta.reader(), ENCRYPTION_CHECK).await?;
    let env = config.encryption_key.as_ref().map(|key| key.expose().as_str());
    if env.is_none() {
        finish_rotation(&config.data_dir, check.as_deref())?;
    }
    let loaded = load_key(env, &config.data_dir, check.is_some())?;
    let secrets = SecretBox::new(&loaded.key);
    verify_or_record(meta, &secrets, &loaded.path).await?;
    if loaded.source == KeySource::Generated {
        tracing::warn!(
            path = %loaded.path.display(),
            "generated an encryption key; back this file up with the data directory"
        );
    }
    Ok(secrets)
}
```

  Pass `secrets` into `AppInner { … }`.
- `crates/cli/src/main.rs`: the `match` that labels `ServeError` (near line 172) gains `ServeError::Crypto(_) => "loading the encryption key",`.
- `test_support/mod.rs`:
  - add `pub(crate) const TEST_KEY: [u8; 32] = [7; 32];`;
  - in the builder, after `MetaDb::open`, add `let secrets = crate::crypto::SecretBox::new(&TEST_KEY);` and `crate::crypto::verify_or_record(&meta, &secrets, &crate::crypto::key_path(&config.data_dir)).await.expect("check value");`;
  - add `secrets,` to `AppInner { … }`.

### Spec

- `docs/spec/04-metadata-db.md`:
  - § Keys & credentials: replace the `llm_connections` DDL with the migration's DDL above, and add one sentence after it: "Connections belong to the org (D-I-4). `secret_enc` and `extra_headers_enc` are sealed (§ Encryption of secrets); `api_key_hint` and `extra_header_names` are what the API shows."
  - § Encryption of secrets: replace the paragraph with the rules in § Sealing and § Key file above. That covers the key source order, the file format and mode, no generation once a check value exists, the check value key `encryption.check`, the format version byte, the key id, the AAD `table:column:row_id`, AES-256-GCM, the random 96-bit nonce, and rotation finishing at startup. Cite D-I-1, D-I-2 and D-I-3.
  - § Metering & instance: the `instance_settings` comment line becomes `-- 'encryption.check': hex HMAC-SHA256 check value of the encryption key (D-I-2)`, and "`instance_settings` is created by track E with its first use (D135)." becomes "`instance_settings` is created by track I (migration `0003`, D156)."
- `docs/spec/12-operations.md` § Process lifecycle: replace "Track E adds: verify encryption key, after `meta.db`." with "After `meta.db`: finish an interrupted key rotation, load or generate the encryption key, and verify its check value (D-I-1, D-I-3)."

### Tests (`crypto.rs` `mod tests`)

- `seal_then_open_round_trips`: `open(seal(b"sk-test", aad), aad) == b"sk-test"`; two seals of the same text differ.
- `open_with_other_aad_fails`: another row id → `Corrupt`; another column → `Corrupt`.
- `open_with_other_key_is_wrong_key`: seal with `[1;32]`, open with `[2;32]` → `WrongKey`.
- `open_rejects_short_and_bad_version`: `&[]` → `Corrupt`; byte 0 set to `0x02` → `Corrupt`; the last byte flipped → `Corrupt`.
- `sealed_layout`: length `1 + 4 + 12 + plaintext.len() + 16`, `sealed[0] == 1`, `sealed[1..5]` is the key id.
- `check_value_is_stable`: for `[0;32]`, 64 lowercase hex characters, equal across two `SecretBox::new`, different for `[1;32]`.
- `decode_key_rules`: the base64 of 32 bytes is ok; 31 bytes → `BadKey`; `"not base64!"` → `BadKey`, and the message does not contain `not base64!`.
- `load_key_generates_once` (tempdir): `load_key(None, dir, false)` → `Generated`; the file exists with mode `0o600` (`std::os::unix::fs::PermissionsExt`); its content is 45 bytes ending in `\n`; no `secret.key.tmp` is left. A second call → `File` with the same key.
- `load_key_prefers_env`: env given, no file → `Env`, and no file was created.
- `load_key_refuses_to_generate_after_a_check`: no env, no file, `has_check = true` → `Missing`, and no file was created.
- `load_key_rejects_a_bad_file`: content `not base64\n` → `BadKey`, and the message names the path.
- `finish_rotation_renames_a_matching_new_key` and `finish_rotation_deletes_a_stale_new_key` (tempdir; check values from `SecretBox::new(&k).check_value()`).
- `encrypted_columns_cover_the_schema` (`crate::meta::test_db()`): `SELECT m.name, p.name FROM sqlite_master m JOIN pragma_table_info(m.name) p WHERE m.type = 'table' AND p.name LIKE '%\_enc' ESCAPE '\'` returns exactly the pairs of `ENCRYPTED_COLUMNS`. Later milestones that add a sealed column must extend the constant, or this test fails.
- `verify_or_record_accepts_same_and_refuses_other` (`crate::meta::test_db()`, which returns `(TempDir, MetaDb)`): first → `Recorded`, same key → `Matched`, another key → `Mismatch`.

`serve.rs` tests: add `a_wrong_encryption_key_fails_startup`.
1. Copy how the existing `a_bad_init_secret_fails_startup` builds a config with a temp data dir and ports 0.
2. Bind with `encryption_key = Some(Secret::new(encode_key(&[1; 32])))` and shut down.
3. Bind again with `[2; 32]` → `Err(ServeError::Crypto(_))`.
4. Bind with `[1; 32]` again → ok (the lock was released).

Add `startup_finishes_an_interrupted_rotation`: bind once (key file generated), shut down; rotate by hand: read the key, compute a new key `n`, write `secret.key.new` with `n`, and replace the stored check with `SecretBox::new(&n).check_value()` through `MetaDb::open` and `put_setting`. Bind again → ok, and `secret.key` now holds `n`. Then `startup_removes_a_stale_new_key`: leave a `secret.key.new` with a random key → bind ok, the file is gone, and `secret.key` is unchanged.

Add `a_lost_key_file_fails_startup`: bind with no env key (the file is generated), shut down, delete `secret.key`, bind again → `Err(ServeError::Crypto(CryptoError::Missing { .. }))`.

### Done when

`just check` passes. `cargo deny check` has no error.

---

## I1.3 — `llmobs admin rotate-encryption-key`

**Goal:** an operator replaces the key with the server stopped, and every sealed column follows.

**Edit only:** `crates/server/src/admin.rs` (including its module doc), `crates/server/src/crypto.rs` (only if a helper is missing), `crates/cli/src/main.rs`, `docs/spec/09-api.md` (§ CLI), `docs/spec/12-operations.md` (§ Backups).

**Commit:** `Add llmobs admin rotate-encryption-key`

### `crates/server/src/admin.rs`

Add to `AdminError`:

```rust
    /// Another process holds `<data_dir>/LOCK`.
    #[error("the server is running: stop it before rotating the key")]
    Locked,
    /// The key could not be loaded, does not match, or a value did not open.
    #[error("{0}")]
    Crypto(String),
    /// The new key is the current key.
    #[error("the new key is the current key")]
    SameKey,
```

Add:

```rust
/// What `rotate_encryption_key` did.
#[derive(Debug)]
pub struct Rotated {
    /// Sealed values re-encrypted.
    pub values: u64,
}

/// Re-encrypts every sealed column under a new key (D-I-3). The server must be stopped.
/// `new_key` is base64 text (from stdin); `None` generates a key, unless the current key
/// comes from `LLMOBS_ENCRYPTION_KEY` (step 6).
pub async fn rotate_encryption_key(config: &ServerConfig, new_key: Option<&str>) -> Result<Rotated, AdminError>;
```

Update the module doc of `admin.rs` ("These open the database without migrating and without the data-dir lock, so they work while `llmobs serve` is running (D136).") by adding: "`rotate_encryption_key` is the exception: it takes the lock and needs the server stopped (D-I-3)."

Steps, in this order:
0. If `<data_dir>/meta.db` does not exist → `NotInitialized` (as `list_users` answers). This also covers a missing data directory, before any lock file is created.
1. Open (create if missing) `<data_dir>/LOCK` with `OpenOptions::new().read(true).write(true).create(true).truncate(false)` and call `file.try_lock()`. `Err(std::fs::TryLockError::WouldBlock)` → `Locked`; another error → `Crypto(format!("could not lock {}", path.display()))`. Keep the `File` alive until the function returns. This copies `lock_dir` in `crates/storage/src/lib.rs`, which is private to storage.
2. `MetaDb::open_existing(&config.data_dir)`, mapping errors as `list_users` does.
3. Read the stored check (`get_setting(reader, ENCRYPTION_CHECK)`). When the env key is unset, call `crypto::finish_rotation(data_dir, check)` (any error → `Crypto(e.to_string())`).
4. `crypto::read_key(env, data_dir)`. `Ok(None)` → `Crypto("no encryption key: start the server once first".into())`. An error → `Crypto(e.to_string())`.
5. The old box is `SecretBox::new(&old.key)`. A missing check, or one that differs (`constant_time_eq`), → `Crypto("the current key does not match this data directory".into())`.
6. New key: `new_key` decoded with `decode_key(text.trim(), "--new-from-stdin")` (error → `Crypto`), else `random_key()` — except that when `old.source == KeySource::Env` and `new_key` is `None`, return `Crypto("the key comes from LLMOBS_ENCRYPTION_KEY: pass the new key with --new-from-stdin")` before any write: a generated key would exist only in memory after the commit. Equal to the old key → `SameKey`.
7. If `old.source == KeySource::File`: remove a stale `secret.key.new`, then `write_key_file(&data_dir.join("secret.key.new"), &new)`. On a `write_key_file` failure, also remove `secret.key.new.tmp`, which the failed write can leave behind (a missing file is ignored).
8. One `meta.write`. For each `(table, column)` in `ENCRYPTED_COLUMNS`:
   - Select `format!("SELECT id, {column} FROM {table} WHERE {column} IS NOT NULL")`. Add the comment `// table and column come from ENCRYPTED_COLUMNS, never from input`.
   - For each row: parse the id, `open` it with the old box and `Aad { table, column, row_id: &id }`. On any failure return `Err(MetaError::Corrupt { table, column })` from the closure, which rolls the transaction back.
   - `seal` with the new box, then `UPDATE {table} SET {column} = ? WHERE id = ?`.
   Then `put_setting(ENCRYPTION_CHECK, new_check)`, and return the count. Copy the two boxes' needs into the closure first: build the old and new `SecretBox` inside it from copies of the two keys, because the closure must be `Send` and borrow nothing. A `MetaError` after the write maps to `Crypto(e.to_string())`; `MetaError::Corrupt` names only the table and column.
   If the transaction fails, delete `secret.key.new` before returning the error.
9. If the source is `File`: `std::fs::rename(secret.key.new, secret.key)`, then fsync the directory. If the rename fails, return `Crypto(format!("the database uses the new key, which is in {}; rename it to secret.key", new_path.display()))`. The next server start would also finish it (`finish_rotation`).
10. Return `Rotated { values }`.

### CLI (`crates/cli/src/main.rs`)

```rust
    /// Re-encrypt stored secrets under a new key. Stop the server first.
    RotateEncryptionKey {
        /// Read the new key (32 bytes, standard base64) from one line of stdin. Default: generate one.
        #[arg(long)]
        new_from_stdin: bool,
        /// Config file. Overrides `LLMOBS_CONFIG`.
        #[arg(long)]
        config: Option<PathBuf>,
    },
```

In `admin_command`:

```rust
        AdminCommand::RotateEncryptionKey { new_from_stdin, config } => {
            let new_key = if new_from_stdin {
                Some(read_key_line(io::stdin().lock())?)
            } else {
                None
            };
            let loaded = load_config(config.as_deref())?;
            let rotated = admin::rotate_encryption_key(&loaded, new_key.as_deref())
                .await
                .context("rotating the encryption key")?;
            let mut stdout = io::stdout().lock();
            writeln!(stdout, "Re-encrypted {} values.", rotated.values)?;
            if loaded.encryption_key.is_some() {
                writeln!(stdout, "Set LLMOBS_ENCRYPTION_KEY to the new key before the next start.")?;
            }
        }
```

Add `fn read_key_line(input: impl BufRead) -> anyhow::Result<String>` next to `read_password_line` (`main.rs` ~line 288): the same body, with the messages "reading the key" and "expected a key on stdin". Update the `Admin` command's doc string (`main.rs` ~line 31, "Manage users in this data directory.") to "Manage users and the encryption key in this data directory."

### Spec

- `docs/spec/09-api.md` § CLI, the `llmobs admin …` row: change its Purpose from "local admin on the data dir (works offline, server may be running)" to "local admin on the data dir (works offline, server may be running, except `rotate-encryption-key [--new-from-stdin]`, which needs the server stopped; D-I-3)".
- `docs/spec/12-operations.md` § Backups: the section's file list names `<data_dir>/secret.key` (or `LLMOBS_ENCRYPTION_KEY`) once, followed by "Without it, LLM connection keys and webhook secrets cannot be read, and the server refuses to start (D-I-1). `llmobs admin rotate-encryption-key` replaces the key while the server is stopped (D-I-3). A backup taken before a rotation can only be read with the key that was current then; keep that key with it."

### Tests (`admin.rs` `mod tests`; reuse its `ready_config()` helper)

Seed with raw SQL. First an org row (`INSERT INTO orgs (id, name, slug, created_at, updated_at) VALUES (…)`, as `0001_identity.sql` defines it). Then connection rows with every `NOT NULL` column of `0003`, and values sealed by the old box with the right `Aad`.
- `rotation_reencrypts_and_replaces_the_file`:
  1. `ready_config()`; generate the key file with `crypto::load_key(None, dir, false)`, and record the check with `verify_or_record`.
  2. Seed two connections, one with `extra_headers_enc` too.
  3. Rotate → `values == 3`.
  4. `secret.key` holds the new key; each value opens with the new box to the original plaintext; the stored check is the new box's; `secret.key.new` is gone.
Seeding the check value: open the database with `MetaDb::open` and call `crypto::verify_or_record(&db, &SecretBox::new(&old), &key_path)`; close it before calling `rotate_encryption_key`.
- `rotation_with_an_env_key_reencrypts_under_a_given_key`: seed the check for `old` and one connection; config `encryption_key = Some(Secret::new(encode_key(&old)))`, `new_key = Some(&format!("  {}\r\n", encode_key(&new)))` (surrounding whitespace is trimmed where the key is decoded) → `values == 1`, no `secret.key` exists, the stored check is the new box's.
- `rotation_with_an_env_key_leaves_secret_key_alone`: also write a `secret.key`; after the rotation its bytes are unchanged, and no `secret.key.new` exists.
- `rotation_with_an_env_key_requires_a_new_key`: `new_key = None` → `Crypto("the key comes from LLMOBS_ENCRYPTION_KEY: pass the new key with --new-from-stdin")`, and nothing was written: no key file exists, and the stored check and row are unchanged.
- `rotation_refuses_while_locked`: open and `try_lock` `<data_dir>/LOCK` in the test → `Locked`, and the stored check is unchanged.
- `rotation_refuses_a_wrong_current_key`: check of key A stored, config key B → `Crypto(_)`; the database is unchanged.
- `rotation_refuses_the_same_key` → `SameKey`.
- `rotation_without_any_key_fails_and_writes_nothing` → `Crypto(_)`, and no `secret.key` exists.
- `rotation_rolls_back_on_a_corrupt_value`: the bad row is seeded first, its `secret_enc` sealed with the old key and its `extra_headers_enc` with another key, so the pass over `secret_enc` writes before the pass over `extra_headers_enc` fails → `Crypto(_)`; the bad row's stored bytes are identical to before the rotation; the good row still opens with the old key; `secret.key` is unchanged; `secret.key.new` does not exist.
- `rotation_finishes_an_interrupted_rotation_first`: `secret.key.new` holds the key of the stored check, as a rotation that crashed after the commit would leave it → it is renamed over `secret.key` first, and the rotation proceeds on that key.
- `rotation_deletes_a_stale_new_key`: `secret.key.new` does not match the stored check → it is deleted, and the rotation proceeds on `secret.key`.
- `rotation_removes_the_tmp_file_when_the_write_fails`: a directory at `secret.key.new` makes `write_key_file` fail → `Crypto(_)`, and `secret.key.new.tmp` does not exist; `secret.key`, the row and the check are unchanged.
- `rotation_on_a_missing_database_names_serve`: an empty temp dir → `NotInitialized`, and no `LOCK` file was created.
- `rotated_debug_prints_only_the_values`.

In `crates/cli/src/main.rs` tests (if it has a clap parsing test module, add to it; otherwise add one): `Cli::try_parse_from(["llmobs", "admin", "rotate-encryption-key", "--new-from-stdin"])` parses to that variant, and `["llmobs", "admin", "rotate-encryption-key", "--new", "x"]` is an error. `read_key_line` drops one trailing `\r\n`, accepts a line without a trailing newline, and answers "expected a key on stdin" on empty stdin.

### Done when

`just check` passes.

---

## I1.4 — Connection rows, API types and the contract

**Goal:** `meta.db` access for connections, the API types, and the OpenAPI declarations with examples. No route yet.

**Edit only:**
- `crates/server/src/meta/connections.rs` (new), `crates/server/src/meta/mod.rs`;
- `crates/server/src/api/connections.rs` (new), `crates/server/src/api/mod.rs`, `crates/server/src/api/keys.rs` (only `optional_time`'s visibility), `crates/server/src/api/openapi.rs`;
- `crates/server/src/api_examples.rs`;
- `crates/server/tests/api_examples.rs`, `crates/server/tests/openapi.rs`;
- `web/src/mocks/writes.test.ts` (`LATER_TRACKS` only);
- `crates/server/openapi.json`, `web/src/api/schema.gen.ts`, `fixtures/api/manifest.json` (generated);
- `fixtures/api/static/llm-connections.json`, `fixtures/api/static/llm-connection.json`, `fixtures/api/static/llm-connection-test.json`, `fixtures/api/requests/llm-connection-create.json` (new);
- `fixtures/api/README.md`.

**Commit:** `Declare the LLM connection API`

### `crates/server/src/meta/connections.rs`

```rust
//! `llm_connections` rows (spec 04, D-I-4). Sealed columns are opaque bytes here.

/// One stored connection. `Debug` is written by hand and prints the sealed columns' lengths only.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct ConnectionRow {
    pub(crate) id: Uuid,
    pub(crate) org_id: OrgId,
    pub(crate) name: String,
    pub(crate) adapter: String,
    pub(crate) base_url: Option<String>,
    pub(crate) secret_enc: Vec<u8>,
    pub(crate) api_key_hint: String,
    pub(crate) extra_headers_enc: Option<Vec<u8>>,
    pub(crate) extra_header_names: Vec<String>,
    pub(crate) default_model: Option<String>,
    pub(crate) structured_output: String,
    pub(crate) max_concurrency: u32,
    pub(crate) requests_per_minute: u32,
    pub(crate) status: String,
    pub(crate) last_error: Option<String>,
    pub(crate) last_error_at: Option<i64>,
    pub(crate) created_by: Option<Uuid>,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
}

/// The editable columns. Insert takes them with the id, org and author; update replaces them all.
#[derive(Clone)]
pub(crate) struct ConnectionFields {
    pub(crate) name: String,
    pub(crate) adapter: String,
    pub(crate) base_url: Option<String>,
    pub(crate) secret_enc: Vec<u8>,
    pub(crate) api_key_hint: String,
    pub(crate) extra_headers_enc: Option<Vec<u8>>,
    pub(crate) extra_header_names: Vec<String>,
    pub(crate) default_model: Option<String>,
    pub(crate) structured_output: String,
    pub(crate) max_concurrency: u32,
    pub(crate) requests_per_minute: u32,
}

pub(crate) async fn list_connections(pool: &SqlitePool, org: OrgId) -> Result<Vec<ConnectionRow>, MetaError>;            // ORDER BY name, id
pub(crate) async fn get_connection(pool: &SqlitePool, org: OrgId, id: Uuid) -> Result<Option<ConnectionRow>, MetaError>;  // WHERE org_id = ? AND id = ?
pub(crate) async fn get_connection_tx(conn: &mut SqliteConnection, org: OrgId, id: Uuid) -> Result<Option<ConnectionRow>, MetaError>;
/// `id` is chosen by the caller, because the AAD of the sealed columns needs it before the insert.
pub(crate) async fn insert_connection(conn: &mut SqliteConnection, id: Uuid, org: OrgId, fields: ConnectionFields, created_by: Option<Uuid>, now: i64) -> Result<ConnectionRow, MetaError>;
pub(crate) async fn update_connection(conn: &mut SqliteConnection, org: OrgId, id: Uuid, fields: ConnectionFields, now: i64) -> Result<Option<ConnectionRow>, MetaError>;
pub(crate) async fn delete_connection(conn: &mut SqliteConnection, org: OrgId, id: Uuid) -> Result<bool, MetaError>;
/// Sets `status`, `last_error` and `last_error_at` (`None` clears both), without touching `updated_at`.
pub(crate) async fn set_connection_status(conn: &mut SqliteConnection, id: Uuid, status: &'static str, error: Option<&str>, now: i64) -> Result<(), MetaError>;
```

- **Binds.**
  - ids: `id.to_string()` and `org.to_string()` (TEXT);
  - `extra_header_names`: `serde_json::to_string(&names)`. It cannot fail for `Vec<String>`; map an error to `MetaError::Corrupt { table: "llm_connections", column: "extra_header_names" }`;
  - integers: `i64::from(u32)`;
  - blobs: `&[u8]`.
- **Insert.** `status = 'ok'`, `created_at = updated_at = now`. A unique violation is `unique_or(error, "a connection with this name already exists")`. Read the row back with `get_connection_tx`; `None` there is `MetaError::Corrupt { table: "llm_connections", column: "id" }`.
- **Update.** `UPDATE llm_connections SET name=?, adapter=?, base_url=?, secret_enc=?, api_key_hint=?, extra_headers_enc=?, extra_header_names=?, default_model=?, structured_output=?, max_concurrency=?, requests_per_minute=?, updated_at=? WHERE org_id=? AND id=?`. Zero rows affected → `Ok(None)`. Same conflict mapping. Read the row back.
- **Row decoding** follows `meta/api_keys.rs`. A private `#[derive(sqlx::FromRow)] struct ConnectionRecord` holds the columns as stored (`String`, `Option<String>`, `Vec<u8>`, `Option<Vec<u8>>`, `i64`, `Option<i64>`). Read it with `sqlx::query_as::<_, ConnectionRecord>`, then convert with a `fn row_from(record) -> Result<ConnectionRow, MetaError>`. In `row_from`:
  - `Uuid::parse_str` for `id` and `created_by`;
  - `OrgId::from_uuid(Uuid::parse_str(…))` for `org_id`;
  - `u32::try_from` for the two integers;
  - `serde_json::from_str::<Vec<String>>` for the names.
  Each failure is `MetaError::Corrupt { table: "llm_connections", column: "<that column>" }`.
- - `insert_connection` and `update_connection` take `fields` by value and move its strings into the binds. `insert_connection` first counts the org's rows (`SELECT COUNT(*) FROM llm_connections WHERE org_id = ?`) and returns `MetaError::Conflict("an organization has at most 100 connections")` at 100.
- Register `pub(crate) mod connections;` in `meta/mod.rs` in alphabetical order. It needs no `cfg_attr` line of its own (§ Conventions).

### `crates/server/src/api/connections.rs`

The types in § Connection contract, with:
- `#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]`;
- `#[serde(deny_unknown_fields)]` on requests;
- a doc comment on every type and field (the OpenAPI document shows them);
- `#[serde(default, skip_serializing_if = "Option::is_none")]` on every `Option` field of every type;
- `#[serde(with = "llmobs_core::time_serde")] #[schema(value_type = String, format = DateTime)]` on `created_at` and `updated_at`;
- `#[serde(default, skip_serializing_if = "Option::is_none", with = "crate::api::keys::optional_time")] #[schema(value_type = Option<String>, format = DateTime)]` on `last_error_at`.

In `api/keys.rs`, change `mod optional_time {` to `pub(crate) mod optional_time {` and its functions from `pub(super)` to `pub(crate)`.

Enums are all `#[serde(rename_all = "snake_case")]` and derive `Copy` too:
- `LlmAdapter { Openai, Anthropic, Gemini }`;
- `StructuredOutputMode { Native, Tool, Prompt }`;
- `ConnectionStatus { Ok, Failing }`;
- `TestErrorClass { Retryable, Pause, Permanent }`.

Add `impl LlmAdapter { pub fn as_str(self) -> &'static str; pub fn from_name(text: &str) -> Option<Self> }`, and the same pair for `StructuredOutputMode` and `ConnectionStatus`: `as_str` returns the snake_case name, and `from_name` is its inverse (`None` for any other text). The handlers store `as_str().to_owned()` and read stored text back with `from_name`; `None` there is `MetaError::Corrupt` for that column. Add a unit test that round-trips every variant. Register `pub mod connections;` in `api/mod.rs`.

### OpenAPI (`api/openapi.rs`)

- `collect_schemas`: insert `LlmAdapter`, `StructuredOutputMode`, `ConnectionStatus`, `TestErrorClass`, `LlmConnection`, `CreateLlmConnectionRequest`, `UpdateLlmConnectionRequest`, `TestLlmConnectionRequest`, `LlmConnectionTestError` and `LlmConnectionTestResult`, plus `set.insert_page::<LlmConnection>("LlmConnectionPage")`.
- Tags: add `tag("llm_connections", "LLM provider connections that evaluators use. Keys are write-only.")` right after the `orgs` tag.
- New `fn connection_paths(paths: PathsBuilder) -> PathsBuilder`, called in `paths()` right after `member_paths`. Parameters are `path_string("org", "Org UUID or slug.")` and `path_uuid("id", "Connection id.")`. There is no `.securities(…)` call: these routes take a session or a PAT, like `list_org_members`.

| operationId | Summary | Method and path | Parameters | Request | Responses |
|---|---|---|---|---|---|
| `list_llm_connections` | List LLM connections | GET `/api/v1/orgs/{org}/llm-connections` | `with_query::<PageParams>(vec![org])` | — | `responses("200", json_named("LlmConnectionPage", "One page of connections, ordered by name. Keys and header values are not included."), QUERY_ERRORS)` |
| `post_llm_connection` | Create an LLM connection | POST same | `vec![org]` | `json_body("CreateLlmConnectionRequest")` | `responses("201", json_of::<LlmConnection>("The connection."), &[400, 401, 403, 404, 409, 429])` |
| `get_llm_connection` | Get an LLM connection | GET `/api/v1/orgs/{org}/llm-connections/{id}` | `vec![org, id]` | — | `responses("200", json_of::<LlmConnection>("The connection."), &[400, 401, 403, 404, 429])` |
| `patch_llm_connection` | Update an LLM connection | PATCH same | `vec![org, id]` | `json_body("UpdateLlmConnectionRequest")` | `responses("200", json_of::<LlmConnection>("The connection."), &[400, 401, 403, 404, 409, 429])` |
| `delete_llm_connection` | Delete an LLM connection | DELETE same | `vec![org, id]` | — | `responses("204", ResponseBuilder::new().description("The connection is deleted.").build(), &[400, 401, 403, 404, 429])` |
| `post_llm_connection_test` | Test an LLM connection | POST `/api/v1/orgs/{org}/llm-connections/{id}/test` | `vec![org, id]` | `json_body("TestLlmConnectionRequest")` | `responses("200", json_of::<LlmConnectionTestResult>("The outcome. A provider failure is ok: false, not an error status."), &[400, 401, 403, 404, 429])` |

Each operation uses `operation(id, "llm_connections", summary)` and `.description(Some(…))`:
- list and get: "Any member of the organization.";
- the writes and the test: "Organization admins and owners.".

Assemble the `PathItem`s like `api_key_paths` does: `PathItem::new(HttpMethod::Get, list)`, then set `.post`. The `{id}` item has get, patch and delete.

Run `just openapi`.

### `web/src/mocks/writes.test.ts`

Its first test fails when a write under `/api/v1/orgs` is in the contract but not in `WRITE_OPERATIONS`. The mocks come in I1.7, so add the four ids to `LATER_TRACKS` now:

```ts
/** Writes that later sub-phases add under the covered prefixes. */
const LATER_TRACKS: string[] = [
  "post_llm_connection",
  "patch_llm_connection",
  "delete_llm_connection",
  "post_llm_connection_test",
];
```

### `crates/server/tests/openapi.rs`

- `OPERATIONS`: add the six `(method, path, operationId)` rows.
- `TAGS`: add `"llm_connections"` right after `"orgs"`.
- `QUERY_OR_BODY`: add `"list_llm_connections"`.
- The error-set chain (the `if *id == … else if …` that builds `expected_errors`): add these arms right before the `READ_OPERATIONS` arm:

```rust
        } else if *id == "post_llm_connection" || *id == "patch_llm_connection" {
            expected_errors.extend(["400", "401", "403", "404", "409", "429"]);
        } else if *id == "get_llm_connection"
            || *id == "delete_llm_connection"
            || *id == "post_llm_connection_test"
        {
            expected_errors.extend(["400", "401", "403", "404", "429"]);
```

- `document_lint`'s `pages` list: add `("LlmConnectionPage", "LlmConnection")`. `assert_nullability`'s `pages` list: add `"LlmConnectionPage"`.
- The security branch needs no change: these operations override no security, so they fall into its `else` arm.

### Fixtures

The round-trip in `tests/api_examples.rs` compares the canonical JSON of the parsed value with the file text. Write each file with its keys in the struct's field order, two-space indentation and a final newline, like the other `static/*.json` files. If a round-trip fails, print the canonical form the test computes and copy it.

- `static/llm-connections.json`: `{"data": [<the connection>], "next_cursor": null}`. The connection:
  - `id` `01900000-0000-7000-8000-000000000400`, `name` `OpenAI`, `adapter` `openai`;
  - `default_model` `gpt-5-mini`, `structured_output` `native`, `max_concurrency` 4, `requests_per_minute` 300;
  - `api_key_hint` `WXYZ`, `extra_header_names` `[]`, `status` `ok`;
  - `created_by` `01900000-0000-7000-8000-000000000100`, `created_at` and `updated_at` `2026-01-01T09:00:00.000000000Z`.
- `static/llm-connection.json`: that connection alone.
- `static/llm-connection-test.json`: `{"ok": true, "model": "gpt-5-mini", "latency_ms": 420}`.
- `requests/llm-connection-create.json`: `{"name": "OpenAI", "adapter": "openai", "api_key": "sk-EXAMPLE-not-a-real-key-WXYZ", "default_model": "gpt-5-mini"}`.
- `fixtures/api/README.md` § Fixed identifiers: add the row "LLM connection | `01900000-0000-7000-8000-000000000400`, `OpenAI`. `requests/llm-connection-create.json` holds a synthetic key; it is not a live key".

`crates/server/src/api_examples.rs`, in the static examples list next to `list_api_keys`:

```rust
        example(
            "list_llm_connections",
            params(&[("org", "acme")]),
            BTreeMap::new(),
            "static/llm-connections.json",
            "application/json",
        ),
        example(
            "get_llm_connection",
            params(&[("org", "acme"), ("id", "01900000-0000-7000-8000-000000000400")]),
            BTreeMap::new(),
            "static/llm-connection.json",
            "application/json",
        ),
        example_created(
            "post_llm_connection",
            params(&[("org", "acme")]),
            "static/llm-connection.json",
        ),
        example(
            "post_llm_connection_test",
            params(&[("org", "acme"), ("id", "01900000-0000-7000-8000-000000000400")]),
            BTreeMap::new(),
            "static/llm-connection-test.json",
            "application/json",
        ),
```

Run `just api-examples`.

`crates/server/tests/api_examples.rs`:
- imports: `use llmobs_server::api::connections::{CreateLlmConnectionRequest, LlmConnection, LlmConnectionTestResult};`;
- `static_files_match_their_types`: `roundtrip::<Page<LlmConnection>>("static/llm-connections.json")`, `roundtrip::<LlmConnection>("static/llm-connection.json")` and `roundtrip::<LlmConnectionTestResult>("static/llm-connection-test.json")`;
- `request_examples_match_their_types_and_the_openapi_document`: `roundtrip::<CreateLlmConnectionRequest>("requests/llm-connection-create.json")`, plus the schema check that `score-batch.json` gets in that test, against `CreateLlmConnectionRequest`;
- `generated_files_roundtrip`: before the `other => panic!` arm, add

```rust
            "list_llm_connections" => roundtrip::<Page<LlmConnection>>(&example.file),
            "get_llm_connection" | "post_llm_connection" => roundtrip::<LlmConnection>(&example.file),
            "post_llm_connection_test" => roundtrip::<LlmConnectionTestResult>(&example.file),
```

### Tests (`meta/connections.rs` `mod tests`)

Use `crate::meta::test_db()` (returns `(TempDir, MetaDb)`). Insert an org with `crate::meta::orgs::insert_org(conn, "Acme", "acme", now)` inside `db.write`; read `meta/orgs.rs` for its exact signature and return type. Tests that set `created_by` first insert a user with `crate::meta::users::insert_user` (as `routes/members.rs` tests do), because `created_by` is a foreign key. Pass `created_by: None` where the test does not need it.
- `insert_list_get_round_trip`: insert `b` then `a` → the list order is `a`, `b`; every field reads back; `get` with another org's id → `None`.
- `duplicate_name_is_a_conflict` → `MetaError::Conflict("a connection with this name already exists")`.
- `org_cap_is_100`: 100 inserts succeed, the 101st is `MetaError::Conflict("an organization has at most 100 connections")`.
- `update_replaces_columns_and_bumps_updated_at`; updating an unknown id → `None`.
- `delete_reports_whether_a_row_went`.
- `set_status_keeps_updated_at`: `failing` with an error, then `ok` with `None`, which clears `last_error` and `last_error_at`; `updated_at` is unchanged.
- `org_delete_cascades`: `DELETE FROM orgs WHERE id = ?` removes the connection.
- `debug_hides_sealed_bytes`: a row with `secret_enc = vec![200, 201, 202]` → `format!("{row:?}")` does not contain `200, 201`.

### Done when

`just check` passes, including `contract_file_is_up_to_date`, `document_lint`, `manifest_covers_the_read_operations`, `manifest_matches_the_operations`, `generated_files_roundtrip` and the web type check.

---

## I1.5 — Connection routes

**Goal:** list, get, create, update and delete work, with the access rules and validation of § Connection contract.

**Edit only:** `crates/server/src/auth/roles.rs`, `crates/server/src/routes/connections.rs` (new), `crates/server/src/routes/mod.rs`, `crates/server/src/http/access.rs` (the `routes()` list only), `crates/server/src/http/access_suite.rs` (`is_list` only), `docs/spec/08-auth.md`, `docs/spec/09-api.md`.

**Commit:** `Add LLM connection routes`

### Permission

`auth/roles.rs`, `OrgPerm`: add `/// Create, edit, delete and test LLM connections (D-I-24).` `ManageConnections,`. In `min_role`, the `Admin` arm becomes `Self::Rename | Self::CreateProject | Self::ManageMembers | Self::ManageConnections => OrgRole::Admin`. Fix any other exhaustive `match` the compiler points to. The existing test that asserts each `OrgPerm::min_role` (near `auth/roles.rs:251`) gains `assert_eq!(OrgPerm::ManageConnections.min_role(), OrgRole::Admin);`.

### `crates/server/src/routes/connections.rs`

`pub(crate) fn routes() -> Vec<RouteDef>` with five rows; the test route comes in I1.6. All rows have `documented: true`.

| Method | Path | Access |
|---|---|---|
| GET | `/api/v1/orgs/{org}/llm-connections` | `Access::Org { perm: OrgPerm::View, session_only: false }` |
| POST | same | `Access::Org { perm: OrgPerm::ManageConnections, session_only: false }` |
| GET | `/api/v1/orgs/{org}/llm-connections/{id}` | `View` |
| PATCH | same | `ManageConnections` |
| DELETE | same | `ManageConnections` |

Register `pub(crate) mod connections;` in `routes/mod.rs`, and `routes.extend(crate::routes::connections::routes());` in `http/access.rs` `routes()` right after the `members` line.

**Extractors.**
- `State(state): State<AppState>`.
- `Extension(access): Extension<OrgAccess>`: the org is `access.org`, an `OrgRow` with `id: OrgId`.
- For writes, `Extension(principal): Extension<Principal>`, then `let user = principal::session_user(&principal)?;`. Despite its name it returns the user of a session or a PAT, and refuses only an API key, which org routes never see.
- The id: `error::Path(IdPath { id }): error::Path<IdPath>` with `#[derive(Deserialize)] struct IdPath { id: Uuid }`. axum ignores the `{org}` segment for a struct that does not name it; `routes/members.rs` `UserPath` is the precedent. A non-UUID id is the existing path 400.

**Handlers.**
- **list**:
  - Take `error::Query(params): error::Query<PageParams>`.
  - Load `let rows = connections::list_connections(state.meta.reader(), access.org.id).await?;`.
  - Convert: `let items = rows.iter().map(|row| Ok((row.name.clone(), row.id.to_string(), connection_json(row)?))).collect::<Result<Vec<_>, ApiError>>()?;`.
  - Answer `error::Json(crate::routes::orgs::page_named(items, params.cursor.as_deref(), params.limit.get())?)`.
- **get**: `get_connection`; `None` → `ApiError::NotFound`.
- **create**: `error::Json(body): error::Json<CreateLlmConnectionRequest>`.
  1. Validate everything (table); on errors answer 400.
  2. `let id = Uuid::now_v7();`.
  3. Seal `api_key` with `Aad { table: "llm_connections", column: "secret_enc", row_id: &id }`.
  4. When `extra_headers` is not empty: build a `BTreeMap<String, String>` keyed by lowercased names, and seal `serde_json::to_vec(&map)` with column `extra_headers_enc`. `extra_header_names` is its keys (sorted by the `BTreeMap`).
  5. `api_key_hint`: if `api_key.chars().count() >= 16`, its last 4 `char`s, else `""`.
  6. Build `ConnectionFields` (`adapter: body.adapter.as_str().to_owned()`; defaults `native`, 4, 300; `base_url` and `default_model` trimmed, and `None` when empty).
  7. `insert_connection(conn, id, access.org.id, fields, Some(user.id), now)` inside `state.meta.write`.
  8. Audit `tracing::info!(target: "audit", connection_id = %id, org_id = %access.org.id, user_id = %user.id, "llm connection created")`.
  9. Answer `(StatusCode::CREATED, error::Json(connection_json(&row)?))`.
  A seal or JSON error is `ApiError::Internal(Box::new(error))`.
- **update**: validate the present fields first (400 before any write). Then one `state.meta.write` closure:
  1. `get_connection_tx` (`None` → return `Ok(None)` from the closure);
  2. merge the PATCH onto the row's values into a `ConnectionFields`. Absent keeps the row's value. `base_url: Some("")` → `None`, same for `default_model`. `api_key: Some(k)` → re-seal with the same id, and a new hint. `extra_headers: Some(map)` → an empty map clears `extra_headers_enc` and the names; otherwise re-seal.
  3. `update_connection`.
  Doing the read, merge and write in one transaction keeps two PATCHes from undoing each other.
  - The closure returns `Result<Option<ConnectionRow>, MetaError>`.
  - Seal *before* the closure, because the closure must be `Send` and cannot borrow `state.secrets`. When the body has `api_key` or `extra_headers`, seal the new values up front with the id from the path, and move the sealed bytes, the new hint and the new names into the closure.
  - `None` → 404. Audit `"llm connection updated"`.
- **delete**: `delete_connection` → `false` → 404; `true` → `StatusCode::NO_CONTENT`. Audit `"llm connection deleted"`.
- `fn connection_json(row: &ConnectionRow) -> Result<LlmConnection, ApiError>`: maps the stored strings back to the enums. An unknown stored value is `ApiError::Internal(Box::new(MetaError::Corrupt { table: "llm_connections", column: "adapter" }))`, or the column at fault.

`http/access_suite.rs` checks every route of `routes()` automatically. Add `"llm-connections"` to the list in `is_list` so the suite also checks that the list answer carries no `*_hash` key.

### Spec

- `docs/spec/08-auth.md`: add after the role table: "LLM connections belong to the org. Org admins and owners manage and test them, and every org member can list them without their keys (D-I-4, D-I-24)."
- `docs/spec/09-api.md` § Orgs, projects, members, keys: replace the row `GET/POST /api/v1/orgs/{org}/llm-connections · GET/PATCH/DELETE …/{id} · POST …/{id}/test (track E)` with "`GET/POST /api/v1/orgs/{org}/llm-connections` · `GET/PATCH/DELETE …/{id}` · `POST …/{id}/test`: org-wide connections; API keys and header values are write-only, and responses carry `api_key_hint` and `extra_header_names` (D-I-4, D-I-5)".

### Tests (`routes/connections.rs` `mod tests`)

Copy the private helpers `signed_in(app, email) -> (Uuid, String)` and `add_member(app, org, user, role)` from `routes/members.rs` tests (lines ~431–470, with their imports and the `cookie_pair` helper they use) into this module. A second user becomes a `member` with `add_member(&app, seeded.org_id, user, OrgRole::Member)`.

To get a PAT, `POST /api/v1/me/tokens` with the owner's cookie and `{"name": "t", "scopes": ["read", "write"]}`; the answer's `secret` field is the bearer value (`Authorization: Bearer <secret>`).

- `api_key_rules`: a 4097-byte key, `" sk-x"`, `"sk-x "` and `"sk\u{7}x"` → 400 with field `api_key`; a 4096-byte key → 201.
- `base_url_rules`: a fragment (`https://x.com/v1#a`), no host (`http:///v1`), 2049 bytes, and `?key=abc` → 400; `?api-version=2024-10-21` → 201.
- `concurrency_bounds`: `max_concurrency` 1 and 64 → 201; 65 → 400.
- `patch_name_conflict_is_409`.
- `other_org_patch_and_delete_are_404`.
- `list_pages_by_name`: three connections, `?limit=1` → `next_cursor` set; following it twice returns the names in order, then `next_cursor: null`.
- `owner_creates_and_reads_back_without_the_key`:
  - POST `fixtures/api/requests/llm-connection-create.json`'s body → 201;
  - `api_key_hint == "WXYZ"`;
  - the response text does not contain `sk-EXAMPLE` and has no `api_key` key;
  - GET by id and the list return the same object.
- `the_stored_key_is_sealed_with_its_row_id`:
  - `secret_enc` read from `meta.db` does not contain the plaintext bytes;
  - `app.state.secrets.open(&bytes, Aad { table: "llm_connections", column: "secret_enc", row_id: &id })` gives the plaintext;
  - opening with another id fails.
- `hint_boundaries`: a 15-character key → `""`; a 16-character key → its last 4; a key ending in `éèàü` with at least 16 `char`s → `"éèàü"`.
- `validation_reports_every_field`:
  - body: name `"  "`, base_url `ftp://x`, default_model `a/b`, max_concurrency 0, requests_per_minute 20000, and 21 headers;
  - answer: 400 with the six field errors (`name`, `base_url`, `default_model`, `max_concurrency`, `requests_per_minute`, `extra_headers`) and their exact messages;
  - nothing is stored.
- `base_url_credentials_are_refused`: `https://u:p@x.com` → 400.
- `header_rules`:
  - `Content-Type` → field `extra_headers.content-type`;
  - name `bad name` → field `extra_headers`, message `invalid header name`;
  - `X-A` with `x-a` → field `extra_headers.x-a`;
  - valid names are returned lowercased and sorted.
- `duplicate_name_is_409`.
- `patch_keeps_absent_fields_and_clears_with_empty_strings`:
  - PATCH `{"default_model": ""}` → `default_model` omitted, and hint and key unchanged (open the stored value);
  - PATCH `{"api_key": "sk-NEWNEWNEWNEWNEW1234"}` → hint `1234`;
  - PATCH `{}` → 200, and only `updated_at` changed (advance the manual clock first with `app.state.clock.advance(Duration::from_secs(1))`).
- `patch_only_checks_present_fields`: a row stored with a valid name; PATCH `{"max_concurrency": 0}` → one error, field `max_concurrency`.
- `patch_unknown_field_is_400`: `{"api_keyy": "x"}` → 400.
- `patch_extra_headers_replaces_and_clears`.
- `member_can_list_but_not_write`: an org `member` → GET 200; POST, PATCH and DELETE → 403.
- `viewer_can_list`.
- `other_org_is_404`: the owner creates a second org (`POST /api/v1/orgs`) and a connection there; that id through `acme`'s path → 404.
- `delete_then_get_is_404`.
- `pat_can_manage`: the owner's `read, write` PAT can POST.
- `no_key_material_in_logs`: wrap a create and a PATCH of the key in `crate::test_support::capture_logs(async { … })`; the captured text does not contain either key.

### Done when

`just check` passes, including `route_table_matches_parameters_and_openapi_security` and the access suite.

---

## I1.6 — Connection test and the shared runner

**Goal:** `POST …/{id}/test` runs one judge call through the eval crate with the connection's decrypted key. The server holds the one `EvalRunner` that later milestones reuse.

**Edit only:**
- `Cargo.toml` (root, only if the `llmobs-eval` path entry is missing), `Cargo.lock`, `crates/server/Cargo.toml`;
- `crates/server/src/lib.rs`;
- `crates/server/src/eval/mod.rs` (new), `crates/server/src/eval/connection.rs` (new);
- `crates/server/src/ingest/mod.rs` (one accessor);
- `crates/server/src/auth/limits.rs` (one limiter);
- `crates/server/src/http/mod.rs`, `crates/server/src/serve.rs`;
- `crates/server/src/test_support/mod.rs`, `crates/server/src/test_support/mock_llm.rs` (new);
- `crates/server/src/routes/connections.rs`.

**Commit:** `Test LLM connections through the eval runner`

### Dependency

`crates/server/Cargo.toml` `[dependencies]`: `llmobs-eval.workspace = true` after `llmobs-core`.

### State

- `http/mod.rs` `AppInner`, after `secrets`:

```rust
    /// One egress client for judges, webhooks and connection tests (D-I-29).
    pub(crate) eval_runner: Arc<llmobs_eval::EvalRunner>,
    /// Decrypted connections, keyed by id and `updated_at`.
    pub(crate) connections: crate::eval::connection::ConnectionCache,
```

- `serve.rs` `bind`, after `open_secrets` succeeds:

```rust
    let eval_runner = match llmobs_eval::EvalRunner::new(llmobs_eval::EgressPolicy {
        allow_private_network: config.security.allow_private_network_egress,
    }) {
        Ok(runner) => Arc::new(runner),
        Err(_error) => {
            release_storage(storage).await;
            meta.close().await;
            return Err(ServeError::Config(ConfigError::Invalid {
                key: "security",
                reason: "could not build the evaluation HTTP client",
            }));
        }
    };
```

  and add `eval_runner, connections: crate::eval::connection::ConnectionCache::default(),` to `AppInner`. `test_support` does the same with `.expect("eval runner")` and the config's policy. Tests that call the in-process mock set the policy with `TestApp::with_config(|config| config.security.allow_private_network_egress = true)`.
- `ingest/mod.rs`: add

```rust
    /// The current price table (D-G-10). Judge runs price their `llm` span with it.
    pub(crate) fn prices(&self) -> Arc<PriceTable> {
        Arc::clone(&self.prices.read().unwrap_or_else(PoisonError::into_inner))
    }
```

- `auth/limits.rs`: add a limiter for connection tests, 10 per minute per user. Read the file first and copy the feedback limiter: a field `connection_tests: KeyedLimiter<Uuid>` with a doc line, the same line in the `Limits` struct doc's list, its construction in `Limits::new()`, and `self.connection_tests.prune(now)` in `Limits::prune` (~line 213). Key it by the user id. A refusal is `ApiError::RateLimited { retry_after_s }`, as the other limiters return. M2's evaluator test runs share this limiter (D-I-21).

### `crates/server/src/eval/mod.rs`

```rust
//! Online evaluation in the server: connections now; test runs, the queue and triggers later (track I).

pub(crate) mod connection;
```

Register `pub(crate) mod eval;` in `lib.rs`, in alphabetical order.

### `crates/server/src/eval/connection.rs`

```rust
/// At most this many decrypted connections are kept.
const CACHE_ENTRIES: usize = 256;

/// Decrypted connections keyed by id, holding the `updated_at` they were built from.
/// Write `impl fmt::Debug` by hand: it prints the entry count only.
#[derive(Default)]
pub(crate) struct ConnectionCache {
    entries: std::sync::Mutex<std::collections::HashMap<Uuid, (i64, Arc<JudgeConnection>)>>,
}

impl ConnectionCache {
    /// The decrypted connection for `row`, from the cache or by opening its sealed columns.
    pub(crate) fn get(&self, secrets: &SecretBox, row: &ConnectionRow) -> Result<Arc<JudgeConnection>, ConnectionError>;
    /// Drops `id`. Called after every update and delete of a connection.
    pub(crate) fn forget(&self, id: Uuid);
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ConnectionError {
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    #[error("corrupt llm_connections.{0}")]
    Corrupt(&'static str),
}
```

- `get`: when the map holds `row.id` with the same `updated_at`, return a clone of the `Arc`. Otherwise:
  - open `secret_enc` and build `llmobs_eval::Secret::new(String::from_utf8(bytes)…)` (write the path in full: `crate::config::Secret` is another type; invalid UTF-8 → `Corrupt("secret_enc")`);
  - open `extra_headers_enc` when present, parse a `BTreeMap<String, String>` (failure → `Corrupt("extra_headers_enc")`), and map it to `Vec<(String, Secret)>`;
  - parse `adapter` and `structured_output` with `llmobs_eval::Adapter::from_name` and `llmobs_eval::StructuredOutput::from_name` (`Corrupt("adapter")` / `Corrupt("structured_output")`);
  - parse `base_url` with `url::Url::parse` (`Corrupt("base_url")`).
  Build `llmobs_eval::JudgeConnection { adapter, base_url, api_key, extra_headers, structured_output }` and insert it. When the map already holds `CACHE_ENTRIES` entries, clear it first. Recover a poisoned lock with `PoisonError::into_inner`.
- In `routes/connections.rs`, call `state.connections.forget(id)` after a successful update and after a successful delete. `forget` is what keeps the cache correct inside this process. The `updated_at` check only matters for rows changed elsewhere; the frozen test clock makes it useless in tests, which is why the route tests below check `forget`.

### Test route (`routes/connections.rs`)

Add the row `POST /api/v1/orgs/{org}/llm-connections/{id}/test`, `Access::Org { perm: OrgPerm::ManageConnections, session_only: false }`, `documented: true`, handler `test_connection`:

1. Check the `connection_tests` limiter for `user.id`.
2. Body `error::Json<TestLlmConnectionRequest>`; the UI always sends an object.
3. Load the row (`get_connection`; `None` → 404).
4. `explicit` = `body.model` trimmed and non-empty. `model` = `explicit`, else `row.default_model`. Neither → 400 with field `model` and message `model is required when the connection has no default model`.
5. `let connection = state.connections.get(&state.secrets, &row).map_err(|error| ApiError::Internal(Box::new(error)))?;`
6. Build the evaluator:

```rust
const TEST_PROMPT: &str = "Reply true.";
let evaluator = llmobs_eval::Evaluator {
    id: row.id,
    name: "connection test".to_owned(),
    config: llmobs_eval::EvaluatorConfig::LlmJudge(llmobs_eval::JudgeConfig {
        connection_id: row.id,
        model: model.clone(),
        prompt: TEST_PROMPT.to_owned(),
        output: llmobs_eval::JudgeOutput::Boolean,
        include_reason: false,
        temperature: None,
        // Reasoning models spend tokens before answering; 1024 can truncate a working call.
        max_output_tokens: 4096,
        timeout_ms: 30_000,
    }),
    output: llmobs_eval::OutputScore {
        name: "connection_test".to_owned(),
        data_type: llmobs_core::enums::ScoreDataType::Boolean,
        categories: None,
    },
};
```

7. Run it:

```rust
let ingest = llmobs_otel::IngestContext {
    // A connection belongs to no project, and the judge trace is discarded.
    project_id: llmobs_core::ids::ProjectId::from_uuid(Uuid::nil()),
    ingest_time: state.clock.now_ns(),
    retention_days: None,
    limits: state.config.ingest.limits(&state.config.media),
};
let prices = state.ingest.prices();
let bytes = crate::auth::secret::random_bytes(16).map_err(|error| ApiError::Internal(Box::new(error)))?;
let bytes = <[u8; 16]>::try_from(bytes.as_slice()).map_err(|error| ApiError::Internal(Box::new(error)))?;
let judge_trace_id = llmobs_core::ids::TraceId::from_bytes(bytes)   // refuses all-zero bytes only
    .map_err(|error| ApiError::Internal(Box::new(error)))?;
let started = std::time::Instant::now();
let run = state.eval_runner.run(llmobs_eval::RunInput {
    evaluator: &evaluator,
    variables: &llmobs_eval::Variables::from_map(BTreeMap::new()),
    connection: Some(connection.as_ref()),
    webhook_secret: None,
    target: llmobs_eval::EvalTarget { trace_id: None, span_id: None },
    origin: llmobs_eval::EvalOrigin::Test,
    judge_trace_id,
    ingest: &ingest,
    prices: &prices,
}).await;
let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
```

   `run.judge_trace` is dropped.
8. Outcome, from `run.outcome`:
   - `Ok(_)` → `ok: true`, `error: None`. Any boolean answer counts: the call reached the provider and the answer fit the schema.
   - `Err(e)` → `ok: false`, `error: Some(LlmConnectionTestError { class, message: e.to_string() })`, where `class` is `e.class()` mapped `ErrorClass::Retryable → TestErrorClass::Retryable` and so on. The display quotes no payload (D160).
9. Status:
   - On `ok`, if `row.status != "ok"`, `set_connection_status(id, "ok", None, now)`.
   - On an error of class `Pause` when `explicit` is `None` (the connection's own default model), `set_connection_status(id, "failing", Some(&message), now)`.
   - A pause-class error on an explicit test model leaves the status alone, because a 404 for an unknown model is pause-class and says nothing about the key.
   - Retryable and permanent errors leave the status alone.
10. Audit `tracing::info!(target: "audit", connection_id = %row.id, org_id = %access.org.id, user_id = %user.id, ok, latency_ms, "llm connection tested")`.
11. Answer `200` with `LlmConnectionTestResult { ok, model, latency_ms, error }`.

### `crates/server/src/test_support/mock_llm.rs`

A small in-process OpenAI-compatible server for server tests. Register it as `pub(crate) mod mock_llm;` in `test_support/mod.rs`.

```rust
/// One scripted reply.
pub(crate) struct Reply {
    pub(crate) status: u16,
    pub(crate) body: serde_json::Value,
}

/// Serves `POST /v1/chat/completions` on 127.0.0.1:0. Replies in order; the last one repeats.
/// Records each request's headers and JSON body. Aborts its task on drop.
pub(crate) struct MockLlm { /* addr: SocketAddr, state: Arc<…>, task: JoinHandle<()> */ }

impl MockLlm {
    pub(crate) async fn start(replies: Vec<Reply>) -> Self;
    /// `http://127.0.0.1:<port>/v1`, a connection's `base_url`.
    pub(crate) fn base_url(&self) -> String;
    pub(crate) fn requests(&self) -> Vec<(axum::http::HeaderMap, serde_json::Value)>;
}

/// A 200 chat completion whose message content is `content`, with usage 10 in and 3 out.
pub(crate) fn completion(content: &str) -> Reply;
```

`completion(content)` body:

```json
{"id": "chatcmpl-test", "object": "chat.completion", "model": "gpt-test",
 "choices": [{"index": 0, "finish_reason": "stop", "message": {"role": "assistant", "content": "<content>"}}],
 "usage": {"prompt_tokens": 10, "completion_tokens": 3, "total_tokens": 13}}
```

Build it with axum: `Router::new().route("/v1/chat/completions", post(handler)).with_state(…)`, served on a `tokio::net::TcpListener` bound to `127.0.0.1:0`. `crates/eval/tests/support/mock.rs` is a model; read it, but do not import it, since it belongs to another crate's tests. Store requests in a `std::sync::Mutex<Vec<…>>`.

### Tests (`routes/connections.rs` `mod tests`, with `TestApp::with_config(|c| c.security.allow_private_network_egress = true)` unless stated)

- `test_succeeds_and_uses_the_decrypted_key`:
  - the mock replies `completion(r#"{"value": true}"#)`;
  - create a connection with `base_url = mock.base_url()`, key `sk-test-key-0123456789`, extra header `x-extra: one`, default model `gpt-test`;
  - POST `/test` with `{}` → `ok: true`, `model == "gpt-test"`, no `error`;
  - the mock saw `authorization: Bearer sk-test-key-0123456789`, `x-extra: one`, and body `model == "gpt-test"`.
- `test_uses_the_body_model`: body `{"model": "other-model"}` → the request's `model` is `other-model`, and the answer's `model` too.
- `test_without_any_model_is_400`: no default model and body `{}` → 400 with field `model`.
- `a_401_marks_the_connection_failing`:
  - the mock answers 401 with `{"error": {"message": "bad key SECRETPAYLOAD", "type": "invalid_request_error"}}` → `ok: false`, `error.class == "pause"`, and the message does not contain `SECRETPAYLOAD`;
  - GET shows `status: "failing"` and `last_error`;
  - a following successful test (mock replies `[Reply { status: 401, body: … }, completion(r#"{"value": true}"#)]` in order) sets `status: "ok"` and removes `last_error`.
- `a_404_on_an_explicit_model_keeps_the_status`: 404 with `{"model": "nope"}` → `class: "pause"`, and the status stays `ok`.
- `a_500_is_retryable_and_keeps_the_status`.
- `a_private_address_is_refused_without_the_setting`: a `TestApp` with the default config (egress denied) → `ok: false`, `class: "permanent"`, and the mock received no request.
- `test_uses_the_patched_key`: test once, PATCH `{"api_key": "sk-second-key-0123456789"}`, test again → the mock's second request carries `Bearer sk-second-key-0123456789`.
- `test_after_delete_is_404`.
- `member_cannot_test` → 403.
- `tests_are_rate_limited`: the 11th test within a minute → 429.
- `cache_follows_updates` (unit test in `eval/connection.rs`): `get`, then `forget`, then a row with a new sealed key → the new key; a row with the same `updated_at` and no `forget` returns the cached value.

### Done when

`just check` passes. No test calls a real provider (D167).

---

## I1.7 — "LLM connections" settings page

**Goal:** org members see the connections; admins and owners add, edit, delete and test them.

**Edit only:** `web/src/routes/$org/settings/llm-connections.tsx` and `.test.tsx` (new), `web/src/components/settings/connection-dialog.tsx` and `connection-test.tsx` (new), `web/src/components/settings/settings-nav.ts`, `web/src/api/queries-settings.ts`, `web/src/lib/roles.ts` and `roles.test.ts`, `web/src/mocks/writes.ts`, `web/src/mocks/writes.test.ts` (`LATER_TRACKS` only), `docs/spec/11-ui.md`, `docs/roadmap.md` (the I row). `web/src/routeTree.gen.ts` is generated and gitignored: do not commit it.

**Commit:** `Add the LLM connections settings page`

### Navigation and queries

- `settings-nav.ts`: `orgSettingsNav` gains `{ to: "/$org/settings/llm-connections", label: "LLM connections" }` after Members. Every org member sees it, because members can list.
- `api/queries-settings.ts`:

```ts
export function connectionsInit(org: string) {
  return { params: { path: { org }, query: { limit: 100 } } };
}

export function connectionsKey(org: string) {
  return $api.queryOptions("get", "/api/v1/orgs/{org}/llm-connections", connectionsInit(org)).queryKey;
}
```

### Page (`routes/$org/settings/llm-connections.tsx`)

`createFileRoute("/$org/settings/llm-connections")`. It reads the role as `members.tsx` does (`me.orgs.find((item) => item.slug === org)?.role`), and sets `const canManage = canManageConnections(role)`.

`lib/roles.ts` gains the client mirror of `OrgPerm::ManageConnections` (D-I-24, D-L-4):

```ts
/** Mirrors `OrgPerm::ManageConnections`: org admins and owners (D-I-24). */
export function canManageConnections(role: OrgRole | undefined): boolean {
  return atLeast(role, "admin");
}
```

`roles.test.ts` gains a test: owner and admin → true; member, viewer and `undefined` → false.

Layout: `<SettingsPage title="LLM connections" description="Provider keys that evaluators use. Keys are stored encrypted and never shown again." actions={…}>`. The action is `<DisabledReason reason={canManage ? undefined : "Only admins and owners can add connections"}><Button disabled={!canManage} onClick={…}>Add connection</Button></DisabledReason>`.

The list is a child component using `$api.useQuery("get", "/api/v1/orgs/{org}/llm-connections", connectionsInit(org))`:
- **loading**: three `Skeleton` rows;
- **error**: `throw query.error` (the route's error component shows it), as `components/settings/org-invites.tsx` does;
- **empty**: `<EmptyState icon={PlugZap} title="No connections yet" description="Add one to run LLM judges." />` (`components/empty-state.tsx`; `PlugZap` from `lucide-react`). Read the component for its exact props;
- **list**: a `Table` (`components/ui/table.tsx`) with the columns
  - Name;
  - Provider ("OpenAI-compatible", "Anthropic", "Gemini");
  - Default model (or "—");
  - Base URL (`new URL(base_url).host`, or "Default");
  - Key (`••••` + `api_key_hint`, or `••••` when the hint is empty);
  - Status: a `Badge` "OK", or "Failing" with `last_error` as visible small text under it;
  - actions.
- **actions per row:**
  - "Test" opens `ConnectionTest`;
  - "Edit" opens `ConnectionDialog` in edit mode;
  - "Delete" opens `ConfirmDialog` (`components/confirm-dialog.tsx`) with `title="Delete connection?"`, `description={`Evaluators that use ${name} will stop working.`}`, `confirmLabel="Delete"`, `destructive`, `pending={remove.isPending}` and `onConfirm`. Read the component for its exact props.
  For non-admins all three buttons are disabled, each inside `DisabledReason` with "Only admins and owners can manage connections".

Mutations: `$api.useMutation(…)`. On success, `queryClient.invalidateQueries({ queryKey: connectionsKey(org) })`. The delete mutation carries no secret, so it needs no `gcTime: 0`.

### `components/settings/connection-dialog.tsx`

`ConnectionDialog({ org, connection, open, onOpenChange }: { org: string; connection?: Schema<"LlmConnection">; open: boolean; onOpenChange: (open: boolean) => void })`.

Inputs are controlled; there is no form library (D-L-11).

**Secrets (D-L-5).**
- The create and patch mutations are created with `{ gcTime: 0 }`, as `create-key-dialog.tsx` does.
- On close and on success: call `mutation.reset()` and clear the key and header-value state.
- The API key input is `type="password"` with `autoComplete="new-password"`.

**Fields, in this order:**
- **Name**: `TextField`; client error "Enter a name.". Prefilled on edit. Do not pass `required` to any field: the HTML attribute blocks the submit in jsdom, and the client errors must show instead.
- **Provider**: `Select` with "OpenAI-compatible" (`openai`), "Anthropic" (`anthropic`), "Gemini" (`gemini`). The default on create is `openai`; prefilled on edit.
- **API key**: required on create (client error "Enter the API key."). On edit it starts empty with the placeholder "Leave empty to keep the current key", and is sent only when not empty.
- **Base URL**: optional. Help text: "For OpenAI-compatible servers such as Azure, vLLM, Ollama, OpenRouter or LiteLLM. Local addresses need the server setting security.allow_private_network_egress."
- **Default model**: optional.
- **Structured output**: `Select` with "Native (default)" (`native`), "Tool call" (`tool`), "Prompt" (`prompt`). It starts unset on create (shows "Native (default)"), and is sent only when the user picked a value. Help text: "Use Tool call or Prompt only if the provider rejects native structured output."
- **Advanced**: a native `<details>` element with `<summary>Advanced</summary>`; there is no collapsible component in `components/ui`. It contains:
  - Max concurrency and Requests per minute: number inputs that start empty on create, with placeholders `4` and `300`, and are sent only when not empty;
  - Extra headers: rows of name and value inputs, with "Add header" and a remove button per row. On edit, the existing header names show with empty values and the note "Values are hidden. Set every header again to change them." Extra headers are sent only when the user changed this section; removing every row sends `{}`. When the section is sent, a row with a name and an empty value is a client error "Enter a value for each header.", and nothing is sent.

**Body.**
- On create: only the fields that were set, so a minimal form sends exactly `{name, adapter, api_key}` plus what was typed.
- On edit: only the fields that changed. Clearing Base URL or Default model sends `""`; `api_key` and `extra_headers` follow the rules above.

**Errors.**
- `fieldErrors(problem)` (`@/api/problem`) are shown under the matching field: `name`, `api_key`, `base_url`, `default_model`, `max_concurrency`, `requests_per_minute`. Any field starting with `extra_headers` goes under the headers section.
- `problemMessage(error, "Could not save the connection.")` (`@/api/problem`, which takes a fallback as its second argument) goes above the submit button (`FormError`). A 409's detail is shown there too.

**Submit labels:** "Add connection" or "Save", with `SubmitButton`.

### `components/settings/connection-test.tsx`

`ConnectionTest({ org, connection, open, onOpenChange })`: a dialog with a Model input, prefilled with `default_model` (or empty), and a "Run test" button.
- The button calls `POST …/{id}/test` with `{model}` when the input is not empty, else `{}`, and shows "Testing…" while pending.
- On `ok`: "Connection works ({latency_ms} ms, model {model})".
- Not ok: "Test failed", the `error.message`, and a hint per class:
  - `pause`: "The key, quota or model access was refused. Rules that use this connection pause until a test of the default model passes.";
  - `retryable`: "The provider did not answer in time or is overloaded. Try again.";
  - `permanent`: "The request was rejected. Check the model name, base URL and structured output mode."
- After every test, invalidate `connectionsKey(org)`, since the status may have changed.
- A 429 (`problemOf(error)?.status === 429`) shows "Too many tests. Wait a minute." above the button.

### Mocks (`mocks/writes.ts`)

Add to `WRITE_OPERATIONS`:
- `{ operationId: "post_llm_connection", method: "post", path: "/api/v1/orgs/{org}/llm-connections" }`;
- `patch_llm_connection` and `delete_llm_connection`, both on `/api/v1/orgs/{org}/llm-connections/{id}`;
- `post_llm_connection_test` on `/api/v1/orgs/{org}/llm-connections/{id}/test`.

Add to `RESPONSES`:
- `post_llm_connection`: `llm-connection.json` with `name` from the body when it is a string, status 201;
- `patch_llm_connection`: the same, status 200;
- `delete_llm_connection`: `noContent()`;
- `post_llm_connection_test`: `llm-connection-test.json`.

Import the two fixtures at the top, like the others. In `writes.test.ts`, empty the `LATER_TRACKS` array that I1.4 filled (back to `const LATER_TRACKS: string[] = [];` with its original comment), since the four writes are now listed.

### Spec (`docs/spec/11-ui.md`)

- § Navigation: replace `/:org/settings/{llm-connections,usage,models}   (own plan, …)` with two lines:
  - `/:org/settings/llm-connections           LLM connections (track I, D-I-27)`;
  - `/:org/settings/{usage,models}   (own plan, once its API contract exists; D123)`.
- § Settings → Org: replace "LLM connections (test button), custom model prices, …" with "LLM connections (track I): list for every member; add, edit, delete and test for admins (D-I-4); custom model prices, usage …".

### Tests (`routes/$org/settings/llm-connections.test.tsx`)

The list GET is served from the manifest (`static/llm-connections.json`). Each write is mocked with `server.use`, and its body is asserted. Use `renderApp` from `@/test/render` as the other settings tests do.

- `lists connections with their status and key hint`: renders "OpenAI", "gpt-5-mini", "••••WXYZ" and "OK"; the nav link "LLM connections" has `aria-current="page"`.
- `adds a connection`:
  - click "Add connection";
  - fill Name `Prod`, API key `sk-secret`, Default model `gpt-5-mini`, and leave Provider at its default;
  - submit → the POST body equals `{name: "Prod", adapter: "openai", api_key: "sk-secret", default_model: "gpt-5-mini"}`, and the dialog closes.
- `refuses an empty key without calling the server`: shows "Enter the API key.", and the `server.use` POST handler was not called.
- `edits without resending the key`: change Name only → the PATCH body is `{name: "Renamed"}`.
- `clears the default model with an empty string`: clear Default model → the PATCH body is `{default_model: ""}`.
- `shows a server field error under its field`: the POST answers 400 with `errors: [{field: "base_url", message: "base_url must be an http or https URL without credentials, fragment or query other than api-version"}]` → the text appears, and the Base URL input has `aria-invalid="true"`.
- `tests a connection`:
  - click "Test", then "Run test" without touching the Model input (it holds `gpt-5-mini`);
  - the POST body is `{model: "gpt-5-mini"}`;
  - "Connection works (420 ms, model gpt-5-mini)" appears.
- `tests with an empty model sends an empty body`: clear Model, then Run test → body `{}`.
- `shows a failed test with its hint`: the answer is `{ok: false, model: "m", latency_ms: 5, error: {class: "pause", message: "provider status 401"}}` → "Test failed", the message and the pause hint appear.
- `shows a failing connection's error`: the GET answers one connection with `status: "failing", last_error: "provider status 401"` → "Failing" and the error text are visible.
- `refuses a header without a value`: on edit, open Advanced, add a row with name `x-a` and no value, Save → "Enter a value for each header.", and no PATCH was sent.
- `deletes after confirming`: Delete, then confirm → the DELETE was called for `01900000-0000-7000-8000-000000000400`.
- `a member sees the list but cannot manage`:
  - `/api/auth/me` answers role `member` (copy the override in `general.test.tsx`);
  - `screen.getByRole("button", { name: "Add connection" })` is disabled, and its `parentElement` has the accessible description "Only admins and owners can add connections" (`DisabledReason` puts the description on the wrapper; see `members.test.tsx`);
  - the row buttons are disabled.

### Done when

`just check` passes. `just web-dev` shows the page at `/acme/settings/llm-connections` with the mock data.

---

## After M1

Report: what the seven commits did, deviations, the `aes-gcm` version, and anything unverified. I1.7 also updates the I row of `docs/roadmap.md` to say M1 is implemented, with the branch name.
