# G · M5 — The embedded UI and the end-to-end tests

Implementation plan for milestone M5 of track G. It has 6 sub-phases, G5.1 to G5.6. Each one is done by one agent in one go and ends with `just check` green and one commit.

**Order and branches.** The coordinator keeps the integration branch `claude/g-m5-impl` and cherry-picks each reviewed commit onto it.
- G5.1 (the `llmobs-ui` crate) and G5.5 (SDK integration suites, `just e2e`) start from `claude/g-m5-impl` as it is and run at the same time.
- G5.2 starts from `claude/g-m5-impl` once G5.1 is on it.
- G5.3 and G5.4 start from `claude/g-m5-impl` once G5.2 is on it, and run at the same time. They need not contain G5.5.
- G5.6 is last.

Parallel sub-phases touch some of the same files (`justfile`, `AGENTS.md`, `crates/cli/Cargo.toml`, `Cargo.lock`) in different places; the coordinator merges them.

**Prerequisites:** M1–M4 are on `claude/g-m5-impl` (`git log` shows `docs: mark G M4 implemented`). Node `26.13.0` is at `$HOME/.local/share/nvm/v26.13.0/bin` (not on the non-interactive `PATH`); `uv`, `pnpm`, `actionlint` and `zizmor` are installed.

**Read first** (every sub-phase):
- `AGENTS.md`, all of it.
- This file's sections up to § G5.1, then your own sub-phase.
- `docs/plans/g-spine/00-design.md` § Embedded UI (M5), § End-to-end test (M5).
- `docs/spec/12-operations.md` (§ Security: the UI CSP; § Process lifecycle), `docs/spec/10-sdk.md` § Testing.
- `docs/mvp-decisions.md`: D150, D-G-18, D-G-19, D-G-20, D-G-35 to D-G-37.

## What M5 is, and what it is not

When M5 is done:
- `llmobs serve` serves the web UI (built from `web/`) on its main listener, with an SPA fallback and the security headers of spec 12; without a web build it serves a plain "web UI not built" page;
- `just build-release` builds the web, then the release binary with the UI inside; `just check-release` checks the UI is embedded;
- a Rust end-to-end test in `just check` starts the real server, sends the SDK conformance fixtures over OTLP/HTTP protobuf, OTLP/HTTP JSON and OTLP/gRPC, signs in, and reads every trace back through the API;
- `just e2e` runs the real Python and TypeScript SDKs against `llmobs serve`, and a CI job runs it.

**Already done elsewhere:** the login, setup, signup and reset pages and the 401 → `/login?redirect=…` handling were built by track L (`web/src/routes/login.tsx`, `web/src/api/session.ts`); the production web build already talks to the real API (MSW starts only in dev mode, `web/src/main.tsx`). So D-G-19's minimal login is not built by G (D-G-36).

**Out of scope:** the release pipeline, Docker and Helm (track N); web feature work.

## Decisions this plan relies on

| # | Summary |
|---|---|
| D150 | No `axum/tracing` feature: axum's own span would record the URI. |
| D-G-18 | The UI is its own crate, `llmobs-ui`; only `llmobs-cli` depends on it; without `web/dist` the binary serves a plain "UI not built" page; `just check` is unchanged. |
| D-G-20 | A Rust e2e test in `just check`; `just e2e` with the real SDKs as its own CI job. |
| D-G-35 | **Own embedding, not `memory-serve`.** `memory-serve` 2.4 depends on `axum` with default features, which include `tracing`, and feature unification would turn `axum/tracing` on for the whole binary (D150). `llmobs-ui`'s `build.rs` embeds `web/dist` itself (`include_bytes!`, a gzip copy of text files, a SHA-256 ETag, the content type from the extension). |
| D-G-36 | **No G login page.** Track L built `/login`, setup, signup and reset and the 401 handling, which D-G-19 asked G to build first. |
| D-G-37 | **Release check without a release CI job.** `just check-release` builds the web and the release binary and checks that `llmobs version` reports the UI as embedded; the release workflow that runs it belongs to track N. |

## Conventions for every sub-phase

- **Branch.** You run in your own git worktree. First run `pwd`, `git branch --show-current` and `git log --oneline -5`. When your branch is not at the branch the brief names, run `git reset --hard <that branch>` (your branch has no work of its own yet), then check `git log` again. Commit only on your own branch; no push, merge or rebase beyond that reset. Never bare `git stash`.
- **Node.** Before any `just` recipe that runs web or TypeScript work (`just check`, `js-install`, `build-release`, `e2e`): `export PATH="$HOME/.local/share/nvm/v26.13.0/bin:$PATH"` in the same command.
- **Checks.** Iterate with `just check-rust` (and the sub-phase's own recipe). Before the commit, `just check` exactly once; it must pass. A foreground command may run 10 minutes; when it may take longer, start it once with `run_in_background`, writing its output to a file in the session scratchpad, and wait for the completion notice. Never poll, never start a second run, leave no loop or server process behind.
- **Errors and panics.** No `unwrap()`/`expect()` outside tests and `build.rs` (see G5.1).
- **Dependencies.** Versions come from the root `Cargo.toml` (`dep.workspace = true`). Keep the enforced crate graph of `AGENTS.md`: `cli` depends on `server`, `core` and (after G5.2) `ui` only.
- **STOP rules.** Stop and report without committing when the plan is wrong about the code in a way you cannot resolve without guessing, or an earlier milestone's test fails for a reason outside your change.
- **Commit.** One commit, the subject given, no trailer lines.
- **Report.** Branch and worktree path; commit hash; what you did; deviations and why; new dependencies and versions; anything unverified.

## G5.1 — The `llmobs-ui` crate

**Files:** `crates/ui/{Cargo.toml,build.rs,src/lib.rs}` (new), `Cargo.lock`, `AGENTS.md` (§ Layout and § Crate dependency graph). The root `members = ["crates/*", …]` already includes `crates/ui`: do not edit it.

### Steps

1. **`crates/ui/Cargo.toml`:** `name = "llmobs-ui"`, `description = "The web UI, embedded at build time"`, the `*.workspace = true` package fields `crates/core/Cargo.toml` has, `[lints] workspace = true`, `build = "build.rs"`. `[dependencies]` `axum.workspace = true` (the workspace entry: no default features, so no `tracing`). `[build-dependencies]` `sha2.workspace = true`, `flate2.workspace = true`, `hex.workspace = true`. `[dev-dependencies]` `tokio.workspace = true`, `tower.workspace = true`, `http-body-util.workspace = true`.
2. **`build.rs`.** Start the file with:
   ```rust
   //! Embeds `web/dist` (D-G-35). A build script reports to Cargo on stdout and fails the build
   //! with a panic, so the workspace's print and unwrap lints do not apply here.
   #![allow(clippy::print_stdout, clippy::unwrap_used, clippy::expect_used)]
   ```
   - `let dist = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/dist");`, `let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));`; print `cargo:rerun-if-changed=build.rs` and `cargo:rerun-if-changed=<dist>` (a missing path makes Cargo rerun the script on every build, which is cheap).
   - When `dist/index.html` does not exist, write `out/assets.rs` containing `const BUILT: bool = false;\nstatic ASSETS: &[Asset] = &[];\n` and return.
   - Otherwise `create_dir_all(out.join("gz"))` and walk `dist` recursively with `std::fs::read_dir` (sorted by path). For each file:
     - `path`: `/` + its path relative to `dist`, with `/` separators;
     - `content_type` from the lowercase extension: `html` → `text/html; charset=utf-8`; `js`/`mjs` → `text/javascript; charset=utf-8`; `css` → `text/css; charset=utf-8`; `json`/`map` → `application/json`; `svg` → `image/svg+xml`; `png` → `image/png`; `ico` → `image/x-icon`; `woff2` → `font/woff2`; `woff` → `font/woff`; `txt` → `text/plain; charset=utf-8`; `webmanifest` → `application/manifest+json`; else `application/octet-stream`;
     - `etag`: `"\""` + the first 16 hex digits of SHA-256(bytes) + `"\""`;
     - `gzip`: for `text/*`, `application/json`, `image/svg+xml`, `application/manifest+json`, a gzip copy (`flate2::write::GzEncoder`, `Compression::new(6)`) written to `out/gz/<n>.gz` (`n` = the file's index), kept only when smaller than the original;
     - a row `Asset { path: <path:?>, content_type: <type:?>, etag: <etag:?>, body: include_bytes!(<absolute file path:?>), gzip: <Some(include_bytes!(<gz path:?>)) | None> }` (`{:?}` formatting escapes every string).
     Write `const BUILT: bool = true;\nstatic ASSETS: &[Asset] = &[\n<rows>];\n` to `out/assets.rs`.
3. **`src/lib.rs`:**
   ```rust
   //! The web UI, embedded at build time (D-G-18, D-G-35).

   /// One embedded file.
   #[derive(Debug)]
   struct Asset { path: &'static str, content_type: &'static str, etag: &'static str, body: &'static [u8], gzip: Option<&'static [u8]> }

   include!(concat!(env!("OUT_DIR"), "/assets.rs"));

   /// Whether `web/dist` was present at build time.
   pub fn embedded() -> bool { BUILT }

   /// Answers every `GET` and `HEAD`: an embedded file, `index.html` for an extension-less path
   /// that matches no file (the SPA fallback), or `503` when no UI was embedded.
   pub fn router() -> axum::Router { router_with(BUILT, ASSETS) }

   fn router_with(built: bool, assets: &'static [Asset]) -> axum::Router {
       axum::Router::new().fallback(move |request: axum::extract::Request| async move { serve(built, assets, &request) })
   }
   ```
   `fn serve(built, assets, request: &Request) -> Response`:
   1. Method not `GET`/`HEAD` → `405`, `Allow: GET, HEAD`, empty body.
   2. `!built` → `503`, `Content-Type: text/plain; charset=utf-8`, body ``web UI not built: run `just build-release`\n``.
   3. Lookup by `request.uri().path()` (exact; `/` means `/index.html`). No match:
      - a path starting with `/assets/`, or whose last segment contains a `.` → `404`, empty body;
      - otherwise the `/index.html` asset (SPA fallback); when that is missing too → `404`.
   4. `If-None-Match` exactly equal to the chosen asset's `etag` → `304` with `ETag`, `Cache-Control` and `Vary` (when the asset has a gzip copy), empty body.
   5. Body: the gzip copy when the asset has one and `Accept-Encoding` lists `gzip` with a non-zero q (split on `,`, trim, `gzip` or `gzip;q=<x>` with `x` not `0`/`0.0`/`0.00`/`0.000`), plus `Content-Encoding: gzip`; else the plain body. `Vary: Accept-Encoding` whenever the asset has a gzip copy. The ETag is the same for both bodies (acceptable with `Vary`).
   6. `Content-Type` (the asset's), `ETag`, `Cache-Control` chosen from the **matched asset's** path, never the request's: `public, max-age=31536000, immutable` when the asset path starts with `/assets/`, else `no-cache` (so the SPA fallback's `index.html` is never immutable).
   7. `HEAD`: same headers, an empty body, and `Content-Length` set to the length of the body a `GET` would send (set the header explicitly; only hyper strips HEAD bodies, not `oneshot`).
   8. Security headers on every answer of this router, 405/503/404 included: `Content-Security-Policy: default-src 'self'; img-src 'self' blob: data:; media-src 'self'; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'`, `X-Content-Type-Options: nosniff`, `Referrer-Policy: same-origin`.
4. **`AGENTS.md`:** § Layout gains `ui/        llmobs-ui       the web UI embedded at build time (D-G-18, D-G-35)`; § Crate dependency graph: "`ui` depends on no workspace crate; `cli` depends on `server`, `core` and `ui`."

### Tests (`src/lib.rs`, on a hand-made table so they do not need `web/dist`)

```rust
static TEST_ASSETS: &[Asset] = &[
    Asset { path: "/index.html", content_type: "text/html; charset=utf-8", etag: "\"aaaa\"", body: b"<html>app</html>", gzip: None },
    Asset { path: "/assets/app.js", content_type: "text/javascript; charset=utf-8", etag: "\"bbbb\"", body: b"console.log(1)", gzip: Some(b"GZ") },
    Asset { path: "/favicon.svg", content_type: "image/svg+xml", etag: "\"cccc\"", body: b"<svg/>", gzip: None },
];
```
Drive `router_with(true, TEST_ASSETS)` with `tower::ServiceExt::oneshot`:
- `/assets/app.js` → 200, the JS type, `cache-control: public, max-age=31536000, immutable`, body `console.log(1)`, `vary: accept-encoding`; `Accept-Encoding: br, gzip` → body `GZ`, `content-encoding: gzip`; `gzip;q=0` → plain body; `If-None-Match: "bbbb"` with gzip → 304.
- `/` and `/some/deep/link` → the index body, `cache-control: no-cache`.
- `/favicon.svg` → 200, `cache-control: no-cache`. `/assets/missing.js` → 404. `/missing.png` → 404.
- `If-None-Match: "aaaa"` on `/` → 304, empty body.
- `HEAD /assets/app.js` → 200, `content-length: 14`, empty body.
- `POST /` → 405 with `allow: GET, HEAD`.
- Every answer above carries the three security headers.
- `router_with(false, &[])` → `GET /` is 503 with the text above.
- `embedded()` equals `Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/dist/index.html").exists()` at test time.

### Done when

`just check` passes. Commit: `ui: embed the web build in llmobs-ui`.

## G5.2 — Serve the UI

**Start from:** `claude/g-m5-impl` with G5.1. **Files:** root `Cargo.toml` (one `[workspace.dependencies]` entry), `crates/server/{Cargo.toml,src/serve.rs,src/http/mod.rs,src/test_support/mod.rs}`, `crates/cli/{Cargo.toml,src/main.rs,src/login.rs,tests/version.rs}`.

### Steps

1. Root `[workspace.dependencies]`: `llmobs-ui = { path = "crates/ui" }` (the other internal entries have no `version`). `crates/cli/Cargo.toml`: `llmobs-ui.workspace = true`. `crates/server/Cargo.toml`: move `tower` from `[dev-dependencies]` to `[dependencies]` (`ServiceExt::oneshot` is used in production code now).
2. `http/mod.rs`:
   - `router(state: AppState, ui: Option<Router>) -> Router` passes `ui` to `finish`; `router_with(state, extra)` and `serve_router` and `otlp_router` pass `None`.
   - `finish(state, routed, ui: Option<Router>)`: the `.fallback(not_found)` becomes `.fallback(move |request: Request| fallback(ui.clone(), request))` with
     ```rust
     /// Unknown API paths are `404` problems; any other path goes to the UI when there is one.
     async fn fallback(ui: Option<Router>, request: Request) -> Response {
         let path = request.uri().path();
         let api = matches!(path, "/api" | "/v1" | "/mcp" | "/healthz" | "/readyz")
             || path.starts_with("/api/") || path.starts_with("/v1/") || path.starts_with("/mcp/");
         match ui {
             Some(ui) if !api => match ui.oneshot(request).await { Ok(response) => response, Err(never) => match never {} },
             _ => not_found().await,
         }
     }
     ```
     (`Router<()>`'s error type is `Infallible`; adjust the `not_found` call to what it is.) Keep `method_not_allowed_fallback` as is. UI requests still pass the shared layers (request log, authentication, CSRF on non-GET); that is accepted (no slug may be `api`, `v1` or `mcp`, which the slug rules already reserve or which would only shadow the UI route).
3. `serve.rs`: `bind(config: ServerConfig, ui: Option<axum::Router>)` builds the main listener with `router(state.clone(), ui)`; `run(config, ui, shutdown)`. The OTLP/HTTP and gRPC listeners never get the UI.
4. Callers: `test_support/mod.rs` (`router(state.clone())` → `router(state.clone(), None)`, every one), every `bind(` in `serve.rs`'s tests (`None`), `crates/cli/src/login.rs`'s test (`None`), `crates/cli/src/main.rs`'s `serve` (`serve::run(config, Some(llmobs_ui::router()), signals.wait())`).
5. `llmobs version` prints a second line, `ui: embedded` or `ui: not built` (`llmobs_ui::embedded()`).

### Tests

- `serve.rs` (over TCP): `bind(config, Some(test_ui()))` where `fn test_ui() -> axum::Router { axum::Router::new().fallback(|| async { "ui" }) }`; `GET /some/page` → `ui`; `GET /api/v1/nope` → 404 problem JSON; `GET /v1/nope` → 404 problem; `GET /healthz` → 200; the OTLP/HTTP listener's `GET /some/page` → 404 (no UI there).
- `bind(config, None)` → `GET /some/page` is the 404 problem.
- `crates/cli/tests/version.rs`: a new test runs the `llmobs` binary (`env!("CARGO_BIN_EXE_llmobs")`) with `version` and asserts the second line is `ui: embedded` when `llmobs_ui::embedded()` is true, else `ui: not built` (an integration test can use the cli's normal dependencies).

### Done when

`just check` passes. Commit: `server: serve the embedded UI as the main listener's fallback`.

## G5.3 — Release build recipes

**Start from:** `claude/g-m5-impl` with G5.2. **Files:** `justfile`, `AGENTS.md` (commands table).

### Steps

1. `justfile`: **replace** the existing recipe (`build-release:` → `cargo build --release -p llmobs-cli`, near line 43) with:
   ```just
   # Build the web UI, then the release binary with the UI embedded (D-G-18)
   build-release: js-install
       pnpm --dir web run build
       cargo build --release -p llmobs-cli

   # Build the release binary and check that the UI is embedded (D-G-37)
   check-release: build-release
       ./target/release/llmobs version | grep -qx 'ui: embedded'
   ```
2. `AGENTS.md` commands table: update the `just build-release` row if it exists ("build the web UI, then the release binary with the UI embedded"), and add `just check-release` ("build-release, then check the UI is embedded").
3. Run `export PATH="$HOME/.local/share/nvm/v26.13.0/bin:$PATH" && just check-release` once (background); it must pass. `web/dist/` is git-ignored (`.gitignore`: `dist/`): `git status` shows nothing new afterwards.

### Done when

`just check` and `just check-release` pass. Commit: `build: build the web before the release binary`.

## G5.4 — The Rust end-to-end test

**Start from:** `claude/g-m5-impl` with G5.2. **Files:** `crates/cli/tests/e2e.rs` (new), `crates/cli/tests/snapshots/e2e__*.snap` (new, accepted), `crates/cli/Cargo.toml` (dev-dependencies).

### Steps

1. `crates/cli` dev-dependencies (workspace entries): `tempfile`, `insta`, `opentelemetry-proto`, `prost`, `tonic`. `reqwest`, `serde_json` and `tokio` are already normal dependencies. Do not add `llmobs-otel` or `llmobs-storage` (crate graph): decode fixtures with `serde_json::from_slice::<ExportTraceServiceRequest>` (that is all `llmobs_otel::decode_json` does).
2. One test, `every_sdk_fixture_round_trips_over_every_transport`, `#[tokio::test(flavor = "multi_thread")]`:
   1. **Config.** Write `<tmp>/llmobs.toml` and load it with `ServerConfig::load(Some(&path))` (as `crates/cli/src/login.rs`'s test does; read it first):
      ```toml
      data_dir = "<tmp>/data"
      http_addr = "127.0.0.1:0"
      otlp_http_addr = "127.0.0.1:0"
      grpc_addr = "127.0.0.1:0"
      public_url = "http://127.0.0.1:8080"

      [init]
      admin_email = "ada@example.com"
      admin_password = "llmobs-e2e-1"
      org = "Acme"
      project = "Proto"
      secret_key = "sk-llmobs-0123456789ABCDEFGHIJKLMNOPQRSTUV"
      ```
      `let running = llmobs_server::serve::bind(config, Some(llmobs_ui::router())).await.unwrap();` (an integration test of `llmobs-cli` can use the cli's normal dependencies, and `llmobs-ui` is one since G5.2).
   2. **UI and API paths.** `GET /` → 200 `text/html` when `web/dist/index.html` exists (`Path` check as in G5.2), else 503; `GET /api/v1/nope` → 404 with `content-type: application/problem+json`.
   3. **Sign in.** `reqwest::Client` without a cookie store. `POST /api/auth/login` with `{"email": "ada@example.com", "password": "llmobs-e2e-1"}` and `Origin: http://127.0.0.1:8080` (every session `POST` needs that exact origin; `public_url` does not follow the port). Take the first `set-cookie`, keep the text before the first `;`, and send it as `Cookie` on every later request (as `login.rs`'s test does).
   4. **Two more projects.** `POST /api/v1/orgs/acme/projects` with `{"name": "Json"}` and `{"name": "Grpc"}` (+ `Origin`) → keep each response's `id`. For each, `POST /api/v1/projects/{id}/api-keys` with `{"kind": "secret", "scopes": ["ingest", "read"]}` (+ `Origin`) → the plaintext key is the response field `secret`. The init project's key is the `secret_key` above; its project id comes from `GET /api/v1/orgs/acme/projects` (the project named `Proto`).
   5. **Send.** For every directory under `concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/otlp/llmobs")` whose name starts with `sdk-` (sorted; index `k` from 0), read `input.json`, parse it as `ExportTraceServiceRequest`, and set every span's `trace_id` to `[0u8; 15]` followed by `k + 1` (span ids stay; the fixtures all use trace `…01`). Send:
      - project Proto: `POST http://<local_addr>/v1/traces`, `Content-Type: application/x-protobuf`, `prost::Message::encode_to_vec`, `Authorization: Bearer <init key>`;
      - project Json: `POST http://<otlp_local_addr>/v1/traces`, `Content-Type: application/json`, `serde_json::to_vec(&request)` (OTLP/JSON through `opentelemetry-proto`'s `with-serde`), its key;
      - project Grpc: `TraceServiceClient::connect(format!("http://{grpc_local_addr}"))`, `export` with metadata `authorization: Bearer <Grpc key>`.
      Every answer is a success.
   6. **Read back.** For each project and fixture `k`, with the trace id hex `t`:
      - `GET /api/v1/projects/{id}/traces?include_eval=true&from=2025-12-31T00:00:00Z&to=2026-01-02T00:00:00Z` lists `t`;
      - `GET /api/v1/projects/{id}/traces/{t}` → the trace detail `{trace, spans: [{depth, detached, span}], truncated}` (a flat pre-order list);
      - for each `spans[i].span.span_id`, `GET /api/v1/projects/{id}/traces/{t}/spans/{span_id}`.
      Build `{"trace": <detail>, "spans": [<span details in that order>]}` and replace, at any depth, the values of the keys `ingest_time`, `project_id` and `score_id` with `"[redacted]"`.
   7. **Compare.** For each fixture the three projects' values must be equal (`assert_eq!`; on a mismatch in another field, add that key to the redaction list and say so in the report — do not change the server). Then `insta::assert_json_snapshot!(fixture_name, value)` with the bare fixture directory name (the file becomes `crates/cli/tests/snapshots/e2e__<fixture>.snap`).
   8. Drop the tonic client and the reqwest client, then `running.shutdown().await.unwrap()`.
3. Accept the snapshots (`INSTA_UPDATE=always cargo test -p llmobs-cli --test e2e`), read each one, and compare by eye with the fixture's `expected.json` (names, kinds, input/output formats); list any surprise in the report rather than editing fixtures.

### Done when

`just check` passes. Commit: `cli: add the end-to-end test over HTTP, JSON and gRPC`.

## G5.5 — SDK integration suites and `just e2e`

**Start from:** `claude/g-m5-impl` as it is (in parallel with G5.1). **Files:** `sdks/python/tests/test_e2e.py` (new), `sdks/python/pyproject.toml` (a pytest marker), `sdks/typescript/src/e2e.test.ts` (new), `tools/e2e/run.sh` (new), `justfile`, `.github/workflows/ci.yml`, `AGENTS.md` (commands table and § Layout `tools/e2e/`), `docs/spec/10-sdk.md` § Testing.

### Steps

1. **Python** `tests/test_e2e.py` (mypy-strict and ruff clean; every `requests` call has `timeout=10`):
   - `pytestmark = [pytest.mark.e2e, pytest.mark.skipif(not os.environ.get("LLMOBS_E2E_BASE_URL"), reason="needs just e2e")]`; add `markers = ["e2e: needs a running llmobs serve (just e2e)"]` to `[tool.pytest.ini_options]`.
   - The test: `llmobs.init(api_key=key, base_url=url)`; an `@llmobs.observe(name="e2e-py")` function that stores `llmobs.current_trace_id()` and returns `"ok"`; `llmobs.score(name="e2e", value=1.0, trace_id=trace_id)`; `llmobs.shutdown()`; then poll every 250 ms for at most 10 s: `GET {url}/api/v1/projects/current/traces/{trace_id}` with `Authorization: Bearer {key}` until 200, assert `body["trace"]["name"] == "e2e-py"`; poll `GET {url}/api/v1/projects/current/scores?trace_id={trace_id}` until `data` has an item with `name == "e2e"` and `value == {"data_type": "numeric", "value": 1.0}`.
2. **TypeScript** `src/e2e.test.ts` (typechecks under `tsconfig.test.json`): `describe.skipIf(!process.env.LLMOBS_E2E_BASE_URL)("e2e", …)`, one test with a 30 s timeout: `init({ apiKey, baseUrl })`; `observe(() => { traceId = currentTraceId(); return "ok"; }, { name: "e2e-ts" })()`; `score({ name: "e2e", value: 1, traceId })`; `await shutdown()`; the same two polls with `fetch` (`trace.name === "e2e-ts"`, a score `value` `{ data_type: "numeric", value: 1 }`). The default `pnpm test` skips it.
3. **`tools/e2e/run.sh`** (`#!/usr/bin/env bash`, `set -euo pipefail`):
   - `cargo build -p llmobs-cli`;
   - three free ports from `python3 -c 'import socket; …'` binding port 0 three times (a race with other processes is possible; the readiness wait fails loudly then);
   - a temp dir (`mktemp -d`) and a `trap` that kills the server and removes the dir on exit;
   - start `target/debug/llmobs serve` in the background with `LLMOBS_DATA_DIR=<tmp>`, `LLMOBS_HTTP_ADDR=127.0.0.1:<p1>`, `LLMOBS_OTLP_HTTP_ADDR=127.0.0.1:<p2>`, `LLMOBS_GRPC_ADDR=127.0.0.1:<p3>`, `LLMOBS_INIT_ADMIN_EMAIL=ada@example.com`, `LLMOBS_INIT_ADMIN_PASSWORD=llmobs-e2e-1`, `LLMOBS_INIT_ORG=Acme`, `LLMOBS_INIT_PROJECT=E2E`, `LLMOBS_INIT_SECRET_KEY=sk-llmobs-0123456789ABCDEFGHIJKLMNOPQRSTUV` (check these names in `crates/server/src/config.rs`'s alias table);
   - wait for `curl -sf http://127.0.0.1:<p1>/readyz` (every 0.5 s, at most 60 s, else print the server log and exit 1);
   - `LLMOBS_E2E_BASE_URL=http://127.0.0.1:<p1> LLMOBS_E2E_API_KEY=<key>` then `uv run --locked --directory sdks/python pytest -m e2e tests/test_e2e.py` and `pnpm --dir sdks/typescript exec vitest run src/e2e.test.ts`.
4. **`justfile`:** `# Run both SDKs against a real llmobs serve (D-G-20)` / `e2e: js-install` / `    bash tools/e2e/run.sh`.
5. **CI:** a job `e2e` in `.github/workflows/ci.yml`: `runs-on` and checkout exactly as the other jobs (pinned SHA, `persist-credentials: false`), then the repository's composite actions `./.github/actions/rust` with `cache: "false"` (as the `rust-deny` job uses it) and `./.github/actions/js`, then `astral-sh/setup-uv` with the exact pinned SHA and `version` the `python-sdk` job uses, `timeout-minutes: 45`, and a step `run: just e2e`. Add `e2e` to `ci-ok`'s `needs`. Copy the pins from the existing jobs; never write a new SHA. Run `just check-workflows`; it must pass.
6. `AGENTS.md`: commands table `just e2e` ("run the Python and TypeScript SDKs against a real `llmobs serve` (D-G-20)"); § Layout `tools/e2e/   run.sh for just e2e`. `docs/spec/10-sdk.md` § Testing: replace "An integration suite against a real `llmobs serve` is added once the ingest and score routes exist." with "`just e2e` runs both SDKs against a real `llmobs serve`: each sends one traced function and one score, then reads them back through the API (D-G-20)."
7. Run `export PATH=… && just e2e` once (background); it must pass.

### Done when

`just check`, `just check-workflows` and `just e2e` pass. Commit: `sdks: run both SDKs against a real server in just e2e`.

## G5.6 — Status

**Files:** `docs/plans/g-spine/00-design.md` (Status line: add "M5 implemented (branch `claude/g-m5-impl`)"), `docs/roadmap.md` (G row status cell: `**Done** on branches: M1 #43, M2 #52, M3 \`claude/g-m3-impl\`, M4 \`claude/g-m4-impl\`, M5 \`claude/g-m5-impl\`.`; plan cell: `(M1–M5 written)`).

### Done when

`just check` passes. Commit: `docs: mark G M5 implemented`.
