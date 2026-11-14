# F5 — Embedded UI, live streams and every read route end to end (M-3, m-10)

Read [README.md](README.md) first. Runs **after F1** (it asserts F1's dotted deep link).

**Goal:**
- The CI `e2e` job builds the web first, so its server has the UI embedded. The Rust e2e test runs there and must take its "UI embedded" branch (it refuses to pass on the `503` branch when `LLMOBS_E2E_REQUIRE_UI` is set), and the debug binary must report `ui: embedded`. The `rust-test` job keeps running the same test without a web build (the `503` branch), as today.
- A second Rust e2e test covers what the first does not: both SSE streams over real HTTP (a `trace` event, a `span` event, `resync` for `Last-Event-ID`), and one read each of sessions, users, scores (after a score write), metrics, schema and media.
- D-G-18 stops saying that release CI checks the UI: the `e2e` job checks the debug build; `check-release` stays track N's (m-10).

No headless browser (user's choice).

**Edit only:**
- `crates/cli/tests/e2e.rs`;
- `justfile` (the `e2e` recipe);
- `docs/mvp-decisions.md` (row D-G-18 only);
- `AGENTS.md` (the `just e2e` row of the commands table).

**Commit:** `Run the embedded UI, live streams and every read route end to end`

## Background

- `justfile`:

  ```
  e2e: js-install
      bash tools/e2e/run.sh
  ```

  `tools/e2e/run.sh` runs `cargo build -p llmobs-cli`, starts `target/debug/llmobs serve` and runs the Python and TypeScript SDK suites against it. The CI `e2e` job (`.github/workflows/ci.yml`) runs `just e2e` then `just assert-clean`; it already installs Rust, Node, pnpm and uv. `web/dist` is git-ignored. `llmobs-ui`'s build script embeds `web/dist` when it exists at build time.
- `crates/cli/tests/e2e.rs` has one test, `every_sdk_fixture_round_trips_over_every_transport`. It:
  1. reserves three ports, writes `llmobs.toml` (data dir, the three addresses, `public_url = ORIGIN`, `[init]` with `ADMIN_EMAIL`, `ADMIN_PASSWORD`, org `Acme`, project `Proto`, `secret_key = INIT_KEY`), loads it with `ServerConfig::load`, and calls `llmobs_server::serve::bind(config, Some(llmobs_ui::router()))`;
  2. derives `http`, `otlp`, `grpc` base URLs and a `reqwest::Client` with a 30 s timeout;
  3. checks `GET /` (`200 text/html` if `llmobs_ui::embedded()`, else `503`) and `GET /api/v1/nope` (`404` problem);
  4. logs in and builds `let api = Api { client, base, cookie }`;
  5. finds the `Proto` project's id in `GET /api/v1/orgs/acme/projects`, creates two more projects, sends every `sdk-*` fixture, reads back and snapshots.
- Helpers: `Api::{get, post}` (JSON, cookie, `Origin` on POST), `read_json`, `sdk_fixtures()`, `trace_id_for(index) -> Vec<u8>` (fifteen zeros then `index + 1`), `post_otlp_http(client, url, key, content_type, body)`.
- Fixture facts:
  - `fixtures/otlp/llmobs/sdk-context/input.json`: spans with `session_id` `chat-42` and `user_id` `u-17`, start times on 2025-12-31.
  - `fixtures/otlp/llmobs/sdk-media-bytes/input.json`: one blob, sha256 `497790947d4666760ce38f3c00e852c71fdb66cae849bae8e9ede352719e1581`, 70 bytes, `application/octet-stream`.
- Score write: `POST /api/v1/scores` with `Authorization: Bearer <secret key>` and body `{"scores":[{"target":{"type":"trace","trace_id":"<32 hex>"},"name":"e2e","value":{"data_type":"numeric","value":1.0}}]}` answers `200`. The init key has the ingest, read and write scopes.
- Read routes, all under `/api/v1/projects/{project}`: `/sessions`, `/sessions/{session_id}`, `/users`, `/users/{user_id}`, `/scores?trace_id=…`, `/metrics?from=…&to=…`, `/schema`, `/media/{sha256}`, `/traces/stream`, `/traces/{trace_id}/stream`. Session list items have `session_id`, user list items have `user_id`. Lists default to the last 24 h, so pass `from=2025-12-31T00:00:00Z&to=2026-01-02T00:00:00Z`.
- The two fixtures use environment `staging` and no experiment, so the default exclusions keep them; no `include_eval` is needed.
- SSE framing: `event: <name>\nid: <n>\ndata: <json>\n\n`; a `resync` has no `id`.

## Changes

### 1. `justfile`

Replace the `e2e` recipe with:

```
# Build the web, run the Rust e2e tests with the UI embedded, then the SDK suites against a real server (D-G-20, D-G-18)
e2e: js-install
    pnpm --dir web run build
    LLMOBS_E2E_REQUIRE_UI=1 cargo test --workspace --all-features --test e2e
    bash tools/e2e/run.sh
    ./target/debug/llmobs version | grep -qx 'ui: embedded'
```

The existing comment line above the recipe, `# Run both SDKs against a real llmobs serve (D-G-20)`, is replaced by the comment above.

### 2. `e2e.rs`: a shared start

Move steps 1, 2 and 4 of the existing test, and the `Proto` id lookup of step 5, into:

```rust
/// A running server with the admin signed in.
struct Server {
    running: llmobs_server::serve::Running,
    http: String,
    otlp: String,
    grpc: String,
    client: reqwest::Client,
    api: Api,
    /// The `Proto` project, which `INIT_KEY` ingests into.
    proto_id: String,
}

/// Writes the config into `dir`, starts the server with the embedded UI and signs in.
async fn start(dir: &Path) -> Server { … }
```

The body is the existing code, unchanged, in the same order; step 3 (the `GET /` and `/api/v1/nope` checks) stays in the first test, right after `let server = start(dir.path()).await;`. Rewrite the first test to use `server.http`, `server.api`, etc.; its behaviour and its snapshots must not change (`cargo insta test` reports no change). `bind` returns `Result<llmobs_server::serve::Running, ServeError>`.

### 3. `e2e.rs`: require the UI when asked

In the first test, right after `start`, add:

```rust
    if std::env::var_os("LLMOBS_E2E_REQUIRE_UI").is_some() {
        assert!(
            llmobs_ui::embedded(),
            "LLMOBS_E2E_REQUIRE_UI is set, but web/dist was not embedded: build the web first"
        );
    }
```

and inside the existing `if llmobs_ui::embedded() {` branch of the `GET /` check, add: `GET {http}/acme/proto/users/alice%40example.com` answers `200` with a `content-type` starting with `text/html` (an app deep link whose last segment has a dot, F1).

### 4. `e2e.rs`: the new test

Add a helper:

```rust
/// Reads `response` until its text contains `needle`, within `wait`. Returns the text read.
async fn read_until(response: &mut reqwest::Response, needle: &str, wait: Duration) -> String {
    let deadline = tokio::time::Instant::now() + wait;
    let mut text = String::new();
    while !text.contains(needle) {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let chunk = tokio::time::timeout(remaining, response.chunk())
            .await
            .unwrap_or_else(|_| panic!("no {needle:?} within {wait:?}; read: {text}"))
            .unwrap()
            .unwrap_or_else(|| panic!("the stream ended before {needle:?}; read: {text}"));
        text.push_str(&String::from_utf8_lossy(&chunk));
    }
    text
}
```

and an `Api` method for a cookie-authenticated `GET` whose response the caller reads itself:

```rust
    async fn get_raw(&self, path: &str, headers: &[(&str, &str)]) -> reqwest::Response {
        let mut request = self
            .client
            .get(format!("{}{path}", self.base))
            .header("cookie", &self.cookie);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        request.send().await.unwrap()
    }
```

Then `#[tokio::test(flavor = "multi_thread")] async fn live_streams_and_every_read_route_work_end_to_end()`:

1. `let dir = tempfile::tempdir().unwrap(); let server = start(dir.path()).await;` and `let project = format!("/api/v1/projects/{}", server.proto_id);`.
2. Let `context_trace = format!("{:032x}", 1)` and `media_trace = format!("{:032x}", 2)`.
3. Open three streams **before** sending anything:
   - `let mut traces = server.api.get_raw(&format!("{project}/traces/stream"), &[]).await;` — assert status `200` and a `content-type` starting with `text/event-stream`;
   - `let mut spans = server.api.get_raw(&format!("{project}/traces/{context_trace}/stream"), &[]).await;` — status `200`;
   - `let mut resumed = server.api.get_raw(&format!("{project}/traces/stream"), &[("last-event-id", "1")]).await;` — status `200`, and `read_until(&mut resumed, "event: resync", Duration::from_secs(5)).await;` passes. Drop `resumed` after that.
4. Send `sdk-context` with trace id `trace_id_for(0)` (bytes …01) and `sdk-media-bytes` with `trace_id_for(1)` (…02), each over OTLP/HTTP protobuf to `{http}/v1/traces` with `INIT_KEY` (rewrite every span's `trace_id` as the first test does; read `input.json` from the fixture directory).
5. `read_until(&mut spans, "event: span", Duration::from_secs(10))` returns text containing `context_trace`. `read_until(&mut traces, &context_trace, Duration::from_secs(10))` returns text containing `event: trace`. Drop both streams.
6. Score write: `POST {http}/api/v1/scores` with `Authorization: Bearer INIT_KEY` and the body from Background, for `context_trace`: status `200`.
7. Reads, each through `server.api.get` (which asserts a success status), with `let window = "from=2025-12-31T00:00:00Z&to=2026-01-02T00:00:00Z";`:
   - `{project}/sessions?{window}`: `data` has an item whose `session_id` is `chat-42`;
   - `{project}/sessions/chat-42`: succeeds;
   - `{project}/users?{window}`: `data` has an item whose `user_id` is `u-17`;
   - `{project}/users/u-17`: succeeds;
   - `{project}/scores?trace_id={context_trace}`: `data` has an item whose `name` is `e2e`;
   - `{project}/metrics?{window}`: succeeds;
   - `{project}/schema`: succeeds.
8. Media, through `server.api.get_raw(&format!("{project}/media/497790947d4666760ce38f3c00e852c71fdb66cae849bae8e9ede352719e1581"), &[])`: status `200`, body length `70`.
9. `drop(server.api); drop(server.client); server.running.shutdown().await.unwrap();` (destructure `server` first if the borrow checker needs it).

If a read answers differently from the facts above (a field name, a default window), stop and report the actual response; do not change the server.

## Docs

### `docs/mvp-decisions.md`, row D-G-18

Replace `` `just build-release` builds the web first and release CI checks the UI is embedded; `just check` is unchanged (spec 11, 12). `` with:

```
`just build-release` builds the web first and `just check-release` checks the release binary embeds it (the release workflow that runs it is track N's, D-G-37). The CI `e2e` job builds the web and runs the e2e tests and SDK suites with the UI embedded; `just check` is unchanged and runs the Rust e2e test without a web build (spec 11, 12; G review M-3).
```

### `AGENTS.md`

In the commands table, set the `just e2e` row's description to `build the web, run the Rust e2e tests with the UI embedded, then the Python and TypeScript SDK suites against a real `llmobs serve``. Keep the row where it is.

## Done when

- `cargo test -p llmobs-cli --test e2e` passes (both tests). Whether `web/dist` exists decides which `GET /` branch the first test takes; CI's `rust-test` job has none.
- `just e2e` passes (it builds `web/dist`).
- `cargo insta test -p llmobs-cli --test e2e` reports no snapshot change.
- `just check` passes, and `just assert-clean` after it.
