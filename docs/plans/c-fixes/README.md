Status: CF1–CF11 done on 2026-11-13

# C · Fixes for the review's major findings

This plan fixes the 14 **major** findings of the track C review (`docs/reviews/c-web-ui.md`, M1–M14). The review's minor findings are out of scope.

The plan has **11 phases, CF1–CF11**:
- One agent does each phase, in order.
- Each phase ends with its checks green and **exactly one commit**.
- Do the phase you were given and nothing else.
- If something in your phase does not match what you find in the code (a line moved, a name differs, a test already exists), stop and report. Do not improvise.

**Read first, for every phase:**
- `AGENTS.md`;
- this README;
- your phase's section;
- the review's entry for your finding (`docs/reviews/c-web-ui.md` § Confirmed findings);
- `docs/plans/c-web-ui/README.md` § Shared rules for every sub-phase. These still apply: API calls go through `$api`/`api`, tests use `renderApp` and `server.use`, no `vi.mock` of modules, no `console.*`.

## Environment

- Node is not on `PATH` in non-interactive shells. Run `export PATH=$HOME/.local/share/nvm/v26.13.0/bin:$PATH` first.
- Install the JS dependencies with `pnpm install --frozen-lockfile` from the repository root. Never add or upgrade a dependency in this plan; no phase needs one.
- Run a single web test file with `pnpm --dir web exec vitest run <path relative to web/>`.
- The route tree is generated: `pnpm --dir web gen:routes`. `pnpm --dir web test` runs it for you.

## Phases

| Phase | Finding | What | File |
|---|---|---|---|
| CF1 | M14 | Test reliability under load: timeouts, unhandled requests fail tests, large datasets built outside the timed test | [01-test-reliability.md](01-test-reliability.md) |
| CF2 | M2, M3, M4 | Filter text never changes meaning: top-level `OR`, `BETWEEN`, click-to-filter | [02-filters-and-views.md](02-filters-and-views.md) |
| CF3 | M5 | Default saved view only when a list page is entered | [02-filters-and-views.md](02-filters-and-views.md) |
| CF4 | M6, M7 | Data table: header scrolls with the body; `j`/`k` start from the open trace | [03-data-table.md](03-data-table.md) |
| CF5 | M1 | External media is never loaded without a click | [04-trace-view.md](04-trace-view.md) |
| CF6 | M11 | Transcript waits for the previous LLM call before trimming a turn | [04-trace-view.md](04-trace-view.md) |
| CF7 | M12, M13 | Tests for tree keyboard, wheel, drag, needle, fit, Shift+Enter, and split ratios | [04-trace-view.md](04-trace-view.md) |
| CF8 | M8 | Dashboard charts: one tab stop each, arrow-key navigation | [05-dashboard.md](05-dashboard.md) |
| CF9 | M10 | Live mode caps: paused buffer and first page | [06-live.md](06-live.md) |
| CF10 | M9 (contract) | `search` on `GET /traces/stream`: contract, spec, decision | [06-live.md](06-live.md) |
| CF11 | M9 (UI) | The live traces list sends `search`; roadmap row | [06-live.md](06-live.md) |

CF1 goes first, because the later phases add tests and need a reliable suite. The other phases do not depend on each other's code, except CF11, which needs CF10's generated `schema.gen.ts`.

## Decisions

These were settled with the user on 2026-11-13. Each phase adds the decisions it implements. They go in a new section, `## Track C — Web UI` in `docs/mvp-decisions.md`, placed after `## Track A1 — Storage engine` and before `## Open`. The phase that adds the first decision (CF2) creates the section, using this exact header and intro:

```markdown
## Track C — Web UI

Decisions from the review of track C (`docs/reviews/c-web-ui.md`) and its fixes (`docs/plans/c-fixes/`).

| # | Topic | Decision |
|---|---|---|
```

| # | Topic | Decision (text to add; the phase gives the exact row) | Phase |
|---|---|---|---|
| D-C-1 | OR in the filter bar | A filter with a top-level `OR` is one raw chip. `BETWEEN … AND …` is never split. Click-to-filter parenthesizes a filter with a top-level `OR`. | CF2 |
| D-C-2 | Default saved view | Applies only when a list page is entered with an empty search: on mount, or from a sidebar, ⌘K or `g` navigation. Never after an in-page change. | CF3 |
| D-C-3 | Chart keyboard | Each dashboard chart is one tab stop. ←/→ move between buckets, Home/End jump, Enter/Space opens the bucket's traces. Points are mouse-only. | CF8 |
| D-C-4 | Live caps | The paused buffer keeps the newest 500 (pill "500+ new traces"). Revealing an overflowed buffer, or a live insert that would grow page 1 past 500 rows, refetches page 1. | CF9 |
| D-C-5 | Live search | `GET /traces/stream` takes `search`. A trace is sent once any of its spans matches, then on every update while the stream stays open. | CF10 |

## Ownership

- CF1–CF9 and CF11 edit only `web/` and docs: `docs/spec/11-ui.md`, `docs/mvp-decisions.md`, `docs/roadmap.md`, and this plan's status line.
- CF10 edits the API contract in `crates/server/src/api/traces.rs`, plus the generated `crates/server/openapi.json` and `web/src/api/schema.gen.ts`, and specs 05 and 09. The user approved this contract change on 2026-11-13.
  - The stream handler itself does not exist yet. Track G owns SSE and implements the matching.
  - CF10 must not add a handler or any server logic.
- `fixtures/api/` is not edited by any phase.

## Checks and commits

- The code snippets in this plan are not formatted. Run `just fmt` before the checks: Biome's line width is 100.
- Web-only phases: `just check-web` green.
- CF10 changes Rust: `just check` green.
- Then `just assert-clean` must show only your intended changes before you commit. Commit with the subject your phase gives, and no attribution lines.
- Before committing, update the `Status:` line at the top of this README, in the same commit. Write `Status: CF1–CFn done on <date>`, with n your phase number.
- Report:
  - what you did;
  - any deviation, and why;
  - anything you could not verify;
  - for CF1, the timings it asks for.
