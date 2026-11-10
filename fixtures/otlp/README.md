# OTLP fixtures

Golden cases for OTLP normalization. Three consumers read this directory. Track B's golden test normalizes each `input.json` and compares the result with `expected.json`. The capture tool (`tools/capture`) records real instrumentations into this layout. SDK tests check that our SDKs emit spans that normalize to the expected output. Every value in `expected.json` encodes spec 02, including § Normalization rules.

## Layout

```
fixtures/otlp/
  README.md
  prices.json
  <group>/<case>/
    case.json
    input.json
    expected.json
```

Group directories are `genai`, `openinference`, `openllmetry`, `vercel`, `langfuse`, `laminar`, `google_adk`, `llmobs` (spans from our own SDKs), and `edge` (cross-cutting edge cases). Create a group directory only when it contains a case. Case directory names are kebab-case and name the scenario.

Every case directory contains exactly three files: `case.json`, `input.json`, and `expected.json`. The validation test allows only `README.md` and `prices.json` beside the group directories, and it rejects a group name outside the list above.

## case.json

`case.json` describes the case.

| Field | Type | Required | Meaning |
|---|---|---|---|
| `description` | string | yes | One or two sentences: what the case exercises. |
| `origin` | `"hand_written"` \| `"captured"` | yes | |
| `source` | object | iff `origin` is `"captured"` | `{instrumentation, instrumentation_version, language, libraries: {name: version}, captured_at}`. `captured_at` is RFC 3339. |
| `spec_refs` | array of strings | yes, at least one | Spec sections this case pins, for example `"02 § Mapping tables / Span kind"`. |
| `settings` | object | no | Per-case ingest overrides. Allowed keys: `max_attribute_bytes`, `media_min_bytes`, `media_max_bytes`, all integers. Any other key is an error. |
| `context` | object | no | Overrides `project_id` (UUID string) and `ingest_time` (RFC 3339). |
| `notes` | string | no | Free text, for example which spec ambiguity the case resolves. |

When `settings` or `context` omit a field, the defaults are:

- `max_attribute_bytes` = 8388608, `media_min_bytes` = 4096, `media_max_bytes` = 33554432 (spec 02 § Limits)
- `project_id` = `"01900000-0000-7000-8000-000000000001"`
- `ingest_time` = `"2026-01-01T00:00:00.000000000Z"`

Every span starts and ends before the case's `ingest_time`, and starts no earlier than 10 seconds before it. With the default ingest time, spans start at `2025-12-31T23:59:50Z` or later and end before `2026-01-01T00:00:00Z`.

## input.json

One OTLP `ExportTraceServiceRequest` in the standard OTLP/JSON encoding (OTLP spec, "JSON Protobuf Encoding"):

- lowerCamelCase field names
- `traceId`, `spanId`, and `parentSpanId` as hex strings
- 64-bit integers (`startTimeUnixNano`, `intValue`, and the rest) as decimal strings
- enum fields (`kind`, `status.code`) as integers
- `AnyValue` objects: `{"stringValue": …}`, `{"intValue": "…"}`, `{"doubleValue": …}`, `{"boolValue": …}`, `{"arrayValue": {"values": […]}}`, `{"kvlistValue": {"values": […]}}`, `{"bytesValue": "<base64>"}`

This is what an exporter configured with `http/json` sends, and what `POST /v1/traces` accepts with `Content-Type: application/json`. The capture tool converts captured protobuf payloads into this encoding.

## expected.json

```json
{
  "spans": [],
  "media": [],
  "rejected_spans": 0
}
```

- `spans` lists every span the normalizer must accept, in `llmobs_core::Span` JSON (D60, D64). Timestamps are RFC 3339 with 9 fractional digits and a `Z` suffix. Optional and empty fields are omitted. Spans are sorted by `(trace_id, start_time, span_id)`.
- `media` lists every blob the normalizer must extract. The bytes are not stored, only their identity: `{"sha256": "<64 lowercase hex>", "mime_type": "image/png", "size_bytes": 70}`. Sorted by `sha256`. Every entry is referenced by some span's `media_refs`, and every `media_refs` entry appears here.
- `rejected_spans` is the number of input spans the normalizer must reject (spec 02 § Normalization rules). It equals the number of input spans minus `spans.len()`.
- Each span's `project_id` and `ingest_time` equal the case context (defaults or overrides).
- A span with `input` also has `input_format`, and the same pairing holds for `output`. When `input_format` is `messages`, `input` parses as canonical messages.
- Every expected `(trace_id, span_id)` occurs in `input.json`.

## Comparison

Track B's golden test, and anyone else comparing a normalizer run with a fixture, uses these rules:

- Compare spans as parsed `Span` values, after sorting both sides by `(trace_id, start_time, span_id)`.
- Compare `cost_input`, `cost_output`, and `cost_total` with an absolute tolerance of `1e-12`. Every other field must be exactly equal.
- Compare `media` exactly, including order, and compare `rejected_spans` exactly.

## Price table

`prices.json` is used instead of the built-in LiteLLM snapshot whenever a fixture is normalized (D67). That keeps goldens stable across snapshot refreshes. The prices are deliberately round and are not real market prices. The file is an array of rows mirroring `model_prices` (spec 04 § Pricing). Prices are USD per 1M tokens:

```json
[{
  "model_name": "…",
  "match_pattern": "<regex>",
  "provider": null,
  "valid_from": "1970-01-01T00:00:00.000000000Z",
  "price_input": 1.0,
  "price_output": 4.0,
  "price_cache_read": null,
  "price_cache_write": null,
  "price_reasoning": null,
  "tiers": null
}]
```

`match_pattern` is matched against the span's model, `coalesce(response_model, request_model)`. The match is anchored and case-insensitive (spec 02 § Cost computation). Write regex escapes as JSON (`\\d`).

The validation test compiles every pattern. It requires a row for each model on a span whose `cost_source` is `computed`. It rejects two rows with the same `provider` whose patterns both match one model used in the fixtures.

## Canonical formatting

`case.json`, `input.json`, `expected.json`, and `prices.json` are pretty-printed with 2-space indentation and end with exactly one newline. Object keys stay in the order produced by `serde_json` with the workspace `preserve_order` setting.

The validation test rejects a file that is not in that form. `just fixtures-fmt` rewrites the JSON files into it (`LLMOBS_FIXTURES_FMT=1` on the formatting test). It pretty-prints the parsed JSON. It does not reorder keys, so write them in serde field order the first time.

## Content rules

Use synthetic content only. No real prompts, no PII, and no real API keys, tokens, emails, or hostnames.

The validation test scans every fixture file for common secret patterns (`sk-`, `sk-ant-`, `AKIA`, `ghp_`, Slack `xox`, `Bearer` tokens) and for email addresses outside `@example.com` and `@example.org`.

Keep each `input.json` under 256 KiB. For a media case, use a tiny blob and lower `media_min_bytes` in `settings` instead of shipping a large file. The bytes themselves stay inside `input.json` as base64. `expected.json` records only the sha256, MIME type, and size. One scenario per case. Add a new case rather than growing an existing one.

## Workflows

**Hand-written case.** Write `case.json` and `input.json`, then write `expected.json` by hand from spec 02. Run `just fixtures-fmt`, then `just fixtures-check`.

**Captured case.** Once the capture tool exists, it writes `case.json` and `input.json`. Track B's golden test in bless mode (`LLMOBS_BLESS=1`) writes `expected.json`. Review that file line by line before committing it.

**Changing an expected output.** Change it only together with the spec change or bug fix that justifies it, in the same commit.
