# Competitor feature inventory (from docs, Nov 2026)

Sources: docs folders of lmnr (Laminar — only internal notes in repo), phoenix, langfuse (no docs in repo: README + fern API + specs), langwatch, helicone, opik, mlflow (GenAI part only).

Legend: ● = solid / headline, ◐ = partial / beta / paid-only, — = absent

## Feature matrix

| Area | Laminar | Phoenix | Langfuse | LangWatch | Helicone | Opik | MLflow |
|---|---|---|---|---|---|---|---|
| OTLP ingest (HTTP) | ● +gRPC | ● +gRPC | ● (main path) | ● | ◐ (async only) | ● | ● |
| Proxy / gateway ingest | — | ◐ (chat proxy) | ◐ beta (Rust) | ● (Go) | ● (core) | ◐ beta | ● |
| Accepts foreign semconv (GenAI, OpenInference, OpenLLMetry, Vercel) | ● | ● | ◐ | ● | — | ● | ● (widest) |
| Own SDKs Py/TS | ● | ● | ● | ● +Go | ● (manual logger) | ● | ● +Java |
| Typed spans (LLM/tool/agent/retriever…) | ● | ● 12 kinds | ● 10 types | ● | — | ● 4 types | ● 14 types |
| Sessions / threads, users, tags, metadata | ● | ● | ● | ● (+customer_id) | ● (headers) | ● | ● |
| Environment / release / version | ● release | — | ● | ● | — | ◐ tags | ● LoggedModel+git |
| Multimodal attachments (S3) | ◐ | ◐ images | ● | ● | — | ● | ● |
| Cost/tokens w/ price table (regex, cache/reasoning tokens) | ● | ● (LiteLLM) | ● (+tiers) | ● 350+ | ● 300+ | ● | ● (LiteLLM) |
| Trace explorer + tree/timeline | ● realtime SSE | ● | ● | ● SSE | ● | ● | ● |
| Filter DSL | ● | ● (py-like) | ● | ● | ● | ● OQL | ● |
| Full-text search on content | ● Quickwit | — | — | ◐ | — | ◐ | ● |
| SQL query access | ● | ◐ MCP only | — (metrics JSON API) | ● | ● HQL | — | — |
| Fixed metrics dashboard | ● | ● | ● | ● | ● | ● | ● |
| Custom dashboards | ● | — (paid) | ● | ● | — | ● | — |
| Scores / feedback (num/bool/cat/text) | ● | ● | ● | ● | ● | ● | ● |
| End-user feedback endpoint (public key) | — | ● | ● | ● | ● | — | ● |
| Human annotation in UI | ● | ● | ● | ● | ◐ | ● | ● |
| Annotation queues | ● | — (paid) | ● | ● | — | ● | ● |
| Datasets (from traces, CSV, versioned) | ● | ● versioned | ● | ● | ● | ● versioned | ● |
| Offline experiments / evaluate() | ● | ● | ● | ● | ◐ | ● | ● |
| Built-in LLM judges library | — | ● | — (templates) | ● LangEvals | ◐ | ● (many) | ● (many) |
| Online evals (LLM-judge on prod, sampling, filters) | ● Signals | — (paid) | ● | ● | ◐ | ● | ● |
| Sandboxed code evaluators server-side | — | ● | ● | ● | ◐ webhooks | ● | ● |
| CI/pytest gating | ● CLI | ● | ◐ | ● | — | ◐ | ● |
| Prompt registry (versions + labels/aliases) | — | ● | ● (+caching) | ● (+git sync) | ● | ● | ● |
| Playground (replay span, multi-model) | ● | ● | ● | ● | ● | ● | ● |
| Prompt / agent optimizer (GEPA, MIPRO…) | — | ◐ | — | ● DSPy | — | ● | ● |
| Agent simulation / red teaming | — | — | — | ● Scenario | — | ◐ | ◐ |
| Alerts (Slack / webhook / email) | ● | — | ◐ | ● | ● | ● | ◐ (gateway budget) |
| Issue clustering / auto-discovery | ● Signals clusters | — | ◐ Topics | ● Topics | — | ● Diagnostics | ● Issues |
| In-app AI assistant | ● | ● PXI | ◐ | ● Langy | — | ● Ollie | ● |
| MCP server + CLI for coding agents | ● | ● | ● | ● | ● | ● | ● |
| Coding-agent tracing (Claude Code, Codex…) | — | ● | ◐ | ● (+cost per PR) | ● | ● | ● |
| Browser session replay | ● rrweb | — | — | — | — | — | — |
| Gateway: routing/fallback/caching/rate-limit/budgets | — | — | ◐ | ● | ● | ◐ | ● |
| PII redaction | ● (model) | ◐ env vars | ◐ EE masking | ● (policies) | ◐ omit | ◐ SDK | ◐ filters |
| Retention policies | — | ● | ◐ EE | ● | — | ◐ TTL | ● archival |
| Orgs → projects multi-tenancy | ● | — (single tenant) | ● | ● | ● | ● | ● workspaces |
| RBAC | ● basic | ● admin/member/viewer | ● | ● custom (EE) | ◐ | ● custom (EE) | ● |
| SSO OIDC | ● | ● | ● (EE for some) | ◐ EE | — | ◐ EE | ● plugin |
| SCIM / audit log | — | — | ◐ EE | ◐ EE | — | — | — |
| Export (S3/Parquet/CSV) | — | ● | ● Parquet to bucket | ● | ● | ● | ◐ |

## Infrastructure

| | Backend | Metadata DB | Trace store | Queue | Other |
|---|---|---|---|---|---|
| Laminar | **Rust** (Actix+Tonic) + Next.js | Postgres | ClickHouse | RabbitMQ (opt) | Redis, Quickwit, S3 (all opt) |
| Phoenix | Python | SQLite / Postgres | same SQL DB | — | — |
| Langfuse | Next.js + worker (+Rust gateway) | Postgres | ClickHouse | BullMQ/Redis | S3 |
| LangWatch | Hono + Go services + Py evals | Postgres | ClickHouse | BullMQ/Redis | S3 |
| Helicone | Next.js + Express + CF Workers | Postgres | ClickHouse | Kafka | S3, Redis |
| Opik | Java + Python | MySQL | ClickHouse | Redis Streams | S3, Zookeeper |
| MLflow | Python | SQLite/PG/MySQL/MSSQL | same SQL DB (+archival to artifacts) | Redis (jobs) | artifact store |

Consensus for scale: Postgres + ClickHouse + Redis + S3. Simple tier (Phoenix, MLflow): a single SQL DB.

## Licensing

- MIT core + EE dir: Langfuse
- Apache-2.0 core + EE license: LangWatch, Opik, Helicone, MLflow (full Apache)
- ELv2: Phoenix (no gates; paid product is a separate SaaS, Arize AX)
- Laminar: OSS with EE stubs (Signals, clustering, agent)

## Typical paid-only gates

SSO, SCIM, audit logs, custom roles, retention, masking/PII, online evals and alerts (Phoenix), annotation queues (Phoenix).

## Payload limits & multimodal (from code)

| Project | Request limit | Per-span limit / truncation | Media extracted? | Media store | Content-addressed | Payload dedup |
|---|---|---|---|---|---|---|
| Laminar | 5 MiB HTTP / 25 MiB gRPC, 413 | none | no (inline base64) | ClickHouse | – | yes: BLAKE3 per message/tool def, per session (`unique_content`) |
| Phoenix | none (gRPC lib default) | client redacts images >32k chars | no | SQL DB | – | no |
| Langfuse | 4.5 MB legacy batch, 512 MiB OTLP | 9.5 MB only logged; list reads cut to 1k chars | SDK + server (OTel data URIs, Anthropic, Gemini, AI SDK) | S3 via presigned PUT | yes (SHA-256 per project) | media only |
| LangWatch | 10 MiB | 8 MiB/attribute → placeholder or trim history; truncated attrs listed | server edge (≤16/span, 5 s budget) | object store | maybe | no |
| Helicone | 10–50 MB | previews ~8 MB / 400 KB | no | bodies >10 MB to S3 | – | cache refs only |
| Opik | 512 MB, 413 | SDK: >20 MB → truncation marker | SDK + server (base64 >256 KB, Tika MIME) | S3/MinIO, presigned multipart | no | no |
| MLflow | none | optional attachment cap | SDK (data URIs, OpenAI audio, Anthropic, Gemini, DALL·E) | artifact store | no (UUID) | no |

Common pattern: 5–10 MB request limit with 413; base64 media extracted and replaced by a reference, stored in object storage.

## Auth & email (from code)

| | Email/pwd self-host | Default auth | Hash | OAuth/OIDC in OSS | SAML | Email transport | Templates |
|---|---|---|---|---|---|---|---|
| Laminar | No (passwordless any-email in dev) | dummy email login | – | GitHub, Google, Entra, Okta, Keycloak | No | Resend only | react-email / Rust HTML |
| Phoenix | Yes | on if non-loopback | PBKDF2-SHA256 | generic OIDC + LDAP | No | SMTP | Jinja2 |
| Langfuse | Yes (`AUTH_DISABLE_USERNAME_PASSWORD`) | on | bcrypt 12 | many + generic OIDC (per-org UI SSO is EE) | No | SMTP URL (nodemailer) / SES | react-email |
| LangWatch | Yes | on | bcrypt 10 | license-gated | license-gated | SES / SendGrid / SMTP / Resend | react-email |
| Helicone | Yes (verification required) | on | scrypt | none | No | SMTP + Resend | inline HTML |
| Opik | No | none (EE only) | – | EE only | EE only | none | – |
| MLflow | opt-in basic auth | none | werkzeug | community plugin | No | none | – |

API keys: SHA-256 (Langfuse, Helicone, Laminar SHA3) of high-entropy random keys; public/secret pairs (Langfuse); ingest-only scope + expiry (Laminar, LangWatch).

## Storage design checks (from code)

**Scores (S3).** All seven keep scores in the same engine as spans. Langfuse and Helicone moved scores from Postgres to ClickHouse. ClickHouse users store scores as append-only rows keyed by id: last-writer-wins on a version column, with `FINAL` / `LIMIT 1 BY` on reads, and lightweight or async deletes by trace_id. Opik keeps one row per author (`authored_feedback_scores`). Phoenix and MLflow use SQL tables with FK cascade.

**Span sort order (S4).**
- Dominant pattern: partition by time, sort by trace_id within the partition (Opik, LangWatch, Laminar).
- Lists come from a time-ordered structure (Laminar: projection ordered by start_time; LangWatch: `trace_analytics` ordered by OccurredAt).
- LangWatch and Opik keep a trace_id → time-range index table, filled by a materialized view.
- Langfuse v4 is time-first, and its UI passes a timestamp hint to trace lookups.

**Judge tracing (S7).** Most trace the judge call and mark it internal:
- Langfuse: env `langfuse-llm-as-a-judge`, linked through `score.executionTraceId`.
- Opik: hidden trace, `source=evaluator`.
- Phoenix: separate hidden project per evaluator.
- MLflow: opt-in, `scorerTraceId`.

The marker blocks recursion and hides these traces from default views. LangWatch does not trace built-in judges; it uses a cost ledger instead. Helicone and Laminar bill judge usage as platform usage.
