# llmobs — MVP definition

Working name: **llmobs** (real name to be decided later).
Competitor research: `research/competitors.md`.

## MVP scope at a glance

**Ingestion**
- OTLP over HTTP + gRPC; normalizes OTel GenAI semconv, OpenInference, OpenLLMetry, Vercel AI SDK attributes.
- 10 MiB HTTP / 32 MiB gRPC request limit (413); attributes >8 MiB truncated with a marker.
- Server-side extraction of base64 media → content-addressed (SHA-256, per project) blobs in object storage.
- Cost + tokens from a price table (LiteLLM import + per-project custom prices), incl. cache/reasoning tokens.

**Storage**
- Single binary, single machine. DataFusion + Parquet (own layout, manifest in SQLite), local disk or S3.
- WAL + in-memory Arrow buffer → spans queryable within seconds (live view).
- Tombstone deletes, physical removal at compaction; retention by dropping time partitions.

**UI** (React/TS, embedded in the binary)
- Trace list (visual filter builder + SQL-like filter DSL + full-text scan), trace tree/timeline with live updates, chat rendering of LLM spans, media rendering, sessions view, fixed metrics dashboard.
- SQL console over the user's project data.
- Saved views (list pages) and saved SQL queries, private or shared.
- Scores and human annotation on spans/traces/sessions.
- Datasets (versioned, CSV/JSONL import, add-from-trace), experiment results/comparison.
- Online judge rules, alert rules, settings (orgs, projects, members, API keys, LLM connections).

**Evaluation**
- Online LLM-as-judge on traces and spans: filter + sampling + cost cap; built-in templates (hallucination, relevance, toxicity, correctness vs expected, custom).
- Built-in Rust checks (exact, contains, regex, JSON valid, JSON schema, Levenshtein); webhook evaluator.
- Experiments run client-side via SDK `evaluate(dataset, task, scorers)`.

**Alerts**: error rate, cost over window, p95 latency, judge score under threshold → Slack webhook, webhook, email.

**Access**
- Orgs → projects; roles owner/admin/member/viewer (+ per-project override).
- Email/password (argon2id), GitHub/Google OAuth, generic OIDC. No SAML in v1.
- API keys: `pk-` (write-only, browser feedback) / `sk-`; SHA-256 hashed; optional expiry; ingest-only scope.
- Public end-user feedback endpoint.

**Interfaces**: REST + OpenAPI, MCP server, CLI, Python + TypeScript SDKs (thin layer over OpenTelemetry).

**GDPR**: delete by user_id, by trace/filter, by project, by org; per-project retention; cascades to all derived data.

**Packaging**: binary, Docker image, docker-compose, minimal Helm chart; documented snapshot backup.

## Decision log

| # | Topic | Decision |
|---|---|---|
| D1 | Positioning | Lightweight, single-binary self-host for bottom-up adoption; revenue from enterprise self-host features, then cost governance (gateway). |
| D2 | Backend language | Rust. |
| D3 | Trace storage | Apache DataFusion + Parquet. Same engine in embedded mode and future scale mode (S3 + Postgres metadata). No ClickHouse. |
| D4 | Ingestion | OTLP (HTTP + gRPC) in v1, normalizing GenAI semconv / OpenInference / OpenLLMetry / Vercel AI SDK attributes. Proxy/gateway later. |
| D5 | SDK | Yes, we ship an SDK. |
| D6 | Tenancy | Organizations → projects from day one. |
| D7 | Query | Both a filter DSL and user-facing SQL. |
| D8 | Frontend | React + TypeScript. |
| D9 | License / business model | Deferred. |
| D10 | Distribution | Self-host first; usage metering + per-org limits built in from day one so cloud is mostly ops + billing later. |
| D11 | Evaluators | LLM-as-judge (online + experiments), built-in Rust checks, webhook evaluator. Sandboxed code evaluators (Monty / WASM) in v2. |
| D12 | Real time | Live trace view is a must: freshly ingested spans queryable within seconds. |
| D13 | Deletion semantics | Deletes visible immediately (tombstones); bytes physically removed at the next file rewrite. |
| D14 | Team / timeline | Solo, AI-assisted, ~2 months target. |
| D15 | Topology | Single machine in v1 (vertical scaling); internal split so it can be distributed later. |
| D16 | Table layout | Own Parquet layout + manifest in the metadata DB. No Iceberg/Delta in v1. |
| D17 | SDK | Python + TypeScript, thin layer over OpenTelemetry; reuse OpenInference/OpenLLMetry instrumentors; REST client for datasets/experiments/scores. |
| D18 | Experiments | Client-side via SDK. No server-side runs in MVP. |
| D19 | Prompt registry | Not in MVP. |
| D20 | Out of MVP | Annotation queues, custom dashboards, playground, issue detection, prompt registry, gateway, SAML, 2FA/passkeys, message dedup. |
| D21 | Judge providers | OpenAI-compatible + native Anthropic + native Gemini; encrypted credentials per org/project. |
| D22 | Roles | owner / admin / member / viewer at org level, optional per-project override. |
| D23 | Performance targets (soft, not MVP-blocking) | 8 cores / 32 GB: ≥50k spans/s ingest; span visible in UI <2 s; filtered trace list over 1B spans p95 <500 ms; 1k-span trace opens <200 ms; full-text search over 30 days <2 s. |
| D24 | GDPR scope | Delete by `user_id`, by trace / search filter, by project, by org; per-project retention in days. Cascades to scores, annotations, eval results, WAL, media blobs (refcounted), dataset items copied from traces. |
| D25 | Full-text search | Brute-force scan in v1; Tantivy index later. |
| D26 | Object storage | Local disk and S3 (via `object_store`) both in v1. |
| D27 | Scope discipline | Everything listed is in the MVP; no "slips first" ordering. |
| D28 | Alerts | Triggers: error rate, cost over window, p95 latency, judge score under threshold. Channels: Slack webhook, generic webhook, email. |
| D29 | Packaging | Single binary with embedded frontend, Docker image, docker-compose, minimal Helm chart (StatefulSet + PVC). |
| D30 | Filter DSL | SQL-like WHERE expressions compiled to DataFusion; visual builder on top. |
| D31 | MCP + CLI | In v1 (minimal: SQL query + fetch trace). |
| D32 | Online judges | Traces and spans (sessions later); handful of built-in templates; cost cap per rule. |
| D33 | End-user feedback | Public write-only key endpoint in v1. |
| D34 | Datasets | Versioned (each change = new version); CSV/JSONL import. |
| D35 | Scores | numeric / boolean / categorical / text, optional reason, on span, trace or session. Correction type later. |
| D36 | Metadata DB | SQLite only in v1. Postgres when going multi-node. |
| D37 | Backups | Documented snapshot procedure. |
| D38 | Self-monitoring | Deferred to just before release. |
| D39 | Stack | tokio/axum/tonic, datafusion/parquet/arrow/object_store + own WAL, sqlx (SQLite), argon2/openidconnect/oauth2, utoipa, lettre + minijinja, Vite/React/TS/TanStack/shadcn/ECharts, OTel-based SDKs, monorepo (`crates/`, `web/`, `sdks/python`, `sdks/typescript`). |
| D40 | Payloads | 10 MiB HTTP / 32 MiB gRPC, 413 above; attributes >8 MiB truncated with marker + list of truncated attrs. |
| D41 | Multimodal | Server-side extraction (data URIs, OpenAI, Anthropic, Gemini formats) → SHA-256 content-addressed blobs per project; span holds `media://` reference. |
| D42 | Message dedup | Later (rely on zstd in v1; schema designed to allow it). |
| D43 | Auth | Email/password on by default (argon2id, ≥10 chars, `AUTH_DISABLE_PASSWORD`, `AUTH_DISABLE_SIGNUP`, first user = owner); GitHub, Google, generic OIDC via env; works without email (copyable invite links, CLI password reset). No SAML in v1. |
| D44 | API keys | `pk-` write-only / `sk-` secret; SHA-256 hashed; optional expiry; ingest-only scope. |
| D45 | Email | SMTP only (`lettre`, `SMTP_URL`), optional; `minijinja` HTML + text templates. Emails: invites, password reset, alerts, optional verification. |
| D46 | Name | Working name `llmobs`; real name later. |
| D47 | Spec defaults | S1–S15 in `spec/README.md` accepted (with S7 and S11 refinements). |
| D48 | Span sort order | Time-sliced files, rows sorted by `trace_id, start_time` (changed from time-sorted after reviewing competitors). |
| D49 | Judge traces | Evaluator executions traced into the user's project under reserved environment `llmobs-eval`, hidden by default, linked from scores. |
| D50 | Saved views | Saved list views and saved SQL queries are in the MVP. |
| D51 | Ports | 8080 (UI/API/OTLP-HTTP/MCP), 4317 (OTLP gRPC), 4318 (OTLP HTTP). |
| D52 | Repo layout | Cargo workspace with 5 crates: `llmobs-core`, `llmobs-otel`, `llmobs-storage`, `llmobs-server`, `llmobs-cli` (binary `llmobs`); `otel` and `storage` never depend on each other. Plus `web/`, `sdks/python`, `sdks/typescript`, `fixtures/otlp`, `tools/`. |
| D53 | Parallel work | Contracts first (Phase 0, serial), then waves of parallel sessions; 3 concurrent sessions for now. |
| D54 | Code name | `llmobs` everywhere in code (crates, binary, `LLMOBS_` env, `llmobs.*` attributes). |
| D55 | Tooling | GitHub Actions CI; `uv` for Python; `pnpm` for TypeScript; `just` as task runner. |
| D56 | Allocator | mimalloc as global allocator of the binary. |
| D57 | Dependency licenses | cargo-deny runs advisories/bans/sources only; license check deferred until our license (D9) is decided. |
| D58 | Agent instructions | `AGENTS.md` at the root (with `CLAUDE.md` symlink); `.cursor/rules` kept. |
| D59 | Config ownership | No config in `llmobs-core`: each crate owns its config section (otel: ingest limits, storage: storage/WAL/compaction, server: auth/SMTP/…); the server assembles the top-level config and does figment loading. |
| D60 | Span contract | `llmobs-core` defines owned Rust `Span`/`Score` structs (serde) and the `Span`/`Score` → Arrow `RecordBatch` conversion; otel outputs `Vec<Span>`, storage consumes batches. |
| D61 | ID types | Newtypes only for `TraceId`, `SpanId` (fixed bytes, hex, all-zero invalid), `ProjectId`, `OrgId` (UUID). Other entity IDs are plain `uuid::Uuid`. |
| D62 | Arrow string columns | In memory every string column is `Utf8`; "(dict)" in spec 01 means Parquet dictionary encoding, not an Arrow `Dictionary` type. |
| D63 | Contract tests | `insta` snapshot tests lock schemas, enums, names and serde formats of `llmobs-core`. |
| D64 | JSON timestamps | Instants are `i64` ns in Rust/Arrow and RFC 3339 UTC strings with 9 fractional digits and `Z` in JSON (fixtures, API). Durations (`ttft_ns`, `duration_ns`) stay JSON integers. |
| D65 | Message extra fields | Canonical message parts and messages keep unmodelled fields in an `extra` map (typed part preserved, exact JSON round-trip); `Unknown` only for unknown/invalid part types. |
| D66 | OTLP fixture format | `fixtures/otlp/<group>/<case>/{case.json, input.json, expected.json}`; input in standard OTLP/JSON; expected = `llmobs_core::Span` JSON + extracted media + rejected count; canonical pretty JSON; validated by a test in `llmobs-otel`. |
| D67 | Fixture prices | Fixtures use a fixed, fictitious test price table (`fixtures/otlp/prices.json`), never the embedded LiteLLM snapshot. |
| D68 | Normalization clarifications | Spec 02 § Normalization rules (map value encoding, consumed-attribute retention, tokens/cost only on llm/embedding, previews, system messages, tool arguments, truncation, rejection, …), pinned by the four hand-written fixtures. |
| D69 | API contract | Code-first with `utoipa`; the generated OpenAPI document is committed as `crates/server/openapi.json` and checked by a test. Operations without handlers are declared with utoipa's builder API until their handler lands. Step 4 covers the UI read set (auth config, `/me`, projects, traces, spans, sessions, users, scores, schema, saved views, metrics, media, SSE shapes); other endpoints are added by the tracks that implement them. |
| D70 | API payloads | Span payloads are returned parsed: `{format, value}` with typed messages; stored text that does not parse degrades to `json` or `text` (spec 09 § Response shapes). |
| D71 | OpenAPI schemas in core | `llmobs-core` depends on `utoipa`; IDs, stored enums, canonical messages and `Score` (with its target and value) implement `ToSchema` there, so each type is defined once. |
| D72 | API examples | `fixtures/api/`: an example project (stored spans + scores), the responses the API returns for it (generated by a test from the dataset, or hand-written for non-derived endpoints), and a manifest. The UI mocks from them; a test checks them against the response types. |
| D73 | API response shapes | Absent optional scalars omitted, lists/maps always present; problem responses carry a `code`; trace detail is a flat pre-order list with `depth`/`detached`, capped at 20 000 spans; latest score per name on list rows (spec 09 § Response shapes). |

## Open

- License (D9).
- Real product name (D46).
