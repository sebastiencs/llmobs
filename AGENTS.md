# AGENTS.md

Instructions for any agent (Claude, Cursor, Codex, …) working in this repository. `CLAUDE.md` is a symlink to this file.

## Project

**llmobs** (working name) is an open-source LLM observability and evaluation platform, written in Rust. It competes with Langfuse, Phoenix, Opik, LangWatch, Helicone, Laminar and MLflow GenAI.

What sets it apart is that it runs as **one binary on one machine**, while staying fast at volume:
- OTLP ingestion;
- a custom storage engine (WAL → memtable → Parquet files, queried with Apache DataFusion);
- a SQLite metadata DB;
- a React UI embedded in the binary;
- Python and TypeScript SDKs built on OpenTelemetry.

The project is unreleased. There are no users, no stored data and no API clients to keep compatible.

## Source of truth

Read these before writing code, and follow them:

| Document | What it is |
|---|---|
| `docs/mvp-decisions.md` | Every product/technical decision (D1, D2, …) and the MVP scope summary |
| `docs/spec/README.md` → `docs/spec/01…12-*.md` | The technical specification: data model, ingestion, storage, metadata DB, query, evaluation, alerts, auth, API, SDK, UI, operations |
| `docs/plans/` | Step-by-step implementation plans; do the sub-phase you were given, nothing more |
| `docs/research/competitors.md` | Why the decisions were made (competitor features and code-level findings) |

If the code has to deviate from the spec, update the spec **in the same change**, and add a decision to `docs/mvp-decisions.md` if the change is not a detail. If the spec is ambiguous or contradicts itself, stop and ask instead of guessing.

## Rules

**No compatibility layers.** When a type, function, format, config key or protocol field changes, update every caller and delete the old path in the same change. No shims, aliases, `#[deprecated]` wrappers, versioned codecs, feature flags or dual read/write paths.

**No unnecessary abstractions.**
- Prefer concrete types and plain functions.
- A trait needs at least two implementations (test doubles don't count unless the test truly can't use the real type).
- No wrapper whose only job is to forward to the type it holds.
- Add a generic, newtype or module split only when it removes real duplication or enforces a real invariant.

**No placeholders.** Don't add stub modules, empty types, `todo!()` functions or CLI subcommands for future work. Code is added together with its implementation and tests.

**Dependencies.**
- Every version is declared once in the root `Cargo.toml` `[workspace.dependencies]`; crates use `dep = { workspace = true }`.
- `datafusion`, `arrow`, `parquet` and `object_store` are a pinned family: never bump one without the others. DataFusion picks the majors. Dependabot ignores arrow-side majors; a DataFusion major PR is completed by hand with the arrow, parquet and object_store versions that release requires. `cargo deny` fails on two versions of a family crate.
- Look up real versions (`cargo search`, `cargo info`); never guess one.
- JS packages use exact versions and respect `minimumReleaseAge`; Python dev tools are pinned by `uv.lock`.
- `typescript` stays on 5.x while `openapi-typescript` requires it (`.github/dependabot.yml` ignores TypeScript majors).

**Errors.** Each library crate defines its own error enum(s) with `thiserror`. `anyhow` is only allowed in `llmobs-cli`.

**Safety and robustness.**
- `unsafe` is forbidden workspace-wide.
- No `unwrap()` / `expect()` outside tests, unless a comment next to it states the invariant that makes it infallible.
- Never panic on input coming from the network or from disk; return an error.

**Data conventions** (spec README § Cross-cutting conventions):
- Time is `i64` nanoseconds since the Unix epoch, UTC.
- Entity IDs are UUIDv7.
- Trace IDs are 16 bytes and span IDs 8 bytes, written as lowercase hex in JSON.
- Money is USD as `f64`.

**Privacy.** Logs and error messages never contain payload content (prompts, completions, attribute values), secrets, API keys or email addresses. Log IDs and sizes instead.

**Tests.**
- Every behaviour change comes with tests.
- Normalization uses golden fixtures in `fixtures/otlp/`.
- Storage needs crash-recovery and concurrency tests.
- `LLMOBS_CRASH_ITERATIONS=N just test` runs the storage crash harness longer (default 25).
- `LLMOBS_TEST_S3_ENDPOINT` (with `LLMOBS_TEST_S3_BUCKET` and the `AWS_*` credentials) runs the storage S3 test against a real S3-compatible server; `crates/storage/tests/s3.rs` shows how to start one (D119).
- Tests cover the current behaviour only.
- UI tests use the MSW mocks from `fixtures/api/`; a page test overrides an endpoint with `server.use` instead of mocking modules.
- Captured OTLP fixtures are recorded with `just capture` (tools/capture) and never edited by hand; `just check-capture` before committing one.

**Reference implementations.** The competitors' source code is checked out in `/home/sebastien/github/{lmnr,phoenix,langfuse,langwatch,helicone,opik,mlflow}`. Read it to compare designs and learn from their mistakes. Never copy code from it.

## Layout

```
crates/
  core/      llmobs-core     shared contracts: IDs, enums, names, messages, Span/Score,
                             Arrow schemas + batch conversion, OpenAPI schemas,
                             no config (D59)                                          (spec 01)
  otel/      llmobs-otel     OTLP decode, normalization, media, truncation, cost      (spec 02)
  eval/      llmobs-eval     evaluator execution: checks, mapping, templates, judges,
                             webhooks, judge traces                                   (spec 06)
  storage/   llmobs-storage  WAL, memtable, Parquet, catalog (catalog.db), compaction,
                             deletion, DataFusion providers, filter DSL, SQL sandbox  (specs 03, 05)
  server/    llmobs-server   API, auth, metadata DB, evaluation, alerts, email, MCP   (specs 04, 06–09)
  cli/       llmobs-cli      the `llmobs` binary                                       (spec 09 § CLI)
web/                         React UI (Vite, TanStack, shadcn/ui, pnpm)               (spec 11)
sdks/python/                 Python SDK (uv)                                           (spec 10)
sdks/typescript/             TypeScript SDK (pnpm)                                     (spec 10)
fixtures/otlp/               OTLP fixtures shared by Rust golden tests and SDK tests  (spec 02 § Testing)
fixtures/api/                example API responses (UI mocks), checked against the server types (spec 09)
tools/                       dev tools (fixture capture, benchmarks); not shipped
tools/capture/               fixture capture: mock provider, OTLP receiver, scenarios (B2)
docs/                        decisions, specs, plans, research
```

**Crate dependency graph (enforced):**
- `core` depends on no workspace crate.
- `otel` and `storage` depend only on `core`. **They never depend on each other**, so they can be built in parallel; `server` connects them.
- `eval` depends on `core` and `otel`.
- `server` depends on `core`, `otel`, `storage` and `eval`; `cli` depends on `server` and `core`.

## Parallel work and ownership

Several sessions may work at the same time, each in its own git worktree and branch.

- **Only edit the directories your brief names.** If you need a change elsewhere, say so in your report instead of making it.
- **`llmobs-core` is a shared contract.** Don't change its public types unless your brief allows it. Request the change, and the coordinating session makes it.
- The same goes for the root `Cargo.toml`: add a dependency only when your work needs it. Never change existing pins unless your brief says so.
- Keep changes on your own branch; don't merge other branches into it.

## Commands

Run everything through `just`:

| Command | What it does |
|---|---|
| `just check` | Rust, web, Python SDK, and TypeScript SDK (D79); **must pass before every commit** |
| `just check-rust` | format check, clippy (`-D warnings`), tests, cargo-deny |
| `just check-web` | web lint, types, tests, production build |
| `just check-py` | Python SDK format, lint, types, tests |
| `just check-ts` | TypeScript SDK lint, types, tests, package exports |
| `just fmt` | format Rust, the web and TypeScript SDK (Biome), and the Python SDK (ruff) |
| `just lint` | clippy, warnings as errors |
| `just test` | all Rust tests |
| `just eval-live` | opt-in live provider tests; ignored by `just check` (D167) |
| `just deny` | supply-chain checks (advisories, bans, sources; licenses deferred, D57) |
| `just deny-advisories` | RustSec advisories only |
| `just assert-clean` | fail if the working tree changed; CI runs it after each job |
| `just ts-smoke <tarball>` | load a packed TS SDK from ESM and CommonJS |
| `just check-workflows` | lint and audit GitHub Actions workflows (actionlint, zizmor) |
| `just openapi` | regenerate `crates/server/openapi.json` and `web/src/api/schema.gen.ts` |
| `just web-dev` | web dev server with the API mocked from `fixtures/api` |
| `just web-dev-server` | web dev server proxied at the real API |
| `just run <args>` | run the `llmobs` binary |
| `just capture <group>/<case>` | record an OTLP fixture from a scenario, e.g. `just capture openinference/openai-chat-tool` |
| `just capture-all` | recapture every scenario |
| `just check-capture` | lint and type-check the capture scenarios, then recapture every scenario and compare (timing excluded) |

Required tools:
- `rustup` (the toolchain is pinned in `rust-toolchain.toml`)
- `just`
- `cargo-deny` (`cargo install cargo-deny --locked`)
- Node (version in `.node-version`)
- `pnpm` (version in the root `package.json` `packageManager`)
- `uv`
- `actionlint` and `zizmor` (only when editing `.github/`)

## CI

`.github/workflows/ci.yml` runs on every pull request and every push to `main`. `ci-ok` is the required status.

CI runs the `just` recipes from the table. A branch that passes `just check` locally and leaves a clean tree passes CI, except for the runtime matrix (Python 3.15, Node 22.12).

Actions are pinned by commit SHA with the tag in a comment; Dependabot updates them. Never use a tag or branch reference.

Workflow `run:` steps call `just` recipes. New checks go into the `justfile` first.

Updated by hand: `rust-toolchain.toml`, `.node-version`, `packageManager`.

## Naming

Crates are prefixed `llmobs-`. The binary is `llmobs`, env vars use the `LLMOBS_` prefix, and span attributes use the `llmobs.*` namespace. API keys are `sk-llmobs-…`, `pk-llmobs-…` and `pat-llmobs-…`.

All of these will be renamed mechanically once the product has its real name (D46). Keep the name in as few places as practical, e.g. one constant for the attribute prefix.

## Commits and reports

- Small commits with a short imperative subject (e.g. `Add WAL group commit`). One sub-phase gives one commit unless the plan says otherwise.
- Don't commit generated artifacts (`target/`, `node_modules/`, `dist/`, `.venv/`).
- When you finish, report:
  - what was done;
  - any deviation from the plan or spec, and why;
  - the exact versions you chose for any new dependency;
  - anything left undone or unverified.
