# MCP servers and SQL access in competitors

Notes behind track H's decisions (D-H-2 to D-H-13), read from the competitors' source trees in November 2026. They describe designs; no code is copied.

## MCP servers

| Product | Library and transport | Auth | Tools | Context limits |
|---|---|---|---|---|
| Laminar | Rust `rmcp`, stateless `POST /v1/mcp`; GET and DELETE answer 405 | project API key | 3: SQL with `{name:Type}` parameters (schema embedded in the description), a trace-context digest, an "ask the platform agent" tool; all read-only | none: no row cap, results bounded only by ClickHouse's 512 MB result limit and 120 s |
| Phoenix | Python FastMCP, streamable HTTP at `/mcp`, on by default | API key or Phoenix's own OAuth (PKCE, DCR, RFC 9728 metadata) | SQL schema and execute, GraphQL, or "code mode" (agent-written Python calling the tools in a sandbox); otherwise one tool per REST operation | 500 rows by default, 5,000 max, 4 MiB per response, 30 s, a read-only transaction, a table allowlist, a concurrency queue of 8 |
| Langfuse | official TS SDK, stateless streamable HTTP, JSON responses | HTTP Basic with a project key; Host/Origin checks | ~90 tools with read-only/destructive hints. Observability: list observations, get observation, field and filter schemas, metrics query | list: 1–100 rows, required window of at most 14 days, field projection, metadata cut to 200 characters; metrics: 1,000 rows; 25 s; per-project cooldown after a resource error |
| LangWatch | official TS SDK, stdio or HTTP with stateful sessions (20 per key, idle reaper) | bearer API key or OAuth | 112: schema discovery, SQL dialect query, trace search (digest output, paged), get trace, analytics, ~100 platform CRUD tools | SQL shows 50 rows and points to the CLI for full exports |
| MLflow | FastMCP over stdio, generated from CLI commands | none at the MCP layer | trace search and get, tags, feedback, evaluation | `max_results`, field projection |
| Opik | separate package; hosted HTTP with OAuth, or stdio | API key or OAuth | generic `read`, `list`, `write`, `schema` | — |

What H takes from them:
- the common core: schema discovery, SQL, trace search, get trace (D-H-11);
- stateless streamable HTTP with JSON responses, as Langfuse and Laminar do; LangWatch's sessions needed caps and a reaper (D-H-2);
- explicit row caps and honest truncation notes (Phoenix), a required window and short metadata values (Langfuse) (D-H-9, D-H-10, D-H-11);
- not Laminar's unbounded results, and not the large write surfaces.

## SQL access and saved queries

- **Laminar**: full ClickHouse SQL behind a sqlparser validator (SELECT only, function blocklist, project-scoped views), `{name:Type}` parameters, a read-only client. Saved queries are project-wide templates any member edits (no owner, no private flag). The editor has CodeMirror with schema autocomplete, a parameters bar, a templates sidebar, and table, JSON and chart tabs.
- **LangWatch**: a ClickHouse dialect run by a restricted read-only database user with row policies and hard limits (10 s, rows and bytes read, result size), plus a view allowlist and an implicit `LIMIT 10000`. Queries are saved as dashboard widgets. Monaco editor, Vega charts.
- **Langfuse**: no raw SQL; a JSON metrics query API (1,000 rows max). Saved queries are dashboard widgets.
- **Phoenix**: SQL through MCP only; no saved queries.

llmobs already has the equivalent of these sandboxes in A2 (D95, D96, D-A2-1, D-A2-2). H adds owner-based sharing for saved queries (D-H-6), which none of them have.

## CLIs

Phoenix `px` (trace and span get/list, `--format pretty|json|raw`), LangWatch `langwatch query run` (`--param`, `--start/--end`, `--format table|json|jsonl|csv`, keyset paging), MLflow `traces search|get` (`table|json`), Opik `export` (`json|csv`). H's `llmobs query` and `traces` follow the same shape (D-H-12).
