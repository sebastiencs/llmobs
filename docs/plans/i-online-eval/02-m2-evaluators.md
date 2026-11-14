# I · M2 — Evaluators, test runs and score definitions

Implementation plan for milestone M2 of track I. It has 10 sub-phases, I2.1 to I2.10. **Haiku** implements each one in one go, in order, with no context beyond this plan and the files it names. Each sub-phase ends with `just check` green and one commit. Sonnet reviews each sub-phase after it is committed.

**Prerequisites** (checked when this plan was written, 2026-11-14):
- M1 is merged on `main` (#63). The design update with D-I-31 to D-I-34 (#68) and this plan live on branch `claude/i-m2-plan`: start your branch from `claude/i-m2-plan`, or from `main` once that branch has merged. Check with `git log --oneline -1 -- docs/plans/i-online-eval/02-m2-evaluators.md` that the plan is there.
- Node `26.13.0` is installed at `$HOME/.local/share/nvm/v26.13.0/bin`. It is not on the non-interactive `PATH`.

**Read first** (every sub-phase):
- `AGENTS.md`, all of it.
- This file's sections up to § I2.1, then your own sub-phase.
- `docs/plans/i-online-eval/00-design.md` § Decisions: D-I-16, D-I-17, D-I-19, D-I-20, D-I-21, D-I-23, D-I-24, D-I-26 and D-I-31.
- `docs/plans/i-online-eval/01-m1-secrets-connections.md` § Conventions: M1's patterns (connections routes, page, fixtures) are the model for M2.
- `docs/mvp-decisions.md`: D138, D150, D155–D165, D-G-26 to D-G-30, D-L-4, D-L-5, D-L-10, D-L-11, and § Track I.
- `docs/spec/04-metadata-db.md` § Evaluation and § Scores definitions; `docs/spec/06-evaluation.md` § Scores and § Evaluators; `docs/spec/09-api.md` § Scores & annotations and § Evaluation & alerts.
- The code each sub-phase points to, before you edit it.

## What M2 is, and what it is not

When M2 is done:
- project members create, edit and delete **evaluators** (LLM judge, check, webhook) under `/api/v1/projects/{project}/evaluators`, from scratch or from a built-in template. A webhook's URL, headers and signing secret are sealed.
- `POST …/evaluators/test` runs an evaluator once, saved or not, on a stored trace or on literal variables. It writes the judge trace (environment `llmobs-eval`) and the judge usage, and returns the outcome. It writes no score.
- project members manage **score definitions**. Every score write is checked against the live definition of its name.
- judge-trace spans no longer count as billable spans in `usage_daily.spans` (D-I-31), and `usage_daily.judge_*` is written.
- the web has an "Evaluators" project page with an editor and a test panel, and a "Score definitions" project settings page.

**Out of scope:** online rules, the job queue, evaluator score writes, manual runs, the delete 409 for evaluators that rules use (M3); triggers (M4); the annotation panel and score ownership (M5); backfill (M6).

## Decisions this plan relies on

| # | Summary |
|---|---|
| D-I-16 | Each run has its own judge trace id. Failures before the evaluator runs write no judge trace. |
| D-I-17 | The executor refuses a target trace that has a span in `llmobs-eval`. |
| D-I-19 | `evaluators.connection_id` is derived from a judge's config. A webhook's `{signing_secret, url, headers}` is sealed in `secret_enc`; `config` keeps `{url_host, timeout_ms, header_names}`. `kind` is immutable. `version` increases on every change to config, output or sealed values. `WebhookConfig.secret_enc` is removed from `llmobs-eval`. |
| D-I-20 | `GET /evaluator-templates` lists E's built-in templates; the UI pre-fills the editor; there is no server-side create-from-template. |
| D-I-21 | `POST /evaluators/test`: synchronous, no score, commits the judge trace, counts usage, answers outcome, variables (cut to 2 000 characters), prompt, usage and judge trace id. 10 per minute per user, shared with connection tests. |
| D-I-23 | Trace loading: every column, roots and target always, then the earliest others, at most 5 000 spans and 64 MiB of payload, `truncated`; 30 s; at most 4 loads at once. |
| D-I-24 | `ProjectPerm::ManageEvaluation` (role `member`, no API key) for writes; `View` for reads. |
| D-I-26 | Score definitions: CRUD, `DELETE` archives, name unique among live definitions, writes checked against the live definition of their name, cached 10 s. An evaluator's output must fit a live definition of its name. |
| D-I-31 | `Ingest::commit` counts only spans outside `llmobs-eval`. `Usage::add_judge` writes `usage_daily.judge_*`. |

## Versions

No new dependency. All crates used here are already dependencies of the crates that use them.

## Conventions for every sub-phase

M1's conventions apply unchanged (`01-m1-secrets-connections.md` § Conventions): Node `PATH`, `just` recipes, contract regeneration (`just openapi`, `just api-examples`), dead code, temporary files in the scratchpad, no `unwrap`/`expect` outside tests, privacy, Rust and web test rules, STOP rules, one commit per sub-phase with no trailer lines, and the report. In addition, these lessons from M1's implementation:

- **Clippy** denies warnings, including `expect_used`, `unreachable_pub`, `unused_qualifications`, `needless_pass_by_value` and `large_futures`. Make a type `pub` only when a `pub` type exposes it. If clippy reports `large_futures` on an `.await`, wrap that future in `Box::pin(…)`.
- **Dead code in test builds.** `#![cfg_attr(not(test), allow(dead_code))]` at a module's top does not silence an item that test builds also leave unused. Such an item needs an item-level `#[allow(dead_code)]` with a comment naming the sub-phase that first uses it; that sub-phase removes the attribute.
- **SQL.** sqlx 0.9 takes `&'static str` SQL. Write each query as a literal. A query whose text is built at run time (only the table and column names of a constant list) is wrapped in `sqlx::AssertSqlSafe(text)` with a comment that the parts come from constants.
- **Log-capture tests** (`crate::test_support::capture_logs`) use plain `#[tokio::test]`, not the multi-thread flavour.
- **Long checks.** The shell caps one foreground command at 10 minutes. Run `just check-rust` as one monitored background task when it would exceed that. Never start two runs at once. Run `just check-web`, `just check-py` and `just check-ts` (each with the Node `PATH`) in the foreground.
- **Migrations** change the migration-count tests (`meta/mod.rs`, `admin.rs`) and the `meta_schema` snapshot. Each sub-phase that adds a migration updates them (§ I2.2 says how).
- **The test clock is frozen** (`Clock::Manual`, set to 2026-01-01 in `test_support/mod.rs`). Advance it with `app.state.clock.advance(Duration)` when a test needs time to pass.
- **Tests that run an evaluator set the clock to the real time first.** The eval crate stamps judge-trace spans with the system clock (`crates/eval/src/runner.rs`, `now_ns`), and the normalizer drops a span whose `start_time` is more than one day after the ingest time (`crates/otel/src/normalize.rs`, `StartTimeOutOfRange`). With the frozen 2026-01-01 clock as the ingest time, every judge span would be dropped. So every test that calls the test-run endpoint sets the clock **right after building the `TestApp` and before `app.seed()`, `signed_in` or `commit_spans`**. `seed()` mints session cookies at the current clock and a session lives 30 days (`auth/session.rs`), so a cookie minted at the frozen 2026-01-01 would be expired once the clock jumps, and every request would answer 401. Setting the clock first also puts the `commit_spans` usage day and the judge usage day on the same UTC date. The code:

```rust
let real_now = i64::try_from(
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos(),
)
.unwrap();
app.state.clock.set(real_now);
```

  Target traces stored with `commit_spans` keep their own fixed times and are not affected: they are read back, not normalized again.
- **Secrets in request types.** A request type that carries a webhook URL, header values, a signing secret or payload variables (`CreateEvaluatorRequest`, `UpdateEvaluatorRequest`, `TestEvaluatorBody`, `EvaluatorTestRequest`) must not derive `Debug`. Write `impl fmt::Debug` by hand, printing only the kind, the name, and `has_config` / `has_signing_secret` / `variables.len()`-style flags and counts.
- **`MetaDb::write` closures** must be `Send + 'static`: move owned values in, borrow nothing from the handler.
- `web/src/routeTree.gen.ts` is gitignored. Never commit it.

## Final layout after M2

```
crates/eval/src/config.rs             WebhookConfig without secret_enc (I2.1)
crates/eval/tests/{runner,webhook}.rs  the `secret_enc: None` lines removed (I2.1)
crates/eval/src/mapping/resolve.rs    pub Variables::iter (I2.1)
crates/server/src/
  ingest/mod.rs                       commit skips llmobs-eval spans in usage; Ingest::context (I2.1)
  usage.rs                            Usage::add_judge, judge columns in the flush (I2.1)
  auth/roles.rs                       ProjectPerm::ManageEvaluation (I2.2)
  crypto.rs                           ENCRYPTED_COLUMNS + ("evaluators", "secret_enc") (I2.2)
  meta/evaluators.rs                  EvaluatorRow, EvaluatorFields, list/get/insert/update/delete, used_by_connection (I2.2)
  meta/score_definitions.rs           DefinitionRow, DefinitionFields, list/get/insert/update/archive (I2.2)
  api/convert.rs                      spans(): every row of span-detail batches (I2.3)
  eval/trace_load.rs                  load_trace: caps, roots, target, dedupe, TraceLoads (I2.3)
  api/evaluators.rs                   Evaluator API types, templates, test request/result (I2.4)
  api/score_definitions.rs            ScoreDefinition API types (I2.4)
  api/openapi.rs                      evaluator_paths, score_definition_paths (I2.4)
  scores.rs                           definition checks (I2.5)
  eval/definitions.rs                 DefinitionCache (I2.5)
  routes/score_definitions.rs         CRUD (I2.5)
  eval/evaluator.rs                   stored ↔ llmobs_eval::Evaluator, validation, sealing (I2.6)
  routes/evaluators.rs                CRUD and templates (I2.6), test (I2.7)
  routes/connections.rs               DELETE 409 when evaluators use the connection (I2.6)
  eval/test_run.rs                    one synchronous run (I2.7)
crates/storage/src/reads/traces.rs    trace_spans, TraceSpansRequest, TraceSpansData (I2.3)
crates/server/migrations/meta/0005_evaluators.sql   (I2.2)
crates/server/tests/openapi.rs, tests/api_examples.rs (I2.4)
fixtures/api/static/{evaluators,evaluator,evaluator-templates,evaluator-test-result,score-definitions,score-definition}.json (I2.4)
fixtures/api/requests/{evaluator-create,evaluator-test,score-definition-create}.json (I2.4)
web/src/routes/$org/$project/evaluators/{index,new,$evaluatorId}.tsx (+ tests)   (I2.8, I2.9)
web/src/routes/$org/$project/settings/score-definitions.tsx (+ test)             (I2.8)
web/src/components/evaluators/{evaluator-form,output-fields,check-fields,test-panel}.tsx   (I2.9, I2.10)
crates/server/src/test_support/mock_webhook.rs   in-process webhook receiver for tests (I2.7)
```

## Shared reference

### Fixed identifiers (fixtures and tests)

| Thing | Value |
|---|---|
| Evaluator (judge) | `01900000-0000-7000-8000-000000000500`, name `Relevance`, score `relevance` (numeric 0–1), connection `01900000-0000-7000-8000-000000000400` (M1's fixture connection), model `gpt-5-mini` |
| Score definition | `01900000-0000-7000-8000-000000000600`, name `relevance`, numeric, `min_value` 0, `max_value` 1 |
| Project in fixtures | `support-bot` of org `acme` (`fixtures/api/README.md`) |

### Evaluator contract

JSON field names are exact. Times are RFC 3339 (`llmobs_core::time_serde`, and `crate::api::keys::optional_time` for optional times). Every `Option` field of every type has `#[serde(default, skip_serializing_if = "Option::is_none")]`. Requests are `#[serde(deny_unknown_fields)]`.

**`EvaluatorKind`**: `"llm_judge"` | `"check"` | `"webhook"` (`#[serde(rename_all = "snake_case")]`, `Copy`, with `as_str` and `from_name -> Option<Self>` like M1's enums).

**`OutputScoreBody`**: `{name: string, data_type: ScoreDataType, categories?: string[]}`. It is the JSON of `llmobs_eval::OutputScore`; `ScoreDataType` is `llmobs_core::enums::ScoreDataType` (already a schema).

**`Evaluator`** (response), fields in this order:

| Field | Type | Notes |
|---|---|---|
| `id` | UUID | |
| `name` | string | 1–128 characters (E's rule) |
| `kind` | `EvaluatorKind` | |
| `config` | object | judge: the `JudgeConfig` JSON as stored; check: the `Check` JSON as stored; webhook: `{url_host, timeout_ms, header_names}` |
| `output_score` | `OutputScoreBody` | |
| `has_signing_secret` | bool | true for a webhook evaluator |
| `version` | integer | starts at 1 |
| `created_by` | UUID, omitted when unknown | |
| `created_at`, `updated_at` | RFC 3339 | |

`config` is `serde_json::Value` with `#[schema(value_type = Object)]`.

**`CreateEvaluatorRequest`**: `name`, `kind`, `config` (object; `serde_json::Value` with `#[schema(value_type = Object)]`), `output_score` (`OutputScoreBody`), `webhook_secret?` (string). `Debug` by hand (§ Conventions).
- For a webhook, `config` is the full E webhook document `{url, timeout_ms?, headers?}` and `webhook_secret` is required.
- For the other kinds, `webhook_secret` must be absent.

**`UpdateEvaluatorRequest`**: `name?`, `config?` (as above), `output_score?`, `webhook_secret?`. There is no `kind`; sending it is a 400 (unknown field). `Debug` by hand.
- For a webhook, a present `config` replaces the URL, the headers and the timeout together; an absent `config` keeps them. A present `webhook_secret` replaces the secret; an empty string is a 400.

**Validation** (create and update). Errors are collected into `ApiError::BadRequest { detail: None, errors }` as M1 does.

| Check | Field | Message |
|---|---|---|
| `kind` is not webhook and `webhook_secret` present | `webhook_secret` | `webhook_secret is only for webhook evaluators` |
| webhook create without `webhook_secret` | `webhook_secret` | `webhook_secret is required` |
| `webhook_secret` not 16–256 characters (`chars().count()`), or with a control character | `webhook_secret` | `webhook_secret must be 16 to 256 characters` |
| `EvaluatorConfig::from_parts(kind, &config)` fails with `ConfigError::Json { message, .. }` | `config` | `invalid configuration: <message>` (`message` is serde's category, line and column, never input) |
| it fails with `ConfigError::Invalid { field, reason }` | `config.<field>` (`name` when `field == "name"`) | `<reason>` |
| it fails with `ConfigError::UnknownKind` (cannot happen: `kind` is a typed enum; map it anyway) | `config` | `invalid configuration` |
| `output_score` does not deserialize as `OutputScore` | `output_score` | `invalid output_score` |
| `Evaluator::validate()` (or `from_parts`) fails with `Invalid { field, reason }` | by `field`: `name` → `name`; `data_type` → `output_score.data_type`; `categories` → `output_score.categories`; `output` → `config.output`; `labels` → `config.output.labels`; `label_values` → `config.output.label_values`; any other `<field>` → `config.<field>` | `<reason>` |
| `output_score.name` is not 1–255 bytes (score writes require it, `scores.rs` `text_ok`) | `output_score.name` | `output_score.name must be 1 to 255 bytes` |
| a webhook header value fails `axum::http::HeaderValue::from_str` or is over 4096 bytes, or two header names are equal after lowercasing (E checks the names only, `validate_headers`) | `config.headers.<lowercased name>` | `invalid header` |
| it fails with `OutputMismatch { expected, found }` | `output_score.data_type` | `data_type must be <expected> for this evaluator` (`as_str` names) |
| a judge's `connection_id` is not a connection of the project's org | `config.connection_id` | `connection not found in this organization` |
| a live score definition named `output_score.name` exists and does not fit (§ Score definition checks) | `output_score` | `output_score does not match the score definition` |

A duplicate name is `409 conflict` with `an evaluator with this name already exists`. A project holds at most 500 evaluators: the 501st create is `409 conflict` with `a project has at most 500 evaluators` (counted inside the insert's transaction, like M1's 100-connection cap).

A PATCH whose stored row changed between the handler's read and its write is `409 conflict` with `evaluator changed, retry` (§ I2.6, optimistic check).

**Storage of an evaluator** (`evaluators` row):
- `config`:
  - judge: `serde_json::to_string(&JudgeConfig)` of the parsed config;
  - check: `serde_json::to_string(&Check)` of the parsed config (after `from_parts`, which folds `value` into `values`);
  - webhook: `{"url_host": <host of the URL>, "timeout_ms": <n>, "header_names": [<lowercased, sorted>]}`.
- `connection_id`: the judge's `connection_id`, else NULL.
- `secret_enc` (webhook only): seal, with `Aad { table: "evaluators", column: "secret_enc", row_id: &id }`, the JSON `{"signing_secret": "...", "url": "...", "headers": {name: value}}`.
- `version` increases by 1 on an update when the stored `config`, `output_score` or the sealed plaintext changes (compare the new strings with the old). A rename alone keeps the version.

**Templates** (`GET …/evaluator-templates`) answer `{data: EvaluatorTemplate[]}` (no cursor; the list is fixed), in `builtin_templates()` order. `EvaluatorTemplate`:
- `id`, `name`, `description`, `prompt`;
- `output` (object, the `JudgeOutput` JSON, `#[schema(value_type = Object)]`);
- `output_score_name`;
- `default_mapping`: `[{variable, expression}]`.

### Test run contract

**`TestEvaluatorBody`**: an evaluator to test, possibly partial (`deny_unknown_fields`, `Debug` by hand):
- `kind` (`EvaluatorKind`, required);
- `name?` (string; absent → the saved name with `evaluator_id`, else `"Test"`);
- `config?` (object, `#[schema(value_type = Object)]`; absent → the saved config, which needs `evaluator_id`);
- `output_score` (`OutputScoreBody`, required);
- `webhook_secret?` (string; absent → the saved secret, which needs `evaluator_id`).

For a saved webhook, an absent `config` takes the saved URL, headers and timeout from the sealed values, and an absent `webhook_secret` the saved secret. The UI never sees those values, so this is how it tests a saved webhook with an edited output or name. A body that sends a new webhook `config` must also send `webhook_secret`, so the saved secret never signs requests to a URL the member just typed.

**`EvaluatorTestRequest`** (`deny_unknown_fields`, **`Deserialize` only**: `TimeParam` has no `Serialize`, like `CreateApiKeyRequest` in `api/keys.rs`; `Debug` by hand): `evaluator_id?` (UUID), `evaluator?` (`TestEvaluatorBody`), `trace_id?` (string, 32 hex), `span_id?` (string, 16 hex), `trace_start?` (`TimeParam`), `variable_mapping?` (`BTreeMap<String, String>`, turned into JSON with `serde_json::to_value` for `VariableMapping::from_json`), `variables?` (`BTreeMap<String, serde_json::Value>`, `#[schema(value_type = Object)]`).

| Rule | Field | Message |
|---|---|---|
| neither `evaluator_id` nor `evaluator` | `evaluator` | `give evaluator_id, evaluator, or both` |
| `trace_id` and `variables` both, or neither (checked first; the `variable_mapping` row below then applies only when `trace_id` is absent) | `variables` | `give trace_id with variable_mapping, or variables` |
| `trace_id` without `variable_mapping` | `variable_mapping` | `variable_mapping is required with trace_id` |
| `span_id` or `trace_start` without `trace_id` | `trace_id` | `trace_id is required with span_id and trace_start` |
| `trace_id` or `span_id` not hex of the right length, or all zeros (parse with `str::parse::<TraceId>()` / `str::parse::<SpanId>()`, which refuse a wrong length, non-hex and all zeros; either case is accepted) | `trace_id` / `span_id` | `trace_id must be 32 hex digits` / `span_id must be 16 hex digits` |
| `variable_mapping` without `trace_id` | `trace_id` | `trace_id is required with variable_mapping` |
| a `variables` key that does not match `^[A-Za-z_][A-Za-z0-9_]{0,63}$` | `variables` | `invalid variable name` |
| the `variables` values are more than 256 KiB in total (the sum of each value's `serde_json::to_string` length) | `variables` | `variables must be at most 256 KiB in total` |
| `variable_mapping` fails `VariableMapping::from_json` | `variable_mapping` | the `MappingError` display |
| mapping needs an item (`needs_item()`) | `variable_mapping` | `expected and item.* are for experiments only` |
| `variables` has more than 32 entries | `variables` | `at most 32 variables` |
| `evaluator` invalid | `evaluator.<field>` | as § Evaluator contract, prefixed |
| `evaluator.config` absent without `evaluator_id` | `evaluator.config` | `config is required without evaluator_id` |
| webhook `evaluator.webhook_secret` absent without `evaluator_id` | `evaluator.webhook_secret` | `webhook_secret is required` |
| webhook `evaluator.config` present without `evaluator.webhook_secret`, even with `evaluator_id` (a new URL must not reuse the saved signing secret) | `evaluator.webhook_secret` | `webhook_secret is required with config` |
| `evaluator_id` given and `evaluator.kind` differs from the saved kind | `evaluator.kind` | `kind must match the saved evaluator` |
| `evaluator_id` unknown in this project | — | 404 |
| the evaluator is a `levenshtein` check and the rendered `output` or `expected` variable is over 64 KiB (checked after the variables are resolved; the distance is quadratic and runs uncancellable in `spawn_blocking`) | `variables` | `levenshtein inputs must be at most 64 KiB each` |

The evaluator that runs:
- `evaluator` alone: that body, with id `Uuid::nil()`.
- `evaluator_id` alone: the saved one.
- both: the body merged over the saved one (absent `name`, `config` and `webhook_secret` taken from the saved evaluator, as above), with the saved id.

What the run uses:
- `RunInput.target` is `EvalTarget { trace_id: None, span_id: None }` in variables mode, and the request's ids in trace mode.
- `RunInput.connection` is `None` for checks and webhooks; `RunInput.webhook_secret` is `None` for judges and checks.
- Test runs do not take the connection's `max_concurrency` or `requests_per_minute` permits (D-I-5); the per-user limit is their only bound. A run can last as long as the judge's own `timeout_ms` (at most 300 s); there is no other server deadline.
- Outbound calls go through `EgressClient` (D166): its resolver drops private addresses at connect time, which also covers DNS rebinding. That is the only SSRF control, and it is enough.

**`EvaluatorTestResult`** (`200`, also when the evaluator failed), fields in this order:

| Field | Type | Notes |
|---|---|---|
| `ok` | bool | |
| `value` | `ScoreValue` (the same JSON as `ScoreCreate.value`), omitted unless `ok` | |
| `reason` | string, omitted when none | |
| `error` | `{class: "retryable" \| "pause" \| "permanent", message}`, omitted when `ok` | the `EvalError` display, or the `MappingError` display |
| `prompt` | string, omitted for checks, webhooks and failures before rendering | the rendered prompt |
| `variables` | object of string to string | each value rendered (a string as itself, other JSON compact), cut to 2 000 `char`s with a final `…` |
| `usage` | `{input_tokens?, output_tokens?, cost_usd?}`, omitted when the run made no `llm` span | |
| `latency_ms` | integer | |
| `trace_truncated` | bool | `false` in variables mode |
| `judge_trace_id` | 32 lowercase hex, omitted when no judge trace was written | |

`error.class` reuses M1's `TestErrorClass` enum. Two failures before the evaluator runs are `ok: false` answers with class `permanent`, no `judge_trace_id`, no `value`, `reason`, `prompt` or `usage`, `variables: {}`, `latency_ms: 0`, and `trace_truncated: false` (`true` for the size cap): a trace over the size cap (`trace too large`) and a mapping error (the `MappingError` display; `MissingVariable` names the user's own variable and expression, which is configuration, not payload). Other load failures are error statuses (table below).

Judge usage is added to `usage_daily` before the judge trace is committed. When the commit then fails (503), the usage stays counted and the answer is lost; this is accepted (the provider was called, so the tokens were spent).

**Errors (non-200):**

| Cause | Status | Code |
|---|---|---|
| body invalid | 400 | `bad_request` with `errors[]` |
| the target trace has a span in `llmobs-eval` | 400 | `bad_request`, detail `target is an evaluation trace` |
| trace not found | 404 | `not_found` |
| storage busy or timed out while loading | 503 | `busy` |
| the judge trace could not be committed (storage busy, disk full) | 503 | `busy` |
| rate limit | 429 | `rate_limited` |

### Score definition contract

**`ScoreDefinitionCategory`**: `{label: string, value?: number}`.

**`ScoreDefinition`** (response): `id`, `name`, `data_type` (`ScoreDataType`), `categories` (array, always present, empty unless categorical), `min_value?`, `max_value?`, `description?`, `archived_at?`, `created_at`, `updated_at`.

**`CreateScoreDefinitionRequest`**: `name`, `data_type`, `categories?`, `min_value?`, `max_value?`, `description?`.
**`UpdateScoreDefinitionRequest`**: `name?`, `categories?`, `min_value?`, `max_value?`, `description?` (`""` clears it), `archived?` (bool; `false` restores). There is no `data_type`, because it is immutable.

`min_value` and `max_value` cannot be cleared through PATCH once set (the null-stripping pass forbids null). Archive and create a new definition instead; this is accepted.

| Check | Field | Message |
|---|---|---|
| `name` trimmed is 1–255 bytes | `name` | `name must be 1 to 255 bytes` |
| categorical: 1–100 categories, each `label` 1–255 bytes, labels unique, each `value` finite | `categories` | `categories must be 1 to 100 unique labels of 1 to 255 bytes` |
| not categorical and `categories` non-empty | `categories` | `categories are only for categorical scores` |
| `min_value` / `max_value` present on a non-numeric type | `min_value` / `max_value` | `ranges are only for numeric scores` |
| a bound not finite, or `min_value >= max_value` when both are set | `min_value` | `min_value must be below max_value` |
| `description` over 1 000 characters | `description` | `description must be at most 1000 characters` |

A live definition with the same name already exists → `409 conflict` with `a score definition with this name already exists`. That applies on create, on rename, and on `archived: false`.

A project holds at most 1 000 live definitions: a create, or a restore, past that is `409 conflict` with `a project has at most 1000 live score definitions` (counted inside the write's transaction).

PATCH validates against the stored `data_type`, so it reads the row first (§ I2.5); a row that changed before the write is `409 conflict` with `score definition changed, retry`.

### Score definition checks

`crate::scores::write` checks each item against the live (non-archived) definition with the same `name` in the project, if one exists:

| Check | Field | Message |
|---|---|---|
| `item.value.data_type() != definition.data_type` | `scores[i].value` | `value must be <data_type> for this score definition` |
| numeric outside `[min_value, max_value]` (each bound only when set) | `scores[i].value` | `value is outside the score definition's range` |
| categorical `label` not among the definition's labels | `scores[i].value` | `label is not a category of this score definition` |

An evaluator's `output_score` fits a live definition of the same name when:
- the data types are equal;
- for a numeric judge, the judge's `min` is at least `min_value` and its `max` at most `max_value`, each bound only when set;
- for a categorical one, every label is among the definition's labels.
Checks and webhooks only need equal types.

---

## I2.1 — Eval crate edits, billable spans and judge usage

**Goal:** `llmobs-eval` has no `secret_enc` field and exposes `Variables::iter`. `commit` no longer counts judge spans as billable. `Usage::add_judge` exists, and the flush writes the judge columns. `Ingest::context` builds an `IngestContext`.

**Edit only:** `crates/eval/src/config.rs`, `crates/eval/src/mapping/resolve.rs`, `crates/eval/src/lib.rs` (only if a re-export needs adjusting), `crates/eval/tests/runner.rs` and `crates/eval/tests/webhook.rs` (only the `secret_enc: None` lines), `crates/server/src/ingest/mod.rs`, `crates/server/src/usage.rs`, `docs/spec/04-metadata-db.md`, `docs/spec/06-evaluation.md`.

**Commit:** `Count only billable spans and record judge usage`

### Eval crate

- `config.rs` `WebhookConfig`: delete the `secret_enc` field and its doc comment, and the `has_secret_enc` line in its `Debug`. The struct's doc comment (near line 538) says "The URL path and query, the header values, and `secret_enc` are omitted"; make it "The URL path and query and the header values are omitted." Remove `secret_enc` from every test in the file that builds or asserts it (the tests near lines 771, 797, 883, 893 and 963); the assertion `rendered.contains("has_secret_enc: true")` near line 797 is deleted.
- `crates/eval/tests/runner.rs` (near lines 349 and 406) and `crates/eval/tests/webhook.rs` (near line 66) build `WebhookConfig { …, secret_enc: None }`. Delete those three `secret_enc: None,` lines, nothing else. Without this the eval crate's tests stop compiling. A test whose only purpose was `secret_enc` loses that assertion and keeps the rest. `WebhookConfig` does not deny unknown fields (unlike the judge and check configs), so a test that deserializes a document containing `"secret_enc"` still parses: keep that test, drop its `secret_enc` assertion, and assert the document parses with the other fields as before.
- `mapping/resolve.rs`: change `pub(crate) fn iter(&self)` on `Variables` to `pub fn iter(&self) -> impl Iterator<Item = (&str, &Value)>` with the doc comment "The variables, sorted by name. Values are JSON." Keep `get` crate-private.

### `Ingest::commit` and `Ingest::context` (`ingest/mod.rs`)

- In `commit`, `usage.add(…)` counts billable spans: compute `let billable = spans.iter().filter(|span| span.environment.as_deref() != Some(EVAL_ENVIRONMENT)).count();` **before** `spans` is moved into the batch task, and pass it instead of `accepted`. `CommitReceipt.accepted` stays the number of appended spans. The reserved name is `llmobs_core::names::environment::EVAL` (the constant `ingest/environment.rs` uses). `ingest/mod.rs` declares a child `mod environment;`, so importing `llmobs_core::names::environment` there collides (E0255), and `environment::EVAL` would resolve to the child module. Import the constant under another name: `use llmobs_core::names::environment::EVAL as EVAL_ENVIRONMENT;`, and compare with `Some(EVAL_ENVIRONMENT)`; never with a string literal. Update the doc comment of `commit`: "…then counts the billable spans (every span outside the reserved evaluation environment, D-I-31) and the request bytes (D-G-30)."
- Add:

```rust
    /// The normalizer's context for an in-process commit into `project` (D-I-21).
    pub(crate) fn context(&self, project: &ProjectRow, now_ns: i64) -> IngestContext {
        IngestContext {
            project_id: project.id,
            ingest_time: now_ns,
            retention_days: project.retention_days,
            limits: self.limits,
        }
    }
```

  and use it in `accept_request` in place of the inline `IngestContext { … }`.

### `Usage::add_judge` (`usage.rs`)

- `UsageCounts` gains `judge_input_tokens: u64`, `judge_output_tokens: u64` and `judge_cost_usd: f64`. Because of the `f64`, drop `Eq` from its derive (keep `PartialEq`, `Default`, `Clone`, `Copy`, `Debug`). `merge` adds them, saturating for the integers and with `+` for the cost.
- Add:

```rust
    /// Adds one judge call's tokens and cost to the day of `now_ns` (UTC) (D-I-31). A zero call changes nothing.
    pub(crate) fn add_judge(
        &self,
        org_id: OrgId,
        project_id: ProjectId,
        now_ns: i64,
        input_tokens: u64,
        output_tokens: u64,
        cost_usd: f64,
    )
```

  It returns early when all three are zero, and merges into the same key as `add`. Its first production caller is I2.7's test run, so give it `#[allow(dead_code)] // test runs call it from I2.7 on` (`usage.rs` has no module-level allow); I2.7 removes it.
- `add` keeps its early return for all-zero counts. `flush` writes the three judge columns too: `INSERT INTO usage_daily (org_id, project_id, day, spans, bytes, scores, judge_input_tokens, judge_output_tokens, judge_cost_usd) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) ON CONFLICT (org_id, project_id, day) DO UPDATE SET spans = spans + excluded.spans, bytes = bytes + excluded.bytes, scores = scores + excluded.scores, judge_input_tokens = judge_input_tokens + excluded.judge_input_tokens, judge_output_tokens = judge_output_tokens + excluded.judge_output_tokens, judge_cost_usd = judge_cost_usd + excluded.judge_cost_usd`. Bind the cost as `f64`. The columns exist since `0002_usage.sql`.

### Spec

- `docs/spec/04-metadata-db.md` § Metering & instance: after the `usage_daily` DDL, add: "`spans` counts billable spans: spans in the reserved `llmobs-eval` environment (judge traces) are stored but not counted (D-I-31). Judge calls add their tokens and cost to `judge_input_tokens`, `judge_output_tokens` and `judge_cost_usd`."
- `docs/spec/06-evaluation.md` § Evaluators › Judge traces: in the sentence "`eval_jobs.cost_usd` and `usage_daily.judge_*` are updated too", append "; judge-trace spans are not billable units (D-I-31, D178)".

### Tests

- `ingest/mod.rs` tests: `judge_spans_are_not_billable`. Commit a `Normalized` with three spans, two with `environment = Some(EVAL_ENVIRONMENT.to_owned())` (the test module imports the constant the same way, `use llmobs_core::names::environment::EVAL as EVAL_ENVIRONMENT;`) and one with `Some("production")`, through `Ingest::commit` (copy how the existing commit tests build `Normalized` and call `commit`). The receipt's `accepted == 3`; after `usage.flush`, `usage_daily.spans == 1`.
- `usage.rs` tests:
  - `judge_usage_is_summed_and_flushed`: two `add_judge` calls on one day → after `flush`, the row has the summed tokens and the cost within `1e-12`;
  - `zero_judge_usage_adds_nothing`: no row;
  - `judge_and_span_counts_share_a_row`;
  - `a_failed_flush_keeps_judge_usage`: copy the existing failed-flush test (it makes the write fail and checks spans, bytes and scores come back), add an `add_judge` call, and assert the judge tokens and cost come back too and are written by the next successful flush.
- `crates/eval` tests still pass after the field removal.

### Done when

`just check` passes. `git grep secret_enc crates/eval` finds nothing. `judge_spans_are_not_billable` shows `spans == 1` in `usage_daily`.

---

## I2.2 — Tables, rows and the evaluation permission

**Goal:** migration `0005_evaluators.sql` with `evaluators` and `score_definitions`; their `meta` modules; `ProjectPerm::ManageEvaluation`; `ENCRYPTED_COLUMNS` covers `evaluators.secret_enc`.

**Edit only:** `crates/server/migrations/meta/0005_evaluators.sql` (new), `crates/server/src/meta/evaluators.rs` (new), `crates/server/src/meta/score_definitions.rs` (new), `crates/server/src/meta/mod.rs` (module list and its migration tests), `crates/server/src/admin.rs` (tests only: `schema_mismatch_names_the_versions` and one new rotation test), `crates/server/tests/snapshots/meta_schema.snap`, `crates/server/src/crypto.rs` (`ENCRYPTED_COLUMNS` only), `crates/server/src/auth/roles.rs`, `docs/spec/04-metadata-db.md`, `docs/spec/08-auth.md`.

**Commit:** `Store evaluators and score definitions`

### Migration

If `main` already has a `0005_*.sql`, use the next free number and say so in the report.

```sql
CREATE TABLE evaluators (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  kind TEXT NOT NULL,
  config TEXT NOT NULL,
  output_score TEXT NOT NULL,
  connection_id TEXT REFERENCES llm_connections(id),
  secret_enc BLOB,
  version INTEGER NOT NULL DEFAULT 1,
  created_by TEXT REFERENCES users(id) ON DELETE SET NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  UNIQUE(project_id, name)
);
CREATE INDEX evaluators_connection ON evaluators(connection_id) WHERE connection_id IS NOT NULL;

CREATE TABLE score_definitions (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  data_type TEXT NOT NULL,
  categories TEXT,
  min_value REAL,
  max_value REAL,
  description TEXT,
  archived_at INTEGER,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX score_definitions_live ON score_definitions(project_id, name) WHERE archived_at IS NULL;
```

`connection_id` has the default `NO ACTION` foreign key: deleting an org cascades to both connections and evaluators within one statement and passes. Deleting a connection alone that evaluators use is refused by the route (I2.6) before SQLite sees it.

**Existing tests:** with migrations `0001`–`0005`:
- the migration count in `meta/mod.rs` (currently `assert_eq!(count, 4)`) becomes 5;
- `open_existing_checks_the_file_and_the_version` inserts version `6`, and expects `found == 6`, `expected == 5`;
- `admin.rs` `schema_mismatch_names_the_versions` inserts `6` and expects `"this binary expects schema 5, found 6"`.

Regenerate the snapshot with `INSTA_UPDATE=always cargo test -p llmobs-server schema_matches_the_snapshot`. Its diff must add only the two tables, the two named indexes, and SQLite's automatic indexes for the primary keys and the `UNIQUE(project_id, name)`. Count them in the diff, and list them in the report.

### `meta/evaluators.rs`

```rust
//! `evaluators` rows (spec 04, D-I-19). `secret_enc` is opaque bytes here.

/// One stored evaluator. `Debug` is written by hand and prints `secret_enc`'s length only.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct EvaluatorRow {
    pub(crate) id: Uuid,
    pub(crate) project_id: ProjectId,
    pub(crate) name: String,
    pub(crate) kind: String,
    pub(crate) config: String,          // JSON text
    pub(crate) output_score: String,    // JSON text
    pub(crate) connection_id: Option<Uuid>,
    pub(crate) secret_enc: Option<Vec<u8>>,
    pub(crate) version: u32,
    pub(crate) created_by: Option<Uuid>,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
}

/// The editable columns. Insert takes them with the id, project and author; update replaces them all.
pub(crate) struct EvaluatorFields {
    pub(crate) name: String,
    pub(crate) config: String,
    pub(crate) output_score: String,
    pub(crate) connection_id: Option<Uuid>,
    pub(crate) secret_enc: Option<Vec<u8>>,
    pub(crate) version: u32,
}

pub(crate) async fn list_evaluators(pool: &SqlitePool, project: ProjectId) -> Result<Vec<EvaluatorRow>, MetaError>;   // ORDER BY name, id
pub(crate) async fn get_evaluator(pool: &SqlitePool, project: ProjectId, id: Uuid) -> Result<Option<EvaluatorRow>, MetaError>;
pub(crate) async fn get_evaluator_tx(conn: &mut SqliteConnection, project: ProjectId, id: Uuid) -> Result<Option<EvaluatorRow>, MetaError>;
pub(crate) async fn insert_evaluator(conn: &mut SqliteConnection, id: Uuid, project: ProjectId, kind: &'static str, fields: EvaluatorFields, created_by: Option<Uuid>, now: i64) -> Result<EvaluatorRow, MetaError>;
pub(crate) async fn update_evaluator(conn: &mut SqliteConnection, project: ProjectId, id: Uuid, fields: EvaluatorFields, now: i64) -> Result<Option<EvaluatorRow>, MetaError>;
pub(crate) async fn delete_evaluator(conn: &mut SqliteConnection, project: ProjectId, id: Uuid) -> Result<bool, MetaError>;
/// True when any evaluator uses connection `id`.
pub(crate) async fn connection_in_use(conn: &mut SqliteConnection, id: Uuid) -> Result<bool, MetaError>;
```

Follow `meta/connections.rs` exactly:
- a private `#[derive(sqlx::FromRow)] struct EvaluatorRecord` (no `Debug`) and a `row_from` that maps each failure to `MetaError::Corrupt { table: "evaluators", column }`;
- literal SQL in each function;
- `fields` taken by value;
- `unique_or(error, "an evaluator with this name already exists")` on insert and update;
- reads back after writes, with `None` becoming `Corrupt { column: "id" }`.
- `version` is stored as `INTEGER` and read with `u32::try_from`.

Register `pub(crate) mod evaluators;` in `meta/mod.rs`.

### `meta/score_definitions.rs`

```rust
/// One stored definition.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DefinitionRow {
    pub(crate) id: Uuid,
    pub(crate) project_id: ProjectId,
    pub(crate) name: String,
    pub(crate) data_type: ScoreDataType,
    pub(crate) categories: Vec<DefinitionCategory>,    // `categories` JSON; NULL is empty
    pub(crate) min_value: Option<f64>,
    pub(crate) max_value: Option<f64>,
    pub(crate) description: Option<String>,
    pub(crate) archived_at: Option<i64>,
    pub(crate) created_at: i64,
    pub(crate) updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub(crate) struct DefinitionCategory {
    pub(crate) label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) value: Option<f64>,
}

pub(crate) struct DefinitionFields {
    pub(crate) name: String,
    pub(crate) categories: Vec<DefinitionCategory>,
    pub(crate) min_value: Option<f64>,
    pub(crate) max_value: Option<f64>,
    pub(crate) description: Option<String>,
    pub(crate) archived_at: Option<i64>,
}

/// `include_archived = false` returns live definitions only. ORDER BY name, id.
pub(crate) async fn list_definitions(pool: &SqlitePool, project: ProjectId, include_archived: bool) -> Result<Vec<DefinitionRow>, MetaError>;
pub(crate) async fn get_definition(pool: &SqlitePool, project: ProjectId, id: Uuid) -> Result<Option<DefinitionRow>, MetaError>;
pub(crate) async fn get_definition_tx(conn: &mut SqliteConnection, project: ProjectId, id: Uuid) -> Result<Option<DefinitionRow>, MetaError>;
pub(crate) async fn insert_definition(conn: &mut SqliteConnection, id: Uuid, project: ProjectId, data_type: ScoreDataType, fields: DefinitionFields, now: i64) -> Result<DefinitionRow, MetaError>;
pub(crate) async fn update_definition(conn: &mut SqliteConnection, project: ProjectId, id: Uuid, fields: DefinitionFields, now: i64) -> Result<Option<DefinitionRow>, MetaError>;
```

- `data_type` is stored with `ScoreDataType::as_str()` and read with `ScoreDataType::parse`; a failure is `Corrupt`.
- `categories` is NULL when the vector is empty, else `serde_json::to_string`.
- The partial unique index raises a unique violation for a duplicate live name: map it with `unique_or(error, "a score definition with this name already exists")`.
- There is no delete: the route archives with `update_definition` (sets `archived_at`).

Register the module.

### Permission and the sealed column

- `auth/roles.rs`, `ProjectPerm`: add `/// Evaluators, online rules, score definitions, test and manual runs (D-I-24).` `ManageEvaluation,`.
  - `min_role` → `Role::Member`.
  - `key_scope` → `None` (an API key cannot manage evaluation).
  - Add the variant to the test that asserts each `min_role` and `key_scope`, if there is one (read the module's tests).
- `crypto.rs` `ENCRYPTED_COLUMNS`: append `("evaluators", "secret_enc")`. Its test `encrypted_columns_cover_the_schema` now passes only with this row (it scans the schema for `*_enc` columns).
- Caps: `insert_evaluator` first runs `SELECT COUNT(*) FROM evaluators WHERE project_id = ?` and returns `MetaError::Conflict("a project has at most 500 evaluators")` at 500. `insert_definition` and an `update_definition` that clears `archived_at` run `SELECT COUNT(*) FROM score_definitions WHERE project_id = ? AND archived_at IS NULL` (excluding the row being restored) and return `MetaError::Conflict("a project has at most 1000 live score definitions")` at 1 000. Constants `PROJECT_CAP` in each module, as `meta/connections.rs` does.

### Spec

- `docs/spec/04-metadata-db.md` § Evaluation: replace the `evaluators` DDL with the migration's, and add one sentence: "`connection_id` is derived from a judge's config; `secret_enc` holds a webhook's sealed `{signing_secret, url, headers}`, and `config` then keeps `{url_host, timeout_ms, header_names}` (D-I-19)." § Scores definitions: replace `UNIQUE(project_id, name)` in the DDL with the partial unique index and add "`DELETE` archives; a name is unique among live definitions (D-I-26)."
- `docs/spec/08-auth.md`: below the role table, add "Evaluators, online rules and score definitions: project `member` and above; API keys cannot manage them (D-I-24)."

### Tests

`meta/evaluators.rs` (with `crate::meta::test_db()`, an org from `meta::orgs::insert_org`, a project from `meta::projects` (read it for its insert function), and a connection from `meta::connections::insert_connection` where `connection_id` is set):
- round trip of every column;
- the list is ordered by name;
- duplicate name → `Conflict`;
- update unknown → `None`;
- `delete_evaluator` reports whether a row went;
- `connection_in_use` is true, then false after the delete;
- deleting the org cascades;
- `Debug` hides `secret_enc` bytes.

`meta/score_definitions.rs`:
- round trip, with categories and without;
- a duplicate live name → `Conflict`;
- an archived duplicate is allowed;
- un-archiving into a clash → `Conflict`;
- `include_archived` filters;
- the 1 000 cap (insert 1 000 rows in a loop, the next is `Conflict`).

`meta/evaluators.rs` also: the 500 cap.

`admin.rs` tests: `rotation_reencrypts_evaluator_secrets`: seed an org, a project and an evaluator row with a `secret_enc` sealed by the old key (`Aad { table: "evaluators", column: "secret_enc", row_id: &id }`), rotate, and open it with the new key. Copy the seeding style of the existing rotation tests (raw SQL with every `NOT NULL` column of `0005`).

`auth/roles.rs` tests: the test that asserts each `ProjectPerm::min_role` and `key_scope` (near lines 262–280) gains `ManageEvaluation` → `Role::Member`, `None`.

### Done when

`just check` passes.

---

## I2.3 — Loading a trace for an evaluator

**Goal:** storage can read every column of one trace's spans with D-I-23's caps, and the server turns them into `llmobs_core::Span`s.

**Edit only:** `crates/storage/src/reads/traces.rs`, `crates/storage/src/reads/mod.rs`, `crates/storage/src/lib.rs` (re-exports), `crates/server/src/api/convert.rs`, `crates/server/src/eval/mod.rs`, `crates/server/src/eval/trace_load.rs` (new), `crates/server/src/http/mod.rs` (one field), `crates/server/src/serve.rs` and `crates/server/src/test_support/mod.rs` (that field's construction).

**Commit:** `Load a whole trace for evaluation`

### Storage (`reads/traces.rs`)

```rust
/// One trace's spans for an evaluator (D-I-23).
#[derive(Debug, Clone, Copy)]
pub struct TraceSpansRequest<'a> {
    /// 32 hex digits, either case.
    pub trace_id: &'a str,
    /// 16 hex digits, either case. Always returned when stored.
    pub target_span_id: Option<&'a str>,
    /// Most spans of the "earliest others" part.
    pub max_spans: usize,
    /// Most rows of the required part. The read stops at `max_required + 1` rows, so the caller
    /// can tell "over the cap" from "exactly at it".
    pub max_required: usize,
    /// Time allowed for both queries together.
    pub timeout: Duration,
}

/// The spans, every column of the `spans` view, in two parts.
#[derive(Debug)]
pub struct TraceSpansData {
    /// Parentless spans and the target span. May repeat a span of `earliest`.
    pub required: Vec<RecordBatch>,
    /// The earliest spans, `start_time ASC, span_id ASC`, at most `max_spans`.
    pub earliest: Vec<RecordBatch>,
    /// True when more than `max_spans` spans were stored.
    pub truncated: bool,
}

/// Every column of one trace's spans: the required ones and the earliest `max_spans`. No
/// default exclusion applies. An unknown trace gives two empty parts.
///
/// # Errors
///
/// [`RunError::InvalidRequest`] for a malformed id, `max_spans == 0` or `max_required == 0`;
/// [`RunError::Timeout`]; [`RunError::TooLarge`] when the query memory pool is exceeded.
pub async fn trace_spans(ctx: &QueryContext, request: TraceSpansRequest<'_>) -> Result<TraceSpansData, RunError>;
```

Implementation, inside one `tokio::time::timeout(request.timeout, …)`. Model it on `load_span` and `load_trace` in the same file:
1. `hex_id` both ids.
2. **required:** `ctx.session().table(SPANS)`, filtered by `col(spans::TRACE_ID).eq(lit(trace_id)).and(col(spans::PARENT_SPAN_ID).is_null().or(col(spans::SPAN_ID).eq(lit(target))))`, then `limit(0, Some(max_required + 1))`. Leave out the `or` clause when there is no target. Collect it with `ctx.fail` mapping, as `load_span` does.
3. **earliest:** the same table, filtered by trace id, sorted with `order_by(spans::START_TIME, &[spans::SPAN_ID], SortOrder::Asc)`, `limit(0, Some(max_spans + 1))`, collected.
4. `truncated` is true when the earliest part has more than `max_spans` rows in total. Then drop rows past `max_spans` with `RecordBatch::slice` on the last batch, as `load_trace` does with its cap.

Export `trace_spans`, `TraceSpansRequest` and `TraceSpansData` from `reads/mod.rs` and `lib.rs`, next to `trace_refresh`.

Storage tests, in the same file's `mod tests`. Build the spans with `crate::test_support::span_row(trace, span, start, ingest)` (set `parent_span_id` on the children yourself; read its doc), and the context with the module's existing `fn context_for(spans: &[Span]) -> QueryContext` helper (near line 1233), which wraps `spans_table`, `traces_table` and a `ProjectQuery`:
- `trace_spans_returns_roots_target_and_earliest`: six spans of one trace (one root, five children with increasing start times), `max_spans = 3`, `max_required = 10`, target the latest child → `required` holds the root and the target, `earliest` holds 3 rows, and `truncated` is true.
- `trace_spans_caps_the_required_part`: three parentless spans, `max_required = 2` → `required` has 3 rows (the caller sees it is over the cap).
- `trace_spans_unknown_trace_is_empty`.
- `trace_spans_rejects_bad_ids` (and `max_spans == 0`).

### Converter (`api/convert.rs`)

Refactor `span(batch, project_id)` so the columns are extracted once per batch. Calling a per-row function that re-extracts every column would be quadratic: `typed()` casts a whole column each time the type differs (Utf8View batches do occur), so each row would re-cast every column.
- Add a private `struct SpanColumns` with one field per column `span()` reads today, of the type its helper returns (`StringArray` for `strings`, `Int64Array` for `ints`, `Float64Array` for `floats`, the `times` type for `times`, `ListArray` for `lists`, `MapArray` for `maps`, `UInt16Array` for `schema_version`). Name each field like the local it replaces.
- `fn span_columns(batch: &RecordBatch) -> Result<SpanColumns, ConvertError>` holds today's `let x = strings(batch, …)?;` lines as field initializers.
- `impl SpanColumns { fn row(&self, row: usize, project_id: ProjectId) -> Result<Span, ConvertError> }` holds today's `Ok(Span { … })` with each local replaced by `self.<field>` and `row` the parameter.
- `span` keeps its signature and empty-batch check, then returns `span_columns(batch)?.row(0, project_id)`.
- Add:

```rust
/// Every row of `batches`, in order. Batches without rows add nothing. Columns are
/// extracted once per batch.
pub(crate) fn spans(batches: &[RecordBatch], project_id: ProjectId) -> Result<Vec<Span>, ConvertError>
```

Its body: for each batch with rows, `let columns = span_columns(batch)?;` then `columns.row(i, project_id)` for `i in 0..batch.num_rows()`.

Test (in `trace_load.rs`, because it needs the real spans view): store three spans of one trace with `app.commit_spans(&seeded.project, spans)`, open a context with `crate::routes::read::open_query(&app.state, project_id, None).await`, read them with `llmobs_storage::trace_spans`, and check `convert::spans` returns the three spans with `name`, `start_time`, `input` and `metadata` equal to the stored ones. The existing `span` tests keep passing unchanged.

### `eval/trace_load.rs`

```rust
/// At most this many spans besides the required ones (D-I-23).
pub(crate) const MAX_SPANS: usize = 5_000;
/// At most this many required spans (parentless ones and the target). More is `TooLarge`.
pub(crate) const MAX_REQUIRED: usize = 1_000;
/// At most this many payload bytes in all (D-I-23).
pub(crate) const MAX_PAYLOAD_BYTES: usize = 64 * 1024 * 1024;
/// Loads running at once (D-I-23).
pub(crate) const MAX_LOADS: u32 = 4;

/// Permits for trace loads, shared by test runs (M2) and jobs (M3).
#[derive(Debug)]
pub(crate) struct TraceLoads(tokio::sync::Semaphore);   // new(): Semaphore::new(usize::try_from(MAX_LOADS).unwrap_or(4)); impl Default calling new()

impl TraceLoads {
    /// The permits, for `load_trace` and for tests that hold them.
    pub(crate) fn semaphore(&self) -> &tokio::sync::Semaphore;
}

/// One loaded trace. Write `impl fmt::Debug` by hand, printing the span count and `truncated`
/// only: `Span` holds prompts and answers.
pub(crate) struct LoadedTrace {
    /// Deduplicated spans: required ones first, then the earliest others, each in start order.
    pub(crate) spans: Vec<Span>,
    /// Spans were left out (the span cap or the byte cap).
    pub(crate) truncated: bool,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum LoadError {
    #[error("trace not found")]
    NotFound,
    #[error("target span not found")]
    NoTargetSpan,
    #[error("trace too large")]
    TooLarge,
    #[error("target is an evaluation trace")]
    EvaluationTrace,
    #[error("storage is busy")]
    Busy,
    #[error("storage read failed")]
    Failed(#[source] RunError),
    #[error("could not read the spans")]
    Convert(#[source] ConvertError),
}

/// Loads `trace_id` of `project` for an evaluator, with D-I-23's caps and D-I-17's guard.
pub(crate) async fn load_trace(
    state: &AppState,
    project: ProjectId,
    trace_id: TraceId,
    target_span: Option<SpanId>,
    around: Option<i64>,
) -> Result<LoadedTrace, LoadError>;
```

Steps:
1. Acquire one permit of `state.trace_loads` with `tokio::time::timeout(READ_TIMEOUT, semaphore.acquire())`. A timeout → `LoadError::Busy`; a closed semaphore cannot happen, so map its error to `Busy` too.
2. Open the query directly, not through `routes::read::open_query`, whose `ApiError` cannot be matched. `state.storage.query(project, llmobs_storage::QueryOptions { around }).await` returns `Result<ProjectQuery, llmobs_storage::error::QueryError>` (the error type lives in the `error` module, not at the crate root; `QueryOptions` and `RunError` are at the root): `QueryError::Busy` → `LoadError::Busy`, and `QueryError::DataFusion(e)` → `LoadError::Failed(RunError::Internal(e))`. Read `QueryError`'s variants in `crates/storage/src/error.rs` and map any other variant to `Failed` with its nearest `RunError`. Then `QueryContext::new(source, None)` returns `Result<QueryContext, RunError>`; an error → `LoadError::Failed`.
3. `trace_spans` with `max_spans: MAX_SPANS`, `max_required: MAX_REQUIRED` and `timeout: READ_TIMEOUT` (`crate::routes::read::READ_TIMEOUT`). `RunError::Timeout` → `Busy`; `RunError::TooLarge` (the query memory pool was exceeded) → `TooLarge`; other errors → `Failed`. More than `MAX_REQUIRED` required rows → `TooLarge`.
4. Convert both parts with `convert::spans`.
5. Both parts empty → `NotFound`.
6. Deduplicate by `span_id`, keeping the row with the greatest `ingest_time` (a retried export can store a span twice).
7. When `target_span` is set and no span has it → `NoTargetSpan`.
8. Any span with `environment.as_deref() == Some(llmobs_core::names::environment::EVAL)` → `EvaluationTrace`.
9. Order and cap with `fn cap(spans: Vec<Span>, target: Option<SpanId>, max_bytes: usize) -> Result<(Vec<Span>, bool), LoadError>` (the `bool` is "something was skipped"):
   - required spans (parentless or the target) first, then the others, each group by `(start_time, span_id)`;
   - payload bytes of a span = the lengths of `input`, `output`, `input_preview`, `output_preview`, `system_instructions`, `tool_definitions` and `request_params`, plus every key and value length in `attributes`, `metadata` and `resource_attributes`, plus the length of `serde_json::to_string(&span.events)` and of `serde_json::to_string(&span.links)` (both derive `Serialize`; a serialization error counts as 0);
   - walk the list and keep a running total;
   - the **target** span that would pass `max_bytes` → `TooLarge` (D-I-23: the target alone over the cap fails);
   - any other span, roots included, that would pass it is skipped, and the flag becomes true.
   `truncated` is the flag, or storage's `truncated`.

The byte cap is applied after the read: what bounds memory during the read is the query memory pool (`storage.query.max_memory`, which turns into `RunError::TooLarge`), the row limits above, and `MAX_LOADS`.

`http/mod.rs` `AppInner` gains `/// Trace-load permits (D-I-23).` `pub(crate) trace_loads: crate::eval::trace_load::TraceLoads,`. `http/mod.rs` already has a module-level `#![cfg_attr(not(test), allow(dead_code))]`, and test builds read the field through the module's tests, so the field needs no attribute. Build it with `TraceLoads::default()` in `serve.rs` and `test_support`. Register `pub(crate) mod trace_load;` in `eval/mod.rs`.

The module has no production caller until I2.7. Put an item-level `#[allow(dead_code)] // test runs use it from I2.7 on` on each of: `MAX_SPANS`, `MAX_REQUIRED`, `MAX_PAYLOAD_BYTES`, `MAX_LOADS`, `TraceLoads`, `LoadedTrace`, `LoadError`, `load_trace`, `cap`, the private dedupe helper, and `convert::spans`, `SpanColumns` and `span_columns` if clippy flags them. I2.7 removes every one of these attributes, and the one on `Usage::add_judge`.

Tests (in `trace_load.rs`, with `TestApp`, `app.seed()`, and `app.commit_spans(&seeded.project, spans)` to store spans; build spans from `crate::test_support::dataset_span(0)` and set `trace_id`, `span_id`, `parent_span_id`, `start_time` and `environment` in the test):
- `loads_and_orders_the_trace`;
- `converts_every_row` (the `convert::spans` test above);
- `dedupes_by_span_id_keeping_the_latest`;
- `unknown_trace_is_not_found`;
- `missing_target_span`;
- `an_evaluation_trace_is_refused`;
- `loads_are_limited`: `let held = app.state.trace_loads.semaphore().try_acquire_many(MAX_LOADS).unwrap();` → `app.state.trace_loads.semaphore().available_permits() == 0` and `try_acquire()` fails; after `drop(held)` it has `MAX_LOADS` permits again. Do not use `tokio::time::pause()`: the server crate's tokio has no `test-util` feature;
- `cap` tested directly with a small `max_bytes`: other spans past the cap are skipped and the flag is set; a large root that is not the target is skipped too; a target over the cap → `TooLarge`.

### Done when

`just check` passes.

---

## I2.4 — The evaluation contract

**Goal:** API types, OpenAPI operations and examples for evaluators, templates, test runs and score definitions. No route yet.

**Edit only:**
- `crates/server/src/api/evaluators.rs` and `crates/server/src/api/score_definitions.rs` (new), `crates/server/src/api/mod.rs`, `crates/server/src/api/openapi.rs`;
- `crates/server/src/api_examples.rs`;
- `crates/server/tests/openapi.rs`, `crates/server/tests/api_examples.rs`;
- `crates/server/openapi.json`, `web/src/api/schema.gen.ts`, `fixtures/api/manifest.json` (generated);
- the fixtures listed in § Final layout, `fixtures/api/README.md`;
- `web/src/mocks/writes.test.ts` (the covered prefixes and `LATER_TRACKS`).

**Commit:** `Declare the evaluation API`

### Types

`api/evaluators.rs`: `EvaluatorKind`, `OutputScoreBody`, `Evaluator`, `CreateEvaluatorRequest`, `UpdateEvaluatorRequest`, `EvaluatorTemplate`, `TemplateMapping { variable, expression }`, `EvaluatorTemplateList { data: Vec<EvaluatorTemplate> }`, `TestEvaluatorBody`, `EvaluatorTestRequest`, `EvaluatorTestError { class: TestErrorClass, message }` (reuse `crate::api::connections::TestErrorClass`), `EvaluatorTestUsage { input_tokens?, output_tokens?, cost_usd? }`, `EvaluatorTestResult`. Fields, order and attributes are those of § Evaluator contract and § Test run contract.

`api/score_definitions.rs`: `ScoreDefinitionCategory`, `ScoreDefinition`, `CreateScoreDefinitionRequest`, `UpdateScoreDefinitionRequest`, `ScoreDefinitionListParams` with `#[serde(default)] include_archived: bool`, `limit: Limit` and `cursor: Option<String>`, written field by field exactly like `api/saved_views.rs` `SavedViewListParams` (derives, `deny_unknown_fields`, the `#[param(...)]` on `limit`).

`f64` fields rule out `Eq`: derive `PartialEq` only where a type holds an `f64`.

Derives:
- response types: `Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema` (plus `Eq` where possible);
- `CreateEvaluatorRequest`, `UpdateEvaluatorRequest`, `TestEvaluatorBody`: `Clone, PartialEq, Serialize, Deserialize, ToSchema`, no derived `Debug` (§ Conventions: write it by hand);
- `EvaluatorTestRequest`: `Deserialize, ToSchema` only, plus a hand-written `Debug`. It cannot derive `Serialize`, because `TimeParam` has none.

Fields typed `serde_json::Value` (`config`, `output`, `variables`) carry `#[schema(value_type = Object)]`, as `SavedView.state` does in `api/saved_views.rs`.

Every field has a doc comment. Register both modules in `api/mod.rs`.

### OpenAPI (`api/openapi.rs`)

- `collect_schemas`: insert every new type, plus `set.insert_page::<Evaluator>("EvaluatorPage")` and `set.insert_page::<ScoreDefinition>("ScoreDefinitionPage")`.
- Tags: add `tag("evaluation", "Evaluators, built-in templates and test runs.")` right after `scores`, and `tag("score_definitions", "Named score shapes that score writes and evaluators must fit.")` right after `evaluation`.
- New `fn evaluator_paths(paths)` and `fn score_definition_paths(paths)`, called in `paths()` right after `score_paths`. Every operation uses `project_param()`; ids use `path_uuid("id", …)`; there is no `.securities` override.

| operationId | Tag | Summary | Method and path (under `/api/v1/projects/{project}`) | Request | Success | Errors |
|---|---|---|---|---|---|---|
| `list_evaluators` | evaluation | List evaluators | GET `/evaluators` (+ `PageParams`) | — | 200 `json_named("EvaluatorPage", …)` | `QUERY_ERRORS` |
| `post_evaluator` | evaluation | Create an evaluator | POST `/evaluators` | `CreateEvaluatorRequest` | 201 `Evaluator` | 400, 401, 403, 404, 409, 429 |
| `get_evaluator` | evaluation | Get an evaluator | GET `/evaluators/{id}` | — | 200 `Evaluator` | 400, 401, 403, 404, 429 |
| `patch_evaluator` | evaluation | Update an evaluator | PATCH `/evaluators/{id}` | `UpdateEvaluatorRequest` | 200 `Evaluator` | 400, 401, 403, 404, 409, 429 |
| `delete_evaluator` | evaluation | Delete an evaluator | DELETE `/evaluators/{id}` | — | 204 | 400, 401, 403, 404, 429 |
| `list_evaluator_templates` | evaluation | List built-in judge templates | GET `/evaluator-templates` | — | 200 `EvaluatorTemplateList` | 401, 403, 404, 429 |
| `post_evaluator_test` | evaluation | Run an evaluator once | POST `/evaluators/test` | `EvaluatorTestRequest` | 200 `EvaluatorTestResult` | 400, 401, 403, 404, 429, 503 |
| `list_score_definitions` | score_definitions | List score definitions | GET `/score-definitions` (+ `ScoreDefinitionListParams`) | — | 200 `json_named("ScoreDefinitionPage", …)` | `QUERY_ERRORS` |
| `post_score_definition` | score_definitions | Create a score definition | POST `/score-definitions` | `CreateScoreDefinitionRequest` | 201 `ScoreDefinition` | 400, 401, 403, 404, 409, 429 |
| `patch_score_definition` | score_definitions | Update or restore a score definition | PATCH `/score-definitions/{id}` | `UpdateScoreDefinitionRequest` | 200 `ScoreDefinition` | 400, 401, 403, 404, 409, 429 |
| `delete_score_definition` | score_definitions | Archive a score definition | DELETE `/score-definitions/{id}` | — | 204 | 400, 401, 403, 404, 429 |

Descriptions:
- reads: "Any project member can read.";
- writes and the test: "Project members and above. API keys cannot call it.";
- `post_evaluator_test`, in addition: "Runs synchronously, writes the judge trace (environment `llmobs-eval`) and the judge usage, and writes no score. Shares the 10 per minute per user limit with connection tests.";
- `delete_score_definition`: "Archives the definition; score writes stop being checked against it."

Run `just openapi`.

### `tests/openapi.rs`

- `OPERATIONS`: the eleven rows.
- `TAGS`: `"evaluation"` and `"score_definitions"` right after `"scores"`.
- `QUERY_OR_BODY`: `list_evaluators`, `list_score_definitions`.
- The error-set chain, before the `READ_OPERATIONS` arm:

```rust
        } else if *id == "post_evaluator"
            || *id == "patch_evaluator"
            || *id == "post_score_definition"
            || *id == "patch_score_definition"
        {
            expected_errors.extend(["400", "401", "403", "404", "409", "429"]);
        } else if *id == "get_evaluator"
            || *id == "delete_evaluator"
            || *id == "delete_score_definition"
        {
            expected_errors.extend(["400", "401", "403", "404", "429"]);
        } else if *id == "post_evaluator_test" {
            // The test only collects 4xx codes; the operation also declares 503.
            expected_errors.extend(["400", "401", "403", "404", "429"]);
```

  (`list_evaluator_templates` takes the default `401, 403, 404, 429` arm.)
- The two `pages` lists: add `("EvaluatorPage", "Evaluator")` and `("ScoreDefinitionPage", "ScoreDefinition")`, and the two names.

### Fixtures and examples

Write each file in its struct's field order, with two-space indentation and a final newline, as M1 did. If a round-trip fails, print the canonical form the test computes and copy it.

- `static/evaluators.json`: a page with the fixture judge evaluator:
  - `config`: `{"connection_id": "01900000-0000-7000-8000-000000000400", "model": "gpt-5-mini", "prompt": "Rate how relevant the answer is to the question.\n\nQuestion: {{ input }}\nAnswer: {{ output }}", "output": {"type": "numeric", "min": 0.0, "max": 1.0}, "include_reason": true, "max_output_tokens": 1024, "timeout_ms": 60000}`. That is `JudgeConfig`'s serialization order, except that it has no `skip_serializing_if` on `temperature`, so the stored and returned JSON has `"temperature": null` after `include_reason`. Write the fixture that way;
  - `output_score`: `{"name": "relevance", "data_type": "numeric"}`;
  - `has_signing_secret` false, `version` 1, `created_by` `…0100`, times `2026-01-01T09:00:00.000000000Z`;
  - `next_cursor: null`.
- `static/evaluator.json`: that evaluator.
- `static/evaluator-templates.json`: the `{data: […]}` of `builtin_templates()`. Write the mapping once now, in `api/evaluators.rs`: `impl From<&llmobs_eval::JudgeTemplate> for EvaluatorTemplate` (`output` through `serde_json::to_value`, which cannot fail for `JudgeOutput`; map an error to `Value::Null`), and `pub fn builtin_template_list() -> EvaluatorTemplateList`. I2.6's handler calls that function. Generate the fixture by printing `serde_json::to_string_pretty(&builtin_template_list())` from a throwaway test, which you do not commit.
- `static/evaluator-test-result.json`:
  - `{"ok": true, "value": {"data_type": "numeric", "value": 0.9}, "reason": "The answer addresses the question.", "prompt": "Rate how relevant …", "variables": {"input": "What is the refund window?", "output": "30 days."}, "usage": {"input_tokens": 120, "output_tokens": 18, "cost_usd": 0.00004}, "latency_ms": 840, "trace_truncated": false, "judge_trace_id": "e1000000000000000000000000000001"}`;
  - `value` is `ScoreValue`'s JSON (`{"data_type": …, "value": …}`), as in `requests/score-batch.json`.
- `static/score-definitions.json` and `static/score-definition.json`: the fixture definition (`categories: []`, `min_value` `0.0`, `max_value` `1.0`, written with the decimal point, since that is how serde writes an `f64`; `description` "How relevant the answer is, from 0 to 1.").
- `requests/evaluator-create.json`: the fixture evaluator's `name`, `kind`, `config` and `output_score`.
- `requests/evaluator-test.json`: `{"evaluator_id": "01900000-0000-7000-8000-000000000500", "trace_id": "a1000000000000000000000000000001", "variable_mapping": {"input": "trace.input", "output": "trace.output"}}`.
- `requests/score-definition-create.json`: `{"name": "relevance", "data_type": "numeric", "min_value": 0.0, "max_value": 1.0}`.
- `fixtures/api/README.md` § Fixed identifiers: add the evaluator and score-definition rows, and "Judge trace (test result example) | `e1000000000000000000000000000001`; it is not in the dataset".

`api_examples.rs`, in the static examples list after the connection examples:
- `example("list_evaluators", BTreeMap::new(), BTreeMap::new(), "static/evaluators.json", "application/json")`;
- `example("get_evaluator", params(&[("id", "01900000-0000-7000-8000-000000000500")]), …)`;
- `example_created("post_evaluator", BTreeMap::new(), "static/evaluator.json")`;
- `example("list_evaluator_templates", …, "static/evaluator-templates.json", …)`;
- `example("post_evaluator_test", BTreeMap::new(), BTreeMap::new(), "static/evaluator-test-result.json", "application/json")`;
- `example("list_score_definitions", …, "static/score-definitions.json", …)`;
- `example_created("post_score_definition", BTreeMap::new(), "static/score-definition.json")`.

`project` is never in `path_params` (README § Manifest). Run `just api-examples`.

`tests/api_examples.rs`:
- imports;
- `static_files_match_their_types` round-trips the six static files;
- `request_examples_match_their_types_and_the_openapi_document` round-trips `requests/evaluator-create.json` and `requests/score-definition-create.json`. `requests/evaluator-test.json` cannot round-trip (`EvaluatorTestRequest` is `Deserialize` only): instead, parse it with `serde_json::from_str::<EvaluatorTestRequest>` to check it deserializes. All three go in the schema-check list (`("post_evaluator", "requests/evaluator-create.json")`, `("post_evaluator_test", "requests/evaluator-test.json")`, `("post_score_definition", "requests/score-definition-create.json")`);
- `generated_files_roundtrip` gets arms:
  - `"list_evaluators"` → `Page<Evaluator>`;
  - `"get_evaluator" | "post_evaluator"` → `Evaluator`;
  - `"list_evaluator_templates"` → `EvaluatorTemplateList`;
  - `"post_evaluator_test"` → `EvaluatorTestResult`;
  - `"list_score_definitions"` → `Page<ScoreDefinition>`;
  - `"post_score_definition"` → `ScoreDefinition`.

### Web mocks test (`web/src/mocks/writes.test.ts`)

- Add `path.startsWith("/api/v1/projects/{project}/evaluators") || path.startsWith("/api/v1/projects/{project}/score-definitions")` to the `covered` expression.
- Fill `LATER_TRACKS` with `post_evaluator`, `patch_evaluator`, `delete_evaluator`, `post_evaluator_test`, `post_score_definition`, `patch_score_definition` and `delete_score_definition`. I2.8 lists the score-definition writes; I2.9 and I2.10 list the evaluator writes and empty the array.

### Done when

`just check` passes, including `contract_file_is_up_to_date`, `document_lint`, `manifest_covers_the_read_operations`, `manifest_matches_the_operations`, `generated_files_roundtrip`, and the web type check and tests.

---

## I2.5 — Score definition routes and checks

**Goal:** CRUD for score definitions, and every score write checked against the live definition of its name.

**Edit only:** `crates/server/src/eval/mod.rs`, `crates/server/src/eval/definitions.rs` (new), `crates/server/src/routes/score_definitions.rs` (new), `crates/server/src/routes/mod.rs`, `crates/server/src/http/access.rs` (`routes()` list), `crates/server/src/http/mod.rs`, `crates/server/src/serve.rs`, `crates/server/src/test_support/mod.rs` (the new field), `crates/server/src/scores.rs`, `docs/spec/06-evaluation.md`, `docs/spec/09-api.md`.

**Commit:** `Add score definitions and check score writes`

### Cache (`eval/definitions.rs`)

```rust
/// How long a project's definitions are reused (D-I-26).
/// An entry is expired when `now - loaded_at >= TTL_NS`.
const TTL_NS: i64 = 10_000_000_000;

/// Live definitions per project, reused for 10 s and dropped on writes. `Debug` prints the count.
#[derive(Default)]
pub(crate) struct DefinitionCache { /* std::sync::Mutex<HashMap<ProjectId, (i64 /* loaded at */, Arc<Vec<DefinitionRow>>)>> */ }

impl DefinitionCache {
    /// The live definitions of `project`, from the cache or from `meta.db`.
    pub(crate) async fn live(&self, meta: &MetaDb, project: ProjectId, now_ns: i64) -> Result<Arc<Vec<DefinitionRow>>, MetaError>;
    /// Drops `project` after a definition write.
    pub(crate) fn forget(&self, project: ProjectId);
}
```

At most 1 024 projects are cached; when full, clear the map first. Do not hold the lock across the `.await`: read under the lock, drop it, query, then insert. Add `/// Live score definitions (D-I-26).` `pub(crate) definitions: crate::eval::definitions::DefinitionCache,` to `AppInner`, with `Default` in `serve.rs` and `test_support`.

### Checks in `scores::write`

- At the start of `write`, after reading `now`, load `let definitions = state.definitions.live(&state.meta, project.id, now).await?;` (a `MetaError` becomes `ApiError` through the existing `From`).
- Pass `&definitions` into `check_item`, which appends the field errors of § Score definition checks when a live definition with `item.name` exists.
- Add `pub(crate) fn fits_definition(output: &llmobs_eval::OutputScore, judge_output: Option<&llmobs_eval::JudgeOutput>, definition: &DefinitionRow) -> bool` (the evaluator rule of § Score definition checks) in `eval/definitions.rs`. I2.6 uses it, so give it an item-level `#[allow(dead_code)] // evaluator saves use it from I2.6 on`, and test it here.

### Routes (`routes/score_definitions.rs`)

| Method | Path | Access |
|---|---|---|
| GET | `/api/v1/projects/{project}/score-definitions` | `Access::Project { perm: ProjectPerm::View, session_only: false }` |
| POST | same | `Access::Project { perm: ProjectPerm::ManageEvaluation, session_only: false }` |
| PATCH | `/api/v1/projects/{project}/score-definitions/{id}` | `ManageEvaluation` |
| DELETE | same | `ManageEvaluation` |

All are `documented: true`. Register the module, and add `routes.extend(crate::routes::score_definitions::routes());` after the scores routes in `http/access.rs`.

Handlers follow M1's `routes/connections.rs` (extractors `ProjectAccess`, `Principal` via `principal::session_user`, `IdPath { id: Uuid }`):
- **list:**
  - `include_archived` from the query;
  - convert the rows;
  - page with `crate::routes::orgs::page_named` keyed by `(name, id)`.
- **create:**
  1. validate (§ Score definition contract);
  2. `Uuid::now_v7()`;
  3. `insert_definition` in `state.meta.write`;
  4. `state.definitions.forget(project.id)`;
  5. audit `tracing::info!(target: "audit", score_definition_id = %id, project_id = %project.id, user_id = %user.id, "score definition created")`;
  6. answer 201.
- **update** (optimistic check, because validation needs the stored `data_type`):
  1. Read the row with `get_definition` (I2.2); unknown → 404.
  2. Merge the PATCH onto it (absent keeps the stored value; `archived: Some(true)` sets `archived_at = now`, `Some(false)` clears it; `description: Some("")` clears it).
  3. Validate the merged values with the full § Score definition contract table, using the stored `data_type`. Report only errors on fields present in the body, plus the cross-field ones (`min_value >= max_value` when the body set either bound; categories when the body set them).
  4. One `meta.write`: `get_definition_tx`, and when the row is gone → 404; when it differs from the row read in step 1 (`DefinitionRow` derives `PartialEq`) → `MetaError::Conflict("score definition changed, retry")`; else `update_definition`.
  5. `forget`; audit "score definition updated".
- **delete:** archive (set `archived_at = now` if not already set) → 204; unknown id → 404; `forget`; audit "score definition archived".
- `fn definition_json(row) -> ScoreDefinition`.

### Spec

- `docs/spec/06-evaluation.md` § Scores, the sentence "If a score definition with the same name exists, values are validated against it (range, categories) — otherwise accepted as is": append "Only live (non-archived) definitions count; definitions are cached 10 s per project (D-I-26)."
- `docs/spec/09-api.md` § Scores & annotations: replace the row `GET/POST /score-definitions · PATCH/DELETE …/{id}` with "`GET/POST /score-definitions` · `PATCH/DELETE …/{id}`: `DELETE` archives and `PATCH {archived: false}` restores; `?include_archived=`; members and above write (D-I-26)".

### Tests (`routes/score_definitions.rs`, `TestApp` + `seed`; member and viewer through M1's copied helpers `signed_in` and `add_member`, copied again here)

- create, list, update and archive round trip, with every field;
- `validation_reports_every_field`: the table's six messages;
- duplicate live name → 409;
- an archived duplicate → 201;
- restoring into a clash → 409;
- a viewer can list but not write (403); a member can write;
- an API key with `read` can list (`seeded.read_key`); one with `write` cannot POST (403). `Seeded` has no `write` key: create one through `POST /api/v1/projects/{p}/api-keys` with the owner's cookie and `{"kind": "secret", "scopes": ["ingest", "read", "write"]}`, and use the answer's `secret`;
- `patch_validates_against_the_stored_type`: a numeric definition, PATCH `{"categories": [{"label": "a"}]}` → 400 `categories`; a categorical one, PATCH `{"min_value": 0.0}` → 400 `min_value`;
- `empty_patch_only_bumps_updated_at`;
- `names_are_trimmed`;
- `list_pages_by_name` (`?limit=1`, following `next_cursor`);
- `another_projects_definition_is_404` (a second project created through `POST /api/v1/orgs/acme/projects`);
- `a_stale_patch_is_409`: test the in-transaction part directly. The handler's step 4 is a function in `routes/score_definitions.rs`:

  ```rust
  /// Step 4 of the update: refuses a row that changed since `expected` was read.
  async fn write_update(
      conn: &mut SqliteConnection,
      project: ProjectId,
      id: Uuid,
      expected: DefinitionRow,
      fields: DefinitionFields,
      now: i64,
  ) -> Result<Option<DefinitionRow>, MetaError>;
  ```

  It returns `Ok(None)` when the row is gone, `Err(MetaError::Conflict("score definition changed, retry"))` when `get_definition_tx` returns a row different from `expected`, and otherwise the result of `update_definition`. The handler's `meta.write` closure only calls it. The test reads the row with `get_definition`, changes its description with a direct `UPDATE score_definitions SET description = 'x', updated_at = updated_at + 1 WHERE id = ?`, then calls `write_update` inside `app.state.meta.write` with the stale row → `Conflict("score definition changed, retry")`.

In `scores.rs` tests (write through `POST /api/v1/projects/{p}/scores` as the existing tests do):
- `scores_must_fit_their_live_definition`: numeric out of range, wrong type and unknown label → 400 with the exact messages; a fitting score → 200;
- `archived_definitions_are_not_checked`;
- `definitions_are_cached_until_a_write`: create a definition, write a score that fails, archive it through the route → the same write succeeds at once (the route's `forget`).

`eval/definitions.rs` tests:
- `fits_definition` for each case of § Score definition checks;
- `live` caches by the manual clock: a change in `meta.db` is not seen 9.999 s later (`advance(Duration::from_millis(9_999))`), and is seen at exactly 10 s (`advance` 1 ms more); `forget` drops the entry at once.

### Done when

`just check` passes.

---

## I2.6 — Evaluator routes and templates

**Goal:** evaluator CRUD with sealed webhook values, the version rule, the templates list, and the connection DELETE 409.

**Edit only:** `crates/server/src/eval/mod.rs`, `crates/server/src/eval/evaluator.rs` (new), `crates/server/src/eval/definitions.rs` (remove `fits_definition`'s allow), `crates/server/src/routes/evaluators.rs` (new), `crates/server/src/routes/mod.rs`, `crates/server/src/http/access.rs`, `crates/server/src/routes/connections.rs`, `crates/server/src/api/openapi.rs` (`delete_llm_connection` gains 409), `crates/server/tests/openapi.rs` (that arm), `crates/server/openapi.json`, `web/src/api/schema.gen.ts` (generated), `docs/spec/06-evaluation.md`, `docs/spec/09-api.md`.

**Commit:** `Add evaluator routes and templates`

### `eval/evaluator.rs`

```rust
/// A webhook's sealed values (D-I-19).
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct WebhookSecrets {
    pub(crate) signing_secret: String,
    pub(crate) url: String,
    #[serde(default)]
    pub(crate) headers: BTreeMap<String, String>,
}
// Write `impl fmt::Debug for WebhookSecrets` by hand: it prints the header names only.

/// A validated evaluator ready to store. No `Debug`: it holds the webhook secrets.
pub(crate) struct Prepared {
    pub(crate) evaluator: llmobs_eval::Evaluator,    // the parsed, validated value
    pub(crate) config_json: String,                  // what `evaluators.config` stores (§ Storage of an evaluator)
    pub(crate) output_json: String,
    pub(crate) connection_id: Option<Uuid>,
    pub(crate) secrets: Option<WebhookSecrets>,      // webhook only
}

/// Validates a body (§ Evaluator contract) for `kind`; `prefix` is "" for the routes and "evaluator." for test runs.
pub(crate) fn prepare(id: Uuid, name: &str, kind: EvaluatorKind, config: &serde_json::Value, output: &serde_json::Value, webhook_secret: Option<&str>, prefix: &str) -> Result<Prepared, Vec<FieldError>>;

/// The runnable evaluator of a stored row: the config parsed back, plus the opened webhook values.
pub(crate) fn load(secrets: &SecretBox, row: &EvaluatorRow) -> Result<(llmobs_eval::Evaluator, Option<WebhookSecrets>), EvaluatorError>;

#[derive(Debug, thiserror::Error)]
pub(crate) enum EvaluatorError {
    #[error(transparent)]
    Crypto(#[from] CryptoError),
    #[error("corrupt evaluators.{0}")]
    Corrupt(&'static str),
}
```

`WebhookConfig` (E's type) still derives `Serialize` with `url` and `headers`. No route may serialize it: only `config_json` (`url_host`, `timeout_ms`, `header_names`) is stored or returned. The `webhook_values_are_sealed` test below checks the response text.

- `prepare`:
  1. Parse `output` as `OutputScore` (field `output_score`), then check `output_score.name` is 1–255 bytes.
  2. For a webhook, `config` is the full document; keep `url` and `headers` aside for `WebhookSecrets`.
  3. `EvaluatorConfig::from_parts(kind.as_str(), config)`.
  4. Build `Evaluator { id, name: name.trim().to_owned(), config, output }` and call `validate()`.
  5. Map every `ConfigError` per the table (field names prefixed with `prefix`).
  6. Webhook secret rules, and the header-value rule of § Evaluator contract: a private `fn check_header_values(headers: &BTreeMap<String, String>, prefix: &str) -> Vec<FieldError>` (each value through `HeaderValue::from_str` and ≤ 4096 bytes; names equal after lowercasing). Model it on `check_extra_headers` in `routes/connections.rs` (do not import it, it is private).
  7. For a judge, `connection_id = Some(config.connection_id)`.
  8. `config_json` per § Storage. Use the URL's host from `url::Url::parse(&url)`, and header names lowercased and sorted.
  9. `output_json = serde_json::to_string(&parsed_output)`, where `parsed_output` is the `OutputScore` parsed in step 1, never the raw request `Value`. A resend with keys reordered or `categories: null` then produces the same string and does not bump `version`.
  The org check of the connection and the definition check need `meta.db`, so they are done by the caller.
- `load`:
  1. `EvaluatorConfig::from_parts(&row.kind, &config)`. For a webhook, `config` first becomes `{url, timeout_ms, headers}` built from the opened `WebhookSecrets` and the stored `timeout_ms`.
  2. Open the secrets with `Aad { "evaluators", "secret_enc", &row.id }`.
  3. Any parse failure → `Corrupt("<column>")`.

### Routes (`routes/evaluators.rs`)

| Method | Path | Access |
|---|---|---|
| GET | `/api/v1/projects/{project}/evaluators` | `View` |
| POST | same | `ManageEvaluation` |
| GET | `/api/v1/projects/{project}/evaluators/{id}` | `View` |
| PATCH | same | `ManageEvaluation` |
| DELETE | same | `ManageEvaluation` |
| GET | `/api/v1/projects/{project}/evaluator-templates` | `View` |

All are `documented: true` and `session_only: false`. Register them after the score-definition routes.

- **create:**
  1. `prepare`.
  2. If a judge: `meta::connections::get_connection(reader, project.org_id, connection_id)`, else the field error of the table.
  3. If a live definition named `output.name` exists: `fits_definition`, else the field error.
  4. Seal the `WebhookSecrets` JSON with the new id.
  5. `insert_evaluator` with `version: 1`.
  6. Audit "evaluator created", plus `url_host` for a webhook (a host is configuration, not payload).
  7. 201 with `evaluator_json(&row)`.
- **update** — an **optimistic check**. A webhook PATCH that changes only the URL must re-seal it together with the stored secret and headers, so opening the stored values (which needs `state.secrets`) cannot happen inside the `Send` closure. The handler therefore prepares everything outside the transaction, and the transaction refuses to write over a row that changed meanwhile:
  1. `get_evaluator` (reader), else 404.
  2. Merge: `name` from the body or the row; `config` from the body, else the row's config re-assembled. For a webhook that is the opened URL and headers plus the stored timeout; read them with `load`. `output_score` from the body or the row's; `webhook_secret` from the body or the opened one.
  3. `prepare` the merged values.
  4. Run the connection and definition checks.
  5. Seal before the transaction; when the merged webhook values equal the opened ones (no `config` and no `webhook_secret` in the body), keep the row's `secret_enc` bytes instead of re-sealing.
  6. Compute `version` in the handler, before the transaction: the row's `version`, plus 1 when `config_json`, `output_json` or the merged `WebhookSecrets` JSON differs from the stored config, the stored output and the opened secrets JSON (compare the `serde_json::to_string` of the opened and the merged `WebhookSecrets`). Computing it from the row read in step 1 is safe, because step 7 refuses a row that changed. Put it in `EvaluatorFields.version`.
  7. In `meta.write`, call `write_update`, a function in `routes/evaluators.rs`:

     ```rust
     /// What the update transaction did.
     enum Outcome {
         Updated(EvaluatorRow),
         Gone,
         ConnectionMissing,
     }

     /// Step 7 of the update: refuses a row that changed since `expected` was read.
     async fn write_update(
         conn: &mut SqliteConnection,
         org: OrgId,
         project: ProjectId,
         id: Uuid,
         expected: EvaluatorRow,
         fields: EvaluatorFields,
         now: i64,
     ) -> Result<Outcome, MetaError>;
     ```

     - `get_evaluator_tx`: gone → `Ok(Outcome::Gone)`. A row different from `expected` (`EvaluatorRow` derives `PartialEq`) → `Err(MetaError::Conflict("evaluator changed, retry"))`, which becomes 409.
     - For a judge (`fields.connection_id` is `Some`), `meta::connections::get_connection_tx(conn, org, connection_id)`: `None` → `Ok(Outcome::ConnectionMissing)`.
     - Otherwise `update_evaluator`. Its `None` → `Outcome::Gone`, and `Some(row)` → `Outcome::Updated(row)`.

     The handler maps `Gone` to 404 and `ConnectionMissing` to the 400 field error `config.connection_id` "connection not found in this organization".
  Audit "evaluator updated". Answer 200.
  On **create**, the same connection re-check (`get_connection_tx`) runs inside the insert's transaction, before `insert_evaluator`; the closure returns the same `Outcome` enum (`Updated` carrying the inserted row).
- **delete:** `delete_evaluator` → 204, or 404. Audit "evaluator deleted". (M3 adds the 409 for evaluators that rules use.)
- **list:** paged by name with `page_named`.
- **get:** 404 when unknown.
- **templates:** `crate::api::evaluators::builtin_template_list()` (I2.4). It must serialize exactly to `static/evaluator-templates.json` (a test checks it).
- `fn evaluator_json(row) -> Result<Evaluator, ApiError>`: `config` and `output_score` parsed back from their JSON text; `has_signing_secret = row.secret_enc.is_some()`.

### Connection DELETE 409 (`routes/connections.rs`)

In the delete handler's `meta.write` closure, call `meta::evaluators::connection_in_use(conn, id)` first. When it is true, return `Err(MetaError::Conflict("connection is used by evaluators"))`, which becomes 409. In `api/openapi.rs`, `delete_llm_connection`'s errors become `&[400, 401, 403, 404, 409, 429]`, and `tests/openapi.rs` moves `delete_llm_connection` into the 409 arm. Run `just openapi`.

### Spec

- `docs/spec/06-evaluation.md` § `webhook`: replace the config example's `"secret_enc": "…"` with a sentence: "The URL, the headers and the signing secret are sealed in `evaluators.secret_enc`; the stored config and API responses show only `url_host`, `timeout_ms` and the header names (D-I-19)."
- `docs/spec/09-api.md` § Evaluation & alerts: replace the row `GET /evaluator-templates` with "`GET /evaluator-templates`: built-in judge templates with their default mapping; the UI pre-fills the editor from one (D-I-20)", and append to the `/evaluators` row "; `kind` is immutable, webhook values are write-only (D-I-19)".

### Tests (`routes/evaluators.rs`; a project from `seed`, a connection created through M1's route by the owner)

- `judge_round_trip`: create the fixture body → 201 equal to `static/evaluator.json` except id and times; GET and the list return it.
- `check_round_trip`: a `contains` check with `value` → stored with `values` (E's folding). `Check::Contains.value` has no `skip_serializing_if`, so the stored and returned config carries `"value": null`; assert that, and the I2.9 form state ignores `value`.
- `webhook_values_are_sealed`:
  - the response has `config == {url_host, timeout_ms, header_names}` and `has_signing_secret: true`;
  - the response text contains neither the secret nor the URL path nor a header value;
  - the stored `secret_enc` opens to the original values with the row-bound AAD.
- `webhook_needs_a_secret` and `webhook_secret_only_for_webhooks`.
- `validation_maps_config_errors`: an unknown check kind, a judge without a prompt and an output type mismatch → the table's fields and messages.
- `connection_must_be_in_the_org`: a connection id of another org → 400 `config.connection_id`.
- `output_must_fit_the_definition`.
- `version_bumps_only_on_content`: a rename → version 1; a prompt change → 2; a webhook secret change → 3.
- `kind_cannot_change`: PATCH `{"kind": "check"}` → 400.
- `patch_webhook_without_config_keeps_the_sealed_values`.
- `duplicate_name_is_409`.
- `member_writes_viewer_reads`; `api_key_cannot_write`.
- `templates_match_the_fixture`: GET equals `fixtures/api/static/evaluator-templates.json` parsed.
- `connection_delete_is_409_while_used`; after the evaluator is deleted → 204.
- `another_projects_evaluator_is_404` (a second project created through the projects route).
- `patch_validation_maps_fields`: PATCH `{"config": {…prompt: ""}}` → 400 `config.prompt`; PATCH `{"output_score": {"name": "", "data_type": "numeric"}}` → 400 `output_score.name`.
- `a_stale_patch_is_409`: read the row with `get_evaluator`, change it directly (`UPDATE evaluators SET updated_at = updated_at + 1 WHERE id = ?`), then call `write_update` inside `app.state.meta.write` with the stale row → `Conflict("evaluator changed, retry")`. Also `write_update` with a judge whose connection row was deleted → `Outcome::ConnectionMissing`.
- `webhook_header_values_are_checked`: a header value with `\r\n` → 400 `config.headers.<name>`; `X-A` and `x-a` → 400 `config.headers.x-a`.
- `evaluator_cap_is_500`: insert 500 rows directly through `meta::evaluators::insert_evaluator` in a loop, then POST → 409.
- `list_pages_by_name`.
- `no_webhook_values_in_logs`: with `capture_logs` (plain `#[tokio::test]`), create and update a webhook evaluator; the logs contain "evaluator created" and the `url_host`, and contain neither the URL path, nor a header value, nor the signing secret.

### Done when

`just check` passes.

---

## I2.7 — Test runs

**Goal:** `POST …/evaluators/test` runs an evaluator once, as § Test run contract says.

**Edit only:** `crates/server/src/eval/mod.rs`, `crates/server/src/eval/test_run.rs` (new), `crates/server/src/eval/trace_load.rs`, `crates/server/src/api/convert.rs` and `crates/server/src/usage.rs` (each only to remove the I2.1 and I2.3 dead-code allows), `crates/server/src/routes/evaluators.rs`, `crates/server/src/test_support/mod.rs` (register one module), `crates/server/src/test_support/mock_webhook.rs` (new), `docs/spec/06-evaluation.md`, `docs/spec/09-api.md`.

**Commit:** `Run evaluators once from the test endpoint`

### `eval/test_run.rs`

```rust
/// Runs one test (D-I-21). `user` is the caller, for the rate limit.
pub(crate) async fn run(state: &AppState, project: &ProjectRow, user: Uuid, body: EvaluatorTestRequest) -> Result<EvaluatorTestResult, ApiError>;
```

Steps:
1. **Rate limit.** Check `state.limits.connection_tests` for `user` exactly as M1's connection test does. A refusal is `ApiError::RateLimited`.
2. **Validate the body** (§ Test run contract), collecting field errors.
3. **Resolve the evaluator:**
   - `evaluator_id` → `get_evaluator` (404 when unknown), then `eval::evaluator::load`, giving `(Evaluator, Option<WebhookSecrets>)`;
   - `evaluator` (a `TestEvaluatorBody`) → when `evaluator_id` is also given, load the saved row first and check `kind` matches; fill an absent `name` (saved name, else `"Test"`), an absent `config` (the saved config; for a webhook, re-assembled from the opened sealed values and the stored timeout, as the update handler does) and an absent `webhook_secret` (the opened saved one). Then `prepare(id, …, prefix "evaluator.")`, with `id` the saved id or `Uuid::nil()`. The rules and messages are § Test run contract's;
   - for a judge, the connection must be in the org (field error);
   - the connection row from `get_connection`, then `state.connections.get(&state.secrets, &row)` (M1's cache).
4. **Variables:**
   - variables mode: `Variables::from_map(body.variables)`;
   - trace mode: `eval::trace_load::load_trace(state, project.id, trace_id, span_id, trace_start)`. Map `NotFound` and `NoTargetSpan` to 404, `EvaluationTrace` to the 400 of the error table, `Busy` to `ApiError::Unavailable { detail: "storage is busy" }`, and `TooLarge` to an `ok: false` answer with `class: permanent, message: "trace too large"`. `Failed` and `Convert` become `ApiError::Internal`.
   - Then `llmobs_eval::resolve(&mapping, Some(&TraceData::new(&loaded.spans, span_id)), None)`. A `MappingError` becomes an `ok: false` answer with class `permanent` and the error's display, with no judge trace.
5. **Rendered variables for the answer:** `variables.iter()`; a `Value::String` as itself, anything else `serde_json::to_string`; cut to 2 000 `char`s, plus `…`.
6. **Prompt** (judges only): `PromptTemplate::compile(&config.prompt).and_then(|template| template.render(&variables))`. `Ok` → `prompt: Some`; an error leaves it out (the runner reports it).
7. **Run:**
   - `judge_trace_id` = 16 bytes from `crate::auth::secret::random_bytes`, then `TraceId::from_bytes`, as M1's connection test does;
   - `let ingest = state.ingest.context(project, now)`, `let prices = state.ingest.prices()`;
   - `state.eval_runner.run(RunInput { evaluator: &evaluator, variables: &variables, connection: connection.as_deref(), webhook_secret: <the signing secret as llmobs_eval::Secret>, target: EvalTarget { trace_id, span_id }, origin: EvalOrigin::Test, judge_trace_id, ingest: &ingest, prices: &prices })`;
   - wrap the future in `Box::pin` if clippy reports `large_futures`;
   - measure `latency_ms` around it.
   - **Check evaluators:** run the whole call off the async workers (D-I-23: checks are CPU work). Capture `let handle = tokio::runtime::Handle::current();` and call `tokio::task::spawn_blocking(move || handle.block_on(async move { runner.run(RunInput { … }).await }))`. `spawn_blocking` needs `'static` owned data, so move in `Arc::clone(&state.eval_runner)` as `runner`, plus clones of the `Evaluator`, the `Variables`, the `IngestContext`, the `Arc<PriceTable>` and the ids, and build `RunInput` from them inside. A `JoinError` is `ApiError::Internal`. Judges and webhooks run directly on the async task.
8. **Usage:**
   - find the span with `span_kind == SpanKind::Llm` in `run.judge_trace.spans`;
   - when there is one, `state.usage.add_judge(project.org_id, project.id, now, input_tokens.unwrap_or(0) as u64 …, cost_total.unwrap_or(0.0))` (convert with `u64::try_from(…).unwrap_or(0)`);
   - `usage` in the answer is `{input_tokens, output_tokens, cost_usd: cost_total}` from that span.
9. **Commit the judge trace:**
   - `crate::ingest::commit_in_task(state, CommitTarget { org_id: project.org_id, project_id: project.id, request_bytes: 0, now_ns: now }, run.judge_trace, state.ingest.reserve_exact(0)?)`;
   - an `IngestError` becomes `ApiError` through the existing `From` (busy → 503);
   - its spans are in `llmobs-eval`, so they add no billable span (I2.1).
10. **Answer** from `run.outcome`:
    - `Ok(outcome)` → `ok: true`, `value: Some(outcome.value)`, `reason: outcome.reason`;
    - `Err(error)` → `ok: false`, `error: Some({ class, message: error.to_string() })`.
    `judge_trace_id` is the hex of the id, and `trace_truncated` is `loaded.truncated`.
11. **Audit:** `tracing::info!(target: "audit", evaluator_id = %…, project_id = %project.id, user_id = %user, ok, latency_ms, "evaluator tested")`.

### Route

Add `POST /api/v1/projects/{project}/evaluators/test`, `Access::Project { perm: ManageEvaluation, session_only: false }`, `documented: true`. Put it in the route list **before** `/evaluators/{id}`; axum matches the static segment first either way, so the order is only for readability. The handler is `test_evaluator`: `principal::session_user`, then `test_run::run`, then `200`.

### Spec

- `docs/spec/06-evaluation.md` § Evaluators: add a paragraph "**Test runs** (D-I-21): `POST /evaluators/test` runs a saved or unsaved evaluator once on a stored trace with a mapping, or on literal variables. It writes the judge trace and the judge usage, writes no score, and answers the outcome, the resolved variables, the rendered prompt and the usage; 10 per minute per user."
- `docs/spec/09-api.md`: replace the row `POST /evaluators/{id}/test` with "`POST /evaluators/test`: `{evaluator_id?, evaluator?, trace_id?, span_id?, trace_start?, variable_mapping?, variables?}`; runs once, writes the judge trace and usage, no score (D-I-21)".

### `test_support/mock_webhook.rs`

An in-process webhook receiver, like M1's `mock_llm.rs` (read it and copy its structure). Register `pub(crate) mod mock_webhook;` in `test_support/mod.rs`.

```rust
/// Serves `POST /hook` on 127.0.0.1:0 and answers `status` with `body` every time.
/// Records each request's headers and raw body. Aborts its task on drop.
pub(crate) struct MockWebhook { /* addr, requests: Arc<Mutex<Vec<(HeaderMap, Bytes)>>>, task */ }

impl MockWebhook {
    pub(crate) async fn start(status: u16, body: serde_json::Value) -> Self;
    /// `http://127.0.0.1:<port>/hook`.
    pub(crate) fn url(&self) -> String;
    pub(crate) fn requests(&self) -> Vec<(axum::http::HeaderMap, bytes::Bytes)>;
}
```

### Tests (`routes/evaluators.rs`; `TestApp::with_config(|c| c.security.allow_private_network_egress = true)`, M1's `MockLlm`, and the clock set to the real time in every test right after `TestApp::with_config(…)` and before `app.seed()`, per § Conventions)

Store a trace with `app.commit_spans(&seeded.project, vec![span])`, where `span` is `crate::test_support::dataset_span(0)` with `trace_id`, `span_id` and `parent_span_id: None` set by the test, `input` `"What is the refund window?"` and `output` `"30 days."` (both with `input_format` / `output_format` `PayloadFormat::Text`), and `environment` `Some("production".into())`.

- `judge_test_on_a_trace`:
  - the mock answers `completion(r#"{"explanation": "Relevant.", "value": 0.9}"#)`;
  - the answer has `ok: true`, `value: {"data_type": "numeric", "value": 0.9}`, `reason: "Relevant."`, `variables.input` equal to the question, a `prompt` containing the question, `usage.input_tokens == 10`, `judge_trace_id` present;
  - the judge trace is readable through `GET /traces/{judge_trace_id}`, and its spans have environment `llmobs-eval`;
  - after `usage.flush`, `SELECT SUM(judge_input_tokens), SUM(spans) FROM usage_daily WHERE project_id = ?` gives `10` and `1` (sums, so a run across midnight UTC still passes) (the one target span stored with `commit_spans` counts; the judge trace's spans do not).
- `test_with_literal_variables`.
- `unsaved_evaluator_runs_without_saving`: no row in `evaluators` afterwards.
- `check_test_runs_without_a_provider`: an `exact_match` check, variables `{output: "a", expected: "a"}` → `ok: true`, `value: {"data_type": "boolean", "value": true}`, no `usage`.
- `mapping_error_has_no_judge_trace`: mapping `{input: "span.input"}` on a trace test without `span_id` → `ok: false`, class `permanent`, no `judge_trace_id`.
- `evaluation_trace_is_refused`: a trace whose root is in `llmobs-eval` → 400.
- `unknown_trace_is_404`.
- `provider_failure_is_reported`: the mock answers 500 → `ok: false`, class `retryable`.
- `long_variables_are_cut`: a 3 000-character output → the answered variable is 2 000 `char`s plus `…`.
- `test_runs_are_rate_limited`: the 11th within a minute → 429.
- `viewer_cannot_test` → 403.
- `no_payload_in_logs`: with `capture_logs` (plain `#[tokio::test]`), the logs contain "evaluator tested" and neither the question nor the answer.
- `webhook_test_signs_with_the_saved_secret`: `MockWebhook::start(200, json!({"value": 0.5}))`; save a numeric webhook evaluator with that URL and the secret `0123456789abcdef`; test it with `{evaluator_id, variables: {output: "x"}}` → `ok: true`, `value` 0.5; the mock saw one request whose `x-llmobs-signature` header starts with `t=` and contains `v1=`.
- `both_given_takes_the_saved_webhook_values`: the same saved webhook; test with `{evaluator_id, evaluator: {kind: "webhook", output_score: {name: "other", data_type: "numeric"}}, variables: {…}}` (no `config`, no `webhook_secret`) → the mock is called (the saved URL and secret were used), and the answer is `ok: true`.
- `both_given_with_another_kind_is_400`: `evaluator.kind` `check` with a saved webhook id → 400 `evaluator.kind`.
- `span_target_resolves_span_variables`: a trace with a root and a child; `span_id` the child, mapping `{output: "span.output"}`, an `exact_match` check → `variables.output` is the child's output.
- `trace_start_is_accepted`: the same trace test with `trace_start` set to the root's start time → `ok: true`.
- `body_rules`: more than 32 variables → 400 `variables`; a 31-hex trace id → 400 `trace_id`; `span_id` without `trace_id` → 400 `trace_id`; neither `evaluator_id` nor `evaluator` → 400 `evaluator`.
- (No 503 test for busy loads: it would need tokio's `test-util` feature. `loads_are_limited` in I2.3 covers the permits, and the `Busy` → 503 mapping is one `match` arm.)

### Done when

`just check` passes. No test calls a real provider (D167). `judge_test_on_a_trace` shows `judge_input_tokens == 10` and `spans == 1`, and `rg 'allow\(dead_code\)\] // .*I2\.7' crates/server` finds nothing (every I2.1 and I2.3 allow is gone).

---

## I2.8 — Evaluators list and score definitions page

**Goal:** the project sidebar has "Evaluators", the list page exists, and project settings have "Score definitions".

**Edit only:** `web/src/components/shell/nav.ts` (and its test if it lists entries), `web/src/components/settings/settings-nav.ts`, `web/src/lib/roles.ts` and `roles.test.ts`, `web/src/api/queries-evaluation.ts` (new), `web/src/routes/$org/$project/evaluators/index.tsx` and `.test.tsx` (new), `web/src/routes/$org/$project/settings/score-definitions.tsx` and `.test.tsx` (new), `web/src/components/evaluators/score-definition-dialog.tsx` (new), `web/src/mocks/writes.ts`, `web/src/mocks/writes.test.ts` (remove the three score-definition ids from `LATER_TRACKS`), `docs/spec/11-ui.md`.

**Commit:** `Add the evaluators list and score definitions pages`

### Shared

- `lib/roles.ts`:

```ts
/** Mirrors `ProjectPerm::ManageEvaluation`: project members and above (D-I-24). */
export function canManageEvaluation(role: OrgRole | undefined): boolean {
  return atLeast(role, "member");
}
```

  with a test: admin and member → true; viewer and `undefined` → false.
- `components/shell/nav.ts` `projectNav`: add `{ to: "/$org/$project/evaluators", label: "Evaluators", icon: Gauge }` after SQL (`Gauge` from `lucide-react`).
- `settings-nav.ts` `projectSettingsNav`: add `{ to: "/$org/$project/settings/score-definitions", label: "Score definitions", adminOnly: false }` after Members.
- `api/queries-evaluation.ts`: `evaluatorsInit(org, project)`, `evaluatorsKey`, `scoreDefinitionsInit(org, project, includeArchived: boolean)`, `scoreDefinitionsKey`. Copy the pattern of `queries-settings.ts`, with `projectRef(org, project)` as the `project` path param and `limit: 100`.

### Evaluators list (`routes/$org/$project/evaluators/index.tsx`)

- `createFileRoute("/$org/$project/evaluators/")`. The role is `useProject().data.role`.
- A header "Evaluators". There is **no** "New evaluator" button in I2.8: the routes `/evaluators/new` and `/evaluators/$evaluatorId` arrive in I2.9, a typed `Link` to an unknown route fails `tsc` (`pnpm typecheck` runs `tsr generate` first), and a button that does nothing is a placeholder. I2.9 adds the button with its route.
- States as M1's page: loading skeletons, the error thrown to the route, and an `EmptyState` with icon `Gauge`, title "No evaluators yet" and description "Create one from a template to score traces with an LLM judge, a check or a webhook."
- A table with columns:
  - Name (plain text in I2.8; I2.9 makes it a `Link` to `/$org/$project/evaluators/$evaluatorId`);
  - Kind ("LLM judge", "Check", "Webhook");
  - Score (`output_score.name` and its data type);
  - Details (judge: `config.model`; check: `config.check`; webhook: `config.url_host`);
  - Version;
  - Updated (`TimeText`).

### Score definitions (`routes/$org/$project/settings/score-definitions.tsx`)

- `SettingsPage` with title "Score definitions" and description "Named score shapes. Annotations and evaluators must fit them."
- An "Add definition" button (disabled with "Only members can manage score definitions" for viewers), and a "Show archived" `Switch` that switches `includeArchived`.
- A table: Name, Type, Range or categories (`0 – 1`, or the labels joined with `, `), Description, Status ("Archived" badge when `archived_at`), and actions "Edit", "Archive" or "Restore".
  - "Archive" is a `ConfirmDialog`: "Archive {name}?", "Scores keep their values; new scores are no longer checked against it.", "Archive".
  - "Restore" sends `PATCH {archived: false}`.
- `components/evaluators/score-definition-dialog.tsx`: controlled form, no form library:
  - Name;
  - Type (a `Select` of numeric, boolean, categorical, text; on create only, shown read-only on edit);
  - for numeric, Minimum and Maximum (optional number inputs);
  - for categorical, label and value rows with "Add category";
  - Description (textarea).
  It sends only set fields on create and only changed fields on edit. Field errors appear under their fields; `problemMessage(error, "Could not save the definition.")` and a 409's detail appear above the submit button. Client errors: "Enter a name.", "Add at least one category." (categorical with no rows).
- `mocks/writes.ts`: `post_score_definition` (201, `score-definition.json` with the body's name), `patch_score_definition` (200, same), `delete_score_definition` (`noContent()`). Remove those three ids from `LATER_TRACKS` in `writes.test.ts`.

### Spec (`docs/spec/11-ui.md`)

- § Navigation: replace `/:org/:project/evaluators · /online-rules · /online-rules/:id   (own plan, …)` with `/:org/:project/evaluators · /evaluators/new · /evaluators/:id   evaluators, editor and test panel (track I, D-I-27)` and `/:org/:project/online-rules · /online-rules/:id   (track I, M3)`. Replace `settings/{score-definitions,channels,data}   (… own plan …)` with `settings/score-definitions   score definitions (track I, D-I-26)` and keep `channels` and `data` on their own line as they are.

### Tests

- `evaluators/index.test.tsx`: the fixture list shows "Relevance", "LLM judge", "relevance", "gpt-5-mini"; the empty state (a `server.use` empty page); the list has no "New evaluator" button yet.
- `settings/score-definitions.test.tsx`:
  - the list renders the fixture;
  - creating a numeric definition sends `{name: "accuracy", data_type: "numeric", min_value: 0, max_value: 1}`;
  - a categorical definition with no categories shows the client error and sends nothing;
  - archiving sends DELETE to the fixture id;
  - "Show archived" requests `include_archived=true` (assert the request URL in the handler);
  - restoring sends `{archived: false}`;
  - a 409 shows its detail;
  - a viewer cannot add.
- `roles.test.ts`: `canManageEvaluation`.

### Done when

`just check-web` passes; then `just check`.

---

## I2.9 — Evaluator editor

**Goal:** `/evaluators/new` and `/evaluators/$evaluatorId` create, edit and delete evaluators of all three kinds, starting from a template for judges.

**Edit only:** `web/src/routes/$org/$project/evaluators/new.tsx`, `$evaluatorId.tsx` and their tests (new), `web/src/routes/$org/$project/evaluators/index.tsx` and `index.test.tsx` (the links), `web/src/components/evaluators/evaluator-form.tsx`, `output-fields.tsx` (new), `check-fields.tsx` and `check-fields.test.tsx` (new), `web/src/api/queries-evaluation.ts`, `web/src/mocks/writes.ts`, `web/src/mocks/writes.test.ts` (remove `post_evaluator`, `patch_evaluator` and `delete_evaluator` from `LATER_TRACKS`).

**Commit:** `Add the evaluator editor`

### `components/evaluators/evaluator-form.tsx`

`EvaluatorForm({ org, project, evaluator, onSaved }: { org: string; project: string; evaluator?: Schema<"Evaluator">; onSaved: (saved: Schema<"Evaluator">) => void })`. Controlled state; no form library (D-L-11). Sections:

1. **Start from a template** (new and kind `llm_judge` only): a `Select` over `GET …/evaluator-templates`, with the note "Replaces the name, prompt and output." under it. Picking one replaces Name (template `name`), Prompt, Output (`output`) and Score name (`output_score_name`), even if the user edited them, and stores `default_mapping` in form state for the test panel (I2.10).
2. **Name** (`TextField`; client error "Enter a name.").
3. **Kind**: a `RadioGroup` (LLM judge, Check, Webhook) on new; read-only text on edit.
4. **Judge fields:**
   - Connection: a `Select` over `GET /api/v1/orgs/{org}/llm-connections` (M1's `connectionsInit`). An empty list shows "No connections. An org admin adds them in Settings › LLM connections." with a link to `/$org/settings/llm-connections`.
   - Model: `TextField`, prefilled with the connection's `default_model` when the connection is picked and Model is empty.
   - Prompt: a monospace `Textarea`, 16 rows. Under it, "Variables: {names}", the distinct names matched by `/\{\{\s*([A-Za-z_][A-Za-z0-9_]*)/g` (only for display; the server is the authority).
   - Output: `OutputFields` (below).
   - Include reason: `Switch`, default on.
   - Advanced (`<details>`, forced open on an error inside, as M1's dialog): Max output tokens (placeholder 1024), Timeout ms (placeholder 60000), Temperature (empty means unset).
5. **Check fields:**
   - Check: a `Select` of `exact_match`, `contains`, `regex`, `json_valid`, `json_schema`, `levenshtein`.
   - Parameters: `CheckFields` (below), one small form per check (the user's choice, 2026-11-14: no hand-written JSON for the common checks). The body's `config` is `{check, ...params}` with only the fields below.
   - The output data type is fixed by the check (boolean, numeric for `levenshtein`) and shown read-only.
6. **Webhook fields:**
   - URL;
   - Timeout ms (placeholder 10000);
   - Headers: rows like M1's extra headers, with `type="password"` values;
   - Signing secret: `type="password"`, `autoComplete="new-password"`, 16–256 characters; client error "Enter a signing secret of 16 to 256 characters." on new.
   - On edit, the section shows `url_host` and the header names, plus a "Replace webhook settings" checkbox. Only when it is checked are URL, Timeout and Headers editable and sent, as one full `config`. The secret is sent only when typed.
   - The output data type is a `Select` of numeric, boolean, categorical.
7. **Score name** (`TextField`) under the output.

`components/evaluators/check-fields.tsx`: `CheckFields({ check, value, onChange, errors }: { check: CheckName; value: CheckParams; onChange: (next: CheckParams) => void; errors: Record<string, string> })`. `CheckName` is the union of the six names, and `CheckParams` is the object of the fields below (the `Check` enum in `crates/eval/src/config.rs`, fields and defaults exactly as there). Picking another check resets `value` to that check's defaults.

| Check | Controls (label → field) | Defaults shown | Sent |
|---|---|---|---|
| `exact_match` | `Switch` "Case sensitive" → `case_sensitive`; `Switch` "Trim whitespace" → `trim` | both on | both, always |
| `contains` | Needle rows, each a `TextField` "Text to find" with a remove button, plus "Add text" → `values` (`string[]`); `RadioGroup` "Match" with "Any" (`any`) and "All" (`all`) → `mode`; `Switch` "Case sensitive" → `case_sensitive` | one empty needle row, Any, off | `values` without empty rows, `mode`, `case_sensitive`. With no non-empty needle, `values` is omitted and a note under the rows says "No text: the output is compared with the expected value." Never sends `value` |
| `regex` | `TextField` "Pattern" (monospace) → `pattern`; help "Rust regex syntax, searched anywhere in the output." | empty | `pattern` |
| `json_valid` | none; the text "Passes when the whole output parses as JSON." | — | nothing |
| `json_schema` | a monospace `Textarea` "JSON Schema" (12 rows) → `schema`, parsed with `JSON.parse` on submit | `{}` | `schema` as the parsed value |
| `levenshtein` | `Switch` "Normalize (trim, ignore case, collapse spaces)" → `normalize` | on | `normalize` |

Client errors, shown under the control and blocking submit:
- `regex` with an empty pattern: "Enter a pattern.";
- `json_schema` text that does not parse, or parses to something other than an object: "The schema must be a JSON object.".

Server field errors named `config.<field>` (for example `config.pattern`, `config.schema`, `config.values`) go under the matching control. Any other `config.*` goes to the section's error line.

`check-fields.test.tsx` renders `CheckFields` alone and checks:
- the controls and defaults of each check;
- that switching checks resets the value;
- the `contains` payload rules: empty rows dropped, `values` omitted when there is nothing, `value` never sent;
- the two client errors.

`components/evaluators/output-fields.tsx`: `OutputFields({ value, onChange, errors })` for the judge's `output`:
- a type `RadioGroup` (Numeric, Boolean, Categorical);
- Numeric: Min and Max (defaults 0 and 1);
- Categorical: label rows with an optional value each.
It produces the `JudgeOutput` JSON (`{type: "numeric", min, max}`, `{type: "boolean"}`, `{type: "categorical", labels, label_values}`). The score's `data_type` follows it, and `categories` equals the labels.

**Body.**
- Create: `{name, kind, config, output_score}`, plus `webhook_secret` for webhooks.
- Edit: only the changed top-level fields (`name`, `config`, `output_score`, `webhook_secret`). A changed field inside `config` sends the whole `config`.
- The create and patch mutations use `{ gcTime: 0 }` and `reset()` on unmount, because webhook values are secrets (D-L-5).

**Errors.** Field errors from the server go under the matching control. The mapping:
- `name` → Name;
- `config` and `config.*` → the kind's section (`config.prompt` → Prompt, `config.connection_id` → Connection, `config.model` → Model, other `config.*` → the section's error line);
- `output_score*` → the output section;
- `webhook_secret` → Signing secret.
Any other field joins the form-level `FormError`, as M1's dialog does.

### Pages

- `routes/$org/$project/evaluators/new.tsx`: a page "New evaluator" with `EvaluatorForm`. `onSaved` navigates to `/$org/$project/evaluators/$evaluatorId` of the saved one.
- `routes/$org/$project/evaluators/$evaluatorId.tsx`: loads `GET …/evaluators/{id}` (404 shows the `NotFound` component). It shows the header (name, kind, version), `EvaluatorForm` with `evaluator`, and a "Delete" button (`ConfirmDialog` "Delete {name}?" / "Its test history is kept as evaluation traces." / "Delete"; after deleting, navigate to the list).
- For viewers, every control is disabled and the submit and delete buttons sit inside `DisabledReason` "Only members can edit evaluators". The test panel (I2.10) is also hidden for them.

### Links on the list

In `evaluators/index.tsx`, add the header's "New evaluator" button: `<Button asChild><Link to="/$org/$project/evaluators/new" params={{ org, project }}>New evaluator</Link></Button>` for members, and for viewers a disabled plain `Button` inside `DisabledReason` "Only members can create evaluators"; and the Name cell a `Link` to `/$org/$project/evaluators/$evaluatorId` with `params={{ org, project, evaluatorId: evaluator.id }}`. `index.test.tsx` gains: the name links to `/acme/support-bot/evaluators/01900000-0000-7000-8000-000000000500`, "New evaluator" links to `/acme/support-bot/evaluators/new` for a member, and a viewer sees it disabled with the description (check the button's `parentElement`, as M1's tests do).

### Mocks

`writes.ts`: `post_evaluator` (201, `evaluator.json` with the body's name), `patch_evaluator` (200, same), `delete_evaluator` (`noContent()`). Remove the three ids from `LATER_TRACKS`. Add `evaluatorKey(org, project, id)` to `queries-evaluation.ts` for invalidation.

### Tests

- `new.test.tsx`:
  - `creates a judge from a template`: pick "Relevance" in the template select (the fixture templates), pick the connection, submit → the POST body has `kind: "llm_judge"`, `config.connection_id`, `config.prompt` equal to the template prompt, `output_score.name: "relevance"`; then it navigates to the evaluator page;
  - `creates a contains check`: Check "contains", type "refund" in the first needle row, pick "All" → `config: {check: "contains", values: ["refund"], mode: "all", case_sensitive: false}`, `output_score.data_type: "boolean"`;
  - `creates a regex check`: pattern `^ok` → `config: {check: "regex", pattern: "^ok"}`;
  - `creates a webhook`: URL, one header, a secret → the body has the full `config` and `webhook_secret`; header value inputs are `type="password"`;
  - `client errors block submit`: an empty name, a `json_schema` check with invalid schema text, and a short secret → their messages, and no request;
  - `server field errors land under their controls`: 400 with `config.prompt` and `output_score.data_type` errors.
- `$evaluatorId.test.tsx`:
  - `edits the prompt`: the PATCH body is `{config: {…whole config with the new prompt}}`;
  - `renames only`: the body is `{name: "…"}`;
  - `webhook edit sends no settings unless replaced`;
  - `deletes after confirming`;
  - `a viewer sees a read-only form`;
  - `unknown id shows not found`.

### Done when

`just check-web` passes; then `just check`.

---

## I2.10 — Test panel

**Goal:** the evaluator page has a test panel that runs the current form, saved or not, on a trace or on variables.

**Edit only:** `web/src/components/evaluators/test-panel.tsx` and `.test.tsx` (new), `web/src/components/evaluators/evaluator-form.tsx` (place the panel and pass state), `web/src/mocks/writes.ts`, `web/src/mocks/writes.test.ts` (remove `post_evaluator_test`; `LATER_TRACKS` becomes empty again), `docs/spec/11-ui.md`, `docs/roadmap.md` (the I row).

**Commit:** `Add the evaluator test panel`

### `components/evaluators/test-panel.tsx`

`TestPanel({ org, project, evaluatorId, body, dirty, defaultMapping }: { org: string; project: string; evaluatorId?: string; body: Schema<"TestEvaluatorBody"> | null; dirty: boolean; defaultMapping: { variable: string; expression: string }[] })`.

`body` is the form's current state as a `TestEvaluatorBody`, or `null` when the form has client errors, which disables Run with "Fix the form first". The form builds it like its create body, except that for a saved webhook whose "Replace webhook settings" box is unchecked it leaves out `config` and `webhook_secret` (the server then uses the sealed values, § Test run contract). `dirty` is true when the form differs from the saved evaluator, and always true on new.

**Modes** (a `Tabs` of "Trace" and "Variables"):
- **Trace:**
  - Trace ID (32 hex; client error "Enter a 32-digit hex trace ID."), Span ID (optional, 16 hex);
  - a mapping table of variable and expression rows. They are prefilled from `defaultMapping`, else from the prompt's variable names with expressions `trace.input` for `input`, `trace.output` for `output` and empty for the others. "Add variable" and remove buttons.
- **Variables:** name and value rows. A value that parses as JSON is sent as that JSON, otherwise as a string.

**Run** sends `POST …/evaluators/test` with:
- `evaluator_id` when editing;
- `evaluator: body` when `dirty` (always on new);
- the mode's fields.
The mutation uses `{ gcTime: 0 }`.

**The result card:**
- `ok`: the value (a number with 3 decimals, "true"/"false", or the label), the reason, and usage ("{input_tokens} in · {output_tokens} out · ${cost_usd}" when present), latency, and "Trace truncated" when set.
- not ok: "Test failed", the message and one hint per class, defined in `test-panel.tsx`:
  - `pause`: "The key, quota or model access was refused. Check the connection in Settings › LLM connections.";
  - `retryable`: "The provider or webhook did not answer in time or is overloaded. Try again.";
  - `permanent`: "The request was rejected or the answer did not fit. Check the model, the prompt and the output type.".
- a "Prompt" `<details>` with the rendered prompt in monospace;
- a "Variables" `<details>` with each name and value;
- "View judge trace": a link to `/$org/$project/traces/$traceId` with `judge_trace_id`, when present.
- A 429 shows "Too many tests. Wait a minute."; a 404 shows "Trace not found."; a 400 with `detail` shows that detail.

`evaluator-form.tsx` renders `TestPanel` below the form for members. It passes the current `TestEvaluatorBody` (or `null`), `dirty`, the id on edit, and the template's `default_mapping` (empty on edit).

**Mocks:** `post_evaluator_test` answers `evaluator-test-result.json`. Remove it from `LATER_TRACKS`, which is now empty.

### Spec and roadmap

- `docs/spec/11-ui.md` § Evaluators & online rules: replace the first bullet with "Evaluators (track I, D-I-20, D-I-21): list; editor for LLM judges (template picker, connection and model, prompt with its variables, output type), checks (type and a small form of its parameters) and webhooks (URL, headers and signing secret, write-only); **test panel** on a trace with a variable mapping, or on literal variables, showing the value, reason, prompt, variables, usage and a link to the judge trace."
- `docs/roadmap.md`, the I row's status: "**In progress.** M1 merged (#63). M2 (evaluators, test runs, score definitions) implemented on branch `<your branch>`. M3–M6 plans to follow."

### Tests (`test-panel.test.tsx`, rendering the editor page `/acme/support-bot/evaluators/01900000-0000-7000-8000-000000000500` through `renderApp`)

- `runs a saved evaluator on a trace`:
  - type the trace id; the mapping is prefilled with `input → trace.input` and `output → trace.output`;
  - Run → the body is `{evaluator_id: "…0500", trace_id, variable_mapping: {input: "trace.input", output: "trace.output"}}` (no `evaluator`, since the form is unchanged);
  - the result shows "0.900", the reason, the usage line and a "View judge trace" link to `/acme/support-bot/traces/e1000000000000000000000000000001`.
- `runs the unsaved form`: edit the prompt, then Run → the body has `evaluator` with the new prompt and `evaluator_id`.
- `runs on literal variables`: Variables tab with `output = 30 days.` and `expected = {"a": 1}` → `variables: {output: "30 days.", expected: {a: 1}}`.
- `shows a failure and its hint`: answer `{ok: false, …, error: {class: "pause", message: "provider status 401"}}` → "Test failed", the message and the pause hint text.
- `tests a saved webhook without resending its settings`: open a webhook evaluator (a `server.use` GET answering a webhook `Evaluator` with `has_signing_secret: true`), change the score name, Run → `evaluator` has `kind: "webhook"` and `output_score`, and no `config` or `webhook_secret`.
- `bad trace id blocks run`.
- `429 shows the wait message`.
- `viewer sees no panel`.

### Done when

`just check` passes. Optional: `just web-dev` shows the evaluators pages on the mock data; say in the report whether you looked.

---

## After M2

Report: the ten commits, deviations, and anything unverified. The roadmap row is updated in I2.10.
