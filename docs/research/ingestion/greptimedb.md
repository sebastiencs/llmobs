# GreptimeDB OTLP trace ingestion — research notes for llmobs B1 (`llmobs-otel`, spec 02)

Repo: `/home/sebastien/travaux/greptimedb`. Local checkout HEAD `edbcb8224` (2026-09-21). Code citations are from that checkout, with paths relative to `src/` unless they start with `tests-integration/` or `Cargo.toml`. History comes from `gh search issues/commits --repo GreptimeTeam/greptimedb`. The GitHub API rate limit cut the commit search short, so the commit list in §9 is partial.

Scope: GreptimeDB is a general time-series and observability database, not an LLM tool. Its OTLP trace path stores spans in a table. It has no semantic normalization, so it is useful mainly for the **edges**: decoding, attribute typing, validation, partial success, error reporting and tests. A short design doc lives in the repo: `frontend/src/instance/otlp/README.md` (211 lines, with a mermaid flow at lines 17-49).

Vocabulary: a *pipeline* (`PipelineWay`) selects the storage shape. For traces, `greptime_trace_v0`/`v1`/`v2` are hard-coded converters, not the user-programmable pipeline engine (§7).

---

## 1. OTLP decoding

- **Crate: upstream `opentelemetry-proto` 0.32**, with `gen-tonic`, `trace`, `logs`, `metrics` and **`with-serde`** (`Cargo.toml:194-200`). Protobuf types are the prost-generated ones, and decoding is a single `ExportTraceServiceRequest::decode(bytes)` (`servers/src/http/otlp.rs:173-177`). Metrics are decoded with types from an `otel-arrow-rust` fork (`otlp.rs:36`, `Cargo.toml:202`). There is no hand-written OTLP model.
- **JSON is rejected for traces, logs and metrics.** If the `Content-Type` subtype is `json`, the handler returns `UnsupportedJsonContentType` (`http/otlp.rs:61-69, 159-161`; error `servers/src/error.rs:277`). The README states this too (`frontend/src/instance/otlp/README.md:59`). `serde` is only used **in tests**, to write OTLP/JSON literals that are then re-encoded to protobuf (`tests-integration/tests/http.rs:7725-7726, 7863`). So GreptimeDB never had to deal with the OTLP/JSON quirks in production.
- What the crate's serde does (from `opentelemetry-proto-0.33.1/src/proto.rs`, the next minor release, which is in our cargo registry): trace and span IDs go through `deserialize_from_hex_string` (`proto.rs:30-50`, which uses `const_hex::decode` and so accepts any length, including empty). `fixed64` timestamps accept a number or a string (`proto.rs:180-215`). `intValue` accepts a number or a string (`proto.rs:76-90`), `bytesValue` is base64, and `SpanKind`/`StatusCode` are `i32` fields (`opentelemetry.proto.trace.v1.rs:167`), so **enum names such as `"SPAN_KIND_SERVER"` are not accepted**. If we use `with-serde`, it handles hex and `int64`-as-string, but not enum names or ID length checks.
- **Compression:** HTTP uses tower-http `RequestDecompressionLayer::new().pass_through_unaccepted(true)` on the OTLP router (`servers/src/http.rs:1486-1498`). With `features = ["full"]` (`servers/Cargo.toml:143`) that enables gzip, deflate, br and zstd. Unknown encodings pass through undecoded and then fail protobuf decode.
- **Size limit:** a global axum `DefaultBodyLimit`, **64 MiB by default** (`http.rs:161`, applied at `http.rs:1046-1050`; `0` disables it). The limit is applied when the handler reads the `Bytes` body, which is after decompression, so it also bounds zip bombs. There is no spans-per-request limit. Requests are split into chunks of `trace_ingest_chunk_size` (512) spans for writing, not rejected (`frontend/src/service_config/otlp.rs:23-24`; `trace_ingest.rs:2129-2143`).
- **gRPC: none for traces.** OTLP-over-gRPC was added in 2024 (`6e860bc0f`, #3105) and **removed** in `c9d70e0e2` (#5605, 2025-02-27), together with the move to pipelines. Issue #5402 ("Add OTel gRPC protocol") is still open. The only OTel gRPC service left is OTel-Arrow metrics, which accepts zstd (`servers/src/grpc/builder.rs:176-190`). The default gRPC receive limit is 512 MiB (`common/grpc/src/channel_manager.rs:39`).
- **Routing:** `POST /v1/otlp/v1/traces` on the main HTTP port (`http.rs:760-770, 1493`). The table comes from a header and defaults to `opentelemetry_traces` (`http/otlp.rs:164`). The **`x-greptime-pipeline-name` header is required for traces**: there is no default, so `PipelineMissing` is returned (`pipeline/src/manager.rs` `from_name_and_default`; `http/otlp.rs:179-184`). This is a usability trap: a stock OTel exporter does not work without a custom header.

## 2. Attributes

- **Wrapper types.** `Attributes(Vec<KeyValue>)` keeps the raw prost key-values. It implements `Serialize` (a JSON object) and converts to `jsonb::Value` (`servers/src/otlp/trace/attributes.rs:101-151`). `OtlpAnyValue<'a>(&AnyValue)` serializes recursively (`attributes.rs:27-89`, serializer at `:54`). Its `Display` prints a string value without quotes and every other value as JSON (`attributes.rs:44-51`), which is the same rule as our spec 02 rule 1.
- **Three storage shapes, one per data model:**
  - **v0**: fixed columns plus span, scope and resource attributes as **JSONB** blobs.
  - **v1**: **flattened dynamic columns** named `span_attributes.<key>`, `scope_attributes.<key>` and `resource_attributes.<key>`, typed from the OTLP value. String, bool, int and double map to the matching scalar column. Arrays and kvlists become a JSONB binary column, and bytes become a raw `Binary` column (`trace/v1.rs:605-700`). The key is built with `format!("{prefix}.{key}")` (`v1.rs:615`), so attribute keys become **column names**. There is no limit on how many columns this creates.
  - **v2**: fixed schema with three **JSON2** columns, `span_attributes`, `scope_attributes` and `resource_attributes` (`trace/v2.rs:96-110`). Events and links go in plain JSON columns (`v2.rs:223-234`). The README says v2 "discards events and links" (`README.md:88-89`), but `v2.rs` writes them, so the doc and the code disagree.
- **Nested values.** `any_value_to_jsonb` maps `kvlistValue` to a JSON object and `arrayValue` to a JSON array, recursively, with no depth limit (`servers/src/otlp/utils.rs:23-51`). A missing value becomes `null`. Duplicate keys in a kvlist collapse into the object map, and the last one wins. `StringValueStrindex` (a profiling-only value) is treated as absent, with a `warn!` (`utils.rs:39-47`; `attributes.rs:83-87`).
- **Bytes are encoded three different ways:** as **hex** strings inside JSONB (`utils.rs:37`), as **raw binary** in a top-level v1 column (`v1.rs:688-697`), and as a **JSON array of numbers** in v2 and `Display`, because `serialize_bytes` goes through serde_json (`attributes.rs:82`; test at `attributes.rs:206`). In v1, raw bytes and JSONB values share the protobuf `BinaryValue`, so a column that receives both needs a dedicated fallback (`TraceBinaryType`, `v1.rs:92-115`; `README.md:110-114`). **Lesson:** pick one bytes encoding. Spec 02 uses base64.
- **Resource and scope.** `service.name` is pulled out into a `service_name` tag column (`span.rs:275-290`), and a bytes value is accepted through lossy UTF-8. It is then skipped in `resource_attributes.*` (`v1.rs:543-546`). Resource and scope attributes are kept per group and **shared through `Arc`** (`SharedAttributes`, `attributes.rs:156-182`; `bbc4c4bf1`, #9253). Spans are grouped per (resource, scope) as `TraceSpanGroup` (`span.rs:57-65, 258-320`).
- **Fixed types for stable semconv keys.** `trace_semconv_fixed_type` pins the column type of `Stable`/`RC` OTel attributes such as `http.response.status_code` (Int64) and `server.address` (String) (`frontend/src/instance/otlp/trace_semconv.rs:17-140`). Development-status attributes, **including all `gen_ai.*`**, go through dynamic type reconciliation instead.

## 3. Pipeline shape

- `http::otlp::traces` decodes the request (`http/otlp.rs:151-213`). Then `Instance::traces` (`frontend/src/instance/otlp.rs:219-253`) checks permissions, derives `trace_conventions` from `schema_url` (`trace_ingest.rs:2156-2193`), calls `span::parse(request)` to get `Vec<TraceSpanGroup>` (`span.rs:258-320`), runs admission on the span count, and then calls `ingest_trace_spans` (`trace_ingest.rs:871-1077`). That function chunks the spans, converts them through `to_grpc_insert_requests_from_spans`, which dispatches on `PipelineWay` (`trace.rs:115-129`) to `v0`/`v1`/`v2::*_to_grpc_main_insert_requests`, writes them, and then writes the `_services`/`_operations` aux tables from **accepted** spans only (`trace.rs:82-112`; `trace_ingest.rs:990-1036`).
- **Intermediate type.** `TraceSpan` (`span.rs:30-51`) has hex `String` IDs, `span_kind` as `"SPAN_KIND_*"` strings (from `as_str_name`, `span.rs:215`), and status as a `"STATUS_CODE_*"` string, or `""` when absent (`span.rs:246-251`). Event times are **ISO-8601 strings** (`span.rs:154`). Timestamps are `u64`.
- **Not trait-based**: one `match` on an enum plus plain functions. Each model has a hand-written converter with a resolved index per fixed column (`FixedTraceColumnIndexes`, `v2.rs:34-111`; `v1.rs:46-91`).
- **Not Arrow.** Rows are protobuf `api::v1::Row`/`Value` (`RowInsertRequests`), built row by row with `alloc_one_row` and `add_row` (`v2.rs:149-240`). Arrow comes later, on the write path.
- **Performance work** (all recent): fewer copies by moving owned `TraceSpan`s into chunks (`chunk_owned`, `trace_ingest.rs:2129`); an `Arc`-shared resource and scope that clones only when it is the last holder (`attributes.rs:173-182`); a fixed-column index cache; aux writes cached (`a907cbfbe`, #9432); "optimize OTLP trace ingestion" (`d9122ece3`, #8604); and an **OTLP trace load regression benchmark in CI** that tracks rows, failures and latency metrics (`cmd/src/bin/query_regression_runner/otlp.rs:34-37`; `f5f5d468b`, #8631).

## 4. Validation

- **Trace and span IDs: not validated at all.** `bytes_to_hex_string` is plain `hex::encode` (`utils.rs:19-21`). Empty, short or all-zero IDs are stored as `""` or `"0000…"`. The parent ID is `None` only when the bytes are empty (`span.rs:220-224`). An all-zero parent is stored as a real parent.
- **Timestamps:**
  - v2 rejects a start or end above `i64::MAX` with `TimestampOverflow` (`v2.rs:137-148`), which per-span fallback turns into one rejected span.
  - v1 casts `u64 as i64` **without a check** (`v1.rs:368-377`), so the value wraps to a negative number.
  - Zero, far-future and `end < start` are all accepted. Duration is clamped with `saturating_sub` to ≥0 and capped at `i64::MAX` (`v1.rs:339-353`). The doc comment explains why: negative durations broke unsigned readers such as Jaeger.
  - **v0 computes `end - start` on `u64`** (`v0.rs:113`). That panics in debug builds and wraps to a huge number in release (`Cargo.toml:394` does not enable overflow checks).
  - Unknown `kind` integers fall back to `SPAN_KIND_UNSPECIFIED`, because prost's `kind()` returns the default for unknown values (`span.rs:215`).
- **Partial success, added 2026** (#7875 → `3f3407fa2`, #7892; causes reported since `3de4feddd`, #8897):
  - A chunk failure before the write is classified by status code (`classify_trace_prewrite_failure`, `trace_ingest.rs:1766-1781`). `InvalidArguments`, `InvalidSyntax`, `Unsupported`, `TableNotFound` and `TableColumnNotFound` lead to **RetryPerSpan**. `DatabaseNotFound` leads to **DiscardChunk**. Retryable or unknown failures lead to **Propagate**.
  - In v2, a conversion failure is retried span by span (`trace_ingest.rs:1097-1130`).
  - **Once a write has been dispatched, its error is propagated** and never retried or counted as rejected. A distributed insert may have committed on some peers, so a retry could duplicate data (`README.md:166-170`).
- **The error message is bounded and safe to log.**
  - At most 4 distinct failure entries are kept, keyed by `(label, cause)`. Repeats show as `(xN)`, and failures past the limit are counted as "N additional failures suppressed" (`trace_ingest.rs:73-125, 1802-1868`).
  - A cause is cut to 256 chars for display, but deduplication uses the full text (`trace_ingest.rs:1794-1798`).
  - Attribute values in diagnostics are cut to **16 chars** (`trace_types.rs:21-34`), with a UTF-8-safe cut and a comment about panics from byte slicing.
  - The message is logged as a `Debug` field "so a newline in an attribute key cannot forge log lines" (`trace_ingest.rs:1053-1060`).
  - The format is `"Accepted N spans, rejected M spans: Rejected span <trace>:<span>: <cause>; …"`.
- **Response mapping** (`http/otlp.rs:332-376`):
  - With no rejection, the response is 200 with no `partial_success`.
  - When some spans are rejected, it is 200 with `partial_success{rejected_spans, error_message}`, but **only if `error_message` is `Some`** (`otlp.rs:353`).
  - When every span is rejected, it is **400** with a `google.rpc.Status` whose **`code: 0`** (OK) (`otlp.rs:363-366`). The metrics path uses `InvalidArgument` (`otlp.rs:308-311`), so the trace path looks like a bug. Partial success is logged at debug and a full rejection at warn, because a partial success repeats on every export (`trace_ingest.rs:1050-1074`).

## 5. Size limits and truncation

- **There is no truncation of attribute values or payloads.** Only the 64 MiB body limit applies (§1). Values of any size reach the JSONB, JSON2 or string columns unchanged. GreptimeDB truncates only **diagnostics** (§4).
- The other bounded thing is the number of failure messages. Nothing bounds attribute count, nesting depth or key length. In v1, every new key is a DDL `ADD COLUMN`.

## 6. LLM/GenAI handling

- **No ingest-time GenAI normalization.** `gen_ai.*` keys are ordinary dynamic attributes. They are not in the fixed-type list (`trace_semconv.rs`), and there is nothing for OpenInference, token counts or cost.
- GenAI appears only at **read time** in the "entity graph": the entity types `gen_ai.agent`, `gen_ai.model` and `gen_ai.tool` (`operator/src/statement/semantic_graph/conventions.rs:104-121`; `546625c45`, #8854).
- Real GenAI users hit type bugs in the dynamic typing:
  - **#7860**: `gen_ai.request.temperature = 0` arrives as `intValue` from JS (`Number.isInteger(0)`) after a `0.7` double had already created a Float64 column, and the whole batch was rejected. This was fixed by Int64→Float64 coercion and an auto `ALTER` (`a8fe6b5e4` #7870, `8d495909d` #7871; decision logic in `trace_types.rs:131-170`).
  - **#10138**: v2 JSON2 users (filtering on `gen_ai.request.metadata.*` keys) lost the fast key discovery they had with v1 columns.

## 7. Pipelines and transforms

- The pipeline engine (`src/pipeline`, a YAML processors-and-transforms DSL) is **not used for traces**. Any pipeline name other than the three built-in trace models returns `NotSupported("Unsupported pipeline for trace")` (`trace.rs:120-128`). The `_pipeline_handler` and `_pipeline_params` arguments are ignored (`frontend/src/instance/otlp.rs:221-224`). The pipeline engine applies to logs.
- So a "pipeline" for traces is just a **choice of storage model**. The models are versioned side by side. v0 is frozen ("duration_nano stays UInt64", `v0.rs:104-108`), and a table is stamped with its model and rejects writes from the other one (`operator::insert::validate_trace_table_model`, `trace_ingest.rs:899-913`). They carry three models forever, which is the compatibility burden our "no compatibility layers" rule avoids.

## 8. Tests and fixtures

- **Unit tests per layer:** AnyValue serialization (`attributes.rs:185-404`); parse grouping and `Arc` sharing checked with `std::ptr::eq` (`span.rs:321-470`); duration saturation (`v1.rs:775`); and failure classification, message dedup, caps and schema reconciliation, about 30 tests (`frontend/src/instance/otlp/trace_ingest/tests.rs:44-1340`).
- **Integration:** a full HTTP server per test (`tests-integration/tests/http.rs:7716` v0, `:7853` v2, `:10411` Jaeger over v2), plus `tests-integration/src/otlp.rs` (standalone and distributed). The inputs are **inline OTLP/JSON strings** (captured from `telemetrygen`) or `json!` literals, which are decoded with the proto serde, encoded to protobuf, POSTed, and checked with SQL queries.
- **No golden-file corpus, no proptest, no fuzzing of OTLP.** `tests-fuzz` targets DDL, insert, WAL and failover, not OTLP decoding. There is a CI load-regression runner (§3).

## 9. Mistakes and known issues

- **#7875** (closed): one bad span rejected the whole batch, even though a `BatchSpanProcessor` batch mixes unrelated traces. This was fixed by partial success (#7892), which came about 2.5 years after trace support (`44280f7c9`, #2627, 2023-10).
- **#7860** and **#6826**: dynamic per-attribute columns break when an SDK sends the same key with different types (int vs double, string vs number). Fixing it took a request-wide type-reconciliation subsystem with prevalidated rewrites, DDL, a re-check after concurrent creates, and a split into single spans (`README.md:116-150`; `3ae8d7d46`, #8485). **This complexity comes entirely from making attribute keys into typed columns.**
- **#5595 → #5622 → #7983 → #10138:** the trace data model has changed four times (v0 JSONB, v1 flattened, v2 JSON2, a proposed PK change). Each version solved one query need and lost another: v1 gave fast key discovery and v2 gave a stable schema.
- **#4918** (`0e0c4faf0`): attributes moved from strings to JSONB. **#6356** (`90a389456`): always write `parent_span_id`. These were early schema fixes.
- **#5605:** gRPC was removed, and #5402 to add it back is still open.
- Bugs still present at HEAD (my reading, not filed as far as I found): the v0 `u64` underflow (`v0.rs:113`); the v1 unchecked `u64 as i64` (`v1.rs:368`); no ID validation (`span.rs:217-224`); a 400 body with `code: 0` (`http/otlp.rs:364`); three different bytes encodings (§2); and the README/v2 events disagreement (§2).
- TODOs: events, links and link attributes should become "Map/List in the future" (`span.rs:48-49, 73, 147`). v2 events wait on `ARRAY(JSON2)` (`README.md:89`).

## 10. Recommendations for llmobs B1

**Adopt**
- **Upstream `opentelemetry-proto` prost types as the input contract**, not a hand-written model. Our `normalize(resource, scope, span, settings)` should take `&Resource`, `&InstrumentationScope` and `&Span` from that crate. GreptimeDB has had no decode bugs on protobuf.
- **Share resource and scope data per group, and borrow per span.** GreptimeDB had to add `Arc` sharing later (#9253). With a pure function taking references, we get that by design. Precompute resource-derived fields (`service_name`, `environment`, `release`) once per `ResourceSpans`.
- **Partial success from the start**, rejecting per span and never per batch (#7875). Rejection reasons should be a closed enum (`InvalidTraceId`, `InvalidSpanId`, …) returned by `llmobs-otel`. The handler crate turns them into a **bounded and deduplicated** message: at most N distinct entries, a count per reason, a "+K suppressed" tail, and no attribute values in it. That matches our privacy rule, which is stricter than GreptimeDB's 16-char excerpts.
- **Saturating duration math and an explicit timestamp policy.** Clamp `end < start` to a duration of 0 (do not reject), and return an error instead of wrapping on `u64 > i64::MAX`. Add a fixture for each case.
- **A decompressed-size limit enforced while reading the body** (GreptimeDB gets this from axum's `DefaultBodyLimit` after `RequestDecompressionLayer`). This belongs in the server crate, but the spec 02 413 rule depends on it.
- **A CI ingest benchmark** that tracks spans/s and rejection counters (#8631), once B1 is done.

**Avoid**
- **Attribute keys as typed columns** (v1). Spec 02 already uses maps. GreptimeDB's type-reconciliation machinery (#7860, #6826, #8485) shows the cost. For the normalized numeric fields we *do* type (tokens, `request_params`), **accept int and double interchangeably** (`intValue` 0 vs `doubleValue` 0.7 for `temperature`), and accept numeric strings where SDKs send them. Pin each case in a fixture.
- **No ID validation.** Spec 02 rule 15 (reject missing, wrong-length or all-zero trace and span IDs, and treat an all-zero parent as absent) fixes a real gap. Also decode hex IDs in OTLP/JSON with a length check: the proto crate's serde accepts any length.
- **Relying only on `with-serde` for OTLP/JSON.** It does not accept enum names (`"kind": "SPAN_KIND_CLIENT"`), which some senders emit. Either wrap the deserializer or test that we accept both forms. GreptimeDB avoided the problem by rejecting JSON, which we cannot do (spec 02 requires `application/json`).
- **More than one bytes encoding.** Use base64 everywhere (spec 02 rule 1) and test it inside nested kvlists and arrays.
- **No size or depth limits.** GreptimeDB has none for values, attribute counts or nesting. Keep our 8 MiB truncation, and add a recursion depth cap for `AnyValue` conversion (`kvlistValue` can nest arbitrarily; a deep nest is a stack-overflow vector in a recursive converter).
- **Duplicate-key ambiguity.** Define which value wins for repeated attribute keys (spans and kvlists) and pin it in a fixture. GreptimeDB's result depends on map insertion order.
- **Inline JSON fixtures inside test code.** Our `fixtures/otlp/` golden directories plus proptest ("never panics on arbitrary attribute maps") go beyond anything GreptimeDB has. Add a fuzz/proptest target that decodes arbitrary bytes into `ExportTraceServiceRequest` and normalizes the result, which would have caught the v0 underflow.
- **Status code bugs in error bodies.** A 400 must carry `google.rpc.Status{code: INVALID_ARGUMENT}`, not 0. Test the encoded error body, not only the HTTP status.
