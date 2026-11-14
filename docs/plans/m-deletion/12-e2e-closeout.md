# M12 — End-to-end test and closeout

Read [README.md](README.md) first.

**Goal:** one end-to-end test over a real `llmobs serve` (the harness of `crates/cli/tests/e2e.rs`) that exercises the whole deletion path, then the documentation closeout: specs checked against what was built, and the roadmap row.

**Edit only:**
- `crates/cli/tests/e2e.rs` (one new test, plus small helpers on `Api` if needed);
- `docs/spec/03-storage.md`, `04-metadata-db.md`, `08-auth.md`, `09-api.md`, `11-ui.md`, `12-operations.md` — only where they disagree with the code;
- `docs/mvp-decisions.md` — only to correct a `D-M-n` row the implementation changed (say which in the report);
- `docs/roadmap.md` (row **M**).

**Commit:** `Close track M with an end-to-end deletion test`

## Read first

- `crates/cli/tests/e2e.rs`: `start(dir, extra_config)` (a real server with an admin signed in, org `Acme`, project `Proto`, secret key `INIT_KEY`), `Api::{get, get_raw, post}`, `read_json`, `read_until`, `fixture_request(name, trace)`, `post_otlp_http`, and the test `live_streams_and_every_read_route_work_end_to_end` (the pattern to follow).
- The SDK fixture `sdk-context` it sends (`fixtures/otlp/…/sdk-context/`): its spans carry a `user_id` and a `session_id`; read their values from the fixture's `expected.json` and use them below.

## Test

`deletion_end_to_end` in `e2e.rs`, `#[tokio::test(flavor = "multi_thread")]` like its neighbours:

1. `start(dir, "")`; `project = /api/v1/projects/{proto_id}`.
2. Send `sdk-context` twice over OTLP/HTTP with `INIT_KEY`, as traces `T1 = format!("{:032x}", 11)` and `T2 = …12` (`fixture_request` rewrites the trace id), and a third trace `T3 = …13` from another fixture with no user (pick one of the SDK fixtures without `user_id`; name it in the report). Check `GET {project}/traces/{T1}` is `200`.
3. Open the trace-list stream (`get_raw("{project}/traces/stream")`).
4. `POST {project}/deletions {kind: "user", user_id: <fixture user>}` → `202`, `status` `resolving`.
5. Poll `GET {project}/deletions/{id}` every 100 ms for at most 10 s until `status` is `pending`; then `trace_count` is 2.
6. `read_until(stream, "event: resync", 5 s)`.
7. `GET {project}/traces/{T1}` and `{T2}` answer `404` (add an `Api::get_status(path) -> u16` helper, since `get` asserts success); `{T3}` is `200`; `GET {project}/users/<fixture user>` is `404`.
8. `POST {project}/deletions/preview {from: "2000-01-01T00:00:00Z", to: "2100-01-01T00:00:00Z"}` → `matched_traces` 1 (only `T3` is left).
9. `DELETE /api/v1/projects/{proto_id}?confirm=proto` → `202` (`kind` `project`). Check the project's slug in `GET {project}` first and use it; the init config names the project `Proto`, so the slug is expected to be `proto`.
10. `GET {project}` → `404`; an OTLP export with `INIT_KEY` → `404` (the key was used, so it is cached; see M5).
11. Poll `GET /api/v1/orgs/acme/deletions` (org slug from `GET /api/v1/orgs`) until the project row is `applied` (≤ 10 s); the user row is `applied` too (superseded).

Reaching `applied` for the user deletion without the drop would need a flush (default `flush.max_age`) and a deletion pass; step 11 gets there through the drop instead. Do not lower timing settings below their minimums (`storage.flush.max_age` ≥ 100 ms) if you add any.

## Closeout

1. Read specs 03 § 7–9, 04 § Deletion requests, 08 (deletion permissions), 09 § Conventions (problem codes `too_many_traces`, `plan_required`) and § Pricing & GDPR, 11 (settings Data tab, trace and user actions), 12 (§ Backups `reapply-deletions --from`, process lifecycle: the deletion worker and the hourly retention task). Fix any sentence the code contradicts; the plan commit wrote them before the code existed.
2. `docs/roadmap.md`, row **M**: Plan `[plans/m-deletion/](plans/m-deletion/) (M1–M12)`; Status `**Done.** …` listing what shipped, or `**Done except M8**` with "M8 waits for I's M3" if M8 was deferred; Decisions `D-M-1–D-M-17`.
3. If M8 has not run, say so in the report and in the roadmap status.

## Done when

- [ ] `deletion_end_to_end` passes inside `just check` (it runs with the other e2e tests in `crates/cli/tests`).
- [ ] Specs and the roadmap match the code; one commit `Close track M with an end-to-end deletion test`.
