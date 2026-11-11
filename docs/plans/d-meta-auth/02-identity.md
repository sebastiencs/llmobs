# Part 2 · Identity store, secrets, passwords, limits (P6–P8)

The data layer and the building blocks of authentication. No HTTP routes yet.

---

## P6 — Identity and tenancy store

**Goal:** typed functions over `users`, `orgs`, `projects`, `org_members` and `project_members`, with email normalization and slug rules.

**Edit only:** `crates/server/src/meta/{users.rs,orgs.rs,projects.rs,members.rs}` (new), `crates/server/src/meta/mod.rs`, `crates/server/src/auth/{mod.rs,roles.rs}` (new; enums only), `crates/server/src/lib.rs`, `crates/server/Cargo.toml`, root `Cargo.toml` (`unicode-normalization`).

**Commit:** `server: add the identity and tenancy store`

### Shapes

Every function takes either `&SqlitePool` (reads) or `&mut SqliteConnection` (inside `MetaDb::write`), plus `now: i64` where it writes. Row structs are `pub(crate)` and mirror the columns (`Option` for nullable ones). IDs are generated with `Uuid::now_v7()` inside the insert functions.

**`auth/roles.rs`** (enums only in P6; P10 adds the rules):

```rust
pub(crate) use crate::api::account::OrgRole;           // owner | admin | member | viewer
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProjectOverride { Admin, Member, Viewer, None }
```

Both parse from and print to the stored strings (`"owner"`, …, `"none"`); an unknown stored string is `MetaError::Corrupt`.

**`meta/users.rs`**
- `pub(crate) fn normalize_email(raw: &str) -> Result<String, EmailError>`: trim, NFC, lowercase (`str::to_lowercase`), then validate: at most 254 bytes, exactly one `@`, non-empty local part (≤ 64 bytes) and domain, the domain has a `.` and no empty label, no whitespace or control characters anywhere. `EmailError` is a unit-like enum (`Invalid`) whose message never contains the input.
- `UserRow { id, email, name, password_hash, email_verified_at, is_instance_admin, disabled_at, created_at, updated_at }`.
- `insert_user(conn, NewUser { email, name, password_hash, email_verified_at, is_instance_admin }, now) -> Result<UserRow, MetaError>`: `email` must already be normalized (debug-assert it); a unique violation → `MetaError::Conflict("email already registered")`.
- `user_by_id(pool, id)`, `user_by_email(pool, normalized)` → `Option<UserRow>`.
- `count_users(pool) -> i64`, and `count_users_tx(conn)` for use inside writes (P11's setup race).
- `set_password_hash(conn, id, Option<&str>, now)`, `set_email_verified(conn, id, now)`, `set_name(conn, id, Option<&str>, now)`, `set_disabled(conn, id, Option<i64>, now)`, `set_instance_admin(conn, id, bool, now)`.
- `count_active_instance_admins(conn) -> i64` (`is_instance_admin = 1 AND disabled_at IS NULL`).
- `list_users(pool, after_email: Option<&str>, limit: u32) -> Vec<UserRow>` ordered by `email` (cursor = the last email, P22 encodes it).

**`meta/orgs.rs`**
- `pub(crate) fn validate_slug(s: &str) -> Result<(), SlugError>`: 2–48 characters, `[a-z0-9-]`, starts and ends with `[a-z0-9]`, no `--`. Reserved (they collide with UI routes of spec 11 and future ones): `admin`, `account`, `api`, `assets`, `cli-auth`, `invite`, `login`, `logout`, `mcp`, `reset`, `settings`, `setup`, `signup`, `static`, `verify`, `v1`, `healthz`, `readyz`. Org slugs only; project slugs share the format but not the reserved list.
- `pub(crate) fn slug_from_name(name: &str) -> String`: lowercase, every run of characters outside `[a-z0-9]` becomes one `-`, trimmed of `-`, cut to 48 (then trimmed again); empty → `"org"` (orgs) / `"project"` (projects); a reserved result gets `-1` appended.
- `OrgRow { id: OrgId, name, slug, state, created_at, updated_at }` (`limits` is not read in D).
- `insert_org(conn, name, slug, now) -> Result<OrgRow, MetaError>` (unique slug → `Conflict("slug taken")`).
- `insert_org_with_derived_slug(conn, name, now)`: tries `slug_from_name`, then `-2`, `-3`, … up to `-99`, inside the same transaction; beyond that `Conflict`.
- `org_by_id`, `org_by_slug` → `Option<OrgRow>`; `rename_org(conn, id, name, now)`.
- `orgs_of_user(pool, user_id) -> Vec<(OrgRow, OrgRole)>` ordered by name.

**`meta/projects.rs`**
- `ProjectRow { id: ProjectId, org_id: OrgId, name, slug, retention_days: Option<u32>, state, created_at, updated_at }`. `retention_days` outside `1..=36500` in storage → `Corrupt`.
- `insert_project(conn, org_id, name, slug, retention_days, now)` and `insert_project_with_derived_slug` (same pattern, unique per org).
- `project_by_id(pool, id)`, `project_by_slugs(pool, org_slug, project_slug)` → `Option<ProjectRow>`.
- `projects_of_org(pool, org_id) -> Vec<ProjectRow>` ordered by name.
- `update_project(conn, id, ProjectPatch { name: Option<String>, retention_days: Option<Option<u32>> }, now)`.

**`meta/members.rs`**
- `org_role(pool, org_id, user_id) -> Option<OrgRole>`; `org_role_tx(conn, …)`.
- `add_org_member(conn, org_id, user_id, role, now)`: insert; already a member → `Conflict("already a member")`.
- `set_org_role(conn, org_id, user_id, role)`; `remove_org_member(conn, org_id, user_id)`: deletes the membership **and** the user's `project_members` rows for that org's projects, in the caller's transaction.
- `count_owners(conn, org_id) -> i64`.
- `org_members(pool, org_id) -> Vec<MemberRow { user: UserRow, role: OrgRole, joined_at }>` ordered by email.
- `project_override(pool, project_id, user_id) -> Option<ProjectOverride>`; `set_project_override(conn, project_id, user_id, ProjectOverride)` (upsert); `remove_project_override(conn, project_id, user_id)`.
- `project_overrides(pool, project_id) -> Vec<(Uuid, ProjectOverride)>`.

### Tests (`meta/*.rs`, against a temp `MetaDb`)

- `normalize_email`: `"  Ada@Example.COM "` → `ada@example.com`; NFC: `"e\u{301}@example.com"` equals `"é@example.com"` after normalization; invalid: no `@`, two `@`, empty local, `a@b`, `a@.com`, spaces inside, 255 bytes; the `EmailError` message does not contain the input.
- Two users differing only by case → second insert `Conflict`.
- Slugs: valid, invalid (`a`, `-a`, `a-`, `a--b`, `A`, 49 chars), reserved; `slug_from_name("Acme, Inc.")` = `acme-inc`; `"!!!"` → `org`; `"Admin"` → `admin-1`; derived conflicts get `-2`, `-3`.
- Project slugs unique per org only (same slug in two orgs ok).
- `remove_org_member` deletes that org's project overrides for the user and keeps overrides in other orgs.
- Foreign keys: deleting a user (raw SQL in the test) cascades memberships and overrides.
- `orgs_of_user` returns the role; `org_members` ordering; `list_users` paging with `after_email`.
- A stored role `"superuser"` → `MetaError::Corrupt` on read.

### Report

Anything in spec 04's DDL that the functions could not express.

---

## P7 — Secrets and passwords

**Goal:** generation, hashing and parsing of every token kind, and the password policy and hasher of D143.

**Edit only:** `crates/server/src/auth/{secret.rs,password.rs,common-passwords.txt}` (new), `crates/server/src/auth/mod.rs`, `crates/server/Cargo.toml`, root `Cargo.toml` (`argon2`, `rand`, `subtle`; `sha2`, `base64` exist).

**Commit:** `server: add token generation and password hashing`

### `auth/secret.rs`

- The product name appears once: build the prefixes from a single `const` (use the existing name constant in `llmobs_core::names` if there is one that is just `"llmobs"`; otherwise `const NAME: &str = "llmobs";` here with a comment pointing at D46).

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyKind { Secret, Public, Personal }   // sk-llmobs- (32), pk-llmobs- (24), pat-llmobs- (32)

pub(crate) struct NewKey { pub(crate) plaintext: String, pub(crate) hash: String, pub(crate) prefix: String }
pub(crate) fn new_key(kind: KeyKind) -> NewKey;
pub(crate) fn parse_key(bearer: &str) -> Option<KeyKind>;   // exact prefix, exact length, base62 tail
pub(crate) fn display_prefix(plaintext: &str) -> String;    // type prefix + first 6 tail chars (D140)

pub(crate) struct OpaqueToken { pub(crate) plaintext: String, pub(crate) hash: String }
pub(crate) fn new_opaque_token() -> OpaqueToken;           // 32 random bytes, base64url without padding (43 chars)
pub(crate) fn is_opaque_token(s: &str) -> bool;            // 43 chars of the base64url alphabet

pub(crate) fn hash_token(plaintext: &str) -> String;       // SHA-256, lowercase hex
pub(crate) fn constant_time_eq(a: &str, b: &str) -> bool;  // subtle
```

- Randomness from the OS CSPRNG (`rand` 0.10: check the name of the OS RNG type in that release, e.g. `rand::rngs::OsRng`/`SysRng`, and whether drawing can fail; an RNG failure is unrecoverable: document why `expect` is acceptable there, or return an error up to the caller — prefer returning `AuthError::Rng`).
- base62 tail: draw bytes, reject bytes ≥ 248, map `b % 62` onto `0-9A-Za-z`. No modulo bias.
- `parse_key` never allocates for a wrong prefix and runs in time independent of the tail's content (it only checks length and alphabet; the DB lookup is by hash).

### `auth/password.rs`

```rust
pub(crate) enum PolicyError { TooShort, TooLong, EqualsEmail, TooCommon }
pub(crate) fn check_policy(password: &str, normalized_email: &str) -> Result<(), PolicyError>;
pub(crate) struct Hasher { permits: Arc<Semaphore>, dummy: String }
impl Hasher {
    pub(crate) async fn new() -> Result<Self, AuthError>;                      // computes `dummy` once (spawn_blocking)
    pub(crate) async fn hash(&self, password: &str) -> Result<String, AuthError>;
    pub(crate) async fn verify(&self, password: &str, phc: Option<&str>) -> Result<Verified, AuthError>;
}
pub(crate) struct Verified { pub(crate) ok: bool, pub(crate) needs_rehash: bool }
```

- `check_policy` works on the NFC form: length in `chars()` 10–128; equal to the email (case-insensitive after NFC) → `EqualsEmail`; NFC-lowercased password in the common list → `TooCommon`.
- `hash` and `verify` NFC-normalize first, acquire a semaphore permit (`max(2, available_parallelism / 2)`), run argon2 in `spawn_blocking`. Params: argon2id, v0x13, m = 19 456 KiB, t = 2, p = 1, 16-byte random salt, PHC string output.
- `verify(…, None)` verifies against `dummy` and returns `ok: false` (D143: unknown emails and password-less users cost the same).
- `needs_rehash`: the PHC parameters are weaker than the current ones, or the algorithm is not argon2id.
- A stored PHC string that does not parse → `ok: false` and an `error` log with the user id at the call site (the hasher itself never logs input).

### `common-passwords.txt`

Download SecLists `Passwords/Common-Credentials/10k-most-common.txt` (MIT) from `https://github.com/danielmiessler/SecLists`, keep the entries of 10 or more characters, lowercase, deduplicate, sort. First lines: `# Source: <raw URL> at commit <sha>, MIT License, entries of 10+ characters, lowercased.` Lines starting with `#` are comments. Load it with `include_str!` into a `LazyLock<HashSet<&'static str>>`. If you cannot download the file, stop and report; do not invent a list.

### Tests

- `new_key` for each kind: prefix, total length, alphabet; 1 000 keys are distinct; `parse_key` accepts them and rejects a wrong prefix, one character short or long, a `-` in the tail, `pat-llmobs-` with a 24-char tail.
- `display_prefix("sk-llmobs-a1B2c3…")` = `sk-llmobs-a1B2c3`.
- `new_opaque_token`: 43 chars; `hash_token` is the known SHA-256 hex of a fixed input.
- Base62 distribution sanity: over 62 000 draws, every symbol appears (not a statistical test, just no symbol missing).
- Policy: 9 chars fails, 10 passes; 128 ok, 129 fails; a 10-char password made of 10 multi-byte characters passes; equals email (different case) fails; a list entry in uppercase fails.
- Hasher: round trip; wrong password `ok: false`; `verify(…, None)` takes the same order of time as a real verify (assert it ran argon2: measure that it takes at least half the time of a real verify — keep the assertion loose, or test that `dummy` is a valid argon2id PHC with current params, which is deterministic; prefer the deterministic test); a hash made with `t = 1` → `needs_rehash`; NFC: hashing `"e\u{301}…"` verifies with `"é…"`.
- Semaphore: 8 concurrent hashes with 2 permits all succeed.

### Report

The SecLists commit hash, the number of entries kept, and the `rand` API used.

---

## P8 — Rate limits and lockout

**Goal:** the in-memory limiters and lockout table of D143, bounded in memory.

**Edit only:** `crates/server/src/auth/limits.rs` (new), `crates/server/src/auth/mod.rs`, `crates/server/src/http/mod.rs` (add `limits: Limits` to `AppInner`), `crates/server/src/serve.rs` (prune task), `crates/server/src/test_support.rs`.

**Commit:** `server: add login rate limits and lockout`

### Types

```rust
pub(crate) struct Bucket { pub(crate) capacity: u32, pub(crate) per: Duration }   // `capacity` tokens refill evenly over `per`
pub(crate) struct KeyedLimiter<K> { bucket: Bucket, max_keys: usize, state: Mutex<HashMap<K, (f64 /*tokens*/, i64 /*updated_ns*/)>> }
impl<K: Hash + Eq + Clone> KeyedLimiter<K> {
    pub(crate) fn check(&self, key: &K, now: i64) -> Result<(), RetryAfter>;   // takes one token
    pub(crate) fn prune(&self, now: i64);                                      // drops keys whose bucket is full again
}
pub(crate) struct RetryAfter(pub(crate) Duration);

pub(crate) struct Lockout { threshold: u32, duration: Duration, max_keys: usize, state: Mutex<HashMap<String, (u32, Option<i64>)>> }
impl Lockout {
    pub(crate) fn is_locked(&self, email: &str, now: i64) -> bool;
    pub(crate) fn record_failure(&self, email: &str, now: i64);   // threshold reached → locked_until = now + duration, count reset
    pub(crate) fn clear(&self, email: &str);
}

pub(crate) struct Limits {
    pub(crate) login: KeyedLimiter<(IpAddr, String)>,   // 10 / minute
    pub(crate) ip_forms: KeyedLimiter<IpAddr>,          // 5 / minute: signup, setup, forgot, resend, invite accept
    pub(crate) email_mail: KeyedLimiter<String>,        // 3 / hour: forgot, resend
    pub(crate) lockout: Lockout,                        // 20 failures → 15 minutes
}
impl Limits { pub(crate) fn new() -> Self; pub(crate) fn prune(&self, now: i64); }
```

- `max_keys` = 100 000 per map. When an insert would exceed it, prune first; if still full, drop the entry with the oldest `updated` (an O(n) scan is fine at this size and rare). Document that a flood of distinct keys can evict a real user's bucket, which only gives that user fresh tokens: it never blocks anyone.
- The mutex is a `std::sync::Mutex` (no `.await` while held); a poisoned mutex is recovered with `into_inner` (the state is plain counters).
- `serve` runs `limits.prune(now)` every 60 s.
- `RetryAfter` rounds up to whole seconds, at least 1.

### Tests

- 10 checks pass, the 11th fails with `RetryAfter` ≈ 6 s; after `clock.advance(6 s)` one more passes.
- Different keys are independent.
- Lockout: 19 failures not locked; the 20th locks; locked for 15 minutes; unlocked at 15 min + 1 ns; `clear` unlocks; a success path (`clear`) resets the count so 19 more failures do not lock.
- Bound: with `max_keys = 10` (constructor parameter for tests), inserting 11 keys keeps 10 and the evicted key is the oldest.
- `prune` removes full buckets only.

### Report

Nothing special beyond the standard report.
