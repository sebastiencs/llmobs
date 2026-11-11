# E · Foundation (E1–E4)

See [README.md](README.md) for the decisions, boundaries and conventions. E1 is documentation only. E2 creates the crate with the evaluator configuration, and E3–E4 add the checks, which are pure functions and need no network.

## E1 — Decisions and spec

**Commit:** `docs: record the evaluation library decisions`

**Edit only:** `docs/mvp-decisions.md`, `docs/spec/02-ingestion.md`, `docs/spec/04-metadata-db.md`, `docs/spec/06-evaluation.md`, `docs/spec/12-operations.md`, `docs/spec/README.md`, `AGENTS.md`, `docs/plans/d-meta-auth/README.md`.

1. **Numbering.** Find the highest `D<n>` in `docs/mvp-decisions.md` on `main` and in the open plan PRs (`gh pr list`). If anything uses 152–164, renumber this plan's decisions to the next free block and update every `D15x`/`D16x` reference under `docs/plans/e-eval/` and `docs/research/eval/`.
2. **`docs/mvp-decisions.md`:** add D155–D167, one row each, shortened to one or two sentences in the style of the existing rows, each ending with the spec section it changes.
3. **Spec 06 § `llm_judge`:**
   - In the JSON example, drop `"temperature": 0` and add a comment line that `temperature` is optional and unset by default; add `"timeout_ms": 60000` (D159).
   - Replace the "Structured output per adapter" and "Fallback for providers without structured output" text with D157 and D158: the three modes, the answer schema (`explanation` first), the tolerant parser, and "no automatic downgrade".
   - Add one paragraph with D159's request rules (Chat Completions, `max_completion_tokens`, one user message, timeouts, 4 MiB cap).
   - In "Judge traces", add D161's rules: built as OTLP and normalized, span ids derived from the trace id, the `webhook` child span of kind `span` (there is no `http` span kind), and `judge.cost_usd` from the `llm` span.
   - "The environment name is reserved: …" gains: "The OTLP handler does the rewrite after normalization; evaluation spans are built in process and keep the name."
4. **Spec 06 § `check`:** a "Defaults" paragraph from D164 (text extraction, each check's defaults, the reason of `json_schema`, the Levenshtein formula and `normalize`).
5. **Spec 06 § `webhook`:** D165's details (timeout default and cap, no redirects, 64 KiB cap, a `200` with a bad body is permanent).
6. **Spec 06 § Variable mapping:** add the `span[kind="…"]` selector, the trailing `?`, the root rule, payload decoding and JSONPath match counts (D162). After the table, a "Rendering" paragraph from D163 (how values render in prompts, the transcript layout).
7. **Spec 06 § Execution:** the retries bullet gains "Failures arrive classified by `llmobs-eval` as retryable, pause or permanent (D160)". Add "pause" to the error breaker: a pause-class error pauses the rule at once with `paused_reason='errors'`.
8. **Spec 06:** a new last section, "## Library", saying in three sentences that `llmobs-eval` implements evaluator execution, mapping, templating and judge traces without a database, and the server track owns the queue, tables, encryption and handlers (D155, D156).
9. **Spec 04 `llm_connections`:** add `structured_output TEXT NOT NULL DEFAULT 'native', -- 'native' | 'tool' | 'prompt' (06)` after `default_model` (D157).
10. **Spec 02 § Validation** (or the section that lists reserved values; find it): one sentence that `environment = "llmobs-eval"` from OTLP is rewritten by the OTLP handler after normalization (D161).
11. **Spec 12:** in the SSRF bullet, list D166's denied ranges, "redirects are never followed", and "the check applies to every resolved address and to literal IPs".
12. **`docs/spec/README.md`** and **`AGENTS.md` § Layout:** add `crates/eval  llmobs-eval  evaluator execution: checks, mapping, templates, judges, webhooks, judge traces (spec 06)`. In the crate dependency graph: "`eval` depends on `core` and `otel`"; "`server` depends on `core`, `otel`, `storage` and `eval`". Do not add the dependency to `crates/server/Cargo.toml`: the server track does that with its first call.
13. **`docs/plans/d-meta-auth/README.md`:** under R-E1, add "Moved to the Wave 2 eval server track by D156." Change nothing else in that plan.

**Checks:** `just check`.

**Report:** the decision numbers used and every spec heading you changed.

## E2 — Crate and evaluator configuration

**Commit:** `eval: add the crate and evaluator configuration`

**Edit only:** the root `Cargo.toml`, `Cargo.lock`, `crates/eval/`.

**Crate.** Package `llmobs-eval`, description "Evaluator execution: checks, variable mapping, templates, LLM judges, webhooks", `[lints] workspace = true`, the workspace `version`/`edition`/`rust-version`/`publish`. Add `llmobs-eval = { path = "crates/eval" }` to `[workspace.dependencies]` next to the other workspace crates. The workspace already globs `crates/*`; check that it does. Dependencies in this sub-phase: `llmobs-core`, `serde`, `serde_json`, `thiserror`, `uuid` (all workspace). Dev: `insta`.

**`secret.rs`.** `pub struct Secret(String)`:
- `Secret::new(String)`, `expose(&self) -> &str`;
- `Debug` prints `Secret([redacted])`; no `Display`, no `Serialize`;
- `Deserialize` is not derived either: callers build it from decrypted values.

Test: `format!("{:?}", Secret::new("sk-live-123".into()))` does not contain `sk-live`.

**`config.rs`.** The JSON shapes of spec 06 § Evaluators, after E1. Every struct uses `#[serde(deny_unknown_fields)]` except where noted.

- `pub enum EvaluatorConfig { LlmJudge(JudgeConfig), Check(Check), Webhook(WebhookConfig) }`. Not a serde enum: the kind lives in the `evaluators.kind` column. `EvaluatorConfig::from_parts(kind: &str, config: &serde_json::Value) -> Result<Self, ConfigError>` parses by kind (`llm_judge`, `check`, `webhook`); `kind(&self) -> &'static str`.
- `pub struct JudgeConfig`:
  - `connection_id: Uuid`;
  - `model: String` (non-empty);
  - `prompt: String` (non-empty, at most 64 KiB; template syntax is checked in E8);
  - `output: JudgeOutput`;
  - `include_reason: bool` (default true);
  - `temperature: Option<f64>` (0 ≤ t ≤ 2 when set);
  - `max_output_tokens: u32` (default 1024, 1–65 536);
  - `timeout_ms: u32` (default 60 000, 1 000–300 000).
- `pub enum JudgeOutput` tagged by `type`, `snake_case`:
  - `Numeric { min: f64, max: f64 }` (finite, `min < max`);
  - `Boolean`;
  - `Categorical { labels: Vec<String>, label_values: BTreeMap<String, f64> }` (2–20 labels, each 1–64 chars, unique after trim and case-folding; `label_values` defaults to empty; every key must be a label; values finite).
- `pub enum Check`, the check evaluator's configuration, tagged by `check` (`snake_case`), each variant with its parameters (defaults per D164). `EvaluatorConfig::Check` holds it directly:
  - `ExactMatch { case_sensitive: bool = true, trim: bool = true }`;
  - `Contains { value: Option<String>, values: Vec<String>, mode: ContainsMode = Any, case_sensitive: bool = false }`. Exactly one of `value` / non-empty `values`; `value` is folded into `values` by validation, so later code reads only `values`. When neither is given, the check compares with `expected` (spec 06: "or the param value"). Write it as: `values` empty and `value` absent means "compare with `expected`".
  - `Regex { pattern: String }`;
  - `JsonValid {}`;
  - `JsonSchema { schema: serde_json::Value }`;
  - `Levenshtein { normalize: bool = true }`.
- `pub struct WebhookConfig`: `url: String` (absolute `http`/`https`, no userinfo; parsed with `reqwest::Url` from E10 — in E2 check the scheme prefix and non-empty host with a small hand-written check, and E10 replaces it with `Url::parse`), `timeout_ms: u32` (default 10 000, 1 000–60 000), `headers: BTreeMap<String, String>` (default empty; at most 20; names are valid HTTP token characters and not one of `content-type`, `content-length`, `host`, `x-llmobs-signature`, case-insensitively), `secret_enc: Option<String>` (opaque; stored by the server, never read here; document that on the field). This struct does **not** deny unknown fields, so the server can add bookkeeping keys without breaking parsing; document why.
- `pub struct OutputScore { name: String, data_type: ScoreDataType, categories: Option<Vec<String>> }` (the `evaluators.output_score` column). `ScoreDataType` comes from `llmobs_core::enums`.
- `pub struct Evaluator { pub id: Uuid, pub name: String, pub config: EvaluatorConfig, pub output: OutputScore }`.
- `impl Evaluator { pub fn validate(&self) -> Result<(), ConfigError> }` checks every rule above and that the output agrees with the config:
  - judge: numeric → `numeric`, boolean → `boolean`, categorical → `categorical` with `categories` equal to `labels`;
  - check: `levenshtein` → `numeric`, every other check → `boolean`;
  - webhook: `numeric`, `boolean` or `categorical` (not `text`);
  - `name` 1–128 chars.
  
  `from_parts` calls the per-struct validation, so an `EvaluatorConfig` that exists is valid; `Evaluator::validate` adds the cross-checks.

**`error.rs`.** `pub enum ConfigError` with one variant per rule family (`UnknownKind { kind }`, `Json { kind, message }` from serde's error *path and category only* — serde_json's `Display` can quote input, so build the message from `err.classify()`, line and column, never `err.to_string()`; `Invalid { field: &'static str, reason: &'static str }`, `OutputMismatch { expected: ScoreDataType, found: ScoreDataType }`). Add `#[cfg(test)] pub(crate) fn assert_no_payload(message: &str, payload: &str)`.

**Tests** (unit, in `config.rs`):
- each kind parses from the spec 06 JSON example (after E1);
- every default;
- each invalid rule gives its `ConfigError` (one test per rule; table-driven is fine);
- unknown fields are rejected for judge and check, accepted for webhook;
- `ConfigError` for a JSON body containing `"prompt": "SECRET-PAYLOAD"` with a type error does not contain `SECRET-PAYLOAD`;
- output mismatch cases.

**Checks:** `just check`.

## E3 — Text extraction and text checks

**Commit:** `eval: add text extraction and the text checks`

**Edit only:** `crates/eval/`, the root `Cargo.toml` and `Cargo.lock` (add `regex` to the crate).

**`text.rs`.**
- `pub(crate) fn extract_text(value: &serde_json::Value) -> String` per D164: a string as is; a messages array (an array whose elements all parse as `llmobs_core::message::Message`) gives the concatenated text parts of the last `assistant` message, joined with `\n`, or `""` if there is none; any other value gives compact JSON (`serde_json::to_string`).
- `pub(crate) fn is_messages(value) -> Option<Vec<Message>>` used by `extract_text` and later by E8's rendering. Use `llmobs_core::message::parse_messages` on the value's JSON text only if there is no `Deserialize`-from-`Value` path; prefer `serde_json::from_value`.

**`check.rs`.**
- `pub struct CheckInput<'a> { pub output: Option<&'a Value>, pub expected: Option<&'a Value> }`.
- `pub(crate) struct CheckResult { pub value: ScoreValue, pub reason: Option<String> }`.
- `exact_match`, `contains` and `regex` as `pub(crate)` functions taking their parameters and a `CheckInput`. The dispatcher `pub fn run_check(check: &Check, input: &CheckInput) -> Result<CheckResult, CheckError>` arrives in E4 with all six arms, so no arm is ever a stub.
- `exact_match`: compare `extract_text(output)` with `extract_text(expected)`; trim both when `trim`; case-fold both (`to_lowercase`) unless `case_sensitive`. Boolean. Reason: `None`.
- `contains`: haystack is the output text; needles are `values`, or the expected text when `values` is empty. Case-fold both sides unless `case_sensitive`. `Any` → at least one; `All` → every one. An empty needle string is invalid configuration (E2 rejects it). Reason: `None`.
- `regex`: compile with `RegexBuilder::size_limit(1 << 20)`; an invalid pattern is `CheckError::InvalidPattern` (E2's validation also compiles it, so a stored evaluator never hits this). `is_match` on the output text. Reason: `None`.
- Missing `output` (or missing `expected` when the check needs it) → `CheckError::MissingVariable { name: "output" | "expected" }`.

Move the regex compile check into E2's `validate` now (`ConfigError::Invalid { field: "pattern", reason: "invalid regular expression" }`), since `regex` is now a dependency.

**Tests:** for `extract_text`: string, number, object, messages with two assistant turns (takes the last), messages with no assistant (`""`), assistant with a non-text part only (`""`), an array that is not messages (compact JSON). For each check: the defaults, each parameter flipped, `contains` with `value`, with `values` in `any` and `all` modes, and with `expected`; non-ASCII case folding (`"ÉCOLE"` vs `"école"`); missing variables; a regex over 1 MiB compiled size is rejected.

**Checks:** `just check`.

## E4 — JSON and similarity checks

**Commit:** `eval: add the JSON and similarity checks`

**Edit only:** `crates/eval/`, the root `Cargo.toml` and `Cargo.lock` (`jsonschema` and `strsim` for the crate; add `strsim = "0.11.1"` to the workspace).

- `json_valid`: `serde_json::from_str::<IgnoredAny>(text)` on the extracted output text. When `output` is already a JSON object or array (not a string), it is valid. Boolean, reason `None`; on failure the reason is `"invalid JSON at line L column C"` (position only).
- `json_schema`: the instance is the output parsed as JSON when it is a string, else the value itself. A string that isn't JSON → `false` with reason `"invalid JSON at line L column C"`. Validate with `jsonschema` (Draft 2020-12, compiled once per call; no remote references: build the validator so it never fetches over the network — check the crate's options for disabling external resolution, and test that a `$ref` to `https://…` fails at validation time in E2's `validate`). On failure the reason is `"<instance path>: <keyword>"` of the first error, e.g. `"/answer: type"`. Never put the instance value or the error's `Display` (which can quote the value) into the reason. E2's `validate` now compiles the schema too.
- `levenshtein`: `strsim::normalized_levenshtein` on the two texts (Unicode scalar values), after normalizing per D164 when `normalize`. Two empty strings → 1.0. Numeric in [0, 1], reason `None`.
- `pub fn run_check(...)` with all six arms.

**Tests:** valid/invalid JSON (string and non-string outputs); schema pass, type failure, required failure, the reason format, and a test that a value like `"SECRET-PAYLOAD"` in the instance never appears in the reason; Levenshtein known pairs (`kitten`/`sitting` ≈ 0.571), normalization on and off, empty strings, multi-byte characters; `run_check` dispatch for each check from a JSON config.

**Checks:** `just check`.
