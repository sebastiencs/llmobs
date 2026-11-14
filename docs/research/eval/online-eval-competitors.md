# Online evaluation: how competitors do it

Research notes (design only, nothing copied). Checkouts in `/home/sebastien/github/{langfuse,phoenix,opik,langwatch,lmnr}`.
Paths below are relative to each repo. "Online eval" = LLM-as-judge (or code check) run automatically on incoming production traces.

Scope caveats:
- **Phoenix**: online evals are only a spec (`internal_docs/specs/online-evals.md`), not implemented in this checkout. Evaluators run on datasets/experiments and in the playground.
- **Lmnr**: the signal processing engine (their online LLM analysis) is enterprise-only (`lmnr-private`); OSS has CRUD, call-site stubs and `docs/internal/*.md` notes. Evidence is from docs, not the worker.
- **LangWatch**: the legacy BullMQ worker was removed; evidence is the new event-sourced module `modules/evaluation` + `.feature` specs (which embed incident write-ups).

## Summary table

| | Langfuse | Phoenix (spec) | Opik | LangWatch | Lmnr |
|---|---|---|---|---|---|
| Target | trace (legacy), observation (new), dataset item | span / trace / session (proposed) | trace, span, thread | trace, thread | trace (signals) |
| Trigger | trace-upsert job (30 s delay) / each observation on OTel ingest | span stored; trace/session on idle | in-process event on trace created/updated **with end_time** | span_received event, 30 s quiet window per trace | root span finished or span name matches (batch of spans) |
| Completeness | fixed delay (trace), none (observation) | idle timeout (proposed) | `end_time != null`; threads closed after 15 min idle | 30 s settle + re-run on every new span | explicit trigger kind `rootSpanFinished` |
| Sampling | deterministic SHA-256 of trace/obs id | deterministic hash rho (proposed) | `SecureRandom` per trace | `Math.random()` at exec time | `sampleRate` 1..95 in metadata |
| Dedup | PK = hash(project, rule, trace, item, obs) | n/a | none | queue job id `exec:tenant:trace:evaluator` + 30 s ttl | duplicates tolerated |
| Queue | BullMQ/Redis + `job_executions` row in Postgres | heartbeat-claimed DB rows (experiment runner pattern) | Redis Streams, consumer group, XAUTOCLAIM | BullMQ-style event-sourcing queue (Redis) | RabbitMQ quorum queues + TTL delay queues |
| Auto-pause | yes, evaluator "blocked" with typed reason | circuit breaker (experiments) | no | yes, >=90% errors over >=50 runs in 1 h | no (run FAILED, config errors non-retryable) |
| Cost cap | no (provider-side only) | no | `maxCostUsd` per evaluation (soft) | per-run cost row | workspace credit + hard limit (cloud) |
| Judge traces | yes, `langfuse-llm-as-a-judge` env in same project | proposed (dedicated project) | yes, hidden `source=evaluator` trace | via workflow runtime, causality depth | run root span |
| Secrets | AES-256-GCM, `ENCRYPTION_KEY` | Fernet (PBKDF2 of `PHOENIX_SECRET`) | AES-GCM/legacy AES, `OPIK_ENCRYPTION_KEY` | `CREDENTIALS_SECRET` | XChaCha20-Poly1305, `AEAD_SECRET_KEY`, AAD=profile id |
| Backfill | yes, batch action over events table, cap 50k | deferred | no | re-run on one stored trace only | not found |

---

## Langfuse

Two generations coexist: legacy trace/dataset rules (`worker/src/features/evaluation/evalService.ts`) and new observation-level evaluators (`worker/src/features/evaluation/observationEval/`). Evaluator v2 = `Evaluator` (+ versions) and `EvaluationRule` (filter, sampling, delay, assignments) in `packages/shared/prisma/schema.prisma`.

### 1. Trigger
- Legacy: trace upsert enqueues a `TraceUpsert` BullMQ job with a **fixed 30 s `delay`** (`packages/shared/src/server/redis/traceUpsert.ts`); the processor calls `createEvalJobs` (`worker/src/queues/evalQueue.ts`, `evalJobTraceCreatorQueueProcessor`). There is no "trace complete" notion, only the delay; the error text literally says "consider extending the job delay" (`evalService.ts` ~L1696). Each rule also has its own `delay` (ms) applied when the execution job is enqueued (`evalService.ts` ~L869).
- Also triggered by dataset-run-item upsert and by `CreateEvalQueue` (UI "run on existing data").
- New: each observation arriving through OTel ingestion is matched in-line against rules (`scheduleEvals.ts`, `scheduleObservationEvals.ts`); execution job queued with `delay: 0`. Observation-level avoids the completeness problem entirely. Late-arriving media is handled by a per-job retry on `media-not-found` (`evalQueue.ts`, `llmAsJudgeExecutionQueueProcessorBuilder`).
- Trace-level batching for "Topics" uses a Redis idle-deadline map ("every admitted event refreshes the trace's idle deadline", `worker/src/features/traceBatching/README.md`): the closest thing to real idle detection, cloud-only.

### 2. Filtering, sampling, dedup
- Filter: structured filter list (`singleFilterList`), evaluated in memory against a cached trace when possible, else via DB lookup (`evalService.ts` ~L400-640). Rules carry `timeScope` NEW/EXISTING so live path ignores backfill-only rules.
- Sampling: deterministic. SHA-256 of a domain-separated string + id, 53 bits to a float in [0,1); sampled if `value < rate` (`worker/src/features/evaluation/deterministicSampling.ts`). Observation-level samples by observation id, trace-level by trace id. Same retry/redelivery = same decision.
- Dedup: execution id is a **deterministic hash of (project, rule, trace, dataset item, observation)** (`createDeterministicJobExecutionId`, `evalService.ts` ~L288). Comment explains why: two producers raced on a read-then-write existence check and doubled LLM spend and scores; the PK now rejects the loser. A re-seen trace "deselected" by a later event cancels its pending job.
- Internal-env traces are skipped at creation (see 6).

### 3. Queue
- BullMQ on Redis. Queues: `TraceUpsert` (concurrency 25), `CreateEvalQueue` (concurrency 2 + limiter), `EvaluationExecution` (default 5; optional shards, secondary queue for noisy projects via `LANGFUSE_SECONDARY_EVAL_EXECUTION_QUEUE_ENABLED_PROJECT_IDS`), `LLMAsJudgeExecution` (5, lockDuration 60 s, maxStalledCount 3) (`worker/src/app.ts`, `env.ts`).
- State is also persisted in Postgres `job_executions` (status `PENDING|COMPLETED|ERROR|CANCELLED|DELAYED`, error, start/end, `executionTraceId`, output score id). Redis is the transport, Postgres is the truth for UI.
- Default job opts: execution queue `attempts: 10`, exponential 1 s (`redis/evalExecutionQueue.ts`); creator queue `attempts: 5`, exp 5 s (`createEvalQueue.ts`).
- Row inserted **before** enqueue; if enqueue fails the row is deleted ("otherwise BullMQ redelivery hits the dedup check and strands the execution at PENDING", `evalService.ts` ~L874).
- Workers re-check executability on pickup (rule inactive, evaluator blocked, template version missing -> mark CANCELLED); processor short-circuits jobs already COMPLETED/ERROR/CANCELLED (`observationEvalProcessor.ts` ~L130-171).
- Retry classification (`evalQueue.ts`): rate-limit/5xx-style LLM errors -> **scheduled re-delivery out to 2 h**, not normal backoff (`worker/src/features/utils/retry-handler.ts`: first retry 5 min, then spread to `MAX_AGE_SECONDS`=7200 over `MAX_ATTEMPTS`=4, +-20 % jitter, status DELAYED). Terminal LLM errors (auth, 404, billing, content filter) and `UnrecoverableError` -> ERROR immediately, no retry. Unknown errors rethrow -> BullMQ retry; the terminal ERROR status is only written on the **final attempt**, otherwise the processor would skip its own retries (explicit comment, `evalQueue.ts` ~L377).
- User-facing error text is only stored for LLM/config errors; other errors store "An internal error occurred" (privacy).

### 4. Failure handling
- **Auto-pause**: evaluator gets `blockedAt`, `blockReason`, `blockMessage` (enum `EvaluatorBlockReason`: auth invalid, billing exhausted, endpoint unreachable, connection missing, default model missing, model config invalid, model unavailable, provider account not ready). Classification in `packages/shared/src/server/evals/classifyEvaluatorLlmError.ts`: 401 -> auth, 402 -> billing, 404 -> model unavailable, plus message patterns ("credit balance is too low", "API key not valid", "DNS lookup failed"...). 403 -> auth **unless** it is a content-filter/moderation rejection of one input (those must not pause the evaluator).
- Pause is a single claim-update (`blockEvaluatorsInTx`: only rows with `blockedAt IS NULL` are claimed so only the first claimer notifies) and sends a project notification (`dispatchProjectNotification`). Queued executions self-cancel on pickup. UI shows `EvaluatorPausedCallout`; some reasons auto-recover when the user edits the definition (`evalConfigBlocking.ts`).
- Deleting an LLM connection / default model pauses every evaluator using it (`blockEvaluatorsUsingProvider`, `...UsingDefaultModel`) and re-enabling the default model resumes only those paused for that reason.
- 429 = retryable path above (long scheduled retry, not a pause).
- Metrics: time-to-first-attempt, terminal outcome with attribution (`evalExecutionMetrics.ts`).

### 5. Cost
- No budget/cap. Judge calls are traced as generations so normal usage/cost computation applies; "Budget has been exceeded"/"insufficient_quota" provider errors map to the billing pause.

### 6. Judge tracing and recursion
- Judge call is written as a trace in the **same project** with `environment` from `LangfuseInternalTraceEnvironment` (`langfuse-llm-as-a-judge`, `-code-eval`, `-prompt-experiment`, `-topics`, ...) (`packages/shared/src/server/llm/types.ts` ~L541). Trace id = deterministic hash of the job execution id (`createW3CTraceId`), stored in `job_executions.execution_trace_id` so UI links to it. Hidden by environment filtering, not by a separate store.
- Recursion guard is **defence in depth**: skipped at job creation (`isInternalEvalEnvironment`, trace-upsert path), skipped again in observation scheduling (comment explains the infinite-loop case), and re-checked at execution. One deliberate exception: prompt-experiment root observations; their judge runs stay in the blocked env.

### 7. Secrets
- `LlmApiKeys` encrypted with AES-256-GCM, 12-byte random IV, format `iv:cipher:tag` hex, key = `ENCRYPTION_KEY` (64 hex chars) (`packages/shared/src/encryption/encryption.ts`). Single key, no versioning/rotation mechanism visible.

### 8. Backfill
- Rule `timeScope` EXISTING: UI saves the rule then enqueues `CreateEvalQueue` (legacy) or creates a **batch action** `ObservationBatchEvaluation` that queries the events table with the rule filter (`web/src/features/batch-actions/server/batchEvaluationService.ts`). Cap `LANGFUSE_MAX_HISTORIC_EVAL_CREATION_LIMIT` = 50 000 (request rejected with "refine your filters"), optional `sampling`, `rowLimit`. Batch executions get their own scope id so a second run is not deduped against the first (`executionScopeId`, `batchActionId`). Sentinel timestamp 2020-01-01 marks "historic" jobs (`evalService.ts` ~L566).

### 9. Manual run / test
- Test evaluator on a sample observation without persisting a score (`web/src/features/evals/v2/server/evaluators/testEvaluator.ts`, UI `.../Testing/components/SampleObservationSelectorBase`). Supports LLM, code and decision-model evaluators.

### 10. UI
- v2: `web/src/features/evals/v2/pages/EvaluatorsPage.tsx`, rules router (`ruleRouter.ts`), evaluator saved-dialog with backfill option (`useEvaluatorSavedBackfill.ts`), paused callout, execution log table (`eval-logs-config.ts`, `evalExecutionsTable.ts`) with status/error/trace link.

---

## Phoenix (spec only)

Implemented: evaluators (`LLM`/`CODE`/`BUILTIN`, `src/phoenix/db/models.py` ~L3070) attached to datasets via `DatasetEvaluators`; experiment runner with background daemon; evaluator preview/inline evaluators (`EvaluatorPreviewInput.py`); optional tracing of evaluator runs via an injected tracer (`api/evaluators.py`).

The online-evals spec (`internal_docs/specs/online-evals.md`, 557 lines) is the most thoughtful write-up of the problem; main points:
1. Trigger classes: span stored; artifact idle (trace/session); late annotation arrives (event-driven, e.g. thumbs-down feedback). Traces have no "done" event: root-span end vs idle period, open question.
2. Filters: today only span-scoped DSL; needs parent/tree-level predicates (open-endedly expensive, may need in-app evaluation).
3. Sampling: deterministic rho = hash(id) in [0,1), `matches filter AND rho < rate`. Commutes with filtering so the cheap gate can run first. Shared rho gives **co-sampling** (nested filters + nested rates give nested sets; cheap code eval and LLM judge on the same sample). Suggests an explicit cohort/salt, and asks whether the hash algorithm should be pinned as a compatibility guarantee.
4. Output: annotations with stable `identifier` give upsert/override for re-evaluation; for hoisted results the key must include the source artifact id. Run history is separate.
5. **Run record is v1-blocking**: annotations cannot express filtered-out / sampled-out / overload-dropped / pending / failed; needs a per-attempt decision record (modelled on `experiment_eval_logs`). Failure taxonomy: retryable, terminal, config error, sampled out, overload drop, disabled, target changed.
6. Reliability: never block ingestion; overload backstop must shed load **visibly**; recovery scan for missed work ("transient queue state must not be the only record of whether something was supposed to be evaluated"); disabling stops new runs immediately; failed runs write no partial annotations; push notification for a silently disabled evaluator is an open question.
7. Loop guard: evaluator traces go to a dedicated project; annotation-arrival triggers open a second recursion loop.
8. Open: SQLite vs PostgreSQL execution topology, multi-replica ownership, roles (who may spend money), prompt-injection of judge via production content, data minimisation.

Reusable pattern (`internal_docs/specs/experiment-runner-background-process.md`):
- In-process daemon, global semaphore (`MAX_CONCURRENT`=20), 0.1 s poll, round-robin fairness across experiments, rate-limit "advisor" pattern (check token bucket before handing a job to the daemon, never block the daemon).
- **Ownership by heartbeat**, works on SQLite and Postgres: `claimed_at`/`claimed_by`, atomic `UPDATE ... WHERE claimed_at IS NULL (or < cutoff) RETURNING`, heartbeat every 5 min, stale after 10 min, orphan scan at startup and every ~10 min with jitter.
- **No queue persistence**: on restart re-query incomplete work from the DB; retry backoff state is lost (accepted).
- Errors: rate limit (429) requeued forever with exponential backoff and bucket notified; transient up to 3 retries (1/2/4 s); permanent (400, bad key) fails. **Circuit breaker**: 5 consecutive failures stops the experiment, separate breakers for task and eval providers, one success resets, no time-based reset; reason in `last_error`. Job timeouts 120 s task / 60 s eval; `MAX_CONSECUTIVE_ERRORS`=50 for daemon bugs; background loops catch-and-retry so a heartbeat failure cannot kill the dispatcher.
- Secrets: Fernet (AES-128-CBC + HMAC) with key derived by PBKDF2-600k from `PHOENIX_SECRET`; if the secret is absent the key is publicly derivable; rotation is manual re-encryption (`src/phoenix/server/encryption.py`).

---

## Opik (Java, Dropwizard, Redis Streams, ClickHouse)

### 1-2. Trigger, filter, sampling, dedup
- `OnlineScoringSampler` (`apps/opik-backend/src/main/java/com/comet/opik/api/resources/v1/events/OnlineScoringSampler.java`) subscribes to **in-memory EventBus** events `TracesCreated` / `TracesUpdated`. Only traces with `endTime != null` are scored: SDK may POST a start event then PATCH on end, so both paths are handled (the update path re-reads traces from ClickHouse; comment flags a replica-lag race).
- Per rule: enabled -> `matchesTriggerScope` (PRODUCTION / EXPERIMENT / BOTH) -> filters (`TraceFilterEvaluationService`) -> `SecureRandom.nextFloat() < samplingRate`. Experiment traces skip filters and sampling; a rule picked in the playground bypasses everything. Evaluator traces are not "logging sources" so they never match (see 6).
- No dedup: same trace can be scored twice (create then update, redelivery). Sampling is non-deterministic.
- Every skip decision is counted (`online_scoring_sampler_decisions_total{decision=sampled|skipped_disabled|skipped_filter|skipped_sampling}`) and written to a **user-facing per-rule log stream** (`UserFacingLoggingFactory`, `UserLog.AUTOMATION_RULE_EVALUATOR`, `GET /v1/private/automations/evaluators/{id}/logs`).
- Threads: `TraceThreadsClosingJob` marks a thread inactive after `timeoutToMarkThreadAsInactive` (default 15 m, per-project override) and then thread-level scoring fires (`TraceThreadOnlineScoringSamplerListener`).
- A config comment admits there is no pre-evaluation dispatch delay yet (attachment upload race; mitigated by retrying the attachment lookup 5 x 300 ms; ticket OPIK-7224).

### 3. Queue
- Redis Streams per scorer type (`stream_scoring_llm_as_judge`, `stream_scoring_user_defined_metric_python`), consumer group `online_scoring`, batch 10, poll 500 ms, long-poll 5 s, `maxRetries` 3, `pendingMessageDuration` 10 m (orphan threshold), stream max len 10 000 approximate trim (`config.yml` ~L1038, `OnlineScoringConfig.java`). Oldest entries are evicted under overload: **silent load shedding**.
- `BaseRedisSubscriber.java`: XAUTOCLAIM of stale pending entries every N polls. Retryability: bug-class exceptions (NPE, IllegalArgument...) are permanent; HTTP 4xx permanent **except 408/425/429** (comment: matching the whole 4xx family by class had made 429 permanent, now classified by status); delivery count from the pending list; after `maxRetries` the entry is retired.
- Pitfall documented in code: XAUTOCLAIM examines at most COUNT*10 entries per call and returns a cursor; discarding the cursor left a permanently unreachable tail for backlogs >~100 (`claimCursor` field). Also: "this pod cannot decode it" is treated as retryable (another pod may).
- Concurrency: governed by batch size/poll cadence per stream, no per-project limit found. LLM provider fallback: on provider 4xx the judge retries with the next configured model (`judgeWithNextModel`).

### 4. Failure handling
- No auto-disable found; failure written to the user-facing rule log. Rules have an `enabled` flag only.

### 5. Cost
- `maxCostUsd` per rule (`AutomationRuleEvaluatorLlmAsJudge`), enforced by `BudgetGuard.java`: per-evaluation **soft** wrap-up trigger for the agentic tool loop (spend computed in-process from token usage and a static price table; overshoot by one wrap-up call by design; fail-open; warns once if the model has no price, since an unpriced model makes the cap silently inert).
- Large traces: estimated tokens > 50 000 route to an agentic path (read/jq/search tools) instead of inlining; per-variable cap 4000 chars; attachment injection cap 50 MB.

### 6. Judge tracing
- `OnlineEvaluationRecorder` writes one trace per evaluation with one `llm` span per round-trip; token usage on spans so normal cost aggregation applies. Trace has `source=EVALUATOR` and `VisibilityMode.HIDDEN`, kept out of the default trace list; samplers only score `isLoggingSource` sources, so no recursion (`domain/evaluation/OnlineEvaluationRecorder.java`, `api/Source.java`).

### 7. Secrets
- `EncryptionUtils.java`: legacy AES (deterministic, no IV) for provider keys, AES-GCM for structured auth config. Key from `OPIK_ENCRYPTION_KEY`; **default is a hardcoded 16-char string** in `config.yml` L1035. Cautionary tale: never ship a default key.

### 8-10. Backfill, manual run, UI
- No backfill or manual re-run found (a comment in `AnnotationQueueRoutingListener.java` says the feature has none, "so an event lost in flight would keep a trace out permanently and silently"). Playground lets users pick rules for experiment traces. UI: `OnlineEvaluationPage`, `RuleEditorPanel` (with sample-data preview `useRuleSampleData.ts`), per-rule logs tab (`useRulesLogsList.ts`).

---

## LangWatch (TypeScript, event sourcing, ClickHouse + Postgres)

"Monitors" = online evaluators (`modules/monitor`, `modules/evaluation`). Specs are rich in incident notes.

### 1. Trigger
- Subscriber `traceEvaluationTrigger` reacts to `span_received` and `origin_resolved` facts, reads the folded trace summary, dispatches **one `executeEvaluation` command per enabled monitor** (`modules/evaluation/process/src/eventing/trace-evaluation-trigger.subscriber.ts`, `evaluation-processing-definition.pipeline.ts`).
- **Debounce twice**: per trace and lane a 30 s quiet window (`TRACE_EVALUATION_TRIGGER_SETTLE_MS`, queue delay + dedup ttl); then the command itself has `delay: 30_000` with dedup by job id. A new span on a recent trace re-triggers (so traces are re-evaluated as they grow; thread-level monitors wait for `threadIdleTimeout`, job id keyed by thread).
- If the folded summary is missing after the settle window the job fails and retries a bounded number of times (12, ~27 min); at the bound it completes with a warning because "unbounded retries blocked a queue group per such trace".
- Guards (each from an incident): derived events (e.g. topic assignment) do not trigger; traces older than a cutoff are never re-evaluated; traces past the span-count cap are dropped ("we drop the work and never the data"); synthetic event spans refused before enqueue.

### 2. Filter, sampling, dedup
- Preconditions (field/operator rules over trace attrs, metadata, labels, topics, models, events) in `evaluation-precondition.service.ts`; required-field check per evaluator type. Sampling at execution time with `Math.random() <= monitor.sample` (`evaluation-execution-preparation.service.ts` ~L167), so drops are recorded as "Excluded by sampling" outcomes.
- Dedup: job id `exec:<tenant>:<trace>:<evaluator>` (`execute-evaluation.intent.ts`); retries use `...:<evaluationId>:attempt:<n>` so a retry never collapses onto the first send. Cost rows keyed by idempotency key to avoid double billing on redelivery (`evaluation-cost.service.ts`). Terminal state reached exactly once per run (`evaluation-run.feature`).

### 3-4. Queue, retry, failure
- Redis-backed event-sourcing queue, commands `serializeByAggregate`. Retry window for evaluator-service-down failures: setting `evaluatorRetryWindowMinutes`, exponential from 5 s doubling, never below the service's `retryAfterMs`, cap 10 min (`rules/evaluation-retry-window.rules.ts`); outside the window the run is recorded as an error result, not lost. Skipped (monitor deleted, evaluator unavailable, preconditions unmet, sampled out) is a distinct outcome from error; failed verdict (`passed=false`) is not an error.
- **Auto-pause**: when >= 50 runs settled in the last hour and >= 90 % are errors, evaluation records a fact per monitor revision (`monitor-error-rate.feature`); monitor module disables the monitor, stores `autoPausedAt/autoPausedReason`, emails org admins **once** (idempotent per revision), UI shows "Paused automatically" badge; re-enabling clears the reason (`modules/monitor/specs/monitor-auto-pause.feature`). Report about an older revision is ignored.

### 5. Cost
- Per-run cost record with currency, idempotent. Model cascade resolves the model per feature (`evaluation-model-cascade.service.ts`). No cap seen.

### 6. Judge tracing and recursion (**2026-05-11 incident**)
- A "run on every trace" monitor whose evaluator workflow emitted its own traces looped: ~500K event-sourcing groups in ~90 min, starving other tenants (`specs/monitors/online-evaluator-loop-prevention.feature`). Fix: numeric **causality depth** attribute `langwatch.reserved.causality_depth` stamped via baggage on every span emitted during an evaluator run, incremented at each evaluator boundary; trigger refuses depth >= 1, per span (a later depth-0 span on the same trace still triggers). Counter `evaluator_loop_blocked_total{reason}`, kill-switch flag. Gotchas recorded: the attribute key is a wire format (rename on either side silently removes the guard), user-submitted `langwatch.reserved.*` attributes are stripped on ingest but the depth key must survive the strip ("the original fix was silently disabled in production"), depth can arrive in any OTLP encoding.

### 7. Secrets
- `CREDENTIALS_SECRET` env (generated by the scaffolder and persisted so deleting `.env` does not rotate it out from under encrypted rows; `apps/server/src/shared/env.ts` ~L215). Model provider credentials mapped in `modules/model-provider`.

### 8-10. Backfill, manual run, UI
- Docs: monitors score traces "as they arrive"; no backfill found. "Re-run an evaluator on one stored trace" exists (`evaluation-service.feature`). Evaluator test via the evaluators UI. UI: Online Evaluations list (`modules/monitor/browser`, `online-evaluations-table`), drawer with preconditions, sampling, trace/thread level (`online-evaluation-drawer.tsx`), automation (alert/dataset) hooks.

---

## Lmnr (Rust app-server; signals)

- Signals = LLM prompt + structured output schema run over traces, producing `signal_events` (ClickHouse) and a per-run row `signal_runs` with `input_tokens`, `cache_read_tokens`, `output_tokens`, `signal_version`, `credit_applied` (`docs/internal/signals.md`, `billing.md`).
- Trigger: `check_and_push_signals(&[TraceAggregation], &spans)` called after the batch's ClickHouse inserts, because triggers decide from the in-memory batch delta and filters read cumulative trace state from `traces_agg` (`app-server/src/signals/mod.rs`; evaluation itself is enterprise). Trigger is a tagged enum: `rootSpanFinished` or `spanName[]`; filters on token count, status, span names, tags; modes `batch`/`realtime`. Gotcha noted: `Includes`/`NotIncludes` on arrays use pure set semantics and an empty array satisfies `NotIncludes`, so "unknown" must be resolved by the caller.
- Sampling: `metadata.sampleRate` 1..95 (a signal is enabled unless metadata says otherwise; no `enabled` column).
- Queue: RabbitMQ quorum queues. Transient failures go to a **delay queue per backoff tier** (message TTL; "every message in one retry queue MUST carry the same TTL" because RabbitMQ expires only at the head); retry budget moves to an `x-lmnr-retry-attempt` header because ack+republish is a new message; fallback to immediate requeue if publish fails; accepted duplicate window; priority bands (continuation steps higher) with the caveat that quorum priority is strict (starvation) and prefetch (128) limits its effect; in-memory `TokioMpscQueue` is the local-dev transport and refuses delayed retry (`docs/internal/mq-redis.md`).
- Failure: LLM profile/store errors are `ConfigError` (non-retryable) and write the run FAILED with a diagnosable message; transient provider errors use the retry ladder. Notification fan-out (Slack/email) is throttled per alert with a `SET NX EX` lock.
- Cost: runs metered in tokens, priced at read time (rate change re-prices history); per-workspace credit decremented atomically in Postgres on run completion; hard limits per workspace with email at soft/hard thresholds; trial/free gated.
- Versioning: append-only `signal_versions` snapshot (prompt, schema, trigger, filters, sampling, model) minted in the same transaction as the signal row (`SELECT ... FOR UPDATE`); results carry `signal_version`; 0 means "predates versioning", not backfilled. New ClickHouse columns must be appended last (RowBinary is positional).
- Secrets: LLM profiles encrypted with XChaCha20-Poly1305, `AEAD_SECRET_KEY` (32-byte hex), **AAD = profile id**, 24-byte random nonce, format `nonce_hex:cipher_hex` (`app-server/src/data_plane/crypto.rs`); decrypted clients cached keyed by `(profile_id, updated_at)` so credentials are decrypted only when the row changes; UI/API show `first3***last3` masks; secret merge/prune on update.
- Test/preview: `signal-test` / `signal-eval` endpoints take a profile+model and validate the route; Runs tab with FAILED reasons.

---

## Lessons for a single-binary Rust product with SQLite metadata DB and in-process workers

1. **Make the DB row the source of truth, the queue only a wake-up.** Langfuse's `job_executions` (status PENDING/DELAYED/COMPLETED/ERROR/CANCELLED, error, trace link) plus Phoenix's "no queue persistence, re-query incomplete work on restart" both point to a SQLite `eval_jobs` table claimed by workers. No Redis needed; recovery after crash is a startup scan.
2. **Deterministic job id = unique key.** Langfuse hash(project, rule, trace, item, observation) as PK, LangWatch `exec:tenant:trace:evaluator`. Use `INSERT ... ON CONFLICT DO NOTHING` on a UNIQUE (rule_id, target_id, scope) key. Insert row **before** wake-up; if wake-up/enqueue fails, nothing is stranded because the row is the queue. Give backfill runs their own scope component so they are not deduped against live runs.
3. **Sampling by hash of the target id, with a pinned algorithm + domain string** (Langfuse, Phoenix spec). Same decision across retries/restarts, and optional co-sampling of rules via a shared hash. Opik/LangWatch use RNG and lose reproducibility. Pin the hash and never change it (Phoenix open question #9). Apply the cheap hash gate before expensive filters.
4. **Pick a completeness rule up front.** Options seen: fixed delay (Langfuse 30 s; error text tells users to "extend the delay"), `end_time` set (Opik), root span finished (Lmnr), quiet window + re-run on new spans (LangWatch), idle timeout for threads/sessions (Opik 15 min). Observation/span-level targets sidestep it (Langfuse new path, delay 0). For a trace target we need a quiet-window debounce keyed on the trace with a max age, and an explicit "re-evaluate on new spans" policy (LangWatch does; it needs the cutoff below).
5. **Cap re-evaluation.** LangWatch incident 2026-05-27: derived events (topic assignment) re-ran 12 monitors over ~863 old traces and saturated the DB. Only genuine new span arrivals trigger; never re-run traces older than a cutoff; drop work for traces past a span-count cap but keep the data.
6. **Recursion guard must be structural, not just a name check.** LangWatch lost 90 minutes and a tenant's capacity to a self-triggering evaluator; Langfuse guards at creation, scheduling and execution; Opik relies on `source=evaluator`. Recommended: judge calls emit spans tagged with a reserved attribute / internal source, excluded from rule matching at ingest time **and** re-checked at execution. Strip user-supplied reserved attributes on ingest but make sure the guard attribute survives that strip (LangWatch's fix was silently disabled by it once). Count blocks in a metric.
7. **Persist a decision record for every outcome, not only successes.** Phoenix spec (v1-blocking), Opik per-rule user log (sampled / skipped_filter / skipped_sampling / disabled), Langfuse CANCELLED reasons, LangWatch "Excluded by sampling" outcomes. One row (or at least counter) per eligibility decision makes "why didn't my evaluator run?" answerable. Keep the log bounded (retention) and free of payload content (Langfuse stores a generic message for internal errors).
8. **Classify errors once, centrally.** Permanent: bad key (401), billing (402), model 404, invalid template/output. Transient: 429, 5xx, timeouts, network. Opik's lesson: do not classify the whole 4xx family as permanent (429/408/425 are transient). Langfuse's lesson: 403 may be a per-input content-filter rejection and must not pause the evaluator. Write ERROR terminal state only on the final attempt, or the retry will skip itself.
9. **Rate limits need long, jittered, age-capped retry.** Langfuse spreads 4 retries over 2 h with +-20 % jitter and status DELAYED; Phoenix requeues 429 indefinitely and tells a token bucket; LangWatch honours `retryAfterMs`. For an in-process worker: store `next_attempt_at` in SQLite, a per-provider token bucket / concurrency semaphore (Phoenix advisor pattern: check before dispatch so the dispatcher never blocks), global + per-provider caps, round-robin across rules for fairness.
10. **Auto-pause with a typed reason, claimed atomically, notified once.** Langfuse: pause on auth/billing/endpoint/model errors (first claimer wins via `WHERE blocked_at IS NULL`); LangWatch: pause on >=90 % errors over >=50 runs/hour, per rule revision, idempotent email. Combine: immediate pause for config-class errors, rate-based pause for the rest; pickup-time re-check cancels queued jobs; editing the definition clears recoverable reasons. Phoenix's open question 19 (silent disable discovered days later) argues for an in-UI banner plus webhook/email.
11. **Budgets and overload visibility.** Opik `maxCostUsd` (soft, per evaluation, fail-open, inert if the model has no price: warn) and Lmnr token metering priced at read time are the only cost controls seen; Langfuse has none. A single binary should add a per-rule/per-project daily token or USD cap with pause-on-exceed, record judge tokens/cost on the judge span, and price at read time so price table fixes re-price history. Opik's stream trim and Phoenix's "overload backstop" show that dropping work must be **visible** (counted + surfaced), never silent.
12. **Judge traces: same store, hidden by default.** Langfuse (internal environment) and Opik (`source=evaluator`, hidden visibility) put judge generations in the normal trace store with deterministic trace ids derived from the job id and link them from the job row; Phoenix proposes a dedicated project. Deterministic id lets the UI link before/after retries.
13. **Secrets: AEAD with AAD, versioned key, no default.** Best design seen: Lmnr XChaCha20-Poly1305 with AAD bound to the row id and cached decrypted clients keyed `(id, updated_at)`. Langfuse uses AES-GCM with a single env key and no rotation; Opik ships a hardcoded default key and a deterministic legacy mode; Phoenix's Fernet key is publicly derivable when the secret is unset; LangWatch's scaffolder persists the secret so it cannot rotate by accident. Do: require a key (generate at first start into the data dir), include a key id in the ciphertext for rotation, bind AAD to the connection id, never return secrets (mask `first3***last3`), delete-connection pauses dependent evaluators.
14. **Backfill is a bounded batch job, not a trigger.** Langfuse: filter + count preview, hard cap (50k), sampling, own scope id, runs through the same job table; sentinel "historic" jobs were a hack (use an explicit column). Rule `timeScope` (NEW vs EXISTING) prevents backfill-only rules from firing live. Opik and LangWatch ship without backfill and users ask for it (Phoenix spec: "something is wrong" if existing traces cannot be evaluated).
15. **Ship test/preview early.** Langfuse test-on-sample-observation (no score persisted), Opik sample-data preview in the rule editor, Lmnr `signal-test`, LangWatch re-run on one stored trace, Phoenix inline evaluator preview. It is also the main way to catch bad variable mappings before they consume budget (Langfuse validates mappings for every evaluator before a batch run).
16. **Version evaluator definitions and stamp results.** Lmnr `signal_versions` + `signal_version` on every run; Langfuse evaluator versions with "current version only" semantics. Results should carry evaluator version, rule id and judge trace id so changes are auditable; run rows older than versioning get 0/NULL rather than a fake backfill.
17. **Process-level pitfalls to avoid.** In-memory event bus loses events on crash and has no retry (Opik annotation routing comment); at-least-once queues need idempotent score writes (Langfuse derived score ids from job id; LangWatch cost key). Heartbeat claims need a stale timeout, orphan scan at startup, and an unbounded-retry ban (LangWatch "blocked a queue group per trace"). Redis-stream claim cursors, RabbitMQ per-message TTL ordering and priority starvation are broker-specific traps we avoid by using SQLite plus a single process, but multi-process (e.g. CLI + server on the same DB) would need the heartbeat/claim pattern from Phoenix.
