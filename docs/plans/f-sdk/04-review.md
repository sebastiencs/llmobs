# F · Closing review (X1)

## X1 — Cross-language review

**Commit:** `sdk: fix findings of the cross-language review` (only if something changes; otherwise no commit, and say so in the report).

**Runs after** P12 and T13 are both merged into one branch. **Edit only:** `sdks/python/`, `sdks/typescript/`, `docs/spec/10-sdk.md` (wording only), `docs/plans/f-sdk/` (to record deviations).

Review both SDKs side by side, reading code, not reports. For each item, record pass or the fix in the report:

1. **Conformance.** `just check-py` and `just check-ts` pass, and both conformance suites cover all 11 cases. Run `cargo test -p llmobs-otel --test golden` too.
2. **Shared contract.** For every row of README § Span attributes, § Serialization, § LLM span, § Payload keys and § Score request, find the implementing line in each SDK and check they agree. List any difference in behaviour that the fixtures do not cover (e.g. a serialization rule, a validation message, a retry delay) and fix the side that disagrees with the README.
3. **Never raise after `init()`.** Grep each SDK for every public entry point and check that internal failures are caught: span start/end, attribute setting, serialization, mask, filter, processor `on_start`/`on_end`, exporter, score queue, instrumentor enable/disable. User exceptions are re-raised unchanged.
4. **Privacy.** Every log call in both SDKs: none includes a payload, an attribute value, an API key, a user id, a session id, metadata, or an exception message from user code. Error messages of `ConfigError` contain option names only (instrumentor names allowed, F1).
5. **No-op paths.** Before `init()`, with `enabled=false`, with no API key, and after `shutdown()`: every public function returns quickly, does not create spans, does not start threads/timers, and does not open sockets. Write a test for any path that lacks one.
6. **Spec 10.** Read `docs/spec/10-sdk.md` end to end against the code; fix wording where the spec is unclear, never behaviour, and list each change.
7. **Packaging.** Python: `uv build` produces a wheel whose metadata lists the dependencies and the 11 extras; `pip install` of the wheel into a fresh venv with `uv venv` + `uv pip install dist/*.whl` and `python -c "import llmobs; llmobs.init(enabled=False)"` works. TypeScript: `pnpm --dir sdks/typescript pack`, then `just ts-smoke <tarball>`; `attw` reports no problem for any entry point.

**Checks:** `just check`.

**Report:** the table of the 7 items with pass/fixed, every fix with its file, and any open question for the user.

## X1 resolution notes (2026-11-13)

Fixes made during the review, all inside `sdks/`:

- **Serialization rule 7 (both SDKs).** A top-level value whose own conversion fails now yields the rule-7 tuple `("<unserializable TYPE>", "text")` in both SDKs (Python used to return the marker as a JSON string with format `json`), and a nested conversion failure is replaced in place by the `"<unserializable CHILD_TYPE>"` marker in both (TypeScript used to drop the whole payload to the rule-7 tuple). Spec 10 § Serialization rule 7 records the interpretation.
- **`BaseException` through `observe` (Python).** `KeyboardInterrupt`, `asyncio.CancelledError` and friends now end the span with the status unset (no error event, no output) and are re-raised — sync, async and both generator wrappers — matching `span()`/`llm_span()`. The Python README's known limits were updated; TypeScript has no equivalent class of exceptions (every throw goes through § Errors), verified.
- **Generator call that cannot bind (Python).** Python checks arguments at call time even for generator functions, so a call with unbindable arguments used to raise with no span at all while the sync wrapper recorded one; the generator wrappers now record the same one-span error (`{"args": …, "kwargs": …}` input, error status). TypeScript never fails at call time; its body error at the first step was already recorded, now pinned by a test.
- **`reduce_output` with no items.** Both SDKs already recorded no output and skipped the reducer; each suite now pins it.
- **TS score `reason`.** A mask returning a non-string for `reason` is serialized (`serialize(...)[0]`), as Python already did.
- **TS `withLlmSpan` runner.** `llm.ts` now goes through `span.ts`'s exported runner (`tracedSpan`) and `stampMetadata` instead of a private copy with different thenable handling.
- **TS README test.** The score example's cleanup flush posts to a stubbed `fetch` instead of retrying real refused connections to 127.0.0.1:9; the byte-identical README check is unchanged.
- **TS `.gitignore`.** `sdks/typescript/.vitest/` (Vitest's JSON reporter output) is ignored.

Accepted differences left as is: TS `setUsage` accepts only safe integers (Python writes any non-negative int; JS numbers cannot hold more), Python's `Retry-After` parsing accepts `"+5"` via `int()` (TS's digit regex does not).
