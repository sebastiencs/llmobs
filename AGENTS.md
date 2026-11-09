# llmobs

llmobs is the working name of a Rust LLM observability platform. One binary serves traces on a single machine, stores columnar data with DataFusion and Parquet, and keeps metadata in SQLite.

## Source of truth

- `docs/mvp-decisions.md` (decisions D1…) and `docs/spec/` (numbered specs) define what to build.
- `docs/research/competitors.md` explains why.
- If code must deviate from the spec, update the spec and add a decision to the log in the same change.

## Rules

Observer has not been released. There are no external callers, on-disk formats, or wire protocols that must keep working. Change the code to the shape it should have.

Do not keep a compatibility layer. When a type, function, format, config key, or protocol field changes, update every caller and delete the old path in the same change. Do not add shims, aliases, deprecated wrappers, versioned codecs, feature flags, or dual read/write paths so old and new code coexist. Do not preserve an old on-disk layout, frame, or API just in case. Tests cover the current behavior only.

Do not add an abstraction you do not need. Prefer a concrete type and a straight function. Add a trait, generic, newtype, or module split only when a second implementation exists or the split removes real duplication. One implementor means no trait. A parameter or a private function is enough when only call-site configuration changes. Do not introduce a wrapper whose only job is to forward to the type it holds.

Reference implementations live in `/home/sebastien/github/{lmnr,phoenix,langfuse,langwatch,helicone,opik,mlflow}`. Read them to compare designs. Never copy code from them.

All dependency versions are declared in the root `[workspace.dependencies]`. Crates reference them with `{ workspace = true }`.

Each library crate defines its own error enum or enums with `thiserror`. `anyhow` is only allowed in `llmobs-cli`.

No `unwrap` or `expect` outside tests unless the invariant is documented in a comment next to the call.

No `unsafe`. It is forbidden workspace-wide.

Time is `i64` nanoseconds since the Unix epoch (UTC) at the storage boundary. IDs follow `docs/spec/README.md` §conventions.

Logs never contain payload content (prompts, outputs, secrets).

Every behaviour change comes with tests.

## Layout and ownership

```
llmobs-core  ◄── llmobs-otel
     ▲       ◄── llmobs-storage
     │                 ▲
     └── llmobs-server ┘ ──► llmobs-otel
               ▲
         llmobs-cli
```

`llmobs-otel` and `llmobs-storage` must not depend on each other. The server glues them together. Only `llmobs-server` and `llmobs-cli` may depend on `llmobs-server`. `llmobs-core` depends on no other workspace crate.

A session only edits the directories its brief names. Changes to `llmobs-core` shared types go through the contract owner, the coordinating session.

## Commands

`just check` runs formatting, clippy (`-D warnings`), tests, and supply-chain checks. Also: `just fmt`, `just lint`, `just test`, `just deny`, `just run …`.

Required tools: rustup (the toolchain is pinned in `rust-toolchain.toml`), `just`, and `cargo-deny` (`cargo install cargo-deny --locked`). Later: `pnpm`, `uv`.

## Naming

Crate prefix `llmobs-`, binary `llmobs`, env prefix `LLMOBS_`, attribute namespace `llmobs.*`. All of these will be renamed mechanically when the product gets its real name (D46).

## Commits

Small commits with an imperative subject. Run `just check` before committing.
