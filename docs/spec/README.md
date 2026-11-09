# llmobs — Technical specification (MVP)

Status: draft v0.1 · Scope: everything in `../mvp-decisions.md`.

`llmobs` is a working name. Every `llmobs` identifier (binary, crates, env prefix `LLMOBS_`, attribute namespace `llmobs.*`, key prefixes) will be renamed mechanically once the product name is chosen.

## Documents

| # | Document | Contents |
|---|---|---|
| 00 | this file | goals, architecture, process layout, crate layout, conventions |
| 01 | [data-model.md](01-data-model.md) | span, trace, session, score, media; canonical message format; span kinds |
| 02 | [ingestion.md](02-ingestion.md) | OTLP endpoints, convention detection and mapping tables, media extraction, truncation, cost |
| 03 | [storage.md](03-storage.md) | WAL, memtable, Parquet layout, catalog, flush, compaction, query path, deletion, retention, media GC |
| 04 | [metadata-db.md](04-metadata-db.md) | SQLite schema (orgs, users, keys, datasets, rules, ...) |
| 05 | [query.md](05-query.md) | filter DSL, SQL console sandbox, full-text search, dashboard metrics |
| 06 | [evaluation.md](06-evaluation.md) | evaluators, online rules, datasets, experiments, scores |
| 07 | [alerts.md](07-alerts.md) | alert rules, evaluation loop, channels |
| 08 | [auth.md](08-auth.md) | users, sessions, OAuth/OIDC, RBAC, API keys, email |
| 09 | [api.md](09-api.md) | REST, SSE, OTLP, MCP, CLI |
| 10 | [sdk.md](10-sdk.md) | Python and TypeScript SDKs |
| 11 | [ui.md](11-ui.md) | pages and interactions |
| 12 | [operations.md](12-operations.md) | configuration, packaging, limits, metering, backups, upgrades |

## Goals

1. One binary, one machine, one data directory (or bucket). `llmobs serve` and it works.
2. Accept traces from any OpenTelemetry GenAI instrumentation without our SDK.
3. Ingested spans visible in the UI in < 2 s.
4. GDPR erasure that actually erases.
5. Fast: soft targets in D23 (≥50k spans/s ingest on 8 cores, trace list p95 < 500 ms over 1B spans).
6. Designed so a later multi-node mode (stateless queriers on S3 + Postgres catalog) does not require a data migration.

## Non-goals (MVP)

Gateway/proxy, prompt registry, playground, annotation queues, custom dashboards, issue clustering, SAML, 2FA, message dedup, multi-node, Postgres, Iceberg export, sandboxed code evaluators. The design must not preclude any of them.

## Architecture

```
                    ┌──────────────────────────── llmobs serve (one process) ─────────────────────────────┐
 OTLP gRPC :4317 ──►│ ingest ─► normalize ─► media extract ─► cost ─► WAL (group commit) ─► memtable      │
 OTLP HTTP :8080 ──►│   │                                                    │            │               │
                    │   │ auth / rate limit / metering                       │            ├─► live bus (SSE)
                    │   ▼                                                    │            └─► eval trigger queue
 REST/SSE  :8080 ──►│ api (axum) ─► query engine (DataFusion) ◄── memtable + Parquet files              │
 MCP       :8080 ──►│                     ▲                                                              │
 Web UI    :8080 ──►│ static assets       │ catalog (SQLite)                                             │
                    │                                                                                     │
                    │ background: flusher · compactor · deletion rewriter · retention · media GC ·        │
                    │             judge workers · alert evaluator · mailer · trace-completion detector    │
                    └──────────────────────────┬──────────────────────────────────────────────────────────┘
                                               ▼
                        data dir:  meta.db (SQLite) · wal/ · object store (local dir or S3):
                                   tables/{spans,traces,scores}/… · media/…
```

Single tokio runtime. CPU-heavy work (Parquet encode, compaction, DataFusion execution) runs on a dedicated rayon/tokio blocking pool so ingestion latency stays flat.

## Crate layout (monorepo)

```
crates/
  llmobs/            # binary: CLI (clap) — serve, admin, query, trace, mcp-stdio
  llmobs-core/       # ids, time, config, errors, shared types (Span, Score, …)
  llmobs-otel/       # OTLP decode (prost/opentelemetry-proto), convention detection, normalization, media extraction
  llmobs-storage/    # WAL, memtable, Parquet writer/reader, catalog, flush, compaction, deletion, retention, GC
  llmobs-query/      # DataFusion session factory, TableProviders, filter DSL, SQL sandbox, metrics queries
  llmobs-meta/       # SQLite (sqlx) repositories + embedded migrations
  llmobs-auth/       # passwords, sessions, OAuth/OIDC, API keys, RBAC checks
  llmobs-eval/       # judge runner, built-in checks, webhook evaluator, online rules, trace-completion
  llmobs-alerts/     # alert evaluation loop + channels
  llmobs-mail/       # lettre + minijinja templates
  llmobs-api/        # axum routers, SSE, OpenAPI (utoipa), MCP server, static assets
web/                 # Vite + React + TS UI (built into crates/llmobs-api via memory-serve)
sdks/python/         # `llmobs` on PyPI
sdks/typescript/     # `llmobs` on npm
docs/
```

Dependency direction: `core` ← `otel`, `meta` ← `storage` ← `query` ← `eval`, `alerts` ← `api` ← `llmobs`.

## Spec-level choices (reviewed and accepted)

These were made while writing the spec and approved afterwards; S3, S4 and S7 were checked against the competitors' code (`../research/competitors.md`).

| # | Choice | Where |
|---|---|---|
| S1 | Canonical message format = OTel GenAI `gen_ai.input/output.messages` JSON schema | 01 |
| S2 | Traces are derived "partial summary" rows merged at query time and at compaction (AggregatingMergeTree-style) | 01, 03 |
| S3 | Scores live in the columnar store (WAL + Parquet, last-writer-wins), not SQLite — same engine as spans, like every competitor; deterministic score IDs give one row per (target, name, author) | 01, 03, 06 |
| S4 | Time-sliced files, rows sorted by `trace_id, start_time` inside each file (Opik/LangWatch/Laminar pattern); `traces` table sorted by time for lists; trace lookup via time hint, then bloom filters, `trace_index` table post-MVP | 03 |
| S5 | Deletion cut-off = request time: data ingested after the request is not deleted | 03 |
| S6 | Trace-level online rules fire when the root span ended + 5 s idle, or after 60 s idle (configurable) | 06 |
| S7 | Evaluator executions are traced into the user's project with reserved environment `llmobs-eval`; excluded from rules, lists, dashboards and alerts by default; score links to the judge trace; GDPR deletion cascades to judge traces | 03, 05, 06, 07 |
| S8 | Sign-up default after bootstrap is `invite_only` | 08 |
| S9 | API key prefixes `sk-llmobs-`, `pk-llmobs-`, `pat-llmobs-`; keys are project-bound; PATs for CLI/MCP | 08 |
| S10 | MCP tools are read-only in MVP; MCP auth via PAT bearer (OAuth for MCP later) | 09 |
| S11 | Ports: 8080 (HTTP: UI, API, OTLP/HTTP, MCP), 4317 (OTLP/gRPC), 4318 (OTLP/HTTP only, standard port) | 09, 12 |
| S12 | Static assets embedded with `memory-serve` | 11 |
| S13 | `llmobs admin backup` command implements the documented snapshot procedure | 12 |
| S14 | SSRF guard on all outbound calls, with an opt-in for private networks (local Ollama/vLLM) | 12 |
| S15 | Saved views (list pages) and saved SQL queries in MVP, private or shared, with per-page defaults | 04, 05, 09, 11 |

## Cross-cutting conventions

| Topic | Convention |
|---|---|
| IDs of entities | UUIDv7, serialized as lowercase hyphenated strings. Public-facing short prefix in API where useful (`org_`, `prj_`… not stored). |
| Trace / span IDs | OTel: 16-byte trace ID, 8-byte span ID; hex lowercase in JSON; `FixedSizeBinary` in Arrow. |
| Time | Nanoseconds since Unix epoch, UTC. Arrow `Timestamp(Nanosecond, "UTC")`. JSON: RFC 3339 with ns precision. |
| Money | USD as `f64`. Price table stores USD per 1M units. |
| JSON in columns | UTF-8 strings containing JSON (`input`, `output`, attribute values). |
| Errors | RFC 7807 `application/problem+json`. |
| Pagination | Opaque cursor (`next_cursor`), `limit` ≤ 1000. |
| Config | Env vars `LLMOBS_*` override optional `llmobs.toml`. |
| Logging | `tracing` crate, JSON or pretty; no payload content in logs. |
