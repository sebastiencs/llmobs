# Phase 0 · Step 1 — Workspace skeleton

Implementation plan, split into small sub-phases. Each sub-phase is meant to be done by one agent in one go, ending in a green build and one commit.

**Read first:** `docs/mvp-decisions.md` (D1–D58), `docs/spec/README.md` (architecture, crate layout), `.cursor/rules/*.mdc` (no compatibility layers, no unnecessary abstractions).

**Scope of step 1:** the repository layout, the Rust workspace with its 5 crates, the toolchain, lints, profiles, the dependency pins, the `llmobs` binary (prints its version, nothing else), cargo-deny, the justfile, the directory layout outside Rust, and the agent instructions.

**Not in step 1** (these are later Phase 0 steps):
- Shared types and Arrow schemas in `core` (step 2).
- The fixture format (step 3).
- The OpenAPI skeleton (step 4).
- The `web/` and `sdks/` scaffolds (step 5).
- GitHub Actions CI (step 6).

Do not add placeholder modules, stub types or empty traits for those.

## Target layout after step 1

```
.
├── AGENTS.md                 agent instructions (P10)
├── CLAUDE.md -> AGENTS.md    symlink (P10)
├── Cargo.toml                virtual workspace manifest (P3)
├── Cargo.lock
├── deny.toml                 (P7)
├── justfile                  (P8)
├── rust-toolchain.toml       (P2)
├── rustfmt.toml              (P2)
├── clippy.toml               (P2)
├── .editorconfig             (P2)
├── .gitignore                (P1)
├── .cursor/rules/            unchanged
├── crates/
│   ├── core/                 package llmobs-core    (lib)
│   ├── otel/                 package llmobs-otel    (lib)
│   ├── storage/              package llmobs-storage (lib)
│   ├── server/               package llmobs-server  (lib)
│   └── cli/                  package llmobs-cli     (bin "llmobs")
├── web/README.md             (P9)
├── sdks/python/README.md     (P9)
├── sdks/typescript/README.md (P9)
├── fixtures/otlp/README.md   (P9)
├── tools/README.md           (P9)
└── docs/                     unchanged (plus this plan)
```

## Crate dependency graph (enforced from P4)

```
llmobs-core  ◄── llmobs-otel
     ▲       ◄── llmobs-storage
     │                 ▲
     └── llmobs-server ┘ ──► llmobs-otel
               ▲
         llmobs-cli
```

- `otel` and `storage` **must not** depend on each other. They are built in parallel by different sessions; the server glues them together (normalize → WAL).
- Only `server` and `cli` may depend on `server`-level crates.
- `core` depends on no other workspace crate.

---

## P1 — Clean slate and `.gitignore`

**Goal:** remove the `cargo new` skeleton and make sure no build artifacts are tracked.

**Do:**
1. Delete `src/main.rs` and the `src/` directory.
2. Delete the existing root `Cargo.toml` and `Cargo.lock`; P3 recreates them.
3. Check `git ls-files target` and run `git rm -r --cached target` if anything is tracked.
4. Rewrite `.gitignore`:
   ```
   /target
   /llmobs-data/
   node_modules/
   dist/
   .venv/
   __pycache__/
   *.pyc
   .pytest_cache/
   .ruff_cache/
   coverage/
   .env
   .env.*
   !.env.example
   .DS_Store
   *.swp
   ```

**Acceptance:**
- `git status` shows only the deletions and `.gitignore` changes.
- `git ls-files | grep -E '^(target|src)/'` prints nothing.

**Commit:** `Remove cargo-new skeleton`

---

## P2 — Toolchain and formatting configuration

**Goal:** a reproducible toolchain and shared formatting rules.

**Do:**
1. `rust-toolchain.toml`:
   ```toml
   [toolchain]
   channel = "<latest stable, exact version, e.g. 1.9x.y>"
   components = ["rustfmt", "clippy"]
   profile = "minimal"
   ```
   Use the latest stable release on the day of implementation, pinned exactly (`rustup check` / https://releases.rs).
2. `rustfmt.toml`. Stable options only, no nightly-only keys:
   ```toml
   edition = "2024"
   style_edition = "2024"
   use_field_init_shorthand = true
   use_try_shorthand = true
   ```
3. `clippy.toml`:
   ```toml
   allow-unwrap-in-tests = true
   allow-expect-in-tests = true
   allow-dbg-in-tests = true
   allow-print-in-tests = true
   ```
4. `.editorconfig`:
   - root
   - utf-8, lf, final newline, trim trailing whitespace
   - 4 spaces for `*.rs`, `*.py`, `*.toml`
   - 2 spaces for `*.{ts,tsx,js,json,yaml,yml,css,html}`
   - Markdown: `trim_trailing_whitespace = false`
   - `justfile`: 4 spaces

**Acceptance:** `rustup show` in the repo picks the pinned toolchain.

**Commit:** `Pin Rust toolchain and formatting config`

---

## P3 — Virtual workspace manifest

**Goal:** the root `Cargo.toml` defines members, shared package metadata, lints, profiles and **version pins** for the dependencies that must be identical across crates.

**Do:** create the root `Cargo.toml`:

```toml
[workspace]
resolver = "3"
members = ["crates/*"]

[workspace.package]
version = "0.0.0"
edition = "2024"
rust-version = "<same minor as rust-toolchain.toml>"
publish = false
repository = "<GitHub URL once created; omit until then>"
# license: intentionally omitted until D9 is decided

[workspace.lints.rust]
unsafe_code = "forbid"
missing_debug_implementations = "warn"
unreachable_pub = "warn"
unused_qualifications = "warn"

[workspace.lints.clippy]
all = { level = "warn", priority = -1 }
dbg_macro = "warn"
todo = "warn"
unimplemented = "warn"
unwrap_used = "warn"
expect_used = "warn"
print_stdout = "warn"
print_stderr = "warn"
large_futures = "warn"
needless_pass_by_value = "warn"
redundant_clone = "warn"
semicolon_if_nothing_returned = "warn"
inefficient_to_string = "warn"

[workspace.dependencies]
# workspace crates
llmobs-core = { path = "crates/core" }
llmobs-otel = { path = "crates/otel" }
llmobs-storage = { path = "crates/storage" }
llmobs-server = { path = "crates/server" }

# --- pinned families (must stay mutually compatible) ---
datafusion = { version = "<latest>", default-features = false, features = [...] }  # see note
arrow = "<version used by that datafusion>"
parquet = { version = "<version used by that datafusion>", default-features = false, features = [...] }
object_store = "<version used by that datafusion>"

# --- runtime basics ---
tokio = { version = "<latest 1.x>", features = ["rt-multi-thread", "macros", "sync", "time", "signal", "fs", "io-util"] }
serde = { version = "<latest 1.x>", features = ["derive"] }
serde_json = "<latest 1.x>"
thiserror = "<latest 2.x>"
anyhow = "<latest 1.x>"
tracing = "<latest>"
tracing-subscriber = { version = "<latest>", features = ["env-filter", "json", "fmt"] }
clap = { version = "<latest 4.x>", features = ["derive", "env"] }
mimalloc = { version = "<latest>", default-features = false }
uuid = { version = "<latest 1.x>", features = ["v7", "serde"] }
chrono = { version = "<latest 0.4.x>", default-features = false, features = ["std", "clock", "serde"] }

[profile.dev]
debug = "line-tables-only"

[profile.dev.package."*"]
opt-level = 2          # DataFusion/Arrow-heavy tests are unusably slow at opt-level 0

[profile.release]
lto = "thin"
codegen-units = 1
debug = "line-tables-only"  # symbolized profiles and backtraces in production

[profile.bench]
inherits = "release"
debug = true
```

**Notes for the implementer:**
- **Version resolution:**
  - Pick the latest `datafusion` first.
  - Set `arrow`, `parquet` and `object_store` to exactly the versions that DataFusion release depends on; check with `cargo tree -i arrow` once a crate uses them.
  - Every other dependency is pinned at its latest release on the day of implementation, as a caret requirement (`"x.y"`).
  - Use `cargo search` / `cargo info` to find versions. Do not guess.
- **Features:**
  - `datafusion`: keep the default features minus anything pulling in compression codecs we do not use. Ensure `parquet` and `nested_expressions` are available.
  - `parquet`: enable `arrow`, `async`, `object_store`, `zstd`, `snap`, `lz4`, `flate2`. Readers must handle files written by other tools.
  - If disabling DataFusion's default features breaks compilation, keep the defaults and leave a comment saying why.
- **Pins only:** these entries pin versions; a crate pulls a dependency only when it uses it (P4/P5). Later steps add new dependencies **here** first, then reference them with `{ workspace = true }`.
- **Clippy level:** the workspace lints stay at `warn`; CI (step 6) and `just lint` run clippy with `-D warnings`. `clippy::pedantic` is intentionally not enabled.

**Acceptance:**
- `cargo metadata --format-version 1 >/dev/null` succeeds. It may complain that there are no members until P4; in that case, merge P3 and P4 into one commit.

**Commit:** `Add workspace manifest with lints, profiles and dependency pins`

---

## P4 — The five crates

**Goal:** five compiling crates with the correct dependency graph and no code beyond a crate-level doc comment.

**Do:**
1. Create `crates/{core,otel,storage,server}` as libraries and `crates/cli` as a binary package.
2. Each `Cargo.toml`:
   ```toml
   [package]
   name = "llmobs-<x>"
   description = "<one line, see below>"
   version.workspace = true
   edition.workspace = true
   rust-version.workspace = true
   publish.workspace = true

   [lints]
   workspace = true

   [dependencies]
   # only workspace crates per the graph, e.g. for otel:
   llmobs-core.workspace = true
   ```
3. Dependencies per crate:
   - `core`: none.
   - `otel`: `llmobs-core`.
   - `storage`: `llmobs-core`.
   - `server`: `llmobs-core`, `llmobs-otel`, `llmobs-storage`.
   - `cli`: `llmobs-core`, `llmobs-server`.
4. `src/lib.rs` of each library: only a `//!` crate doc. Two to five lines giving the crate's responsibility, with a pointer to the spec section:
   - `core`: shared types, Arrow schemas, IDs, time, config (spec 01, README §conventions).
   - `otel`: OTLP decoding, convention detection and normalization, media extraction, truncation, cost computation (spec 02).
   - `storage`: WAL, memtable, Parquet files, catalog, compaction, deletion, retention, media GC, DataFusion table providers, filter DSL and SQL sandbox (specs 03, 05).
   - `server`: HTTP/gRPC API, auth, metadata DB, evaluation, alerts, email, MCP (specs 04, 06–09).
   - `cli`: the `llmobs` binary (spec 09 §CLI).
5. `crates/cli/Cargo.toml` adds:
   ```toml
   [[bin]]
   name = "llmobs"
   path = "src/main.rs"
   ```

**Rules:**
- No modules, types, traits or functions in this sub-phase.
- An empty `lib.rs` with only the doc comment is correct.

**Acceptance:**
- `cargo build --workspace` succeeds.
- `cargo clippy --workspace --all-targets -- -D warnings` is clean.
- `cargo tree -p llmobs-otel` does not contain `llmobs-storage`, and `cargo tree -p llmobs-storage` does not contain `llmobs-otel`.

**Commit:** `Add the five workspace crates`

---

## P5 — Minimal `llmobs` binary

**Goal:** the binary runs, reports its version and uses mimalloc. Nothing else.

**Do:**
1. `crates/cli` dependencies: `clap`, `mimalloc` (workspace).
2. `src/main.rs`:
   - `#[global_allocator] static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;`
   - A clap `Parser` with `#[command(name = "llmobs", version, about = "LLM observability server")]` and a subcommand enum containing only `Version`. Running `llmobs version` and `llmobs --version` both print `llmobs <version>`.
   - Printing goes through `std::io::Write` on a locked stdout. Alternatively, put `#[allow(clippy::print_stdout)]` on the single function, with a comment explaining it is the CLI's output.
3. Do **not** add `serve`, `admin` or other subcommands yet. Later steps add them together with their implementation.

**Acceptance:**
- `cargo run -p llmobs-cli -- --version` prints `llmobs 0.0.0`.
- `cargo run -p llmobs-cli -- version` prints the same.
- Clippy is clean with `-D warnings`.

**Commit:** `Add minimal llmobs binary`

---

## P6 — Workspace-level `cargo test` baseline

**Goal:** prove the test harness works so later sessions only add tests.

**Do:**
- In `crates/cli`, add one integration test, `tests/version.rs`, that runs the built binary with `--version` and asserts the output starts with `llmobs `.
- Use `std::process::Command` with `env!("CARGO_BIN_EXE_llmobs")`. Do not add a test dependency for this.

**Acceptance:** `cargo test --workspace` passes.

**Commit:** `Add binary smoke test`

---

## P7 — cargo-deny configuration

**Goal:** supply-chain checks. **No license check** until the license is decided (D57).

**Do:** create `deny.toml` (cargo-deny ≥ 0.16 schema):

```toml
[graph]
all-features = true

[advisories]
version = 2
yanked = "deny"
ignore = []

[licenses]
# D57: license policy deferred until our own license is chosen.
# Do not remove this section; replace `allow` with the real policy then.
version = 2
allow = []
confidence-threshold = 0.8
# The `licenses` check is NOT run (see justfile `deny` recipe), so an empty allow-list is fine for now.

[bans]
multiple-versions = "warn"
wildcards = "deny"
allow-wildcard-paths = true   # path deps between workspace crates
highlight = "all"

[sources]
unknown-registry = "deny"
unknown-git = "deny"
allow-registry = ["https://github.com/rust-lang/crates.io-index"]
allow-git = []
```

**Acceptance:** `cargo deny check advisories bans sources` passes. Duplicate-version warnings are acceptable.

**Commit:** `Add cargo-deny config (licenses deferred)`

---

## P8 — `justfile`

**Goal:** one entry point for common commands. P9 adds no web/SDK recipes; the step that scaffolds them adds them.

**Do:** create `justfile`:

```just
# List recipes
default:
    @just --list

# Format all Rust code
fmt:
    cargo fmt --all

# Check formatting
fmt-check:
    cargo fmt --all -- --check

# Lint (warnings are errors)
lint:
    cargo clippy --workspace --all-targets --all-features -- -D warnings

# Run all Rust tests
test:
    cargo test --workspace --all-features

# Supply-chain checks (licenses deferred, D57)
deny:
    cargo deny check advisories bans sources

# Debug build
build:
    cargo build --workspace

# Release build of the binary
build-release:
    cargo build --release -p llmobs-cli

# Run the binary with arguments, e.g. `just run --version`
run *args:
    cargo run -p llmobs-cli -- {{args}}

# Everything CI runs for Rust
check: fmt-check lint test deny
```

**Acceptance:**
- `just check` passes. This requires `cargo-deny` to be installed (`cargo install cargo-deny --locked`); AGENTS.md documents it.

**Commit:** `Add justfile`

---

## P9 — Layout outside Rust

**Goal:** the non-Rust directories exist, with a short README saying what goes there and which step fills them. No scaffolding.

**Do:** create these files, each 3–8 lines:

| File | Content |
|---|---|
| `web/README.md` | Web UI: Vite + React + TypeScript, TanStack Router/Query/Table, shadcn/ui + Tailwind, ECharts, pnpm. Built output is embedded into the binary with `memory-serve`. Spec: `docs/spec/11-ui.md`. Scaffolded in Phase 0 step 5. |
| `sdks/python/README.md` | Python SDK `llmobs` (uv project, MIT license independent of the server, D… see spec 10). Thin layer over OpenTelemetry. Spec: `docs/spec/10-sdk.md`. Scaffolded in Phase 0 step 5. |
| `sdks/typescript/README.md` | TypeScript SDK `llmobs` (pnpm package). Same spec. Scaffolded in Phase 0 step 5. |
| `fixtures/otlp/README.md` | OTLP fixtures shared by the Rust golden tests (`llmobs-otel`) and the SDK integration tests: one directory per convention, a captured payload plus the expected normalized spans. Format defined in Phase 0 step 3. Spec: `docs/spec/02-ingestion.md` §Testing. |
| `tools/README.md` | Developer tools that are not part of the product, e.g. `tools/capture/` (fixture capture from real instrumentations, Wave 1) and benchmark scripts. |

**Acceptance:** the files exist; nothing else is added under these directories.

**Commit:** `Add non-Rust directory layout`

---

## P10 — Agent instructions (`AGENTS.md` + `CLAUDE.md`)

**Goal:** any agent working on the repo (Claude, Cursor, Codex, …) gets the rules in one file.

**Do:**
1. Create `AGENTS.md` with these sections, concise (aim for under 150 lines):
   - **Project:** one paragraph. llmobs is the working name; it is a Rust LLM observability platform (single binary, DataFusion + Parquet, SQLite).
   - **Source of truth:**
     - `docs/mvp-decisions.md` (decisions D1…) and `docs/spec/` (numbered specs) define what to build.
     - `docs/research/competitors.md` explains why.
     - If code must deviate from the spec, update the spec (and add a decision to the log) in the same change.
   - **Rules:**
     - Copy the substance of `.cursor/rules/no-compat-no-abstractions.mdc`: no compatibility layers, no single-implementation traits, no pass-through wrappers.
     - Reference implementations are in `/home/sebastien/github/{lmnr,phoenix,langfuse,langwatch,helicone,opik,mlflow}`. Read them to compare designs; never copy code from them.
     - All dependency versions are declared in the root `[workspace.dependencies]`; crates use `{ workspace = true }`.
     - Error handling: each library crate defines its own error enum(s) with `thiserror`; `anyhow` is only allowed in `llmobs-cli`.
     - No `unwrap`/`expect` outside tests unless the invariant is documented in a comment next to it.
     - No `unsafe` (forbidden workspace-wide).
     - Time is `i64` nanoseconds since the Unix epoch (UTC) at the storage boundary; IDs follow spec README §conventions.
     - Logs never contain payload content (prompts, outputs, secrets).
     - Every behaviour change comes with tests.
   - **Layout and ownership:**
     - The crate graph from this plan (otel and storage never depend on each other).
     - Who owns which directory during parallel work: a session only edits the directories its brief names.
     - Changes to `llmobs-core` shared types go through the contract owner, the coordinating session.
   - **Commands:** `just check`, `just fmt`, `just lint`, `just test`, `just deny`, `just run …`. Required tools: rustup (the toolchain is pinned), `just`, `cargo-deny`. Later: `pnpm`, `uv`.
   - **Naming:** crate prefix `llmobs-`, binary `llmobs`, env prefix `LLMOBS_`, attribute namespace `llmobs.*`. All of these will be renamed mechanically when the product gets its real name (D46).
   - **Commits:** small, imperative subject. Run `just check` before committing.
2. Create the symlink `ln -s AGENTS.md CLAUDE.md`.
3. Leave `.cursor/rules/` unchanged.

**Acceptance:**
- `readlink CLAUDE.md` prints `AGENTS.md`.
- `AGENTS.md` mentions every rule above.

**Commit:** `Add agent instructions`

---

## P11 — Final verification

**Do (no new files):**
1. From a clean checkout (`git clean -xdf` in a scratch clone), run `just check`.
2. Confirm the layout matches the tree at the top of this document.
3. Confirm `cargo tree -p llmobs-otel -e normal` and `cargo tree -p llmobs-storage -e normal` show only `llmobs-core` as a workspace dependency.
4. Confirm `cargo run -p llmobs-cli --release -- --version` works.

**Report back:**
- The exact pinned versions chosen: Rust toolchain, datafusion, arrow, parquet, object_store, tokio.
- Any deviation from this plan, with the reason.
