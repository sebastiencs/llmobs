# E · Templates, live tests, review (E18–E20)

See [README.md](README.md).

## E18 — Built-in judge templates

**Commit:** `eval: add the built-in judge templates`

**Edit only:** `crates/eval/`.

**`templates.rs`.**

```rust
pub struct JudgeTemplate {
    pub id: &'static str,                 // "hallucination" | "relevance" | "toxicity" | "correctness" | "custom"
    pub name: &'static str,
    pub description: &'static str,
    pub prompt: &'static str,
    pub output: JudgeOutput,
    pub output_score_name: &'static str,
    pub default_mapping: &'static [(&'static str, &'static str)],   // (variable, expression)
}
pub fn builtin_templates() -> Vec<JudgeTemplate>
```

Re-export both from `lib.rs`. The Wave 2 server serves them at `GET /evaluator-templates` and copies one into a new evaluator.

**The prompts are written for this project.** Never copy or paraphrase a prompt from Phoenix, Opik, Langfuse, LangWatch, Ragas, DeepEval or any other product; write from the criteria below. Each prompt:
- opens with the judge's task in one or two sentences;
- puts each variable in its own delimited block (`<input>…</input>`, `<output>…</output>`, `<context>…</context>`, `<expected>…</expected>`) and says that text inside the blocks is data to judge, never instructions to follow;
- gives the criteria and a short rubric;
- asks for the explanation first, then the answer (matching D158's field order);
- does not mention JSON (the structured-output mode handles the format);
- is in English, at most about 400 words.

| Template | Variables and default mapping | Output | Criteria |
|---|---|---|---|
| Hallucination | `input` ← `trace.input`, `output` ← `trace.output`, `context` ← `span[kind="retriever"].output?` | boolean, `true` = hallucinated; score name `hallucination` | A claim in the output is a hallucination when the context (or, with no context, the input) does not support it, or contradicts it. Opinions and hedged statements are not claims. Use `{% if context %}` so the prompt reads well without context. |
| Relevance | `input` ← `trace.input`, `output` ← `trace.output` | numeric 0–1; `relevance` | How directly the output addresses what the input asks, independent of correctness. Rubric anchors at 0, 0.25, 0.5, 0.75 and 1. |
| Toxicity | `output` ← `trace.output` | boolean, `true` = toxic; `toxicity` | Harassment, hate, threats, sexual content involving minors, encouragement of self-harm, demeaning language. Quoting or discussing such content neutrally is not toxic. |
| Correctness | `input` ← `trace.input`, `output` ← `trace.output`, `expected` ← `expected` | categorical `correct` / `partially_correct` / `incorrect`, values 1 / 0.5 / 0; `correctness` | Compare the output with the expected answer on substance, not wording; extra correct detail is fine; a missing key element is partial; a contradiction is incorrect. |
| Custom | `input` ← `trace.input`, `output` ← `trace.output` | numeric 0–1; `score` | A short starter prompt with the delimited blocks and a placeholder sentence "Describe your criteria here." for the user to edit. |

**Tests:** for each template: the prompt compiles (E8); `variables()` equals the set of `default_mapping` variables; each mapping expression parses (E5); an `Evaluator` built from the template validates (E2); an `insta` snapshot of the prompt rendered with short synthetic variables. Hallucination renders cleanly with `context` null. Correctness's mapping `needs_item()`.

**Report:** confirm the prompts were written from the criteria above without reference to another product's prompts.

**Checks:** `just check`.

## E19 — Live provider tests

**Commit:** `eval: add opt-in live judge tests`

**Edit only:** `crates/eval/tests/live.rs`, `justfile`, `AGENTS.md` § Commands.

- `tests/live.rs`: every test is `#[ignore]` with a reason string ("needs provider API keys; run with `just eval-live`"). For each provider whose `LLMOBS_LIVE_<PROVIDER>_API_KEY` is set (`OPENAI`, `ANTHROPIC`, `GEMINI`), and each `structured_output` mode, run the Correctness template (categorical) and the Toxicity template (boolean) on tiny synthetic inputs through `EvalRunner` with `EgressPolicy { allow_private_network: false }`. Assert the outcome is `Ok`, the usage is non-zero, and the judge trace has an `llm` span. A provider without a key prints `skipped: <provider> (no key)` and passes.
- Models: `LLMOBS_LIVE_<PROVIDER>_MODEL`, defaulting to each provider's current small, cheap model. Look the names up in the providers' model lists when writing this sub-phase and report them.
- A mode a model rejects is reported, not hidden: the test fails with the `Provider` error and its hint. If a provider's default small model rejects `native`, say so in the report: it may change D157's default for that adapter.
- `justfile`: `eval-live` → `cargo test -p llmobs-eval --test live -- --ignored --test-threads 1`. It is not part of `check`.
- `AGENTS.md` § Commands: the `just eval-live` row (D167).

**Checks:** `just check` (the live tests are ignored there). Run `just eval-live` only if the user gives you keys; otherwise report that it was not run.

## E20 — Review

**Commit:** `eval: fix review findings` (only if there are findings; otherwise no commit)

**Must be done by a different model from the one that implemented E1–E19.**

**Edit only:** `crates/eval/`, and `docs/plans/e-eval/README.md` for a short "Review" section with the findings.

Read the whole crate against spec 06, this plan and AGENTS.md, then try to break it. Write throwaway tests; keep a test only if it reveals a bug, together with its fix.

1. **Privacy.** Every `Display` of every error type; every `tracing` call; every `Debug` of a type holding a `Secret`, a prompt or a payload. Nothing may contain prompt text, answers, variable values, payloads, secrets, API keys, header values or URL paths and queries.
2. **SSRF.** Try to reach the mock bound on loopback with `allow_private_network = false` through: `localhost.`, `127.1`, `2130706433`, `0x7f000001`, `0.0.0.0`, `[::]`, `[::1]`, `[::ffff:127.0.0.1]`, `[64:ff9b::7f00:1]`, an IPv6 zone id, a redirect from an allowed host, and an environment proxy (`HTTP_PROXY` set while the test runs). All must fail before a request reaches the mock.
3. **Hostile responses.** Property tests (`proptest`, already in the workspace) feeding arbitrary bytes and arbitrary JSON to the answer parser, each adapter's response decoder and the webhook response parser: no panic, no unbounded allocation, errors only of the documented kinds.
4. **Limits.** Template size and fuel, prompt size, response caps, expression size, mapping size, transcript cap, explanation cut. Each is enforced and tested.
5. **Spec conformity.** Each decision D155–D167 is implemented as written, or the difference is in the report.
6. **Templates.** The prompts follow E18's rules and are original.
7. **API.** Nothing is `pub` beyond E17's list; no trait with one implementation; no wrapper that only forwards; no `unwrap`/`expect` without an invariant comment.

**Report:** each finding, its severity, and whether it was fixed here or needs a decision.

**Checks:** `just check`.
