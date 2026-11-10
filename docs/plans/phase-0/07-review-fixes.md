# Phase 0 · Step 8 — Fixes from the Phase 0 review

Implementation plan, split into small sub-phases. Each sub-phase is done by one agent in one go, in the order below, and ends with `just check` green and one commit.

**Prerequisites:** steps 1–7 merged (`docs: close Phase 0`).

**Read first:**
- `AGENTS.md`, all of it. Its rules apply to every line of this plan: no compatibility layers, no unnecessary abstractions, no placeholders, every dependency in the root `[workspace.dependencies]`, no `unwrap`/`expect` outside tests without an invariant comment, no payload content in errors.
- `docs/mvp-decisions.md`: D52, D64, D65, D73, D78, D79, and **D82** (written by R8; its text is below).
- The spec sections named in each sub-phase.

## Why this step exists

After Phase 0 was finished, a full review compared every plan (steps 1–7) with the code. It found:
- no blocking defects;
- a few real bugs, all small;
- places where the code decided something the spec does not say;
- tests that are weaker than they look.

The user answered the open questions. This plan fixes everything the review found, except the items listed in § Not in this plan.

## User decisions behind this plan

| Question | Answer |
|---|---|
| Message and part fields that keep an explicit JSON `null` (D65) are plain `string` in `openapi.json` | Make those fields nullable in the schema. D65 is unchanged. |
| Specs 01/05 and D73 list `tags` on sessions; `SessionSummary` has none | Add `tags`: the sorted, de-duplicated union of the session's trace tags. |
| Default exclusions (spec 05) apply to sessions, users and metrics, but only traces and spans accept `include_eval` / `include_experiments` | Add both parameters to the session list and detail, the user list and detail, and metrics. |
| Four rules exist only in the fixtures (`ai.settings.*`, `ai.response.text` by span kind, the Vercel `finish_reason` source, `input.mime_type`) | Write them into spec 02. |
| Optional fields the server omits are generated as `T \| null` in `schema.gen.ts` | Omitted optionals are not nullable. Only fields that can really be `null` stay nullable. |
| A matching price row has a null unit price for tokens that are present; `cost_output` on embedding spans | No fallback (except the existing `reasoning` → `output`): that part and `cost_total` are null. Embedding `cost_output` is null. (D82) |
| `.cursor/` is untracked while D58 says it is kept | Ignore it. This plan does not touch `.cursor/` or its Open-list entry. |
| Which `LLMOBS_SAMPLE_RATE` strings both SDKs accept | Plain ASCII decimal: `^(?:[0-9]+(?:\.[0-9]*)?\|\.[0-9]+)$`, then the `[0, 1]` range check. |

One detail differs from the wording of the Vercel question. The question proposed reading `finish_reason` from `ai.response.finishReason` first. Spec 02 § Convention detection says GenAI attributes take precedence over Vercel ones. So R8 reads `gen_ai.response.finish_reasons` first and falls back to `ai.response.finishReason`. The existing fixture carries both, with the same value.

## Decision this plan adds

| # | Decision |
|---|---|
| D82 (new, R8) | **Cost with missing unit prices.** When a price row matches, every token count that is present and greater than 0 needs a unit price. `reasoning` falls back to `output`; nothing else falls back. If a needed unit price is null, the part it belongs to (`cost_input` or `cost_output`) is null, and so is `cost_total`. The other part is still computed, and `cost_source` stays `computed`. Embedding spans use the input price only: `cost_output` is null and `cost_total = cost_input`. User decision. |

## Sub-phases

| # | Area | Commit |
|---|---|---|
| R1 | Core: message `extra` precedence, strict RFC 3339, enum errors | `core: fix message extra precedence, strict RFC 3339 and enum errors` |
| R2 | Core and workspace housekeeping | `core: tidy test layout, shared struct fields and arrow features` |
| R3 | API nullability | `api: keep D65 nulls in the schema and stop marking omitted fields nullable` |
| R4 | Session tags | `api: add tags to sessions` |
| R5 | Exclusion parameters on sessions, users and metrics | `api: accept include_eval and include_experiments on sessions, users and metrics` |
| R6 | API contract tests and small fixes | `api: validate examples against the OpenAPI document` |
| R7 | OTLP fixture validation | `fixtures: tighten OTLP fixture validation` |
| R8 | Spec 02 rules and D82 | `docs: pin Vercel and OpenInference rules and record D82` |
| R9 | New and adjusted OTLP cases | `fixtures: pin zero parent ids and payload size after media extraction` |
| R10 | SDK configuration parity | `sdk: align base URL and sample-rate parsing across SDKs` |
| R11 | Web fixes | `web: fix theme sync, project list loading and shadcn setup` |
| R12 | Close-out | `docs: close the Phase 0 review` |

R3, R4 and R5 must land before R6: R6 validates the examples against the OpenAPI document, and that only passes once the document is right. R7 must land before R8 and R9, which change the validator and add cases to it. The other sub-phases are independent, but run them in order anyway, one at a time.

Every sub-phase that changes a Rust API type runs `just openapi` and `just api-examples` and commits the regenerated `crates/server/openapi.json`, `web/src/api/schema.gen.ts`, `fixtures/api/responses/**` and `fixtures/api/manifest.json` in the same commit. Never edit those files by hand.

---

## R1 — Core: message `extra` precedence, strict RFC 3339, enum errors

**Edit only:** `crates/core/src/{message.rs,time_serde.rs,enums.rs,error.rs}` and their tests, plus `crates/server/src/api/common.rs` (for the time parser only).

### 1. Modelled fields win over `extra` on output

`object()` in `crates/core/src/message.rs` (around line 432) inserts the typed fields first and then every `extra` entry with `map.insert`. That overwrites a typed field when `extra` holds the same key. Example:
- a message is parsed from `{"role":"user","name":null,"parts":[]}`;
- `name` becomes `None` and `"name": null` stays in `extra` (D65, so the JSON round-trips);
- a caller then sets `message.name = Some("x")`;
- serialization still writes `"name": null`.

The same happens for `id`, `modality` and `mime_type` on parts, and for an `extra` key `"type"` on any part.

Fix: in `object()`, skip every `extra` key that is already among `fields`. Keep the key order of spec 01 § Canonical message format and plan 02 C4: modelled fields first, then the remaining `extra` entries in their stored order. Update the doc comment of `leftovers` and `object` to state the rule: "a modelled field that has a value always wins; an explicit `null` is written back only while the modelled field is `None`".

Tests (in `message.rs`):
- `typed_name_wins_over_null_in_extra`: parse the message above, set `name = Some("x")`, serialize, and assert the JSON is exactly `{"role":"user","name":"x","parts":[]}`.
- `typed_part_fields_win_over_extra`: build a `Part::Uri` whose `extra` holds `"type": "blob"` and `"modality": null`, with `modality = Some(Modality::Image)`. Assert the JSON has `"type": "uri"` and `"modality": "image"`, and each key appears once.
- `explicit_null_still_round_trips`: the existing round-trip tests for `"name": null` and `"id": null` must still pass unchanged. Do not edit them.

### 2. One strict RFC 3339 parser

`time_serde.rs` (line 45) and `TimeParam` in `crates/server/src/api/common.rs` (line 245) both call chrono's `DateTime::parse_from_rfc3339`. chrono is more lenient than plan 02 C5 ("0–9 fractional digits"):
- it truncates more than 9 fractional digits without error;
- it accepts a space as the date/time separator;
- it accepts second `60` (leap second).

Add one public function in `crates/core/src/time_serde.rs`:

```rust
/// Parses an RFC 3339 timestamp into nanoseconds since the Unix epoch.
///
/// Stricter than chrono: the separator is `T` or `t`, at most 9 fractional digits,
/// no leap second, any `Z`/`z` or `±hh:mm` offset. The error never contains the input.
pub fn parse_rfc3339(text: &str) -> Result<i64, TimeError>
```

- `TimeError` is a small `thiserror` enum in `crates/core/src/error.rs`, or a new variant of `CoreError` if that reads better. Its messages are static (`"invalid RFC 3339 timestamp"`, `"timestamp out of range"`). It must not include the input.
- Check the shape before calling chrono:
  - byte 10 is `T` or `t`;
  - if a `.` follows the seconds, count the digits up to the offset and reject more than 9.
- After parsing, reject a leap second. chrono represents it as `nanosecond() >= 1_000_000_000`.
- Convert to `i64` nanoseconds with checked arithmetic, as today. Out of range is an error, not a panic.
- The serde `deserialize` in `time_serde.rs` and `TimeParam`'s RFC 3339 branch both call `parse_rfc3339`. Delete their own chrono calls. `TimeParam` keeps its own error message (`TIME_ERROR`), which also covers epoch milliseconds; map any `TimeError` to it.

Tests in `time_serde.rs`, one per rule:
- accepted: `2026-01-01T00:00:00Z`, `2026-01-01t00:00:00z`, 0 fractional digits, 9 fractional digits, `+05:30` and `-00:00` offsets;
- rejected: 10 fractional digits, a space separator, `23:59:60Z`, a missing offset, and a year outside the `i64` nanosecond range.

Each rejection asserts that the error message does not contain the input. Add one test in `common.rs` showing that `TimeParam` rejects 10 fractional digits.

### 3. Stored-enum serde errors don't repeat the input

The `stored_enum!` macro in `crates/core/src/enums.rs` derives `serde::Deserialize`. Serde's "unknown variant" error repeats the whole rejected string. `parse()` already maps to `CoreError::UnknownVariant` with the value truncated to 64 characters.

- Remove `serde::Deserialize` from the derive list in the macro.
- Generate a `Deserialize` impl in the macro: deserialize a `String` (or `&str` via a visitor), call `Self::parse`, and map the error with `serde::de::Error::custom(err)`.
- Keep `Serialize`, the `strum` derives and `ToSchema` as they are.

Test in `enums.rs`: deserializing `"\"" + 200 × 'x' + "\""` as `SpanKind` fails, the message contains at most 64 `x` characters, and it names the kind (`span_kind`). The existing snapshot of the enum spellings must not change.

### Done when

- The new tests pass.
- `just check` is green, and the `crates/core/tests/snapshots` files did not change.

---

## R2 — Core and workspace housekeeping

**Edit only:** `crates/core/**`, root `Cargo.toml`, `Cargo.lock`, `docs/spec/01-data-model.md`.

1. **`lib.rs` holds only the crate doc and module declarations** (plan 02 layout).
   - Move the `#[cfg(test)] mod schema_drift { … }` block (`crates/core/src/lib.rs`, lines 20–188) to `crates/core/src/schema_drift.rs`.
   - Declare it in `lib.rs` as `#[cfg(test)] mod schema_drift;`.
   - The tests in it must stay unchanged.
2. **One definition of the event and link struct fields.**
   - `batch.rs` (`event_list`, `link_list`) redefines the struct fields that `schema.rs` (`event_struct`, `link_struct`) already defines.
   - Expose the field lists from `schema.rs` as `pub(crate)` functions returning `Fields`, and build the batch builders from them.
   - The `batch.schema() == spans_schema()` tests already guard the result; they must still pass.
3. **`arrow` without unused default features.**
   - `arrow = "59.2"` brings in `arrow-csv` and `arrow-json`, which no crate uses. Plan 02 C6 allowed this only with a report note.
   - In the root `[workspace.dependencies]`, write `arrow = { version = "59.2", default-features = false }`.
   - In `crates/core/Cargo.toml`, enable only the features core needs. Find them by building. The IPC round-trip test probably needs `ipc`; enable that in `[dev-dependencies]` if only tests use it.
   - Keep the version unchanged: the DataFusion family stays pinned together (AGENTS.md).
   - Done when `cargo tree -e features -p llmobs-core` shows no `arrow-csv` and no `arrow-json`.
4. **Remove `prost` from the root `[workspace.dependencies]`.**
   - No crate uses it. Check with `grep -rn prost crates/*/Cargo.toml`.
   - It comes back with the crate that needs it (AGENTS.md § Dependencies).
5. **Spec 01 § Canonical message format:** add one sentence.

   > A message without `parts` is read as an empty list and written back with `"parts": []`. This is the only case where the JSON does not round-trip exactly.

Done when `just check` is green and `Cargo.lock` lost only `prost` and the arrow sub-crates core no longer needs.

---

## R3 — API nullability

**Edit only:** `crates/core/src/message.rs` (the schema functions), `crates/server/src/api/**`, `crates/server/tests/openapi.rs`, the generated files, `web/src/**` (only to fix type errors caused by the regenerated types), `docs/spec/09-api.md`, and `docs/mvp-decisions.md` (Open list).

Spec 09 says an absent optional field is omitted, never `null`. Today utoipa marks every `Option<T>` field and query parameter as nullable:
- 93 optional response properties are nullable (for example `Kpi.value`, `SpanSummary.model`);
- 34 query parameters are nullable;
- the generated TypeScript says `field?: T | null`.

Meanwhile the D65 message fields, which really can be `null`, are typed as plain strings.

### 1. D65 message and part fields are nullable

In the hand-written schemas in `crates/core/src/message.rs`, make these properties nullable. They are exactly the `optional` lists passed to `leftovers()`:

| Component | Properties |
|---|---|
| `Message` | `name`, `finish_reason` |
| `ToolCallPart` | `id` |
| `ToolResultPart` | `id` |
| `UriPart` | `modality`, `mime_type` |
| `BlobPart` | `modality`, `mime_type` |

Check the component names against `part_components()` and use the real ones.
- A string property becomes `type: ["string", "null"]`.
- `modality` (a `$ref` to `Modality`) becomes `oneOf: [{"$ref": "#/components/schemas/Modality"}, {"type": "null"}]`.
- Each description ends with: "Omitted when absent. An explicit `null` from the source is kept (D65)."

### 2. Omitted optionals are not nullable

Add one plain function in `crates/server/src/api/openapi.rs` and call it once where the document is built:

```rust
/// Spec 09: an absent optional field is omitted, never `null`.
///
/// utoipa marks every `Option<T>` nullable. This removes `null` from every property
/// that is not `required` and from every optional query parameter. The D65 message and
/// part components are skipped: their optional fields can really be `null`.
fn drop_null_from_omitted_optionals(openapi: &mut utoipa::openapi::OpenApi)
```

- For a property schema whose type is a list containing `null`, remove `null`. If one type remains, write it as a single type.
- For `oneOf: [X, {"type": "null"}]` (either order), replace the wrapper with `X`. Keep the wrapper's `description`, `default` and `example` if it has them. If `X` is a `$ref` and the wrapper has a description, keep the description by wrapping the ref in a one-element `allOf`, which is what utoipa does elsewhere for described refs. Pick whichever form the generated TypeScript renders as `field?: X`.
- Skip `Message` and the seven part components. Take their names from core (`<Message as ToSchema>::name()` and the part component list), not from string literals.
- Required properties are not touched. The seven `*Page.next_cursor` fields stay required and nullable: the field is always present, and `null` means "no more pages".
- Walk nested inline object schemas too (properties of properties), not only top-level component properties.

A per-field `#[schema(nullable = false)]` / `#[param(nullable = false)]` would also work (utoipa-gen 6.0.1 honours it). It is not used here because it would be 127 annotations, and every new field would need one too.

### 3. Every omitted optional really is omitted

For every `Option<…>` field of a type that appears in a response (`crates/server/src/api/**`, and core `Span`/`Score`/`Message` as exposed), check that it has `#[serde(default, skip_serializing_if = "Option::is_none")]`. One that doesn't would send `null` while the schema now says it can't. For each such field, either:
- add `skip_serializing_if`, or
- if `null` is meaningful there, mark it `required` and nullable in the schema, and say why in its doc comment.

List every field you changed in the report. R6 adds the test that catches any you miss.

### 4. Lint test

In `crates/server/tests/openapi.rs`, add to the existing document lint:
- No query parameter of any operation is nullable.
- A component property is nullable only if it is `required`, or its component is `Message` or one of the seven part components.
- The exact set of nullable properties in the D65 components matches the table in step 1.
- The exact set of required nullable properties is the seven `*Page.next_cursor` fields.

### 5. Regenerate and fix the UI

- Run `just openapi`.
- In `web/src/api/schema.gen.ts`, optional fields should now read `field?: T`, and query parameters `param?: T`.
- Fix any TypeScript error in `web/src/**` this causes, for example code passing `null` for an absent query parameter (pass `undefined` or omit the key). Do not cast.

### 6. Docs

- **Spec 09 § Response shapes:** after the sentence that says absent optionals are omitted, add:

  > In the OpenAPI document an optional field is therefore not nullable. Two exceptions: the message and part fields that keep an explicit source `null` (D65, spec 01), and `next_cursor`, which is always present and `null` on the last page.

- **`docs/mvp-decisions.md` § Open:** remove the item "Optional query parameters are generated as `T | null` …".

### Done when

- The lint test passes.
- Every remaining `| null` in `web/src/api/schema.gen.ts` belongs to a D65 field or a `next_cursor` (`grep -n '| null'`, then check each line).
- `just check` is green.

---

## R4 — Session tags

**Edit only:** `crates/server/src/api/sessions.rs`, `crates/server/tests/{api_examples.rs,support/mod.rs}`, `fixtures/api/dataset/spans.json`, `fixtures/api/README.md` (only if it describes the dataset traces), and the generated files.

Spec 01 § Session lists `tags` among the session aggregates ("distinct union"), as do spec 05's `sessions` view and D73. `SessionSummary` has no `tags`, and the module comment at the top of `sessions.rs` says the list "does not show tags". That comment contradicts the spec; delete it.

1. Add to `SessionSummary`, after `user_id`:

   ```rust
   /// Sorted distinct union of the session's trace tags.
   pub tags: Vec<String>,
   ```

   Serialize it the same way `TraceSummary.tags` is serialized: check `traces.rs` around line 78 and use the same serde attributes, so an empty list is handled identically in both.
2. In `crates/server/tests/support/mod.rs`, `session_summary()` computes `tags` as the sorted, de-duplicated union of `trace.tags` over the session's traces. It uses the same traces the other aggregates use, so default exclusions apply to tags too.
3. **Dataset:** today no span has tags. Add tags to three root spans in `fixtures/api/dataset/spans.json`, keeping the file's key order and formatting:

   | Span | Tags |
   |---|---|
   | `b100000000000011` (trace 1 root) | `["beta"]` |
   | `b100000000000021` (trace 2 root) | `["beta", "refund"]` |
   | `b100000000000051` (trace 5 root, `llmobs-eval`) | `["eval"]` |

   Session `sess-001` then has tags `["beta", "refund"]` by default. With `include_eval=true` (R5) they are `["beta", "eval", "refund"]`.
4. Add a scenario assertion in `api_examples.rs` next to the existing ones: `sess-001` has `tags == ["beta", "refund"]`, and `sess-002` has `tags == []`.
5. Run `just api-examples` and `just openapi`. `traces.json` and the trace details change too, because trace tags come from span tags. That is expected.

Done when the scenario assertion passes and `just check` is green.

---

## R5 — Exclusion parameters on sessions, users and metrics

**Edit only:** `crates/server/src/api/{common.rs,sessions.rs,users.rs,metrics.rs,openapi.rs}`, `crates/server/tests/{api_examples.rs,openapi.rs,support/mod.rs}`, `docs/spec/09-api.md`, and the generated files.

Spec 05 § Default exclusions applies to "list pages, user aggregates, dashboard series and alert queries" unless the request opts in. Spec 09 § Response shapes says the same for the session and user lists and details. Today only `list_traces`, `stream_traces` and `list_spans` accept `include_eval` and `include_experiments`. The other params structs use `deny_unknown_fields`, so sending either parameter is a 400.

1. **List and metrics params:** add both fields, with the same doc comments and serde attributes as `TraceListParams` (`traces.rs`, around line 750), to `SessionListParams`, `UserListParams` and `MetricsParams`. Their doc comments say "traces" or "sessions" as fits; the defaults stay `false`.
2. **Detail params:** `get_session` and `get_user` have no query parameters today. Add one struct in `common.rs` and use it for both operations:

   ```rust
   /// Query parameters of the session and user detail endpoints. Spec 05 § Default exclusions.
   #[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, IntoParams)]
   #[serde(deny_unknown_fields)]
   pub struct ExclusionParams {
       /// Include traces from the evaluation environment. Default false.
       #[serde(default)]
       pub include_eval: bool,
       /// Include experiment traces. Default false.
       #[serde(default)]
       pub include_experiments: bool,
   }
   ```

   It is not flattened into the list params: serde's `flatten` does not work with `deny_unknown_fields`.
3. **OpenAPI descriptions:**
   - Attach the `EXCLUSIONS` description (`openapi.rs`, around line 570) to `list_sessions`, `get_session`, `list_users`, `get_user` and `get_metrics`. Where the operation already has a description (`get_user`: "Sessions are the user's 100 most recent."), put `EXCLUSIONS` first, then the existing text.
   - For `get_metrics`, add: "The evaluation cost series always counts evaluation traces (spec 05 § Dashboard)." Check the section name in spec 05 and cite the real one.
4. **Lint** (`tests/openapi.rs`): the set of operations that have an `include_eval` parameter is exactly
   `{list_traces, stream_traces, list_spans, list_sessions, get_session, list_users, get_user, get_metrics}`.
   - Each of them also has `include_experiments`.
   - Each description starts with the `EXCLUSIONS` text.
5. **Examples** (`api_examples.rs` `build()`): add these manifest entries, following the `traces-include-eval.json` pattern:

   | Operation | Params | Query | File |
   |---|---|---|---|
   | `list_sessions` | — | `include_eval=true` | `responses/sessions-include-eval.json` |
   | `get_session` | `session_id=sess-001` | `include_eval=true` | `responses/sessions/sess-001-include-eval.json` |
   | `list_users` | — | `include_eval=true` | `responses/users-include-eval.json` |
   | `get_user` | `user_id=user-42` | `include_eval=true` | `responses/users/user-42-include-eval.json` |

   - The support helpers take the visible trace set as an argument, instead of always filtering with `is_default_visible`. Keep one helper per aggregate; don't add a parameterised builder abstraction.
   - Add scenario assertions:
     - default `sess-001` has `trace_count` 2;
     - with `include_eval`, `sess-001` has `trace_count` 3 and tags `["beta", "eval", "refund"]`;
     - `user-42`'s `trace_count` goes from 3 to 4.

     Check these counts against the dataset table in plan 04 and fix the numbers here if the dataset says otherwise.
   - No metrics example is added: `static/metrics.json` stays the only one.
6. **Spec 09:**
   - § Observability data table: the rows for `GET /sessions` · `GET /sessions/{session_id}`, `GET /users` · `GET /users/{user_id}` and `GET /metrics` gain "`include_experiments=false`, `include_eval=false`", written like the `GET /traces` row.
   - § Response shapes, the "Default exclusions" bullet becomes:

     > **Default exclusions** (05) apply to the trace, span, session and user lists, to session/user detail and to metrics; not to trace/span detail or scores. Every endpoint they apply to accepts `include_eval` and `include_experiments`.

7. Run `just api-examples` and `just openapi`.
8. Check that the MSW mocks pick the right file. `web/src/mocks` selects the entry with the largest matching query subset, so a request without `include_eval` must still get the default file. `just check-web` runs `manifest.test.ts`; add a case there if no existing test covers two entries for one operation that differ only by query.

Done when the lint and scenario assertions pass and `just check` is green.

---

## R6 — API contract tests and small fixes

**Edit only:** `crates/server/**`, `fixtures/api/static/schema.json`, root `Cargo.toml` (one dev dependency), `Cargo.lock`, and the generated files.

### 1. Validate every example against the OpenAPI document

Today the fixture tests check that each response round-trips through its Rust type. That proves serde symmetry, not that the JSON matches `openapi.json`. That gap is how the D65 nullability bug went unnoticed.

- Add `jsonschema` as a dev-dependency of `llmobs-server`. Look up the latest version released at least 7 days ago with `cargo search jsonschema` and `cargo info jsonschema`.
  - Declare it in the root `[workspace.dependencies]` with `default-features = false`. Enable only what's needed for local `$ref` resolution, with no HTTP or file resolvers.
  - `just deny` must pass.
  - Report the version and the features you enabled.
- New test `examples_match_the_openapi_document` in `crates/server/tests/api_examples.rs`. For every manifest entry whose `content_type` is `application/json`:
  - look up the operation by `operation_id` in `crates/server/openapi.json` (the committed file, which `contract_file_is_up_to_date` already ties to the code);
  - take the response schema for the entry's `status` and content type;
  - validate the file's JSON against it, with `#/components/schemas/...` refs resolved against the whole document (use the document as the root resource; OpenAPI 3.1 schemas are JSON Schema 2020-12);
  - collect every failure as `<file>: <instance path>: <error kind>`, then fail once with all of them. Do not print instance values: example data is synthetic, but the habit matters.
- This covers both `responses/` and `static/` files, since both are in the manifest.

### 2. Check the manifest against the operations

In the same file, a test `manifest_matches_the_operations` checks, for every entry:
- `operation_id` exists and is a `GET`;
- the `params` keys are exactly the operation's path parameters;
- every `query` key is a declared query parameter of the operation;
- `status` is a declared response of the operation;
- `content_type` is declared for that status.

### 3. No unchecked files

A test `every_fixture_file_is_used` fails on:
- any file under `fixtures/api/static/` that no manifest entry names;
- any file under `fixtures/api/media/` that no manifest entry and no dataset span `uri` part references.

Only `responses/` has a missing/extra/different check today.

### 4. Write switches need `=1`

`tests/openapi.rs` (around line 122) and `tests/api_examples.rs` (around line 478) write files when `LLMOBS_OPENAPI_WRITE` / `LLMOBS_API_EXAMPLES_WRITE` is set to any value, including `0`.
- Write only when the value is exactly `1`.
- Any other non-empty value fails the test with: "set LLMOBS_OPENAPI_WRITE=1 to write, or unset it" (and the same for the other variable).

The `just` recipes already pass `=1`.

### 5. Small fixes

- `common.rs` around line 80: the `Problem.title` doc comment contains the rustdoc link "[`Self::status`]". It ends up verbatim in `openapi.json`. Write "`status`" as plain code instead.
- Remove the `ToSchema` derive from the nine query-params structs that are never registered as components: `TraceListParams`, `TraceStreamParams`, `TraceDetailParams`, `SpanListParams`, `SessionListParams`, `UserListParams`, `ScoreListParams`, `SavedViewListParams` and `MetricsParams`. Keep `IntoParams`. If one of them turns out to be needed, keep that derive and say why in the report.
- `ScoreBrief` doc (`traces.rs`, line 25) says "trace, span, session, or user row". Only traces and sessions carry scores; write "trace or session row".
- `fixtures/api/static/schema.json`: the `traces` view marks column `name` as nullable. `TraceSummary.name` is not optional, and a trace without a root takes its earliest span's name (spec 01 § Root semantics). Set it to not nullable.
- `SavedView.state` is a `serde_json::Value` while `CreateSavedView.state` is a `Map`. Spec 09 says the server only checks that it is an object. Make `SavedView.state` and `UpdateSavedView.state` (an `Option`) a `serde_json::Map<String, Value>` too, so a fixture with a non-object `state` fails to parse.

Done when the three new tests pass and `just check` is green. If the schema validation finds a real mismatch that R3–R5 missed, fix the type, not the test, and list it in the report.

---

## R7 — OTLP fixture validation

**Edit only:** `crates/otel/tests/**`, `fixtures/otlp/README.md`.

### 1. `input.json` rejects unknown keys

`opentelemetry-proto` 0.33.1 uses `#[serde(default)]` without `deny_unknown_fields`. A misspelled key such as `startTimeUnixNanos` is silently dropped, and the field becomes 0 or empty. Today `input_json()` (`crates/otel/tests/fixtures.rs`, line 51) only checks that there is at least one span.

Add a walker over the raw JSON (`serde_json::Value`) that checks every object against the OTLP/JSON key set of its message type. Write the table as plain data in the test support module:

| Object | Allowed keys |
|---|---|
| request | `resourceSpans` |
| `ResourceSpans` | `resource`, `scopeSpans`, `schemaUrl` |
| `Resource` | `attributes`, `droppedAttributesCount`, `entityRefs` |
| `ScopeSpans` | `scope`, `spans`, `schemaUrl` |
| `InstrumentationScope` | `name`, `version`, `attributes`, `droppedAttributesCount` |
| `Span` | `traceId`, `spanId`, `traceState`, `parentSpanId`, `flags`, `name`, `kind`, `startTimeUnixNano`, `endTimeUnixNano`, `attributes`, `droppedAttributesCount`, `events`, `droppedEventsCount`, `links`, `droppedLinksCount`, `status` |
| `Span.Event` | `timeUnixNano`, `name`, `attributes`, `droppedAttributesCount` |
| `Span.Link` | `traceId`, `spanId`, `traceState`, `attributes`, `droppedAttributesCount`, `flags` |
| `Status` | `message`, `code` |
| `KeyValue` | `key`, `value` |
| `AnyValue` | exactly one of `stringValue`, `boolValue`, `intValue`, `doubleValue`, `arrayValue`, `kvlistValue`, `bytesValue` |
| `ArrayValue` | `values` |
| `KeyValueList` | `values` |

- Check the table against the `.proto` files of the pinned `opentelemetry-proto` version (in the cargo registry source). If that version has a field this table lacks, add it and say so in the report.
- A failure reads `<case>: input.json <json path>: unknown key "<key>"`, or `… : AnyValue must have exactly one value key`. Keys are not payload, so they can be printed.
- Add a unit test of the walker:
  - a valid request passes;
  - `startTimeUnixNanos` on a span fails with the right path;
  - an `AnyValue` with two value keys fails.

### 2. Report every broken case

`load_all_cases()` (`support/mod.rs`, line 217) panics on the first `case.json` that fails to parse, so one bad case hides every other failure. Plan 03 asked for that panic, but it contradicts the same plan's "report every failing case".
- Make it return the loaded cases together with a list of load failures.
- Every test reports the load failures with its own.
- A broken case is skipped by the checks that need it; the other cases are still checked.

### 3. Checks that are missing

Add each as a failure collected per case:
- **Spec 02 rule 3:** a span whose kind is not `llm` or `embedding` has all token fields absent, all cost fields absent and no `cost_source`.
- A span with no `cost_source` has all cost fields absent.
- `expected.media` has no duplicate `sha256`. Today sorted-equals-sorted passes duplicates (`fixtures.rs`, line 472).
- `settings.media_min_bytes` ≤ `settings.media_max_bytes`, after defaults are applied.
- Group and case directory names match `^[a-z0-9]+(?:[-_][a-z0-9]+)*$`. The README allows `google_adk` as a group, so groups accept `_`; case names are kebab-case and accept only `-`. Use two patterns and say which in the message.
- A group directory with no case directory is a failure ("create a group directory only when it contains a case", README).
- `input.json` is at most 256 KiB (README § Writing cases).
- **Timestamp window:** every span starts and ends before the case's `ingest_time`, and starts no earlier than `ingest_time − 10 s`. Before adding this check, read the README and plan 03 for the exact window: the review recalled "start ≥ 23:59:50Z, end before `ingest_time`". If the README states no window, add this one to the README in the same commit.

Each new check gets one negative unit test on an in-memory case, next to the existing negative tests if there are any. Otherwise put them in a `#[cfg(test)]` block in the support module.

### 4. Stale comment

`support/mod.rs` line 349 says `time_serde` "borrows a `&str`". `time_serde` deserializes an owned `String` now (and after R1, calls `parse_rfc3339`). Remove the workaround the comment justifies. Deserialize the timestamp straight from the `serde_json::Value`, or call `llmobs_core::time_serde::parse_rfc3339` directly, then fix the comment.

### 5. OTLP/JSON conformance test

In `crates/otel/tests/otlp_json.rs`, add cases that decode:
- a `kvlistValue` (nested `values`);
- a `bytesValue` (base64);
- an `intValue` written both as a JSON string (`"42"`) and as a JSON number (`42`).

Assert the decoded `AnyValue`. If the pinned decoder rejects one of these forms, the test asserts the rejection, and the report says so: that is a finding for the ingestion track.

Done when every current case passes, the new negative tests fail for the right reason, and `just check` is green.

---

## R8 — Spec 02 rules and D82

**Edit only:** `docs/spec/02-ingestion.md`, `docs/mvp-decisions.md`, `crates/otel/tests/**`, `fixtures/otlp/README.md` (if it restates cost rules), and `fixtures/api/dataset/spans.json` (only if step 4 finds a span to fix).

### 1. Spec 02 text

Make these edits. Keep the surrounding wording.

- **§ Input / output, Vercel row:** replace the Output cell with:

  > On `llm` spans: `ai.response.text` + `ai.response.toolCalls` → one assistant message (finish reason: rule 16). On other kinds: `ai.response.text`, else `ai.response.object`, as a non-message value (rule 10). Tool: `ai.toolCall.result`

- **§ Normalization rules, rule 2:** add a fourth bullet:

  > - *Format hints* (`input.mime_type`, `output.mime_type`) are not payload attributes. They are consumed attributes and follow the previous bullet, even when the payload they describe was dropped.

- **Rule 3:** change the last sentence to:

  > `provider`, `request_model`, `response_model`, `operation` and `request_params` are normalized on every kind.

- **Rule 8:** append:

  > Vercel `ai.settings.<key>` contributes `<key>` unchanged (no case conversion: `ai.settings.maxRetries` → `maxRetries`), with its OTLP value type (`intValue` → JSON number). The source attributes are consumed attributes (rule 2).

- **Rule 10:** change the last sentence to:

  > So Vercel `ai.prompt` (JSON object string) is `json`, and `ai.response.text` on a non-`llm` span is `text`.

- **Add rule 16:**

  > 16. **Finish reason of a message built from flat attributes** (Vercel `doGenerate`/`doStream`): the first element of `gen_ai.response.finish_reasons` when present, else `ai.response.finishReason`. GenAI wins by the precedence of § Convention detection. Both source attributes are consumed attributes (rule 2).

- **§ Cost computation:**
  - Replace step 4 with:

    > 4. `cost_input = uncached_input·input + cache_read·cache_read_price + cache_write·cache_write_price`; `cost_output = non_reasoning_output·output + reasoning·reasoning_price`; all /1e6. A term whose token count is 0 or absent contributes 0, even if its unit price is null. A term whose token count is greater than 0 and whose unit price is null (after `reasoning` → `output`) makes its part null; nothing else falls back. `cost_total = cost_input + cost_output`, null if either is null. `cost_source` stays `computed` (D82).

  - Replace "Embedding spans use `input` price only." with:

    > Embedding spans use the `input` price only: `cost_output` is null and `cost_total = cost_input` (D82).

### 2. D82

Add the D82 row from § Decision this plan adds to the decision table in `docs/mvp-decisions.md`, after D81. Remove these two items from § Open:
- "When a price row matches but a unit price is null …";
- "For embedding spans, is `cost_output` always null or always 0 …".

### 3. Validator

Change `recompute`, `charge` and `compare_cost` in `crates/otel/tests/fixtures.rs` (around lines 600–665) to follow the new rules:
- `charge(tokens, price) -> Option<f64>`: `Some(0.0)` when tokens is 0, `None` when the price is null, otherwise `Some(tokens * price)`.
- `recompute` returns `(Option<f64>, Option<f64>, Option<f64>)` for input, output and total:
  - a part is `None` if any of its terms is `None`;
  - total is `None` if either part is;
  - for embedding spans, output is `None` and total equals input.
- `compare_cost` takes an expected `Option<f64>`:
  - both `None` passes;
  - one `None` fails with "… is absent; recomputed …" or "… is present; recomputed absent";
  - two numbers compare within `1e-12`, as today.
- Delete the "price row … cannot price the token counts" failure; that case is now a valid null part.
- Add unit tests for `recompute`:
  - cache-write tokens on a row with a null cache-write price give null input and total, with output still computed;
  - an embedding span gives null output;
  - reasoning tokens with a null reasoning price use the output price.

### 4. Check existing data

- `grep -rn '"embedding"' fixtures/` for embedding spans in `fixtures/otlp/**/expected.json` and `fixtures/api/dataset/spans.json`.
- A span of kind `embedding` with `cost_output: 0` (or any value) must have it removed.
- If you change `fixtures/api/dataset/spans.json`, run `just api-examples` in the same commit.
- Report what you found, even if nothing changed.

Done when the validator tests pass, every case still passes, and `just check` is green.

---

## R9 — New and adjusted OTLP cases

**Edit only:** `fixtures/otlp/**`.

Follow `fixtures/otlp/README.md` § Workflows for hand-written cases: write `case.json` and `input.json`, write `expected.json` by hand from spec 02, run `just fixtures-fmt`, then `just fixtures-check`.

1. **New case `edge/zero-parent-span-id`:** pins rule 15's last sentence ("An all-zero `parentSpanId` is treated as absent").
   - One trace with two spans:
     - the root has `parentSpanId` `"0000000000000000"`;
     - its child points at the root.
   - Use `chain` kind spans (no tokens, so no cost) and the `llmobs` convention, or no convention.
   - In `expected.json`, the root has no `parent_span_id` and the child keeps its parent. Nothing is rejected.
   - `spec_refs`: `"02 § Normalization rules / 15"`.
2. **New case `edge/media-before-truncation`:** pins rule 14's "Payloads are measured **after** media extraction".
   - The existing `edge/media-truncation-rejection` case can't show this: its 70-byte PNG is about 96 base64 characters, barely longer than the `media://<sha256>` reference that replaces it.
   - Generate a valid PNG of about 600–900 bytes, deterministically:
     - a Python stdlib script, run once and not committed;
     - 16×16 RGB with a fixed pattern;
     - `zlib` level 0;
     - write its exact size and sha256 in the case `notes`.
   - `input.json`: one `llm` span with a GenAI `gen_ai.input.messages` attribute holding one user message with a short text part and that image as an inline blob part.
   - `settings`:
     - `media_min_bytes` below the PNG size;
     - `max_attribute_bytes` = 512, or any value such that the message is larger than the limit before extraction and smaller after.

     Compute both sizes and write them in `notes`.
   - `expected.json`:
     - the input message has a `uri` part with `media://<sha256>`, not truncated;
     - `media` has the one entry.

     Do not change the existing media case.
3. **`genai/chat-cache-tokens`:** the scope name `opentelemetry.instrumentation.anthropic` belongs to OpenLLMetry, which emits `gen_ai.prompt.N.*`, not the `gen_ai.input.messages` this case uses.
   - Rename the scope in `input.json` to `example.genai` and update `scope_name` in `expected.json`.
   - Detection does not use the scope, so nothing else changes.

Done when `just fixtures-check` and `just check` are green.

---

## R10 — SDK configuration parity

**Edit only:** `sdks/python/**`, `sdks/typescript/**`, `docs/spec/10-sdk.md`, `pnpm-lock.yaml` (only if step 5 changes it).

Spec 10 and D78 say both SDKs resolve the configuration the same way, and any difference is a bug.

### 1. One base-URL parser in both SDKs

Today Python uses `urlsplit` and TypeScript a regex. Python raises a plain `ValueError` for `http://[::1`, which TypeScript accepts. Make both SDKs apply the same hand-written rule, and stop using `urlsplit` in Python:

1. The value must match `^(https?)://([^/?#]+)` case-insensitively. Group 2 is the authority.
2. The host is the authority after its last `@`. If the host starts with `[`, it must match `^\[[0-9A-Fa-f:.]+\](?::[0-9]*)?$`. Otherwise it must contain neither `[` nor `]`, and the part before any `:` must be non-empty.
3. The query is the text after the first `?` that comes before the first `#`; the fragment is the text after the first `#`. Both must be empty (today's rule: an empty `?` or `#` is accepted).
4. The result is the value with trailing `/` removed.

Error messages stay the same: `must be an absolute http or https URL` for rules 1–2, and `must not include a query or fragment` for rule 3. Spec 10 rule 6 gains one sentence:

> A bracketed host must contain only hex digits, `:` and `.`; brackets anywhere else are rejected.

### 2. `sample_rate` overflow

Python `_check_sample_rate` calls `float(value)`, which raises `OverflowError` for `10**400`. Catch `OverflowError` there and raise the usual `ConfigError("sample_rate: must be a number between 0 and 1")`. TypeScript needs no change: a too-large `number` is already out of range, and a `bigint` is not a `number`.

### 3. One `LLMOBS_SAMPLE_RATE` grammar

Python `float(raw)` accepts `0.2_5` and Unicode digits; TypeScript `Number(raw)` accepts `0x1`, `0b1`, `0o1` and `Infinity`. In both SDKs, after the existing trim and blank-means-unset handling:
- the env string must match `^(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)$`. In Python, write the class as `[0-9]` (not `\d`, which matches Unicode digits);
- otherwise raise `LLMOBS_SAMPLE_RATE: must be a number between 0 and 1`;
- then convert and apply the existing finite `[0, 1]` check.

The argument path (`sample_rate=` / `sampleRate:`) is unchanged.

Spec 10 rule 7 becomes:

> 7. `sample_rate` must be a finite number in `[0, 1]`. From `LLMOBS_SAMPLE_RATE` it must also be a plain ASCII decimal (`1`, `0.25`, `.5`, `1.`): no sign, exponent, digit separators, hex, or non-ASCII digits.

### 4. Tests, the same cases in both SDKs

- **Base URL:**
  - accepted: `http://[::1]:8080` and `HTTPS://X.example/`;
  - rejected: `http://[::1`, `http://::1]`, `http://[zz]`, `http://a]b`, `http://` and `http://:8080`;
  - an `http://[::1` test asserts `ConfigError` specifically, not just any exception.
- **Sample rate from env:**
  - accepted: `1`, `0`, `0.25`, `.5`, `1.`, `0.0`, ` 0.5 ` (trimmed);
  - rejected: `+0.5`, `-0`, `5e-1`, `0x1`, `0b1`, `0o1`, `0.2_5`, `١`, `Infinity`, `NaN`, `1.5`.
- **Sample rate argument:** Python `10**400` raises `ConfigError`.
- **TypeScript precedence:** `config.test.ts` (lines 75–91) checks only that an argument beats env. Add "env beats default" for every option, mirroring the Python cases, including env-only `environment`, `baseUrl` and `sampleRate`.
- **Test isolation:**
  - Python `test_env_none_reads_os_environ` (`test_config.py`, line 360) sets only `LLMOBS_API_KEY`. Make it `monkeypatch.delenv(name, raising=False)` every `LLMOBS_*` name from the env constants, plus `GIT_COMMIT` and `SOURCE_VERSION`, before setting the key.
  - TypeScript `config.test.ts` around line 264 deletes `LLMOBS_API_KEY` without restoring its earlier value. Save and restore it the way the same test restores `LLMOBS_BASE_URL`.

### 5. Duplicate Biome pin

`sdks/typescript/package.json` declares `@biomejs/biome` 2.6.4, which the root `package.json` already declares.
- Remove it from the SDK if `pnpm --dir sdks/typescript run check` still finds `biome` afterwards. Run `pnpm install` to update the lockfile.
- If the SDK script cannot find the binary without it, keep it and say why in the report.

Done when both SDKs pass the same cases and `just check` is green.

---

## R11 — Web fixes

**Edit only:** `web/**`, `biome.json`.

1. **Theme after switching back to System** (`web/src/components/theme-provider.tsx`, lines 60–79).
   - The problem: `prefersDark` is read once at mount, and the `change` listener runs only while the theme is `system`. If the OS scheme changes while Light or Dark is selected, choosing System shows the stale value.
   - The fix: in the effect, when `theme === "system"`, call `setPrefersDark(media.matches)` before adding the listener.
   - Test, using the existing `web/src/test/match-media.ts` stub:
     1. start with Light;
     2. switch the stubbed OS scheme to dark without dispatching an event;
     3. select System;
     4. assert `resolvedTheme` is `dark` and `<html>` has the dark class.
2. **No empty state while loading** (`web/src/routes/$org/index.tsx`, line 44).
   - The problem: "No projects yet" renders whenever `projects.data` is undefined, including while the query is pending.
   - The fix: render the existing `Skeleton` rows while the query is pending. Show the empty state only when the data has loaded and the list is empty.
   - Test: `server.use` a handler for the projects endpoint that waits on a promise the test resolves. Before resolving, the skeleton shows and "No projects yet" does not. After resolving with an empty page, the empty state shows.
3. **Biome override for `web/src/components/ui/**`** (`biome.json`, around line 42).
   - Plan 05 allowed turning off only `style/useImportType` there. The override also turns off the formatter and `organizeImports`.
   - Remove those two parts, keep `useImportType: off`, and run `just fmt` so the shadcn files are formatted like the rest.
   - New shadcn components get formatted by `just fmt` when they are added.
4. **Unused `web/src/lib/utils.ts`.**
   - It only re-exports `cn` from the `cn` package, and every component imports from `"cn"` directly.
   - Delete the file and set `aliases.utils` in `web/components.json` to `"cn"`. The shadcn CLI rewrites the registry's `@/lib/utils` import to that alias, so future components import `cn` the same way the existing ones do.
   - If the shadcn `components.json` schema rejects a bare package name there, keep the file instead. In that case, change the six components to import from `@/lib/utils` (the shadcn default), and say so in the report.
5. **`web/src/routes/home.test.tsx` line 21:** remove the redundant `server.resetHandlers()`; `test/setup.ts` already resets after each test.

Done when the new tests pass and `just check` is green.

---

## R12 — Close-out

**Edit only:** `docs/**`.

1. **Spec 05, line 129:** "spans whose parent is missing attach to a synthetic root" contradicts spec 09 and the code. Replace with:

   > spans whose parent is missing, or that sit in a parent cycle, are emitted as roots with `detached: true`, in pre-order by `(start_time, span_id)` (09 § Response shapes).

2. **Audit:**
   - The decision table runs D1–D82 with no gaps, and every `D<n>` cited in `docs/`, `crates/`, `web/src/`, `sdks/` and `AGENTS.md` exists.
   - § Open lists only the license (D9), the real name (D46) and `.cursor/`.
   - `grep -rn 'synthetic root\|T | null' docs/spec` finds nothing.
3. **Final checks:**
   - From a fresh clone of the branch: `just check`, `just check-workflows`, then `just assert-clean`.
   - Run `just openapi`, `just api-examples` and `just fixtures-fmt`, then `just assert-clean`. The generators must leave the tree clean.
4. **Report:** go through § Findings covered below, one line per row: done (commit), or not done and why.

---

## Findings covered

| Finding | Sub-phase |
|---|---|
| Message `extra` overwrites modelled fields | R1 |
| RFC 3339 parsing more lenient than plan 02 | R1 |
| Stored-enum serde errors repeat the input | R1 |
| `lib.rs` holds a test module | R2 |
| Event/link struct fields defined twice | R2 |
| `arrow` default features unused | R2 |
| `prost` declared but unused | R2 |
| Message without `parts` round-trip not in spec | R2 |
| D65 fields typed non-nullable in OpenAPI | R3 |
| Omitted optionals typed nullable | R3 |
| Sessions have no `tags` | R4 |
| No `include_eval` / `include_experiments` on sessions, users, metrics; `EXCLUSIONS` description missing | R5 |
| Examples not validated against the OpenAPI document | R6 |
| Manifest not checked against operations; static/media files not swept | R6 |
| Write switches trigger on any value | R6 |
| Rustdoc link in `Problem.title`; unused `ToSchema` derives; `ScoreBrief` doc; `schema.json` trace `name`; `SavedView.state` type | R6 |
| `input.json` misspellings pass | R7 |
| `load_all_cases` panics on the first bad case | R7 |
| Missing validator checks (rule 3, cost without source, duplicate media, min > max, names, empty groups, 256 KiB, timestamp window) | R7 |
| Stale `time_serde` comment | R7 |
| OTLP/JSON conformance gaps | R7 |
| Vercel and OpenInference rules only in fixtures | R8 |
| Validator decides cost questions on its own | R8 (D82) |
| Rule 15 zero parent not pinned | R9 |
| "Measured after media extraction" not really tested | R9 |
| `genai` case scope is OpenLLMetry's | R9 |
| Python invalid IPv6 URL raises `ValueError` | R10 |
| Python huge `sample_rate` raises `OverflowError` | R10 |
| `LLMOBS_SAMPLE_RATE` parsed differently by the two SDKs | R10 |
| TS precedence test misses env over default; test isolation | R10 |
| Duplicate Biome pin in the TS SDK | R10 |
| Stale theme after switching back to System | R11 |
| "No projects yet" while loading | R11 |
| Biome override wider than plan 05 allowed | R11 |
| Unused `web/src/lib/utils.ts`; redundant `resetHandlers` | R11 |
| Spec 05 "synthetic root" | R12 |

## Not in this plan

These review findings are left as they are, on purpose:

| Finding | Why it stays |
|---|---|
| `ScoreValue::Numeric(NaN)` built in Rust is only rejected on deserialize | It is a public enum variant; enforcing finiteness needs a constructor and private fields, a larger change than the risk. Ingestion and the API only build scores from deserialized input. |
| The column-constant test can't catch a constant missing from both lists | Catching it would mean parsing the source in a test. |
| Per-row allocations in `batch.rs` | Performance work belongs with the storage benchmarks (Wave 1). |
| Optional `Uuid` fields accept braced and urn forms | Harmless on input; output is always hyphenated. |
| Two `base64` versions (`0.23.1` ours, `0.22.1` from `opentelemetry-proto`) | `multiple-versions = "warn"`; it resolves when the proto crate updates. |
| `Score` responses include `project_id` while `SpanDetail` doesn't | Allowed by plan 04 ("core types keep their own serde format"). |
| Server serde enums (`TraceSort`, …) echo invalid query values, truncated | Query tokens are not payload content. |
| `.cursor/` untracked | User decision: ignore it. |
| `just check` fails in a non-interactive shell without Node on `PATH` | A local shell setup issue (nvm only loads in interactive fish), not a repo problem. |
