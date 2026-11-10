# B1 · Media, truncation and cost (N14–N17)

See [README.md](README.md). These run in the pipeline order of spec 02: payloads (N9–N13) → **media** → **truncation** → **cost**.

## N14 — Media extraction

**Edit only:** `crates/otel/src/{media.rs,normalize.rs,lib.rs}`, `crates/otel/Cargo.toml` (`sha2`, `base64` as dependencies, workspace), and fixtures under `fixtures/otlp/edge/`.

**Commit:** `otel: extract media blobs from payloads`

**Prerequisite:** C2 (`llmobs_core::MediaBlob`) is on `main`. If it isn't, stop and report.

`media.rs`:

```rust
pub(crate) struct MediaSink<'a> { limits: &'a IngestLimits, blobs: BTreeMap<String /*sha*/, MediaBlob>, refs: BTreeSet<String> }
impl MediaSink<'_> {
    /// Returns the replacement for one candidate: Some(uri) when extracted, Some(dropped marker) when too large,
    /// None when below media_min_bytes or not valid base64 (left inline).
    fn extract(&mut self, base64: &str, declared_mime: Option<&str>) -> Option<Replacement>;
}
pub(crate) fn extract_payload(p: &mut Payload, sink: &mut MediaSink);        // messages, json, text
pub(crate) fn extract_json(v: &mut serde_json::Value, sink: &mut MediaSink); // system_instructions, tool_definitions
```

**Candidates** (spec 02 § Media extraction, N1 § 7):
- `blob` parts in messages;
- data URIs `data:<mime>;base64,<payload>` inside any JSON string or `Text` payload. The data URI substring is replaced by `media://<sha256>`. In a `uri` part whose `uri` is a data URI, the part becomes `{"type":"uri", modality, mime_type, uri: "media://…"}`;
- raw JSON shapes in `Json` payloads and in the `Unknown` parts of messages:
  - OpenAI `image_url.url` (a data URI) and `input_audio.data`;
  - Anthropic `source {type: "base64", media_type, data}`;
  - Gemini `inline_data`/`inlineData {mime_type|mimeType, data}`;
  - Vercel `{type: "image", image}` / `{type: "file", data, mediaType|mimeType}`, where the base64 string itself is replaced by `media://<sha256>`.

**Decoding and limits:**
- Base64 is the standard alphabet, with or without padding. Whitespace is not allowed. Anything else is not a candidate and stays inline.
- The decoded size must be at least `media_min_bytes`. Anything smaller stays inline.
- Above `media_max_bytes`: a part becomes `{"type":"uri","uri":"llmobs:dropped","reason":"too_large","size":N}` (core `marker` constant), and a raw string becomes `llmobs:dropped`. No blob is created.
- Check the size from the base64 length **before** decoding: `len * 3 / 4` minus padding. A 32 MiB+ blob is never decoded.

**MIME type:** the declared type wins when it's present and syntactically `type/subtype`. Otherwise the magic-byte table is used:

| Type | Signature |
|---|---|
| `image/png` | `89 50 4E 47 0D 0A 1A 0A` |
| `image/jpeg` | `FF D8 FF` |
| `image/gif` | `GIF87a` / `GIF89a` |
| `image/webp` | `RIFF????WEBP` |
| `application/pdf` | `%PDF-` |
| `audio/wav` | `RIFF????WAVE` |
| `audio/mpeg` | `ID3`, or `FF FB` / `FF F3` / `FF F2` |
| `audio/ogg` | `OggS` |
| `audio/flac` | `fLaC` |
| `video/mp4` | `????ftyp` (bytes 4–7) |
| `video/webm` | `1A 45 DF A3` |

Anything else is `application/octet-stream`. The `modality` of a created part comes from the MIME top-level type (`image`, `audio`, `video`), else stays absent.

**Output:** SHA-256 lowercase hex. The same blob twice in a request gives one `MediaBlob`. `span.media_refs` lists the span's hashes, sorted and unique. `Normalized.media` is sorted by `sha256`.

Fill `Normalized.media` (adding the field now if N4 couldn't).

Fixtures:
- update `edge/media-before-truncation` and `edge/media-truncation-rejection` so they pass, minus truncation, which comes in N15;
- add `edge/media-data-uri-in-text`;
- add `edge/media-anthropic-raw-json`;
- add `edge/media-too-large` (set `media_max_bytes` small in `settings`);
- add `edge/media-below-min`;
- add `edge/media-dedup` (the same image on two spans).

The golden harness compares `{sha256, mime_type, size_bytes}`. Make the harness map `MediaBlob` to that.

Unit tests:
- each candidate shape;
- the magic table;
- the size check before decoding;
- invalid base64;
- dedup.

---

## N15 — Truncation

**Edit only:** `crates/otel/src/{truncate.rs,normalize.rs,lib.rs}`, plus fixtures under `fixtures/otlp/edge/`.

**Commit:** `otel: truncate oversized payloads and attributes`

This applies after media extraction (rule 14), with `limit = max_attribute_bytes`.

**What is checked:**
- `input`, `output`, `system_instructions`, `tool_definitions` (by their stored text);
- every value in `attributes` and `resource_attributes`;
- each event's `attributes` JSON;
- `status_message`;
- `name` (spans can carry absurd names).

`truncated` gets the column name (`input`, `output`, …, `name`, `status_message`), the attribute key, or `events[<i>].attributes`. It's sorted and unique.

**Strings** (rule 14): cut at the largest char boundary ≤ `limit`, then append `…[llmobs: truncated, original N bytes]` (core marker prefix + `N bytes]`).

**Messages** (spec 02 § Truncation):
1. Keep every `system` message, the first `user` message, and as many of the last messages as fit, counting from the end. Insert the placeholder message `[llmobs: K messages truncated]` (core marker) where messages were removed. Each candidate is measured by its serialized length.
2. If the kept list still exceeds the limit, cut the longest string leaf in the list (text or reasoning `content`, a string tool `arguments`/`response`) with the string rule. Cut it to the length that makes the list fit, or to 1 KiB if that's not enough. Repeat, longest first, until it fits or no leaf is longer than 1 KiB.
3. If it still doesn't fit, store the serialized messages cut with the string rule as `Text`, change the format to `text`, and record `normalize_error` code `truncate:<field>`.

Add steps 2 and 3 to spec 02 § Truncation in this commit; they settle an ambiguity.

**JSON payloads:** apply the string rule to the compact JSON text, and the format becomes `text`, since the cut text is no longer valid JSON. Record this in spec 02 in the same commit.

**Previews** are computed after truncation, from the stored value. Move the preview call if N9 placed it earlier.

Fixtures:
- make `edge/media-before-truncation` and `edge/media-truncation-rejection` pass, and remove them from `PENDING`;
- add `edge/truncate-messages-middle` (system, first user, last N kept, placeholder);
- add `edge/truncate-single-huge-message` (step 2);
- add `edge/truncate-json-payload`;
- add `edge/truncate-attribute-and-name`.

Use a small `max_attribute_bytes` in `settings`.

Unit tests:
- a char boundary with 4-byte characters;
- a value exactly at the limit (not truncated) and one byte over;
- each messages step.

---

## N16 — Price table and cost

**Edit only:**
- `crates/otel/src/{cost.rs,error.rs,normalize.rs,context.rs,lib.rs}`;
- `crates/otel/tests/{golden.rs,support/mod.rs}`;
- fixtures under `fixtures/otlp/{edge,genai}/`;
- `docs/spec/02-ingestion.md` (the two clarifications below).

**Commit:** `otel: compute span cost from the price table`

### Types (public, README § Public API)

```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriceRow {
    pub scope: PriceScope,                    // serde default Builtin; fixtures/otlp/prices.json has no scope field
    pub model_name: String,
    pub match_pattern: String,
    pub provider: Option<String>,
    #[serde(with = "llmobs_core::time_serde")] pub valid_from: i64,
    pub price_input: Option<f64>, pub price_output: Option<f64>,
    pub price_cache_read: Option<f64>, pub price_cache_write: Option<f64>, pub price_reasoning: Option<f64>,
    pub tiers: Option<Vec<PriceTier>>,
}
pub struct PriceTier { pub min_input_tokens: i64, /* same five Option<f64> prices */ }
pub enum PriceScope { Builtin, Org, Project }   // priority Project > Org > Builtin
pub struct PriceTable { rows: Vec<PriceRow>, set: regex::RegexSet /* one per row: ^(?i:pattern)$ */ }
impl PriceTable { pub fn new(rows: Vec<PriceRow>) -> Result<Self, PriceError>; }
```

- `PriceError::InvalidPattern { index }` carries no pattern text. The patterns come from users through the server.
- Build the `RegexSet` with a size limit: `RegexSetBuilder::size_limit`, 16 MiB total. Over the limit → `PriceError::TooLarge`.
- The test-support `PriceRow` in `tests/support/mod.rs` is replaced by this type. Delete the duplicate.

`normalize` gains its third parameter `prices: &PriceTable`. Update every caller: the golden test, unit tests and proptests.

### Computation (spec 02 § Cost computation, D82, D107)

1. **Reported cost** (N1 § 5 keys), on `llm`/`embedding` spans only. Rule 3 keeps cost columns null on other kinds, so a reported cost there stays in `attributes` (pinned by a fixture). On an `llm`/`embedding` span, any reported key gives `cost_source = reported`, and D107 fills the columns.
2. **Lookup**, only on `llm`/`embedding` spans that have a model (`coalesce(response_model, request_model)`).
   - Candidates are the rows whose regex matches the model, whose `valid_from ≤ start_time`, and whose provider matches the span's.
   - **Clarification A, added to spec 02:** a row with a `provider` matches when the span's provider equals it, **or the span has no provider**.
   - Pick by scope priority, then the latest `valid_from`, then the longest `match_pattern`, then the lowest row index for determinism.
3. **Tier:** the tier with the greatest `min_input_tokens ≤ input_tokens` (input including cache). A tier's null prices fall back to the row's base price, not to null.
   - **Clarification B, added to spec 02:** which tier applies, and the fallback to base prices.
4. **Formula:** spec 02 steps 3–4 and D82. `reasoning` falls back to `output`. A term with tokens > 0 and a null price nulls its part. Embedding spans follow D82: `cost_output` null and `cost_total = cost_input`.
5. **No match** → all three null, `cost_source = none`.
6. **Provider inference** (spec 02 § Model, provider): when the span has no provider and a row with a provider matched, `provider` = that row's provider. This happens even when the cost is null.

Fixtures (all with the test table `fixtures/otlp/prices.json`; extend it if needed, and review the change against D67):
- `edge/cost-reasoning-fallback`;
- `edge/cost-missing-price-nulls-part` (D82);
- `edge/cost-embedding` (D82);
- `edge/cost-tier`: add a tiered row to `prices.json`, which also needs `fixtures.rs` validation of `tiers`;
- `edge/cost-reported-partial` (D107);
- `edge/cost-reported-on-chain-span`;
- `edge/cost-valid-from` (two rows, two dates);
- `edge/provider-inferred-from-price-row`.

`genai/chat-cache-tokens`, `openinference/llm-flattened-tool-call` and `vercel/generate-text` need cost. Remove from `PENDING` whichever ones pass, and report the remaining diffs of the others.

Unit tests:
- the priority order;
- case-insensitive anchoring (`gpt-4o` must not match `gpt-4o-mini`);
- an invalid regex → `PriceError` without text;
- a `RegexSet` size limit;
- token counts near `i64::MAX` (the f64 math stays finite).

---

## N17 — Built-in price snapshot

**Edit only:**
- `crates/otel/prices/builtin.json` (new);
- `crates/otel/src/{cost.rs,lib.rs}`;
- `crates/otel/tests/prices.rs` (new);
- `justfile`.

**Commit:** `otel: embed the converted LiteLLM price snapshot`

`pub fn builtin_price_rows() -> Vec<PriceRow>` parses `include_str!("../prices/builtin.json")`. The file is ours and is checked by a test, so a parse failure is a bug, and an `expect` with that invariant comment is allowed. Every row has `scope: Builtin`. The server seeds `model_prices` from it at startup (spec 04 § Pricing).

`tests/prices.rs`:
- `builtin_snapshot_is_valid`:
  - parse it;
  - `PriceTable::new` succeeds;
  - no two rows share `(match_pattern, provider, valid_from)`;
  - all prices are ≥ 0 and finite;
  - `valid_from = 0`;
  - patterns are anchored-safe, with no `^`/`$` inside since we add them;
  - the file is canonical JSON (2-space indent, trailing newline);
  - at least these models match: `gpt-4o`, `gpt-4o-mini`, `claude-sonnet-4-5`, `gemini-2.5-pro`, `text-embedding-3-small`.
- `refresh` (env-gated, like `fixtures-fmt`). When `LLMOBS_LITELLM_JSON=<path>` is set, it reads LiteLLM's `model_prices_and_context_window.json`, converts it, and writes `crates/otel/prices/builtin.json`. Otherwise it returns immediately. The conversion code lives in the test file, since it isn't shipped.

Conversion rules (also written as a comment at the top of the test):
- Keep entries whose `mode` is `chat`, `completion`, `responses` or `embedding`, with at least one of `input_cost_per_token` / `output_cost_per_token`.
- The model name is the LiteLLM key with a leading `<litellm_provider>/` removed. A key that still contains `/` after that is skipped (proxy routes such as `openrouter/…`).
- When two kept entries give the same model name, keep the entry whose key has no `/`. Otherwise keep the first in key order. Report the count of collisions.
- `match_pattern` = `regex::escape(model_name)`.
- `provider` = the canonical provider when `litellm_provider` maps to one of `openai`, `anthropic`, `gcp.gemini` (`gemini`), `gcp.vertex_ai` (`vertex_ai*`), `aws.bedrock` (`bedrock*`), `azure.ai.openai` (`azure*`), `mistral_ai` (`mistral`), `x_ai` (`xai`), `groq`, `deepseek`, `cohere`. Anything else gets `None`.
- Prices: `input_cost_per_token` → `price_input` × 1e6, `output_cost_per_token` → `price_output` × 1e6, `cache_read_input_token_cost` → `price_cache_read`, `cache_creation_input_token_cost` → `price_cache_write`, `output_cost_per_reasoning_token` → `price_reasoning`.
- Tiers: `*_above_<N>k_tokens` → `min_input_tokens = N·1000`, with the matching prices.
- Rows are sorted by `(model_name, provider)`.

`justfile`:

```
# Regenerate the built-in price snapshot from a downloaded LiteLLM JSON (D102)
prices-update litellm_json:
    LLMOBS_LITELLM_JSON={{litellm_json}} cargo test -p llmobs-otel --test prices -- refresh
```

The download is manual (the URL goes in the recipe comment); the recipe itself never fetches.

**Generate the first snapshot now.** Download LiteLLM's JSON from its GitHub repository (record the commit SHA in the report), run the recipe, and commit `builtin.json`. Check the size: if it's over 1 MiB, report it rather than trimming it silently.

Add a line to spec 02 § Cost computation: "The snapshot's source commit is recorded in the commit message of each refresh."

### Report

- the LiteLLM commit;
- the row count;
- the collisions;
- the file size.
