# API example project

Synthetic data for UI mocks, the OpenAPI examples, and later storage and API tests. Nothing here is a real customer, key, or address. The emails are `ada@example.com`, `bob@example.com`, `guest@example.com` for the invite examples, and `new@example.com` for the admin-created account.

## Layout

| Path | Written by |
|---|---|
| `dataset/spans.json`, `dataset/scores.json` | hand |
| `static/*.json` | hand, except `static/sql-result.json` |
| `media/<sha256>.png` | hand |
| `responses/` | `just api-examples`: the real read handlers over `dataset/` |
| `manifest.json` | `just api-examples` |
| `static/sql-result.json` | `just api-examples`: the real `run_sql` handler's answer to `requests/sql-query.json` |
| `requests/*.json` | hand | example request bodies, checked against their types and `openapi.json` |

Do not edit `responses/`, `manifest.json` or `static/sql-result.json` by hand. Regenerate them with `just api-examples`.

`get_schema` is checked against `static/schema.json`, which A2's catalog test also reads. `get_metrics` is generated over `[2026-01-01T10:00:00Z, 11:00:00Z)` with `bucket=10m`. Its manifest query is empty, so the mocks serve it for any range.

`dataset/spans.json` is a JSON array of spans in the same shape as `fixtures/otlp/*/expected.json`, sorted by `(trace_id, start_time, span_id)`. `dataset/scores.json` is a JSON array of scores. Both use RFC 3339 timestamps with nine fractional digits.

## Fixed identifiers

| Thing | Value |
|---|---|
| Org | `01900000-0000-7000-8000-000000000010`, slug `acme`, name `Acme` |
| Project | `01900000-0000-7000-8000-000000000001`, slug `support-bot`, name `Support bot`, retention 30 days |
| Second project | `01900000-0000-7000-8000-000000000002`, slug `playground`, name `Playground` |
| User | `01900000-0000-7000-8000-000000000100`, `ada@example.com`, `Ada Example`, instance admin, org owner |
| Member | `01900000-0000-7000-8000-000000000101`, `bob@example.com`, `Bob Example`, org member. Used by the member examples |
| API key | `01900000-0000-7000-8000-000000000200`, prefix `sk-llmobs-EXAMPL`. `api-key-created.json` uses the same synthetic `sk-llmobs-EXAMPLE0…` secret as `setup-response.json`. It is not a live key |
| Personal token | `01900000-0000-7000-8000-000000000201`, prefix `pat-llmobs-EXAMPL`. `personal-token-created.json` uses the synthetic `pat-llmobs-EXAMPLE0…` secret. It is not a live token |
| Invitation | `01900000-0000-7000-8000-000000000300` for `guest@example.com`. `invite-created.json` uses the synthetic accept token `EXAMPLE0EXAMPLE0EXAMPLE0EXAMPLE0EXAMPLE0EXA`. It is not a live invitation |
| LLM connection | `01900000-0000-7000-8000-000000000400`, `OpenAI`. `requests/llm-connection-create.json` holds a synthetic key; it is not a live key |
| Created account | `01900000-0000-7000-8000-000000000102`, `new@example.com`. `admin-user-created.json` reuses that synthetic token in the reset link. It is not a live reset |
| Trace ids | `a1000000000000000000000000000001` through `…0006` |
| Span ids | `b1000000000000` plus the trace digit and the span digit. Trace 1 span 2 is `b100000000000012` |
| Missing parent of trace 6 | `b1000000000000ff` |
| PNG | `497790947d4666760ce38f3c00e852c71fdb66cae849bae8e9ede352719e1581` |

Session and user ids match `[a-z0-9-]+` so they can be file names. Trace 5 uses environment `llmobs-eval` and is left out of the default lists.

## Manifest

`manifest.json` is an array sorted by `(operation_id, file)`. Each entry names one example response:

```json
{
  "operation_id": "get_trace",
  "path_params": {"trace_id": "a1000000000000000000000000000001"},
  "query": {},
  "status": 200,
  "content_type": "application/json",
  "file": "responses/traces/a1000000000000000000000000000001.json"
}
```

`project` is never in `path_params`. The web mocks read `manifest.json`, match any `project`, and pick the query entry with the largest matching subset. `get_media` points at `media/<sha256>.png` with content type `image/png`.

`just check` runs the checks that remain in `crates/server/tests/api_examples.rs`: each static file and generated response round-trips through its Rust type, the responses match the OpenAPI schemas, the manifest covers every read operation and names only declared paths and statuses, the dataset is consistent (spans sorted, span ids unique within a trace, every score target exists, every `media_refs` entry matches the SHA-256 of `media/<sha>.png`, session and user ids stay `[a-z0-9-]+`), every file is used, and the scenario assertions hold. `just check` also runs the generator's up-to-date test in `crates/server/src/api_examples.rs`, which fails when a file differs from what the handlers return.
