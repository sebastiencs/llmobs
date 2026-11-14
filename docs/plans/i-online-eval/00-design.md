# I · Online evaluation — design

Track I turns the evaluation library (`llmobs-eval`, track E) into a product feature. It covers:
- LLM connections, with their API keys encrypted at rest;
- evaluators, and the test panel that runs one once;
- online rules, which pick traces and spans by filter and sampling, under a cost cap;
- a durable job queue in `meta.db`, run by in-process workers;
- judge traces written through G's in-process commit, and scores written through G's score core;
- "run evaluator on this trace", backfill, score definitions and the human annotation form;
- the UI pages for all of these.

Experiments and datasets are track J. J reuses this track's queue, executor and score path (§ Interfaces for later tracks).

This document holds the decisions, the architecture, the interfaces and the milestones. Each milestone gets its own plan of small sub-phases. GLM implements them one at a time, with no other context, so the plans leave nothing to guess:
- [`01-m1-secrets-connections.md`](01-m1-secrets-connections.md): written.
- `02-m2-evaluators.md` … `06-m6-backfill-closeout.md`: not written yet (§ Milestones).

**Read first:** `AGENTS.md`; `docs/mvp-decisions.md` (D135, D136, D138–D140, D150, D155–D167, Track G, Track L, Track I); specs 04, 06, 08, 09, 11 and 12; E's plan [`../e-eval/README.md`](../e-eval/README.md); G's design [`../g-spine/00-design.md`](../g-spine/00-design.md); the research note [`../../research/eval/online-eval-competitors.md`](../../research/eval/online-eval-competitors.md).

## Decisions

The user chose each option marked "(user)" on 2026-11-14. The other decisions follow from those or from the code, and are recorded in `docs/mvp-decisions.md` § Track I. Where a decision changes a spec or an earlier decision, it says so.

### Secrets and connections

| # | Decision |
|---|---|
| D-I-1 | **Encryption key (user).** The key is `LLMOBS_ENCRYPTION_KEY` (config `encryption_key`, a `Secret<String>`): 32 bytes in standard base64.<br>When the variable is unset, the server reads `<data_dir>/secret.key`.<br>When neither exists and `meta.db` has no check value yet, the server generates the file (mode 0600, base64 then a newline) and logs a warning to back it up. When a check value exists but no key is available (a restore without the key), startup fails and nothing is generated.<br>A check value in `instance_settings` refuses a different key. A key file readable by group or others logs a warning.<br>Competitors either require an env key (Langfuse, Laminar), generate one once and persist it (LangWatch), or fall back to a weak default (Opik, Phoenix). |
| D-I-2 | **Ciphertext format.** AES-256-GCM (`aes-gcm` 0.11). A sealed value is `0x01` (format version) ‖ 4-byte key id ‖ 12-byte random nonce ‖ ciphertext and 16-byte tag.<br>- The key id is the first 4 bytes of HMAC-SHA256(key, `llmobs.key-id.v1`).<br>- The associated data is the ASCII `table:column:row_id` (D156).<br>- The check value is the hex HMAC-SHA256(key, `llmobs.encryption-check.v1`), stored as `instance_settings('encryption.check')`.<br>This replaces spec 04's "`HMAC(key, "llmobs")`" and its unversioned layout. |
| D-I-3 | **Key rotation.** `llmobs admin rotate-encryption-key [--new-from-stdin]` takes the new key from one stdin line, or generates one when the current key comes from `secret.key`; a key on the command line would show in `ps`. When the current key comes from the environment, generating is refused — the new key would exist only in memory after the commit — so the new key must be given with `--new-from-stdin`.<br>It needs the server stopped: it takes `<data_dir>/LOCK` itself with `File::try_lock`, and fails while the lock is held. This is the one admin command that does not run beside a live server, unlike D136 and spec 09.<br>It verifies the current key, then re-encrypts every column listed in `crypto::ENCRYPTED_COLUMNS` and replaces the check value, in one transaction.<br>- When the key came from `secret.key`, it writes `secret.key.new` (fsynced) before the transaction and renames it over `secret.key` after. At startup, a leftover `secret.key.new` whose key matches the check value is renamed into place, which finishes a rotation interrupted by a crash.<br>- When the key came from the environment, the new key was given on stdin, and the CLI prints a reminder to set `LLMOBS_ENCRYPTION_KEY` to it before the next start. |
| D-I-4 | **Connections are org-wide (user).** `llm_connections` loses spec 04's `project_id`. Org admins manage connections at `/api/v1/orgs/{org}/llm-connections`. Every org member can list them, without keys, to pick one in an evaluator.<br>Each connection has `max_concurrency` (default 4, 1–64) and `requests_per_minute` (default 300, 1–10 000), shared by every rule in every project that uses it.<br>The API key and the extra header values are write-only. Responses show `api_key_hint`: its last 4 characters when it has at least 16, else empty. `base_url` is shown to every member, so its query may hold only `api-version` (Azure); other query keys are refused, since providers put keys there. An org holds at most 100 connections.<br>The form explains `security.allow_private_network_egress` in help text; the UI cannot read the setting (spec 12 is changed to say so). |
| D-I-5 | **Connection test.** `POST …/llm-connections/{id}/test` with `{model?}` (else the default model; neither is a 400) runs one boolean judge call ("Reply true.") through `EvalRunner`, with a 30 s timeout, and discards the judge trace. It answers `200 {ok, model, latency_ms, error?: {class, message}}`.<br>A success sets `status = 'ok'` and, from M3 on, resumes the rules paused for that connection. A pause-class error sets `status = 'failing'` only when the test used the connection's default model: a 404 for a model typed in the test is pause-class but says nothing about the key. Other errors leave the status alone. Connection tests count no usage (they belong to no project).<br>Connection tests and evaluator test runs do not take the connection's concurrency or rate permits. They are limited per user instead (D-I-21). |

### Queue and execution

| # | Decision |
|---|---|
| D-I-6 | **Concurrency (user).** One dispatcher and `eval.workers` worker tasks per process (default 16, 1–256).<br>A job is claimed only when a worker slot, its connection's concurrency slot and a token of its connection's bucket are all free. The bucket holds `max(1, rpm / 6)` tokens (10 s of burst) and refills continuously at `rpm / 60` per second.<br>A webhook evaluator runs at most 4 jobs at once.<br>Claims go by strict priority (manual, then rule, then backfill), and rotate across projects within a priority, one job per project per round. Backfill can wait behind constant live load; this is accepted. |
| D-I-7 | **Trigger on completion (user).** Trace rules and span rules both fire when the trace is complete: its root span has ended and 5 s passed without a new span, or `trace_completion_timeout_s` (project setting, default 60, 10–3600) passed without one.<br>Spec 06's per-batch span-rule evaluation (also spec 02 § Pipeline) is dropped, because it needed an in-memory filter engine in the ingest path. Traces that complete while a rule is disabled or paused, or before it exists, are not evaluated by it; backfill is the catch-up. |
| D-I-8 | **Pending traces live in memory.** `Ingest::commit` gains a `pending: &PendingTraces` argument. It records `(project, trace_id, first_seen, last_seen, root_ended)` for projects in the tracked set, and touches no database on the ingest path.<br>The tracked set holds the projects that have at least one enabled, unpaused rule. It is rebuilt from `meta.db` at startup and after every rule create, update, delete, pause and resume.<br>Spans whose environment is `llmobs-eval`, or that carry `llmobs.experiment_id`, are never recorded. An entry keeps `first_seen`, `last_seen`, `min_start` (the earliest span `start_time`), `root_ended` and a failed-read count.<br>The map holds at most 200 000 traces. Past that, new traces are dropped: a counter grows, and a `warn` is logged at most once a minute with the count.<br>A flusher writes only the changed entries to `pending_traces` every 5 s and at shutdown, in transactions of at most 1 000 rows, deletes first. It snapshots the changes under the map's lock. Startup loads the table. A crash therefore loses at most 5 s of tracking. |
| D-I-9 | **Matching through storage, with strict exclusions.** Every 2 s the sweeper takes the completed traces of each tracked project, in batches of 500, at most 2 000 per project per tick. For each enabled, unpaused rule it runs one read per batch:<br>- `trace_refresh` for trace rules;<br>- the new `span_refresh` for span rules, which returns at most 200 matching spans per trace, earliest first.<br>Both take the rule's filter and the ids. Batches are grouped so that the `min_start` (the earliest span `start_time` seen, kept by `PendingTraces`) of every trace in a batch lies within 12 hours, and `QueryOptions.around` is the middle of that range, so the `± 1 day` file window covers the whole batch.<br>A read that fails (`Timeout`, `Busy`, or another error) leaves the batch's traces in the map, to retry at the next tick. After 5 failed ticks a trace is dropped, with a `warn` and a counter.<br>The sweeper reads rules and project settings from in-memory caches in `EvalQueue`. Rule writes refresh the rules cache; the settings cache is refreshed every 60 s. Ticks never overlap: the loop waits for one to finish before the next.<br>Both get a new flag, `strict_exclusions: true`. It excludes evaluation traces and experiment traces even when the filter mentions `environment`, which A2's normal exclusions allow (spec 05). For span rules the test is on the span's trace (`traces.environment` and the trace's `llmobs.experiment_id`), not on the span.<br>A trace is removed from the map only if its `last_seen` did not change during the reads. The ceiling is about 1 000 completed traces per second per project; past it, the map fills and drops (D-I-8). |
| D-I-10 | **Sampling by SHA-256.** A target is sampled when `sample_rate >= 1.0`, or when the first 8 bytes (big-endian `u64`) of SHA-256(`llmobs.sample.v1` ‖ rule id (16 bytes) ‖ trace id (16) ‖ span id (8, span rules only)) are below `(sample_rate · 2^64) as u64`. `sample_rate` is in `(0, 1]`.<br>This replaces spec 06's xxh3 so that no new hash crate is added. Re-processing a trace never samples it differently.<br>A span rule samples first, then keeps the 50 earliest sampled spans per trace. |
| D-I-11 | **Job identity.** `eval_jobs.dedup_key` is UNIQUE: `rule:<rule_id>:<trace_hex>:<span_hex or ->` for rule and backfill jobs, `manual:<job_id>` for manual runs.<br>A rule enqueue is `INSERT … ON CONFLICT(dedup_key) DO NOTHING`, so a trace that completes twice never runs a rule twice on a target while the row is kept (7 days).<br>A backfill enqueue is `ON CONFLICT DO UPDATE … WHERE status IN ('failed','skipped')`, which re-queues a failed or skipped target and leaves done, queued or running ones alone. The update resets `status = 'queued'`, `kind = 'backfill'`, `priority = 2`, `attempts`, `claims` and `load_retries` to 0, `error_class`, `error`, `judge_trace_id`, `score_id`, the usage columns and `finished_at` to NULL, and `not_before` and `updated_at` to now.<br>Live enqueues of a rule are capped at 10 000 per UTC clock hour (a fixed window). Extra matches are dropped and counted on the rule (`dropped_last_hour` in its response). Backfill has its own cap, `max_targets`, and does not count here. This caps runaway loops through instrumented webhooks or proxies, which carry no `llmobs-eval` marker. |
| D-I-12 | **Job states (supersedes spec 06's lease).**<br>- `queued → running` (claim: `claims` += 1). `attempts` is not touched at claim time: the worker increments it in its own short write right before `EvalRunner::run`, after the trace loaded, the guards passed, the variables resolved and the budget was reserved. So `attempts` counts provider or webhook calls exactly.<br>- `running → done \| failed \| queued` (retry, or pause, below).<br>- `queued → skipped` (budget, expired after 24 h).<br>There is no lease. Startup sets `running` jobs back to `queued`, except jobs with `claims > 3`, which become `failed` (`interrupted too often`), so a job that crashes the process cannot loop.<br>A janitor every minute re-queues `running` jobs whose `updated_at` is older than 10 minutes (the longest judge timeout is 300 s).<br>Each job runs in its own task; a panic is caught as a `JoinError`, and the job fails with `internal error`.<br>`finish` is fenced: `UPDATE … WHERE id = ? AND status = 'running' AND claims = <claims at this claim>`. When it updates no row (the job was re-claimed after the janitor or a restart, or its rule or evaluator was deleted), the result is dropped silently, and the trace and score already written stay. The janitor is a safety net only: the longest path (30 s load, a 300 s call, three 5 s write retries) stays under 10 minutes. |
| D-I-13 | **Retries (user).** A retryable error (D160) goes back to `queued` with `not_before = now + min(5 s · 2^(attempts−1), 10 min) ± 20 %`, up to 6 attempts; then the job is `failed`. The first retry waits 5 s, where spec 06's `2^attempts · 5 s` waited 10 s.<br>A permanent error fails the job at once.<br>A target trace that is not found, or a storage read that is busy or timed out, is retried 3 times, 30 s apart, counted in `load_retries`, then the job fails with `trace not found` or `storage unavailable`.<br>When the provider call succeeded but the judge trace or the score could not be written (storage backpressure, disk full), the worker retries the writes 3 times, 5 s apart, keeping the outcome. Then the job takes the retryable path, which calls the provider again. |
| D-I-14 | **Pause (user).**<br>- A pause-class error (D160) on a rule job marks the connection `failing` with the error text, pauses every enabled rule in the org whose evaluator uses that connection (`paused_reason = 'connection'`), and puts the job back to `queued` (`not_before = now`; the pause path does not change `attempts`, and the next run increments it as usual).<br>- For a webhook evaluator, it pauses every rule of that evaluator (`'errors'`).<br>- A manual job with a pause-class error fails, and still marks the connection `failing`.<br>- A breaker pauses a rule (`'errors'`) when, among its last 50 jobs that reached the evaluator (done, or failed with `EvalError::Judge`, `Webhook` or `Egress`), at least 20 exist and half or more failed. Failures about one target's data (trace not found, the recursion guard, a missing variable, `score definition mismatch`, `interrupted too often`) do not count. The window is in memory: it is reset by a resume and by a connection key change, and lost on restart, deliberately.<br>Paused rules keep their queued jobs. A queued job older than 24 h becomes `skipped` (`expired`).<br>"Resume" on a rule clears its pause. A successful connection test, or a PATCH of the connection's key, sets the connection `ok` and resumes its `'connection'` pauses. A successful job does not clear `failing`. Resuming a `'connection'` pause by hand while the connection is still `failing` is allowed; the next pause-class error pauses the rule again. A manual job or a test that hits a pause-class error marks the connection `failing` without pausing rules.<br>The UI shows banners. A `'connection'` pause names the connection and says other projects may be affected. There is no email. |
| D-I-15 | **Budget (user).** `budget_usd` with `budget_period` `day` or `month` (UTC) applies to `llm_judge` rules only; on another kind it is a 400.<br>Saving a budgeted rule needs a judge model with a price (else 400). So does a PATCH of an evaluator that a budgeted rule uses.<br>The worker, after resolving the variables, renders the prompt (`PromptTemplate::compile(…).render(&vars)`) and estimates `ceil(chars / 4)` input tokens plus `max_output_tokens` output tokens at the model's price (`PriceTable::price_for`).<br>Spend is kept per UTC day in `rule_spend`. A month's spend is the sum of its days. The in-memory view (spend + reservations per rule) is loaded at startup.<br>If `spend + reserved + estimate > budget`: the rule pauses (`'budget'`), this job and the rule's other queued jobs become `skipped` (`budget`), and no call is made. Otherwise the estimate is reserved. After the call the reservation becomes the actual cost: the `llm` span's `cost_total`, or the estimate when the provider answered with usage it could not price. A call that failed without any usage releases the reservation (cost 0), and so does every exit before a call. Each retry reserves again. Reservations live in memory only; a crash forgets them, and the spend recorded in `rule_spend` stays. Budget-skipped jobs are not re-run when the period turns, but a backfill can re-queue them (D-I-11). Backfill jobs spend the rule's budget, and can pause the live rule.<br>A ticker every 30 s clears `'budget'` pauses when the period changes. A PATCH of `budget_usd` or `budget_period` also clears a `'budget'` pause.<br>If the model lost its price at run time, the job fails permanently (`model has no price`) and the rule pauses (`'errors'`).<br>There is no org-level judge-spend limit (D-P-13): judges run on the customer's own connection, and per-rule budgets cover them. |
| D-I-16 | **Judge trace per attempt.** Each attempt gets its own judge trace id: the first 16 bytes of SHA-256(`llmobs.judge-trace.v1` ‖ job id (16 bytes) ‖ `attempts` after the worker's increment, as `u32` BE).<br>The job's `judge_trace_id` and the score link the last attempt.<br>Spec 06 said a retry reuses the job's id. Trace detail keeps the first row of a span id, so the retry would have been hidden.<br>Failures before the evaluator runs (the trace not loading, the recursion guard, a mapping error) write no judge trace. Spec 06's "every execution is traced" holds from `EvalRunner::run` on. |
| D-I-17 | **Recursion isolation.** Three guards:<br>1. `commit` never tracks evaluation or experiment spans (D-I-8);<br>2. every matching read (live, preview, backfill) uses strict exclusions (D-I-9);<br>3. the executor fails a job whose loaded target trace has a span in `llmobs-eval` (permanent, `target is an evaluation trace`).<br>Recursion through an instrumented webhook or proxy is bounded by D-I-11's per-rule hourly cap. |
| D-I-18 | **Evaluator scores.** `crate::scores::write` gains `Writer::Evaluator { kind, evaluator_id, rule_id: Option<Uuid>, job_id }`:<br>- source `llm_judge`, `check` or `webhook` by kind;<br>- the derived id's author is the rule id, or the evaluator id for a manual run;<br>- `evaluator_id` and `rule_id` are set on the row;<br>- `session_id` and `user_id` are copied from the target trace's root span;<br>- metadata is the outcome's `judge.*` keys plus `<prefix>.eval.job_id` and `<prefix>.eval.evaluator_version`, whose names are built in the server with `llmobs_name!` (not added to core).<br>Tokens and cost for `eval_jobs`, `rule_spend` and `Usage::add_judge` are read from the judge trace's `llm` span (`input_tokens`, `output_tokens`, `cost_total`), for a failed parse as well as a success. |
| D-I-19 | **Evaluator storage.**<br>- `evaluators` gains `connection_id`, derived from a judge's `config.connection_id` on every write (FK, `ON DELETE NO ACTION`, so an org or project cascade still deletes).<br>- It also gains `secret_enc`: for a webhook, the sealed JSON `{signing_secret, url, headers}`. `config` then keeps only `{url_host, timeout_ms, header_names}`.<br>- `WebhookConfig.secret_enc` is removed from `llmobs-eval`.<br>- The webhook secret is required on create; on PATCH, absent keeps it and an empty string is a 400. The URL and header values are write-only like the secret. Responses show `url_host`, `header_names` and `has_signing_secret`.<br>- `version` increases on every change to `config`, `output_score` or the sealed webhook values.<br>- `kind` is immutable after create. A PATCH that would break a rule using the evaluator is a 409 that lists the rule ids: every D-I-30 check is re-run for each rule, and a budgeted rule still needs a judge with a priced model.<br>- Queued jobs run the evaluator and the rule's mapping as they are at execution time; `eval_jobs.evaluator_version` records which version ran.<br>- For a run, the server assembles a `WebhookConfig` from the sealed JSON (`url`, `headers`) and the stored `timeout_ms`, then calls `Evaluator::validate`. The signing secret is chosen by the user, 16 to 256 characters.<br>- Deleting an evaluator that rules use is a 409. Deleting a connection that evaluators use is a 409. Both are application checks. Creating or editing a webhook evaluator is an audit event with its `url_host`. |
| D-I-20 | **Templates in the UI.** `GET /evaluator-templates` lists E's built-in templates with their default mapping. "Create from template" pre-fills the editor. The server has no create-from-template call. |
| D-I-21 | **Test runs.** `POST /evaluators/test` runs once, synchronously, with `{evaluator_id}` (saved), `{evaluator}` (unsaved, its `webhook_secret` required for a webhook), or both. With both, fields the body leaves out (`name`, `config`, `webhook_secret`) come from the saved evaluator, including its sealed webhook values, which the UI never sees; `kind` must match.<br>The target is `{trace_id, span_id?, variable_mapping}` or literal `{variables}`.<br>It writes no score. It commits the judge trace (origin `Test`, `environment = 'llmobs-eval'`) so the panel can link it, and counts usage.<br>The answer: the outcome or the error; the resolved variables (each value cut to 2 000 characters in the response only, read with a new public `Variables::iter`); the rendered prompt for judges; usage; the judge trace id.<br>An unsaved evaluator runs with the nil UUID as its id. A test run's judge trace id is 16 random bytes.<br>Each user may run 10 tests per minute (connection tests included).<br>- A body that sends a new webhook `config` must also send `webhook_secret`: the saved signing secret never signs requests to a URL the member just typed. |
| D-I-22 | **Manual runs.** `POST /evaluators/{id}/run` with `{trace_id, span_id?, variable_mapping}` enqueues a job at the highest priority (origin `Test` in the judge trace, so no `rule_id`) and answers `202` with the job. The UI polls `GET /eval-jobs/{id}`. Manual runs have no budget and count in usage. |
| D-I-23 | **Trace loading.** The executor loads the target trace with a new storage read, `trace_spans`. It returns every column of the trace's spans: always the parentless spans and the target span, then the earliest others, at most 5 000 spans and 64 MiB of payload columns in all, with a `truncated` flag. The timeout is 30 s, and at most 4 loads run at once.<br>A new multi-row converter in `api/convert.rs` turns the batches into `llmobs_core::Span`.<br>`QueryOptions.around` is the job's `trace_start_ns`: the matched row's `start_time`, set at enqueue (manual runs take it from the trace detail the UI already loaded, sent as `trace_start` in the body).<br>A trace over the cap still runs on the kept spans, and the score metadata gets `<prefix>.eval.truncated = "true"`. A target span over 64 MiB alone fails permanently (`trace too large`).<br>A `check` evaluator runs its whole `EvalRunner::run` inside `spawn_blocking` (with `Handle::block_on`), because regex, JSON Schema and Levenshtein on large payloads are CPU work.<br>A storage `Timeout` or `Busy` is a load retry (D-I-13); a dropped project skips the job; anything else fails it permanently.<br>- The required spans (parentless ones and the target) are themselves capped at `MAX_REQUIRED` = 1 000; more is `trace too large`. Without that cap, the read of the required part would have no bound but the query memory pool. |
| D-I-24 | **Permissions.**<br>- `ProjectPerm::ManageEvaluation` (role `member`; API keys cannot use it) covers evaluators, rules, score definitions, test and manual runs, and backfill. Reads use `ProjectPerm::View`.<br>- `OrgPerm::ManageConnections` (role `admin`) covers connection writes and tests. Listing connections is `OrgPerm::View`.<br>None of these routes is session-only (spec 08). `web/src/lib/roles.ts` gains the two client mirrors. |
| D-I-28 | **Pruning.** Every hour, `done`, `failed` and `skipped` jobs older than 7 days are deleted, and `queued` jobs whose `created_at` is older than 24 h (7 days for backfill jobs) become `skipped` (`expired`). Jobs of a project that is not `active`, or whose org is not `active`, are deleted, and its pending traces are dropped; the dispatcher and the sweeper skip such projects. |
| D-I-29 | **Config.** New keys: `encryption_key` (`LLMOBS_ENCRYPTION_KEY`), `eval.workers` (16), `security.allow_private_network_egress` (`false`, D166). The egress policy builds the one `EvalRunner` in `AppInner`. Timing constants (sweep, flush, dispatch, janitor) live in an `EvalTiming` struct with `DEFAULT` and `TEST` values, like `live::StreamTiming`, not in config. |

### Rules, backfill, annotation and pages

| # | Decision |
|---|---|
| D-I-25 | **Backfill (user).** `POST /online-rules/{id}/backfills` with `{from, to, max_targets (1–10 000), sample_rate?, dry_run}`:<br>- The window is at most 30 days. The rule must be enabled and unpaused (else 409), and only one backfill per rule runs at a time (409; a partial unique index on `backfills(rule_id) WHERE status = 'running'`).<br>- It pages `trace_list` or `span_list` over the window with the rule's filter and strict exclusions.<br>- A dry run answers synchronously: the match count (stops at `max_targets + 1`, shown as "more than"), the sampled count, and an estimate. The estimate is sampled count × (template length + 2 000 characters per mapped variable) / 4 input tokens + `max_output_tokens`, at the model's price.<br>- `to` is at most now − 5 minutes, so the window holds no trace that is still arriving.<br>- A real run creates a `backfills` row and answers `202`. A background task enqueues the jobs (priority 2, D-I-11 conflict rule) and records `matched` and `enqueued`. Before enqueueing a page, it skips targets that already have a live score from this rule (the `rule_id` score filter of M3). Job rows are pruned after 7 days, but scores are not, so a 30-day window never pays twice for a scored target. The rule budget applies when the jobs run. Backfill jobs expire after 7 days in the queue, not 24 h.<br>- A rule deleted during a run stops the task at its next insert, which marks the backfill `failed`.<br>- A backfill running at shutdown is marked `failed` (`interrupted`) at the next start. There is no cancel; disabling the rule stops its jobs. `GET /online-rules/{id}/backfills` lists them. |
| D-I-26 | **Annotation (user).**<br>- Score definitions get CRUD under project settings (`ManageEvaluation`). `DELETE` archives; a name is unique among non-archived definitions (partial unique index). Definition categories are `[{label, value?}]`, as spec 04.<br>- A score write is checked against the non-archived definition of its name: type; numeric range; for categorical, the label is one of the categories. Definitions are cached per project for 10 s, and dropped on definition writes.<br>- When a non-archived definition of that name exists, an evaluator's `output_score` must match it: the same type; a judge's `{min, max}` inside the definition's range; its labels a subset of the definition's. A judge's `label_values` are not compared with the definition's category values. The executor checks the definition before the call (step 6) as well as at the score write. An evaluator score that no longer fits fails permanently (`score definition mismatch`).<br>- The trace view (trace and selected span) and the session page get an annotation panel built from the definitions. It writes `human` scores through G's route, without `score_id`, so the id is derived per annotator.<br>- Ownership (new in the score route): a session or PAT write whose given `score_id` names an existing `human` score of another user, or a `DELETE` of one, is 403 unless the caller is a project admin. The check makes its own `scores_by_id` lookup for every supplied id, UUIDv7 included, because `scores::write` only looks up non-v7 ids. API keys and non-human scores keep G's rules: a key with `write` can still overwrite a human score, which is accepted. |
| D-I-27 | **Pages (user).**<br>- Connections at `/$org/settings/llm-connections` in org settings.<br>- The project sidebar gets "Evaluators" (`/$org/$project/evaluators`, `/evaluators/new`, `/evaluators/$id`) and "Online rules" (`/$org/$project/online-rules`, `/online-rules/new`, `/online-rules/$id` with jobs, spend, backfills and the score distribution, which the client computes from the rule's latest 1 000 scores).<br>- Score definitions at `/$org/$project/settings/score-definitions`.<br>- The trace view gets "Run evaluator", and every evaluator-made score gets "View judge trace". |
| D-I-30 | **Rule validation.** On create and PATCH:<br>- the filter is parsed by storage for the rule's view (`invalid_filter` with `position`);<br>- the mapping has 1–32 variables, each parsed by `VariableMapping`;<br>- `expected` and `item.*` are refused (experiments only);<br>- a bare `span.*` in a trace rule is refused (`needs_target_span`);<br>- every variable of a judge template (`PromptTemplate::variables`) is mapped; a check maps `output`, and `expected` when its params need it;<br>- `sample_rate` is in `(0, 1]`.<br>`target` and `evaluator_id` are immutable after create, because queued jobs depend on them. |

### Billing and entitlements (D169–D179)

| # | Decision |
|---|---|
| D-I-31 | **Judge-trace spans are not billable.**<br>- D173 bills units (spans plus scores) from `usage_daily`, and D178 says judge-trace spans do not count. `Ingest::commit` (G's code, D-G-30) adds every appended span to `usage_daily.spans`, so judge traces committed in process would be billed.<br>- The exclusion lives in G's counter: `commit` counts only the spans whose `environment` is not the reserved `llmobs-eval`.<br>- The OTLP path can never carry that environment past `accept`, which rewrites it to `llmobs-eval-external` (D-G-7). So the rule touches only in-process commits, and holds for every future in-process caller.<br>- Judge tokens and cost stay in `usage_daily.judge_*`.<br>- Evaluation scores do count as units: evaluator scores go through `scores::write`, which already adds them to `usage_daily.scores`.<br>- `usage_daily` already has the `judge_input_tokens`, `judge_output_tokens` and `judge_cost_usd` columns (migration `0002_usage`). M2 adds `Usage::add_judge`, and the flush writes those columns.<br>- M2 makes both changes, because its test runs are the first in-process commits of judge traces. |
| D-I-32 | **One active-rule limit, one check.**<br>- D178: on the Free plan an org may have 1 active online-evaluation rule; paid plans and self-hosted instances have no limit. Track P enforces it from the org's `Entitlements` (D179).<br>- An active rule is an enabled rule (`online_rules.enabled = 1`), paused or not, because a paused rule can resume at any moment. A rule stuck in an `'errors'` pause therefore still takes the Free org's one slot until someone disables or deletes it. This is accepted, and the UI's error message says so.<br>- I provides `meta::online_rules::count_enabled_rules(conn, org_id) -> Result<u64, MetaError>`. It counts the enabled rules of every project of the org, and runs inside the rule write's transaction, so two concurrent activations cannot both pass.<br>- I also provides `eval::rule_limit::check(active_after: u64, limit: Option<u64>) -> Result<(), RuleLimitError>`, where `active_after = count_enabled_rules(…) + 1`, counted in the same transaction. Every create with `enabled: true`, and every PATCH that turns `enabled` from false to true, calls it once. A refusal writes nothing. It answers `409 conflict` with the detail `the plan allows <limit> active online rules`, but only until track P's M4, which changes it to `403 plan_limit` (D-P-14), the code K uses for its alert-rule limit. If K or P has already added `plan_limit` when M3 starts, M3 uses it directly.<br>- Until P lands, the call site passes `limit: None` (allowed). P's M4 replaces that argument with `entitlements::of(&state, org_id).limits.max_active_online_rules` (`Option<u64>`), and maps `RuleLimitError` to `ApiError::PlanLimit`.<br>- A smaller entitlement (a downgrade, a removed override, an expired licence) never pauses or disables existing rules. Only creating or activating a rule is refused until the org is back under the limit.<br>- The check is a small pure function with real logic and its own tests, including the refusal; it is not a placeholder module. Its one production caller passes `None` until P, which is the same pattern as the price table (D-G-10). `GET /online-rules` responses carry `org_enabled_rules`, so the UI can show the count. |
| D-I-33 | **Open core.** Online evaluation has a free allowance (D178), so all of track I stays in the MIT core and nothing of it goes under `ee/` (D172). |
| D-I-34 | **Plan data window.** Evaluators must not read data older than the org's plan window (D175). Track P enforces this centrally, in A2's query time window. I needs only to read every target, match and backfill page through `Storage::query` and A2's reads (`trace_spans`, `trace_refresh`, `span_refresh`, `trace_list`, `span_list`), never around them. The window is `QueryOptions::visible_from`, set by one server function (D-P-21). P's M4 makes every call site take its `QueryOptions` from `entitlements::query_options(&state, org, around)`, a one-line change at each of I's call sites. Then a hidden trace is "not found" (D-I-13), and a backfill window that reaches past the plan window simply matches nothing there. |

## Scope

**In I:** everything in the decisions above. That means migrations `0004`–`0008` in `meta.db`, the crypto module, connections, evaluators, score definitions, online rules, the queue, the trigger, judge traces, evaluator scores, `usage_daily.judge_*`, backfill, the annotation panel and every page listed in D-I-27.

**Not in I:**
- datasets, experiments and their server evaluators [J] (J calls the executor, § Interfaces);
- alerts on judge scores and a notification when a rule pauses [K];
- plan enforcement [P]: the active-rule limit (D-I-32), the data window (D-I-34) and the usage page (D179). I provides the count and the check that P plugs into. There is no org-level judge-spend limit (D-P-13);
- custom model prices (`model_prices`, D-G-10) [unassigned];
- annotation queues (D19);
- `llmobs admin check` (spec 09 lists it; no track has built it): when it is built, it verifies the encryption key too.
- deleting judge traces when their target is deleted [M]. A judge trace holds the prompt, the variables or the webhook body, so track M must delete the `llmobs-eval` traces whose `llmobs.eval.target_trace_id` it deletes. `eval_jobs` rows hold no payload and are pruned after 7 days.
- a spend limit per connection or per org. There is none (D-P-13): any project `member` can create an unbudgeted rule on an org connection, and only the connection's `max_concurrency` and `requests_per_minute` bound the spend. This is accepted; per-rule budgets (D-I-15) are the control.

**Edits outside `crates/server`, `crates/cli`, `web/` and `fixtures/api/`.** Each one is small, and is listed in the milestone that makes it. The coordinator should know about them:
- `crates/eval`: `Adapter::from_name`, `StructuredOutput::from_name` and `UnknownName` become public (M1); `WebhookConfig.secret_enc` is removed and `Variables::iter` becomes public (M2); `RunInput.judge_trace_id`'s doc stops saying a retry reuses the id (M3).
- `crates/otel`: `PriceTable::price_for(model, provider, at) -> Option<&PriceRow>`, a public wrapper over the private `lookup` (M3).
- `crates/storage`, next to `trace_refresh` and in the request types:
  - `trace_spans` (M2);
  - `strict_exclusions` on `TraceRefreshRequest` and `ListScope` (M3);
  - `span_refresh` and a `validate_filter(view, text)` read (M3, which rule validation needs);
  - `ScoreListRequest` filters `evaluator_id` and `rule_id` (M3).
- Root `Cargo.toml`: `aes-gcm` (M1); `llmobs-eval` becomes a dependency of `llmobs-server` (M1).
- `crates/core` is not changed.

**Edits to other tracks' code inside `crates/server`:** G's `Ingest::commit` (`ingest/mod.rs`) counts only spans outside `llmobs-eval` (D-I-31, M2), with a test in that module.

**Files other tracks, or several I milestones, also edit:** `scores.rs` (M2, M3, M5), the route list in `http/access.rs`, `api/openapi.rs`, `crates/server/openapi.json`, `web/src/api/schema.gen.ts`, `api_examples.rs`, `serve.rs` and `ingest/mod.rs`. G's fix plan (branch `claude/g-fixes-plan`, `F4`) moves `commit` into its own task, so the M4 plan is written after that lands. Migration numbers are the next free ones when a milestone starts (track H took `0003`, so M1's is `0004`). If another track took a number first, renumber yours on rebase; a migration that already merged is never renumbered.

## Architecture

### State

`AppInner` (`crates/server/src/http/mod.rs`) gains plain fields (D-G-1):

| Field | Type | Milestone | What |
|---|---|---|---|
| `secrets` | `crate::crypto::SecretBox` | M1 | the loaded key: seal and open |
| `eval_runner` | `Arc<llmobs_eval::EvalRunner>` | M1 | one egress client for judges, webhooks and connection tests |
| `connections` | `crate::eval::connection::ConnectionCache` | M1 | decrypted connections by `(id, updated_at)` |
| `eval` | `crate::eval::EvalQueue` | M3 | wake-up `Notify`, per-connection permits and buckets (rebuilt when `updated_at` changes; jobs in flight finish on the old ones), webhook permits, budget view, breaker windows, rule hourly counters |
| `pending` | `crate::eval::trigger::PendingTraces` | M4 | the completion map and the tracked project set |

`Ingest` gains `prices() -> Arc<PriceTable>` (M1) and `context(project: &ProjectRow, now_ns) -> IngestContext` (M2), so the executor builds `RunInput.ingest` and `.prices` exactly as OTLP ingest does.

### Modules (end state)

```
crates/server/src/
  crypto.rs              SecretBox, Aad, key loading and generation, check value, ENCRYPTED_COLUMNS (M1)
  meta/
    instance_settings.rs (M1)  connections.rs (M1)  evaluators.rs, score_definitions.rs (M2)
    online_rules.rs, eval_jobs.rs, rule_spend.rs (M3)  pending_traces.rs (M4)  backfills.rs (M6)
  eval/
    mod.rs               EvalQueue, EvalTiming, spawn, shutdown (M3)
    connection.rs        ConnectionCache (M1)
    test_run.rs          one synchronous run (M2)
    execute.rs           load, resolve, estimate, run, commit, score (M2 core, M3 jobs)
    dispatch.rs          dispatch_once, claim, worker pool, janitor (M3)
    budget.rs, breaker.rs (M3)
    trigger.rs           PendingTraces, sweep_once, flush_once (M4)
    backfill.rs          (M6)
  usage.rs               Usage::add_judge, judge columns in the flush (M2: test runs count usage)
  scores.rs              Writer::Evaluator (M3), definition checks and ownership (M2, M5)
  api/   connections.rs (M1), evaluators.rs, score_definitions.rs (M2), online_rules.rs, eval_jobs.rs (M3), backfills.rs (M6)
  routes/ same split
crates/server/src/admin.rs  rotate_encryption_key (M1)
crates/server/migrations/meta/
  0004_connections.sql (M1)  0005_evaluators.sql (M2)  0006_eval_queue.sql (M3)  0007_pending_traces.sql (M4)  0008_backfills.sql (M6)
```

Every background loop exposes a `*_once(&AppState)` function that tests call directly, with `Clock::Manual`. The loops themselves only call `*_once` on a tokio interval from `EvalTiming`. The end-to-end test in M6 uses `EvalTiming::TEST` and real time.

### Tables (end state)

Each table comes in the migration of its milestone. Spec 04 is updated with each migration. Text ids are UUIDv7, times are ns, and JSON is `TEXT`. Foreign keys that must not block an org or project cascade use the default `NO ACTION`; the 409s are application checks.

```sql
-- 0004_connections.sql (M1)
CREATE TABLE instance_settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE llm_connections (
  id TEXT PRIMARY KEY,
  org_id TEXT NOT NULL REFERENCES orgs(id) ON DELETE CASCADE,
  name TEXT NOT NULL, adapter TEXT NOT NULL, base_url TEXT,
  secret_enc BLOB NOT NULL, api_key_hint TEXT NOT NULL,
  extra_headers_enc BLOB, extra_header_names TEXT NOT NULL DEFAULT '[]',
  default_model TEXT, structured_output TEXT NOT NULL DEFAULT 'native',
  max_concurrency INTEGER NOT NULL DEFAULT 4, requests_per_minute INTEGER NOT NULL DEFAULT 300,
  status TEXT NOT NULL DEFAULT 'ok', last_error TEXT, last_error_at INTEGER,
  created_by TEXT REFERENCES users(id) ON DELETE SET NULL,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
  UNIQUE(org_id, name)
);

-- 0005_evaluators.sql (M2)
CREATE TABLE evaluators (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  name TEXT NOT NULL, kind TEXT NOT NULL,
  config TEXT NOT NULL,                  -- judge/check config; webhook: {url_host, timeout_ms, header_names}
  output_score TEXT NOT NULL,
  connection_id TEXT REFERENCES llm_connections(id),   -- judges; derived from config
  secret_enc BLOB,                       -- webhook: sealed {signing_secret, url, headers}
  version INTEGER NOT NULL DEFAULT 1,
  created_by TEXT REFERENCES users(id) ON DELETE SET NULL,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
  UNIQUE(project_id, name)
);
CREATE TABLE score_definitions (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  name TEXT NOT NULL, data_type TEXT NOT NULL,
  categories TEXT,                       -- JSON [{label, value?}]
  min_value REAL, max_value REAL, description TEXT, archived_at INTEGER,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX score_definitions_live ON score_definitions(project_id, name) WHERE archived_at IS NULL;

-- 0006_eval_queue.sql (M3)
CREATE TABLE online_rules (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  evaluator_id TEXT NOT NULL REFERENCES evaluators(id),
  name TEXT NOT NULL, target TEXT NOT NULL, filter TEXT,
  variable_mapping TEXT NOT NULL, sample_rate REAL NOT NULL DEFAULT 1.0,
  budget_usd REAL, budget_period TEXT,   -- both or neither
  enabled INTEGER NOT NULL DEFAULT 1,
  paused_reason TEXT, paused_at INTEGER, paused_detail TEXT,   -- 'budget' | 'errors' | 'connection'
  created_by TEXT REFERENCES users(id) ON DELETE SET NULL,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
  UNIQUE(project_id, name)
);
CREATE TABLE eval_jobs (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  kind TEXT NOT NULL,                    -- 'rule' | 'manual' | 'backfill'
  priority INTEGER NOT NULL,             -- 0 manual, 1 rule, 2 backfill
  rule_id TEXT REFERENCES online_rules(id) ON DELETE CASCADE,
  evaluator_id TEXT NOT NULL REFERENCES evaluators(id) ON DELETE CASCADE,
  variable_mapping TEXT,                 -- manual runs; rule jobs read the rule's
  trace_id BLOB NOT NULL, span_id BLOB,
  trace_start_ns INTEGER NOT NULL,       -- storage time hint (D-I-23)
  dedup_key TEXT NOT NULL UNIQUE,
  status TEXT NOT NULL,
  attempts INTEGER NOT NULL DEFAULT 0, claims INTEGER NOT NULL DEFAULT 0, load_retries INTEGER NOT NULL DEFAULT 0,
  not_before INTEGER NOT NULL,
  error_class TEXT, error TEXT,
  judge_trace_id BLOB, score_id TEXT, evaluator_version INTEGER,
  input_tokens INTEGER, output_tokens INTEGER, cost_usd REAL, latency_ms INTEGER,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, finished_at INTEGER
);
CREATE INDEX eval_jobs_ready ON eval_jobs(status, priority, not_before);
CREATE INDEX eval_jobs_rule ON eval_jobs(rule_id, created_at);
CREATE INDEX eval_jobs_finished ON eval_jobs(finished_at) WHERE finished_at IS NOT NULL;
CREATE TABLE rule_spend (
  rule_id TEXT NOT NULL REFERENCES online_rules(id) ON DELETE CASCADE,
  day INTEGER NOT NULL,                  -- UTC midnight, ns
  spent_usd REAL NOT NULL DEFAULT 0,
  PRIMARY KEY (rule_id, day)
);

-- 0007_pending_traces.sql (M4)
CREATE TABLE pending_traces (
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  trace_id BLOB NOT NULL, first_seen INTEGER NOT NULL, last_seen INTEGER NOT NULL,
  min_start INTEGER NOT NULL, root_ended INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (project_id, trace_id)
);

-- 0008_backfills.sql (M6)
CREATE TABLE backfills (
  id TEXT PRIMARY KEY,
  rule_id TEXT NOT NULL REFERENCES online_rules(id) ON DELETE CASCADE,
  from_ns INTEGER NOT NULL, to_ns INTEGER NOT NULL, max_targets INTEGER NOT NULL, sample_rate REAL NOT NULL,
  status TEXT NOT NULL,                  -- 'running' | 'done' | 'failed'
  matched INTEGER NOT NULL DEFAULT 0, enqueued INTEGER NOT NULL DEFAULT 0, error TEXT,
  created_by TEXT REFERENCES users(id) ON DELETE SET NULL,
  created_at INTEGER NOT NULL, finished_at INTEGER
);
CREATE UNIQUE INDEX backfills_running ON backfills(rule_id) WHERE status = 'running';
```

### Secrets (M1)

```
startup: config.encryption_key ─┐
         <data_dir>/secret.key ─┼─▶ crypto::load_key ──▶ SecretBox ──▶ verify_or_record(meta) ──▶ AppInner.secrets
         neither, no check: generate ┘   (KeySource)                  instance_settings('encryption.check')
```

`SecretBox::seal(&self, plaintext, Aad) -> Result<Vec<u8>, CryptoError>` and `open(&self, sealed, Aad) -> Result<Vec<u8>, CryptoError>`. `open` checks the version byte and the key id before it decrypts. A different key id is `WrongKey`, and any other failure is `Corrupt`. The messages never contain bytes of the value. The exact interfaces are in the M1 plan.

### Queue (M3)

```
 enqueue (trigger M4, manual run M3, backfill M6): INSERT … ON CONFLICT(dedup_key) …; EvalQueue::wake()
                    ▼
 dispatch_once (every 1 s or on wake), one MetaDb::write:
   SELECT j.* FROM eval_jobs j JOIN evaluators e ON e.id = j.evaluator_id
     LEFT JOIN online_rules r ON r.id = j.rule_id
   WHERE j.status = 'queued' AND j.not_before <= :now
     AND (j.rule_id IS NULL OR (r.enabled = 1 AND r.paused_reason IS NULL))
     AND COALESCE(e.connection_id, e.id) NOT IN (:saturated)   -- connections without slot or token, webhook evaluators at 4
   -- fairness: at most 20 candidates per (priority, project), via
   -- ROW_NUMBER() OVER (PARTITION BY j.priority, j.project_id ORDER BY j.not_before) <= 20
   ORDER BY j.priority, j.not_before LIMIT 400
   → pick in Rust: strict priority, then one job per project per round, try_acquire permits
   → UPDATE eval_jobs SET status='running', claims=claims+1, updated_at=:now WHERE id IN (…) AND status='queued'
                    ▼
 worker task per job (≤ eval.workers): execute(job, permits) → finish(job, result): one write
   (job row, rule_spend, pauses, connection status); a missing row is a no-op (D-I-12)
```

**Execution** (`eval::execute::run_job`):
1. load the evaluator (current version) and the rule;
2. decrypt the connection, or the webhook values;
3. load the trace (`trace_spans`, D-I-23; load errors per D-I-13);
4. recursion guard (D-I-17);
5. resolve the variables (a failure is permanent, and there is no trace);
6. for a judge: render, estimate, check and reserve the budget (D-I-15);
7. a short write `attempts += 1`; `EvalRunner::run` with origin `Rule(rule_id)` (rule and backfill) or `Test` (manual);
8. `Ingest::commit` the judge trace (`CommitTarget { request_bytes: 0 }`; its spans are not billable and `commit` does not count them, D-I-31);
9. on success: `scores::write` with `Writer::Evaluator`; then `usage.add_judge(tokens, cost)` either way;
10. `finish` with the outcome, D-I-13 and D-I-14.

**Startup** (in `serve::bind`, after `meta.db` opens): finish an interrupted key rotation, load and check the key (M1); set `running` jobs back to `queued` and fail those with `claims > 3`; mark `running` backfills `failed`; load the budget view, the rule and settings caches, `pending_traces` and the tracked set; spawn the dispatcher, workers, janitor, pruner, budget ticker, sweeper and flusher; then bind the ports.

**Shutdown** (in `Running::shutdown`, after the HTTP drain and before the existing `tasks` abort):
1. a stop signal (a `tokio::sync::watch` channel in `EvalQueue`) tells every eval loop and backfill task to stop; they are joined, not aborted;
2. `flush_once` runs one last time;
3. workers get 3 s to finish, then they are aborted, and their jobs stay `running` until the next start;
4. G's existing steps follow. With G's fix F4, `wait_for_commits` also covers the judge-trace commits that workers issued. Then the usage flush, storage and `meta.db`.

Test-run handlers are HTTP requests, so the drain covers them. The total (20 s drain, 3 s workers, flushes, 30 s storage) stays inside spec 12's 60 s.

### Trigger (M4)

```
Ingest::commit(…, pending) ─after append─▶ PendingTraces::observe(project, &spans, now)   (memory only)
sweep_once (every 2 s) ─▶ completed = root_ended && idle ≥ 5 s || idle ≥ timeout
   └─ per tracked project, batches of 500 ─▶ per enabled unpaused rule: trace_refresh | span_refresh (strict)
        └─ sample ─▶ hourly cap ─▶ enqueue ─▶ remove ids whose last_seen is unchanged
flush_once (every 5 s, at shutdown) ─▶ changed entries to pending_traces (≤ 1 000 rows per transaction, deletes first)
startup ─▶ load pending_traces; rebuild the tracked set
```

### Errors and status codes (new routes)

| Cause | Status | Code |
|---|---|---|
| body or field invalid (names, ranges, mapping, config) | 400 | `bad_request` with `errors[]` |
| the rule filter does not parse | 400 | `invalid_filter`, with `position` |
| connection or evaluator outside the caller's org or project | 404 | `not_found` |
| duplicate name; deleting a connection used by evaluators, or an evaluator used by rules; a backfill on a paused rule, or a second backfill | 409 | `conflict` |
| test-run limit | 429 | `rate_limited` |
| a test run's trace could not be loaded (busy, timeout) | 503 | `busy` |
| a test run's trace not found | 404 | `not_found` |
| editing someone else's human score | 403 | `forbidden` |
| creating or activating a rule over the plan's active-rule limit (D-I-32) | 409 (403 once P's M4 lands) | `conflict` (`plan_limit` once P's M4 lands) |

### API shapes (summary; each milestone plan gives the exact types)

Every route is under `/api/v1/projects/{project}/…`, except connections (`/api/v1/orgs/{org}/llm-connections`). `POST /evaluators/test` and `POST /online-rules/preview` are `ManageEvaluation`, so a read-only PAT cannot call them, because they spend money or run heavy reads.

- `OnlineRule` adds read-only fields:
  - `paused_reason?`, `paused_at?`, `paused_detail?`;
  - `spend: {period_start, spent_usd, reserved_usd}` (when a budget is set);
  - `jobs_last_24h: {queued, running, done, failed, skipped}`;
  - `dropped_last_hour`.
- `GET /online-rules` answers `{data, next_cursor, org_enabled_rules}`, where `org_enabled_rules` is D-I-32's count.
- `GET /eval-jobs?rule_id=&evaluator_id=&status=&kind=&limit=&cursor=`: newest first. It replaces spec 09's `GET /online-rules/{id}/jobs`.
- `EvalJob`: `{id, kind, status, rule_id?, evaluator_id, trace_id, span_id?, attempts, error_class?, error?, judge_trace_id?, score_id?, input_tokens?, output_tokens?, cost_usd?, latency_ms?, not_before, created_at, finished_at?}`.
- `POST /online-rules/preview` `{target, filter?, from?, to?}` → `{matched}`: the count over the last 24 h by default, at most 10 000, with strict exclusions.
- `POST /online-rules/{id}/resume`, `POST /evaluators/{id}/run`, `POST /evaluators/test`, `GET /evaluator-templates`, and `GET/POST /online-rules/{id}/backfills`.

### Logs and privacy

Logs carry ids, kinds, statuses, durations, counts and error classes. They never carry prompts, answers, variables, API keys, header values, webhook URLs or secrets (AGENTS.md § Privacy).

`eval_jobs.error`, `online_rules.paused_detail` and `llm_connections.last_error` hold `EvalError`'s display, which E guarantees quotes no payload (D160). `JudgeError::Provider`'s `provider_message` and `hint` can echo input, so they are never stored or shown.

A webhook judge trace stores the request body (the variables), like any judge trace stores its prompt. It lives in the user's own project, under `llmobs-eval`.

Audit events (`target = "audit"`) cover connection create, update, delete and test, key rotation, and rule pause and resume.

## Interfaces for later tracks

- **J (experiments).** `eval::enqueue` with `kind = 'experiment'` and a dedup key `exp:<experiment_id>:<item_id>:<evaluator_id>`, and `execute` with `ItemData`. J adds the `experiment_id` and `dataset_item_id` columns to `eval_jobs` in its own migration, and the `EvalOrigin::Experiment` arm.
- **K (alerts).** Rule pauses are audit events and `online_rules` columns. Judge scores are ordinary scores with `source` set.
- **P (plans and entitlements).**
  - `usage_daily.judge_*` is written by a new `Usage::add_judge(org, project, now, input_tokens, output_tokens, cost_usd)`, whose counts join the 10 s flush. Connection tests belong to no project and count nothing.
  - `usage_daily.spans` excludes judge-trace spans (D-I-31).
  - `count_enabled_rules` and `rule_limit::check` are the hooks for the active-rule limit (D-I-32).
  - `rule_spend` holds per-rule judge spend.

## Milestones

Each milestone is one plan file. Its sub-phases are one commit each, and each ends with `just check` green. Each plan names the spec sections it rewrites.

| # | Plan | Contents | Depends on |
|---|---|---|---|
| **M1** | [`01-m1-secrets-connections.md`](01-m1-secrets-connections.md) | config keys; `crypto.rs`; key loading and the startup check; `instance_settings`; `llm_connections` migration; connections API with test; `rotate-encryption-key`; the eval crate's public names; `Ingest::prices`; org settings page "LLM connections" | G merged |
| **M2** | `02-m2-evaluators.md` | `evaluators` and `score_definitions` migration; storage `trace_spans`; multi-row Arrow → `Span`; `Ingest::context`; evaluators API (webhook values sealed); templates; test run; score-definition API and checks in `scores::write`; `WebhookConfig.secret_enc` removed; `commit` stops counting `llmobs-eval` spans as billable (D-I-31), with `Usage::add_judge` (D-I-31) and a test that a test run adds judge tokens and cost but no spans to `usage_daily`; Evaluators pages with the test panel; score definitions page | M1 |
| **M3** | `03-m3-queue-rules.md` | `online_rules`, `eval_jobs`, `rule_spend`; storage `strict_exclusions`, `span_refresh`, `validate_filter` and score filters; rules API with validation, `count_enabled_rules` and the `rule_limit::check` call on create and activation (D-I-32); a test that evaluator scores count in `usage_daily.scores`; dispatcher, workers, janitor, retries, breaker, pauses, budgets (`PriceTable::price_for`); evaluator score writes; manual runs; jobs API; pruning; shutdown order; Online rules pages; "Run evaluator"; "View judge trace" | M2 |
| **M4** | `04-m4-triggers.md` | `PendingTraces` and the `commit` argument; `pending_traces` migration; `sweep_once` and `flush_once`; sampling; the hourly cap; `trace_completion_timeout_s` in project settings; `POST /online-rules/preview` and the editor's counter | M3, G fixes F4 |
| **M5** | `05-m5-annotation.md` | annotation panel on trace, span and session; the score-ownership check | M2 |
| **M6** | `06-m6-backfill-closeout.md` | `backfills` migration, API and dialog; end-to-end Rust test (`llmobs serve`, the mock provider, OTLP in, judge score out); spec and roadmap closeout | M4 |

M5 can run in parallel with M3 or M4 (different files, except the route list).

**Spec sections each milestone rewrites:**
- M1: 04 § Keys & credentials, § Encryption of secrets, § Metering & instance; 08 roles note; 09 § Orgs and § CLI; 11 § Navigation and § Settings; 12 § Configuration, § Process lifecycle and § Backups.
- M2: 04 § Evaluation (evaluators) and § Scores definitions; 06 § Evaluators (webhook storage, test runs); 09 § Evaluation and § Scores; 04 § Metering & instance (`spans` counts billable spans: not `llmobs-eval`, D-I-31).
- M3: 04 § Evaluation (queue tables); 06 § Online rules › Execution; 09 § Evaluation; 12 § Process lifecycle (no leases); 06 § Online rules (the active-rule limit and its check, D-I-32).
- M4: 02 § Pipeline; 06 § Online rules › Triggering and Sampling.
- M5: 06 § Scores (ownership); 11 annotation.
- M6: 06 § Backfill; 09 backfills.

## API contract gaps (found while designing)

1. No evaluation operation is in `openapi.json` yet: connections, evaluators, templates, test, rules, jobs, backfill and score definitions all come with their milestone.
2. Spec 09 lists `/orgs/{org}/llm-connections` "(track E)". It is track I's, without project scope (D-I-4).
3. Spec 09's `POST /evaluators/{id}/test` becomes `POST /evaluators/test` with an id or a body (D-I-21).
4. Spec 09's `GET /online-rules/{id}/jobs` becomes `GET /eval-jobs?rule_id=` (§ API shapes).
5. Spec 09's `POST /online-rules/{id}/backfill` becomes `POST /online-rules/{id}/backfills` with a list (D-I-25).
6. `POST /evaluators/{id}/run`, `GET /eval-jobs/{id}`, `POST /online-rules/{id}/resume` and `POST /online-rules/preview` are new.
7. `GET /scores` cannot filter by `evaluator_id` or `rule_id`. M3 adds the filters in storage and in `ScoreListParams`.
8. `UpdateProjectRequest` has no typed `trace_completion_timeout_s` (M4 adds it).
9. Spec 06's `eval_jobs` lacked dedup, priority, kind, claim and usage columns, and its `lease_until` is dropped (D-I-12).
10. Spec 12 says the connection form shows `allow_private_network_egress`. No endpoint exposes it to members, so the form explains it instead (D-I-4).

## Risks

- **`meta.db` writer contention.**
  - Claims, finishes, pauses and pending-trace flushes all take the single writer. Each is one short transaction, and flushes are cut at 1 000 rows. At 16 workers this is a few dozen writes a second.
  - Ingest takes no writer lock.
- **Sweeper reads.** `trace_refresh` scans the project's traces within `around ± 1 day` once per call, so the cost grows with the number of enabled rules. Ten rules on a busy project mean ten reads every 2 s. M4 measures this with the A1 benchmark data. If it is too slow, one read per batch for all trace rules without a filter is the first optimisation.
- **Clocks.** Judge-trace span times come from the eval crate's system clock, while `IngestContext.ingest_time` comes from `state.clock`. The normalizer drops a span whose `start_time` is more than one day after the ingest time (`StartTimeOutOfRange`, whatever the retention), so a test on the frozen `Clock::Manual` (2026-01-01) would lose every judge span. Tests that run an evaluator therefore set the manual clock to the real time first (M2 plan § Conventions). In production both clocks are the system clock.
