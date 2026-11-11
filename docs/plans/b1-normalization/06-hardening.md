# B1 · Hardening (N23)

See [README.md](README.md).

## N23 — Property tests

**Edit only:** `crates/otel/Cargo.toml` (`proptest` as a dev-dependency), root `Cargo.toml` (`proptest` in `[workspace.dependencies]`), `crates/otel/tests/proptest.rs`.

**Commit:** `otel: property-test decoding and normalization`

**Dependency:** `proptest` 1.12 (checked 2026-11-10; its MSRV of 1.88 is below our toolchain). Use `default-features = false, features = ["std"]` unless a needed strategy requires more. Report the features used.

**Strategies:**
- `AnyValue` up to depth 40, so it crosses the 32 limit, with random keys, including duplicates and every convention prefix (`gen_ai.`, `gen_ai.prompt.`, `llm.input_messages.`, `ai.`, `traceloop.`, `langfuse.`, `lmnr.`, `gcp.vertex.agent.`, `llmobs.`, `input.value`, …).
- Indexed keys with random indices and suffixes.
- Strings that are sometimes valid JSON, double-encoded JSON, data URIs with valid or invalid base64, very long text (up to 64 KiB), or non-ASCII.
- IDs of length 0–20, including all zeros.
- Timestamps across the whole `u64` range.
- Events with the GenAI event names.
- Resource attributes and scopes.

Run each property with `ProptestConfig { cases: 256, .. }`, which keeps `just test` under about 10 s on this machine. Use `LLMOBS_PROPTEST_CASES` to raise the case count locally. Report the runtime.

**Properties:**
1. `normalize` never panics (any `ExportTraceServiceRequest`, a small `IngestLimits`, the test price table).
2. `spans.len() + rejections.total()` equals the number of input spans.
3. Every accepted span:
   - `end_time >= start_time`;
   - its JSON round-trips through `llmobs_core::Span` serde;
   - `input_format` is set iff `input` is set (same for `output`);
   - with `messages` format, `input` parses with `parse_messages`;
   - `input_preview` and `output_preview` are ≤ 512 scalars;
   - `media_refs` is a subset of `Normalized.media` hashes;
   - `truncated` is sorted and unique;
   - `conventions` is in precedence order without duplicates.
4. Every stored string field and attribute value is ≤ `max_attribute_bytes` plus the marker length.
5. Rule 3: on a span whose kind isn't `llm` or `embedding`, all token and cost columns are `None`.
6. `decode_protobuf` never panics on arbitrary bytes, and `decode_json` never panics on arbitrary strings, including deep nesting (`[[[[…]]]]` 10 000 levels). `serde_json`'s recursion limit must turn that into an error; assert it does.
7. `Rejections::message()` never contains any string from the input. Seed the input with a sentinel such as `SENTINEL-7f3a` in names, keys and values, and check that it never appears.
8. Determinism: normalizing the same request twice gives equal results.

Any failure found while writing these is fixed in this commit, with a regression test in the module it concerns. List the fixes in the report.

Finally, check that `PENDING` in `tests/golden.rs` is **empty**. If it isn't, report every remaining case and its diff. B1 is not done until it's empty.
