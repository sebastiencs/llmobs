# Review: track G (spine), milestones M2, M4 and M5

Reviewed on `main` at `005dcc6` (2026-11-14). Baseline: `docs/plans/g-spine/00-design.md`, `02-m2-read-api.md`, `04-m4-live-updates.md`, `05-m5-ui-and-e2e.md`; decisions `D-G-1`…`D-G-37`; specs 02, 05, 09, 11, 12. M3 (score writes, gRPC, usage) was reviewed per phase and is out of scope except where it meets the others.

Code read:
- `crates/server/src/{routes/{read,traces,sessions,users,metrics,media,streams}.rs, live.rs, serve.rs, usage.rs, http/{mod,csrf,error}.rs, ingest/mod.rs}`;
- `crates/storage/src/reads/traces.rs` (`trace_refresh`) and `filter/scope.rs`;
- `crates/ui/{build.rs,src/lib.rs}`, `crates/cli/tests/e2e.rs`, `tools/e2e/run.sh`, the SDK e2e tests, the `justfile` and the CI `e2e` job.

Method: one cold read by one reviewer, checked against the plans and specs. Nothing was run: findings are from reading the code, and each says so when it rests on an assumption. No code was changed.

Known items from #55's description (per-query 30 s timeout, metrics cache byte size, media blobs read whole, cancelled-flush window, the flaky `an_empty_database_logs_the_setup_url_once`, gRPC PAT and not-found test gaps) are **not repeated** unless this review found a new aspect of them.

## Summary

M2, M4 and M5 match their plans closely. The read handlers are thin and uniform, they all sit behind `Access::Project { View }`, error mapping follows D94 and D-G-23, and no handler logs filter, search or payload text. The live bus is bounded (64 events × 500 summaries, nothing built with no subscriber), slots are released on drop, and the trace stream's re-read/retry/resync logic is careful. The UI crate never touches the file system (lookup in a static table), so there is no path traversal.

The weak points are at the seams, and they are the kind a per-commit review does not see:

- **The UI fallback 404s any deep link whose last path segment contains a dot** (M-1). User ids are often emails, so a refresh on `/…/users/alice%40example.com` is a 404. The same family as the live-list bug found by the manual smoke test.
- **A stream outlives its credential, and the trace stream never ends** (M-2).
- **No CI job runs the real UI against the real server** (M-3). The Rust e2e test only runs its "UI embedded" branch if `web/dist` happens to exist; `check-release` runs nowhere. The manual smoke test found a bug because nothing automatic could.
- **`commit` is not cancel-safe**: a client that disconnects while the append is in flight leaves spans stored but never published to the live bus nor counted in usage (M-4).

Counts: **0 blocker, 4 major, 12 minor**.

## Status per milestone

| Milestone | Plan | Status | Note |
|---|---|---|---|
| M2 read API (G2.1–G2.8) | `02-m2-read-api.md` | Matches the plan | All 12 read routes exist with `View`, 422/503/416 mapping per D-G-23, ranges per D-G-24. Gaps: m-3, m-6, m-7 |
| M4 live updates (G4.1–G4.n) | `04-m4-live-updates.md` | Matches the plan | Bus, slots, trace and span streams, shutdown. Gaps: M-2, M-4, m-1, m-4, m-5 |
| M5 UI and e2e (G5.1–G5.6) | `05-m5-ui-and-e2e.md` | Matches the plan; G5.3 and G5.6 as checked by the coordinator | Gaps: M-1, M-3, m-8, m-9 |

## Major

**M-1. The UI fallback returns 404 for deep links whose last segment contains a dot.**
- Location: `crates/ui/src/lib.rs:69-73`; the route list in `web/src/routes/$org/$project/{users/$userId,sessions/$sessionId}.tsx`.
- Reference: D-G-35 ("paths with an extension are `404`"), spec 11 (client routes).
- Problem: a path that matches no asset is treated as a file when `last_segment.contains('.')`. The server sees the raw (percent-encoded) path, so `/acme/proj/users/alice%40example.com` has the segment `alice%40example.com` and gets a bare 404 instead of `index.html`. Reload, a pasted link, or opening in a new tab breaks for every end user whose id is an email, and for session ids such as `chat.2026-01`. In-app navigation still works, so tests that click through do not see it. Not run; follows from the code and from the test list (`/missing.png` is the only dotted case).
- Fix: treat a path as a file only when it is under `/assets/` or its last segment ends in an extension that is a known asset extension (`.js .css .map .svg .png .ico .woff .woff2 .json .txt .webmanifest`), or send `Accept: text/html` navigations (`Sec-Fetch-Mode: navigate` / `Accept` containing `text/html`) to `index.html` regardless of dots. Add a test with `/a/b/users/alice%40example.com` and `/sessions/s.1`.

**M-2. A stream outlives the credential that opened it, and the trace stream has no end.**
- Location: `crates/server/src/routes/streams.rs:253-329` (trace stream loop, no idle or age limit), `:121-173` (span stream, idle only), `:466-471`.
- Reference: D-G-34 and the M4 plan accept "access is not re-checked while a stream is open"; spec 09 § SSE says only that the span stream ends after 10 min without a span.
- Problem: after logout, session expiry, key revocation, a role downgrade or removal from the project, an already open trace stream keeps receiving live trace summaries (with input/output previews) for as long as the TCP connection lives, which can be days. The span stream lasts as long as the trace keeps getting spans. The 100-slot cap is per owner, so a revoked user's old streams also keep occupying slots. Accepting this "for M4" was reasonable while sessions were a stub; D and L have since shipped real revocation, so the accepted gap now defeats it.
- Fix: give every stream a maximum age (for example 15 min; the web client already reconnects and refetches, and its reconnect re-authenticates), and end the stream when the principal's session expiry passes. Record the age in D-G-34 and spec 09. A cheaper stronger option: carry the session id or key id into the task and re-read the web session row once per ping (every 15 s).

**M-3. No automatic check runs the embedded UI on the real server, nor the live path end to end.**
- Location: `crates/cli/tests/e2e.rs:175-183`, `justfile:47-49`, `.github/workflows/ci.yml` (`e2e` job runs `just e2e` only), `tools/e2e/run.sh`.
- Reference: M5 plan "What M5 is" ("the UI on the real API"), D-G-20, D-G-37.
- Problem:
  - In CI the Rust e2e test runs in `rust-test`, which has no `web/dist`; it takes the `503` branch (`e2e.rs:182`). The 200 branch, the SPA fallback and the gzip/ETag path on the real binary are never exercised. `check-release` is in no CI job (D-G-37 hands it to track N), so "ui: embedded" is also never checked.
  - The e2e test reads only traces and spans. Sessions, users, scores, metrics, schema, media and both SSE streams have no test over real HTTP, so nothing covers ingest → live bus → SSE → client, which is exactly where the manual smoke test found its bug.
  - The two SDK e2e tests (`sdks/*/…e2e…`) poll the list and detail only.
- Fix:
  - Add `just check-release` to the `e2e` CI job (it already builds with Node), and run the Rust e2e test there with the UI embedded so the `200` branch runs.
  - Extend `e2e.rs`: open `/traces/stream` and a span stream with `reqwest` bytes streaming, ingest a span over OTLP, and assert the `trace`/`span` event (and `resync` on `Last-Event-ID`); add one read each of sessions, users, scores, metrics, schema and media; a deep link with a dot (M-1).
  - Optionally one headless-browser smoke step for the live list (out-of-order arrival).

**M-4. `commit` is not cancel-safe: stored spans can be neither published nor counted.**
- Location: `crates/server/src/ingest/mod.rs:312-325` (`append_spans(..).await`, then `live.publish`, then `usage.add`); callers `routes/otlp.rs:87`, `ingest/grpc.rs:125` (not detached).
- Reference: D-G-2 (acknowledge after the WAL), D-G-11/D-G-30 (counters add after each acknowledgement), D-G-12 (publish after `append_spans` returns).
- Problem: hyper drops the handler future when the client disconnects (exporter timeout, load balancer reset). If that happens while `append_spans` is awaiting, the append may still complete inside storage, but `publish` and `usage.add` never run. Result: spans that exist but do not appear in open live lists until the trace gets another span, and usage that is under-counted exactly when the server is slow. No await sits between `publish` and `usage.add`, so only the append window matters. Whether storage's append itself is cancel-safe is the "cancelled-flush window" already known; this is its server-side consequence. Not run.
- Fix: run `append_spans` + `publish` + `usage.add` in a detached `tokio::spawn` (own the `Arc`s) and await its `JoinHandle` in the handler, so a dropped request does not skip the bookkeeping; the shutdown path must await these tasks before the usage flush. Add a test that drops the request future mid-append and asserts one event and the counters.

## Minor

| # | Phase | Location | Finding | Suggested fix |
|---|---|---|---|---|
| m-1 | M4 | `routes/streams.rs:253-329`, `:144`, `:258` | A client that stops reading blocks the task in `tx.send(..).await` (buffer 16); neither stream has a write timeout, so the slot is held until the kernel gives up (about 15 min). The blocked task also stops reading the bus. | Wrap every send in `timeout(30 s)`; on expiry end the stream. |
| m-2 | M4 | `routes/streams.rs:203-211` vs D-G-22 | D-G-22 says the open validates `filter` and `search`, but `check_filter` passes `search: None` and, with no ids, `trace_refresh` only parses the filter. Search has no syntax, so nothing can fail, but there is also **no length cap** on `search` (nor on the list routes): only the URI limit. Each open search stream runs an `ILIKE` over input and output every tick. | Cap `search` at 1 KiB in `TraceListParams`, `SpanListParams` and `TraceStreamParams`; reword D-G-22. |
| m-3 | M2 | `routes/read.rs:82-125`, `routes/sessions.rs:87-126` | Each score lookup gets its own 30 s budget. A list is up to 2 × 30 s; a session detail up to 5 sequential 30 s steps. Refines the known "per-query 30 s timeout". | One `Instant` deadline per request, passed down as the remaining time. |
| m-4 | M4 | `live.rs:94-111`, `routes/streams.rs:150`, `:264` | Project isolation of the bus rests on a single `event.project_id != project` comparison in each of two loops. **No test sends project B's spans while a project A stream is open** (`add_project` appears nowhere in `streams.rs`). | Add one test per stream; consider a per-project channel later. |
| m-5 | M4 | `live.rs`, D-G-12 | A score write publishes nothing. A new score or feedback on a trace does not update an open live list or detail until a span for that trace arrives. Spec 09 is silent. | Document it in spec 09 § SSE, or publish a "touched trace" event from `scores.rs`. |
| m-6 | M5/M3 | `serve.rs:484-497`, `usage.rs:86-125` | `shutdown` aborts the usage task and then flushes. If the abort lands inside `flush` after `mem::take` and before the write returns, the taken counts are in neither memory nor the database (the restore only runs on a returned error). At most one 10 s window, only at shutdown. | Stop tasks with a cancellation signal and let them finish, or take the counts inside the write closure. |
| m-7 | M2 | `routes/*.rs` tests; `http/error.rs:492-525` | The 422 `query_timeout`, 422 `query_too_large` and 503 `busy` answers are tested as mappings only, not through a read route with a real `Busy`/`Timeout`; the media 503 mapping (`media.rs:105-112`) has no test. `RunError::Failed(message)` is shown to the caller and may quote literals (A2 m-7). | Add one route-level test per code; keep the message, but note it in D94. |
| m-8 | M5 | `crates/ui/src/lib.rs:96`, `:25` | `If-None-Match` is compared with `==`: weak validators (`W/"…"`), lists and `*` never get a 304. The CSP omits `base-uri` and `form-action`, which `default-src` does not cover; spec 12 gives the exact string, so spec and code must change together. | Parse the list and weak form; add the two directives to spec 12 and the const. |
| m-9 | M5 | `crates/ui/build.rs:75-85` | Every file in `web/dist` is embedded and served, including `.map` files if the web build emits them (not checked here). | Skip `*.map` in the build script, or confirm `vite` emits none. |
| m-10 | M5 | `docs/mvp-decisions.md` D-G-18 vs D-G-37; `AGENTS.md` § CI | D-G-18 says "release CI checks the UI is embedded"; D-G-37 and the plan say the release workflow is track N's. Nothing runs `check-release` (M-3). | Align D-G-18; add the recipe to CI (M-3). |
| m-11 | M2 | `routes/metrics.rs:99-114` | The cache key holds the raw `from`/`to`. A dashboard that sends `to = now` changes the key on every request, so the 10 s cache rarely hits; concurrent identical misses all run the query. The full clear at 256 entries drops hot entries too. Refines the known "metrics cache byte size". | Single-flight per key; evict by age, not clear-all; decide whether the UI should round `to`. |
| m-12 | M4 | `routes/streams.rs:44-45`, D-G-33 | In the search re-read, a trace that did not match its first span is forgotten; it matches later only if a later span matches in the window. Intended by D-G-33, but spec 09 § SSE ("sent once any of its spans matches") reads as if any earlier span counts. | Say "any span that arrives while the stream is open" in spec 09. |

## Interactions checked and found sound

- **Auth and CSRF:** every read and stream route is `Access::Project { View, session_only: false }`; writes with a session go through the origin check, bearer requests skip it; `/v1/traces` on the OTLP listener has the same layers. The UI fallback runs behind the same authentication layer and never serves `/api/`, `/v1/`, `/mcp/`, `/healthz`, `/readyz` (`http/mod.rs:148-162`); an unknown API path is a 404 problem.
- **Route overlap:** `/traces/stream` (static) and `/traces/{trace_id}` do not clash; a malformed trace id is a 400, an unknown one a 404 (tested).
- **Privacy of logs:** read errors log only `RunError::kind()`; stream warnings carry no text; `search_text_is_never_logged` and `invalid_filter_…_is_not_logged` cover the two routes with user text. SSE event names and ids are static or digits, so `Event` cannot panic.
- **Bounds:** limit 1–1000, cursor decoding in A2, media range arithmetic uses checked and saturating operations, stream slots are released on drop, and the live bus builds nothing without a subscriber.
- **Shutdown:** `live.close()` runs before the HTTP drain, stream tasks hold no `AppState`, usage and `last_used` are flushed after the drain and before storage shutdown.

## Spec ambiguities

1. Spec 09 § SSE gives the span stream's idle end but nothing for the trace stream, and nothing about a stream and a revoked credential (M-2).
2. Spec 09 does not say whether score writes should reach live streams (m-5).
3. Spec 12's CSP string lacks `base-uri` and `form-action` (m-8).
4. Spec 11 does not say how SPA paths that contain dots are served (M-1); D-G-35 decided by extension.
5. D-G-22's "validates `filter` and `search`" has no meaning for `search` (m-2).

## Open questions for the coordinating session

- M-2: a fixed maximum stream age, or a per-ping access check? The first is a few lines; the second needs the stream task to hold a meta handle.
- M-3: is a headless-browser step in CI wanted, or is the extended Rust e2e test enough?
