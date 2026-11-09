# API example project

Synthetic data for UI mocks, the OpenAPI examples, and later storage and API tests. Nothing here is a real customer, key, or address. The only email is `ada@example.com`.

## Layout

| Path | Written by |
|---|---|
| `dataset/spans.json`, `dataset/scores.json` | hand |
| `static/*.json` | hand |
| `media/<sha256>.png` | hand |
| `responses/` | `just api-examples` |
| `manifest.json` | `just api-examples` |

Do not edit `responses/` or `manifest.json` by hand. Regenerate them with `just api-examples`.

`dataset/spans.json` is a JSON array of spans in the same shape as `fixtures/otlp/*/expected.json`, sorted by `(trace_id, start_time, span_id)`. `dataset/scores.json` is a JSON array of scores. Both use RFC 3339 timestamps with nine fractional digits.

## Fixed identifiers

| Thing | Value |
|---|---|
| Org | `01900000-0000-7000-8000-000000000010`, slug `acme`, name `Acme` |
| Project | `01900000-0000-7000-8000-000000000001`, slug `support-bot`, name `Support bot`, retention 30 days |
| Second project | `01900000-0000-7000-8000-000000000002`, slug `playground`, name `Playground` |
| User | `01900000-0000-7000-8000-000000000100`, `ada@example.com`, `Ada Example`, instance admin, org owner |
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

`project` is never in `path_params`. Mocks match any project. `get_media` points at `media/<sha256>.png` with content type `image/png`.

`just check` loads these files in `crates/server/tests/api_examples.rs`. Hand-written JSON has to deserialize into its Rust type and serialize back to the same bytes, so keep the field order and the omitted empty fields. Spans stay sorted by `(trace_id, start_time, span_id)`, span ids are unique within a trace, every score target exists in the dataset, and every `media_refs` entry matches the SHA-256 of `media/<sha>.png`. Session and user ids stay `[a-z0-9-]+`. The manifest names every read operation except the two SSE streams, and every `file` exists.
