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

## Test

The fixture `sdk-context` (under `fixtures/otlp/llmobs/`) is the only SDK fixture with an end user: `user_id` `u-17`, session `chat-42`. `sdk-span-manual` has no user; use it for T3.

Add these helpers next to `Api` (the existing `Api::post`, `Api::get` and `post_otlp_http` assert success, so they cannot check a 404):
- `Api::get_status(&self, path: &str) -> u16`: like `get`, returns `response.status().as_u16()` without reading the body;
- `Api::delete(&self, path: &str) -> Value`: a `DELETE` with the cookie and the `origin` header (the CSRF check, as `Api::post` sends it), through `read_json`;
- `async fn otlp_status(server: &Server, request: &ExportTraceServiceRequest) -> u16`: `server.client.post(format!("{}/v1/traces", server.http))` with `authorization: Bearer {INIT_KEY}`, `content-type: application/x-protobuf` and `request.encode_to_vec()`, returning the status (the same request `post_otlp_http` builds; copy it).

`deletion_end_to_end`, `#[tokio::test(flavor = "multi_thread")]` like its neighbours:

1. `start(dir, "")`; `project = /api/v1/projects/{proto_id}`; assert `GET {project}` has `slug` `proto`.
2. Send `sdk-context` twice over OTLP/HTTP with `INIT_KEY` (`post_otlp_http`), as traces `T1 = format!("{:032x}", 11)` and `T2 = …12` (`fixture_request` rewrites the trace id), and `sdk-span-manual` as `T3 = …13`. `GET {project}/traces/{T1}` is `200`.
3. Open the trace-list stream (`get_raw("{project}/traces/stream", &[])`).
4. `POST {project}/deletions {kind: "user", user_id: "u-17"}` → `202`, `status` `resolving`.
5. Poll `GET {project}/deletions/{id}` every 100 ms for at most 10 s until `status` is `pending`; then `trace_count` is 2.
6. `read_until(stream, "event: resync", 5 s)`.
7. `get_status("{project}/traces/{T1}")` and `{T2}` are `404`; `{T3}` is `200`; `get_status("{project}/users/u-17")` is `404` (`routes/users.rs` answers 404 when the user has no rows; check it, and if it returns an empty `200`, assert instead that `GET {project}/users` lists no `u-17`).
8. `POST {project}/deletions/preview {from: "2025-12-31T00:00:00Z", to: "2026-01-02T00:00:00Z"}` (the window the existing e2e uses; the SDK fixtures start on 2025-12-31) → `matched_traces` 1 (only `T3` is left).
9. Right before the drop, send one more `sdk-span-manual` export with `INIT_KEY` and assert `otlp_status` is `200`: this (re)caches the key, whose cache entry lives 60 s from insertion.
10. `Api::delete("/api/v1/projects/{proto_id}?confirm=proto")` → `kind` `project`.
11. `get_status(project)` → `404`; `otlp_status` with `INIT_KEY` → `404` (a cached key reaches the project check; only a key not used in the last 60 s would get `401`, which is the one way this step can flake).
12. Poll `GET /api/v1/orgs/acme/deletions` (the org slug `start` uses) until the project row is `applied` (≤ 10 s); the user row is `applied` too (superseded by the drop).

Reaching `applied` for the user deletion without the drop would need a flush (default `flush.max_age`) and a deletion pass; step 12 gets there through the drop instead. Do not lower timing settings below their minimums (`storage.flush.max_age` ≥ 100 ms) if you add any.

## Closeout

1. Read specs 03 § 7–9, 04 § Deletion requests, 08 (deletion permissions), 09 § Conventions (problem codes `too_many_traces`, `plan_required`) and § Pricing & GDPR, 11 (settings Data tab, trace and user actions), 12 (§ Backups `reapply-deletions --from`, process lifecycle: the deletion worker and the hourly retention task). Fix any sentence the code contradicts; the plan commit wrote them before the code existed.
2. `docs/roadmap.md`, row **M**: Plan `[plans/m-deletion/](plans/m-deletion/) (M1–M12)`; Status `**Done.** …` listing what shipped, or `**Done except M8**` with "M8 waits for I's M3" if M8 was deferred; Decisions `D-M-1–D-M-17`.
3. If M8 has not run, say so in the report and in the roadmap status.

## Done when

- [ ] `deletion_end_to_end` passes inside `just check` (it runs with the other e2e tests in `crates/cli/tests`).
- [ ] Specs and the roadmap match the code; one commit `Close track M with an end-to-end deletion test`.
