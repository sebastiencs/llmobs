# G fixes — plan

This plan fixes the major findings of the G review ([`docs/reviews/g-spine.md`](../../reviews/g-spine.md), PR #58): M-1, M-2, M-3 and M-4. It also fixes the minor findings that touch the same code: m-1, m-2, m-4, m-5 (docs only), m-8, m-10 and m-12 (docs only). m-9 needs no change: `vite build` emits no `.map` file (`build.sourcemap` is off by default; `web/dist` has none). The other minors (m-3, m-6, m-7, m-11) are a backlog.

The work is split into 5 phases, F1–F5. One agent implements one phase. Each phase:
- lists the files it may edit;
- gives the exact behaviour, the tests and the doc changes;
- ends with `just check` green and one commit.

**Do only the phase you were given.** If something in the phase file does not match the code, or a step fails in a way the file does not cover, stop and report: do not improvise a different design.

**Status:** decisions settled with the user (2026-11-14). Nothing is implemented.

**Read first** (every phase):
- `AGENTS.md`;
- this README;
- the review findings your phase fixes (`docs/reviews/g-spine.md`; if PR #58 is not merged yet, read it with `git show origin/claude/competent-tharp-04cf4f:docs/reviews/g-spine.md`);
- your phase file.

| Phase | File | Fixes | Commit subject |
|---|---|---|---|
| F1 | [01-ui-fallback.md](01-ui-fallback.md) | M-1, m-8 | `Serve the app for dotted deep links and tighten the UI headers` |
| F2 | [02-stream-lifetime.md](02-stream-lifetime.md) | M-2, m-1, m-4, m-5, m-12 | `End live streams after 15 minutes or a stalled client` |
| F3 | [03-search-cap.md](03-search-cap.md) | m-2 | `Cap search text at 1 KiB` |
| F4 | [04-commit-task.md](04-commit-task.md) | M-4 | `Finish an ingest commit even when the client goes away` |
| F5 | [05-e2e-coverage.md](05-e2e-coverage.md) | M-3, m-10 | `Run the embedded UI, live streams and every read route end to end` |

## Order

```
F1 ─▶ F5        (F5 asserts F1's dotted deep link)
F2 ─┐
    ├─▶ F3      (F3 edits routes/streams.rs after F2, and adds D-G-39 after F4's D-G-38)
F4 ─┘
```

- F1, F2 and F4 may run in parallel, in separate worktrees. Their source files do not overlap.
- F3 runs after F2 and F4. F5 runs after F1.
- Shared docs, each phase editing only its own rows or paragraphs:
  - `docs/mvp-decisions.md`, section `## Track G — Integration spine`: F1 amends D-G-35, F2 amends D-G-34, F3 amends D-G-22 and adds D-G-39, F4 adds D-G-38, F5 amends D-G-18. New rows go in ID order after D-G-37: F4 adds D-G-38, then F3 adds D-G-39 after it.
  - `docs/spec/09-api.md` § SSE and the `/traces/stream` row of the routes table: F2 only.
  - `docs/spec/12-operations.md` (CSP line): F1 only. `docs/spec/11-ui.md`: F1 only. `docs/spec/05-query.md` § Full-text search: F3 only.

## Decisions

The user chose (2026-11-14):

| # | Topic | Choice | Phase |
|---|---|---|---|
| M-2 | Streams and revoked credentials | A fixed maximum stream age of 15 minutes for both streams. The web client already reconnects when a stream ends, and the reconnect authenticates again. No per-ping access check. | F2 |
| M-3 | End-to-end coverage | Extend the Rust e2e test and run it with the UI embedded in the CI `e2e` job. No headless browser. | F5 |
| m-5 | Score writes and live streams | Document that a score write does not reach open streams. No new event. | F2 |
| m-8 | CSP | Add `base-uri 'none'` and `form-action 'self'` to the CSP, in spec 12 and in the code together. | F1 |

New and amended decision rows, with their exact text, are in the phase files:

| # | Topic | Phase |
|---|---|---|
| D-G-18 | Embedded UI: the CI `e2e` job runs the UI embedded; the release workflow stays track N's | F5 (amend) |
| D-G-22 | Stream validation: only the filter can be invalid | F3 (amend) |
| D-G-34 | Stream slots and failures: 15-minute maximum age, 30 s send timeout | F2 (amend) |
| D-G-35 | UI embedding: which missing paths are files | F1 (amend) |
| D-G-38 | Ingest commits run in their own task | F4 |
| D-G-39 | Search text is at most 1,024 bytes | F3 |

## Conventions for every phase

- **Scope.** Edit only the files the phase lists. If you need another file, stop and report.
- **Errors.** Use the existing `ApiError`, `IngestError` and `RunError` types; add no variant unless the phase says so. Messages and logs never include payload or search text. `unwrap`/`expect` only in tests, or with a comment stating the invariant.
- **Tests.**
  - Every behaviour in the phase has a test that fails without the change. The phase lists them; write all of them.
  - Use the existing helpers the phase names (`TestApp`, `seed`, `send_stream`, `TestStream`, `commit_spans`, `dataset_span`, …).
  - Existing tests must keep passing unchanged unless the phase names the test and the new expectation.
- **Docs.** Spec and decision text is given verbatim. Write it exactly; adjust only surrounding punctuation if the context requires it.
- **Checks.**
  - Run `just fmt` before `just check` (it sorts imports; `just check` runs the format check).
  - `just check` must pass before the commit. Iterations may use `cargo test -p <crate> <filter>`.
  - Node is not on the non-interactive `PATH` on the dev machine: run `export PATH="$HOME/.local/share/nvm/v26.13.0/bin:$PATH"` in the same command as any `just` recipe that runs web or TypeScript work (`just check`, `just e2e`, `js-install`).
- **Commit.** One commit per phase, with the subject from the table above. No `Co-Authored-By` or other attribution trailer.
- **Report.** At the end, report:
  - what was done;
  - every deviation from the phase file and why;
  - anything left unverified;
  - the exact failing output of any "Done when" command that failed and was then fixed.

## After the plan

When all five phases have landed, the coordinating session adds a "Fixed by" line under each fixed finding in `docs/reviews/g-spine.md`, notes m-9 as needing no change, and updates the G row of `docs/roadmap.md`. No phase does that.
