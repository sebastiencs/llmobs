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
- `datafusion`, `arrow`, `parquet` and `object_store` are a pinned family: never bump one without the others.
- Look up real versions (`cargo search`, `cargo info`); never guess one.

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
- Tests cover the current behaviour only.

**Reference implementations.** The competitors' source code is checked out in `/home/sebastien/github/{lmnr,phoenix,langfuse,langwatch,helicone,opik,mlflow}`. Read it to compare designs and learn from their mistakes. Never copy code from it.

## Layout

```
crates/
  core/      llmobs-core     shared contracts: IDs, enums, names, messages, Span/Score,
                             Arrow schemas + batch conversion, OpenAPI schemas,
                             no config (D59)                                          (spec 01)
  otel/      llmobs-otel     OTLP decode, normalization, media, truncation, cost      (spec 02)
  storage/   llmobs-storage  WAL, memtable, Parquet, catalog, compaction, deletion,
                             DataFusion providers, filter DSL, SQL sandbox            (specs 03, 05)
  server/    llmobs-server   API, auth, metadata DB, evaluation, alerts, email, MCP   (specs 04, 06–09)
  cli/       llmobs-cli      the `llmobs` binary                                       (spec 09 § CLI)
web/                         React UI (Vite, TanStack, shadcn/ui, pnpm)               (spec 11)
sdks/python/                 Python SDK (uv)                                           (spec 10)
sdks/typescript/             TypeScript SDK (pnpm)                                     (spec 10)
fixtures/otlp/               OTLP fixtures shared by Rust golden tests and SDK tests  (spec 02 § Testing)
fixtures/api/                example API responses (UI mocks), checked against the server types (spec 09)
tools/                       dev tools (fixture capture, benchmarks); not shipped
docs/                        decisions, specs, plans, research
```

**Crate dependency graph (enforced):**
- `core` depends on no workspace crate.
- `otel` and `storage` depend only on `core`. **They never depend on each other**, so they can be built in parallel; `server` connects them.
- `server` depends on `core`, `otel` and `storage`; `cli` depends on `server` and `core`.

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
| `just check` | format check, clippy (`-D warnings`), tests, cargo-deny; **must pass before every commit** |
| `just fmt` | format the Rust code |
| `just lint` | clippy, warnings as errors |
| `just test` | all Rust tests |
| `just deny` | supply-chain checks (advisories, bans, sources; licenses deferred, D57) |
| `just run <args>` | run the `llmobs` binary |

Required tools:
- `rustup` (the toolchain is pinned in `rust-toolchain.toml`)
- `just`
- `cargo-deny` (`cargo install cargo-deny --locked`)
- later: `pnpm` (web, TS SDK) and `uv` (Python SDK)

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
