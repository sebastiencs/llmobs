# Evaluation research: LLM judges, checks, mapping, judge traces

Notes for track E (`docs/plans/e-eval/`). They compare how Phoenix, Opik, Langfuse and LangWatch build LLM-as-judge evaluators. The checkouts in `/home/sebastien/github/` were read at their HEADs of 2026-11-06 to 2026-11-09. These are design notes only: no code or prompt was copied, and none may be.

Abbreviations: **P** Phoenix, **O** Opik, **LF** Langfuse, **LW** LangWatch.

## G1 · Forcing structured output

| | OpenAI | Anthropic | Gemini | No support |
|---|---|---|---|---|
| P server | forced named function | forced tool, except models that reject it | function calling `ANY` | error, no text parsing |
| P library | strict `json_schema`, falls back to a tool on a 400 and caches the method | native `output_config.format`, then a tool | `response_schema` | — |
| O | non-strict JSON schema | prompt instruction ("only a JSON object") | JSON schema | prompt instruction; Ollama and custom endpoints too |
| LF | strict `json_schema` (AI SDK) | `structuredOutputMode` passthrough | AI SDK | forced "json" tool, or an instruction |
| LW | forced function | forced function, retried with `auto` when refused | forced function | — |

- **Newer Claude models reject a forced `tool_choice`** (P keeps a hand-written allowlist of models that still accept it; LW retries with `auto`). Spec 06's "Anthropic forced tool call" would break on them. → D157: native schema per provider, plus a per-connection mode.
- Automatic downgrades need state (P caches the method per model, LW per process) and must never trigger on a transient error. → D157 picks an explicit mode instead.
- OpenAI strict mode needs `additionalProperties: false` and every property required. Anthropic's native output rejects number constraints. → D158: the schema carries no `minimum`/`maximum`; ranges are checked after parsing.

## G2 · Output schema and parsing

- Every product puts the explanation **before** the answer. LF found that thinking models treat a field named `reasoning` as a scratchpad and drop the score, so the field the model sees is not called `reasoning`. → D158: `explanation`, then `value` or `label`.
- Real model output that broke parsers: JSON inside markdown fences, two JSON objects back to back (reasoning models), quoted numbers (`"0.8"` stored as 0 in an old O bug), `reason` returned as an array, a renamed score key, an empty answer after a content filter or an exhausted reasoning budget. → D158's tolerant parser.
- Unknown categorical labels: the P server maps them to no score; the P library and LF raise. → D158 raises `UnknownLabel`, after normalizing case and whitespace.
- Errors are typed in O (`NO_ANSWER`, `NOT_JSON`, `NOT_A_JSON_OBJECT`, `NO_SCORE_FIELDS`) and shown in the rule's log. → `JudgeError` variants in E9.

## G3 · Retries, timeouts, rate limits

- O retries 408, 425, 429, 5xx and transport errors in process (3 times), then the queue redelivers; parse failures are not retried. LF retries once in the call, then re-queues over two hours. LW retries everything except 400, including parse failures.
- LF **pauses the evaluator** on 401, 402, 404 and `insufficient_quota`, never on a content-filter 403.
- Timeouts: O 60 s, LF 120 s. O caps judge `max_tokens` so a judge finishes inside the timeout: a timed-out call is retried and billed again.
- → D160: E makes one call and classifies the error (retryable, permanent, pause); spec 06's queue owns retries. Default timeout 60 s, connect 10 s.

## G4 · Tracing judge calls

- O and LF write the judge trace into the user's project (O hides it with a visibility flag, LF with a reserved environment prefix). P creates a hidden project per evaluator. LW records no judge spans.
- LF derives the judge trace id from the job execution id, so a retry reuses it.
- Recursion guards: LF never evaluates environments starting with its prefix; O samples only SDK and experiment sources; LW has a causality-depth attribute.
- → Spec 06 already matches LF (reserved `llmobs-eval` environment). D161: the caller supplies the trace id, and E derives span ids from it.

## G5 · Variable mapping and templates

- P: JSONPath; no match raises; one match is a scalar, several are a list. Mustache (strict) or f-strings.
- O: prefix plus JSONPath (recursive descent and filters rejected); unresolved variables are silently dropped. Mustache or Jinja2. `{{spans}}`-style variables inject a trace tree, truncated with a hint.
- LF: a regex `{{var}}` replace that **leaves `{{var}}` in the prompt** when the key is missing. Its transcript renderer caps each block and the total.
- → D162/D163: missing variables are an error unless the expression is marked optional (`?`); JSONPath with zero matches is "absent", one is the value, several are an array.

## G6 · Deterministic checks

- P: `contains` (case-insensitive by default, `require_all`), `exact_match` (case-sensitive, **no trim**), `regex` (search or full match), Levenshtein distance, JSON distance.
- LW: `exact_match` that trims, strips punctuation and lowercases by default; contains/regex rule sets; JSON validity with an optional schema.
- O SDK: equals (case-insensitive by default), contains, regex, is-JSON, Levenshtein ratio.
- LF: none built in.
- → D164 pins our defaults.

## G7 · Webhook evaluators

None of the four has an HTTP webhook evaluator: they run user code in a sandbox. LF signs its automation webhooks with `t=…,v1=…` HMAC-SHA256, the same shape spec 06 chose. → D165.

## G8 · Judge cost

All four price judge usage with their normal price tables. O warns when a judge model has no price, because a budget then silently stops working. LW sums usage over retries. → D161: the judge's `llm` span goes through B1's normalizer and its price table, and the outcome flags an unpriced judge model.

## Pitfalls to remember

1. A forced `tool_choice` is refused by newer Claude models and by Claude with extended thinking.
2. Reasoning models reject `temperature ≠ 1` (spec 06's example sent 0 to `gpt-5-mini`). → D159: temperature is optional and unset by default.
3. Anthropic requires `max_tokens`.
4. A content-filter refusal must not pause an evaluator.
5. An out-of-range value can fail a whole batch insert: validate every value against the output definition before it becomes a score.
