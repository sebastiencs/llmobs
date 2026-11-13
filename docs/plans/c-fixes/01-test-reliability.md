# CF1 — Test reliability under load (M14)

See [README.md](README.md) for the rules that apply to every phase.

**Edit only:**
- `web/vite.config.ts`;
- `web/src/test/setup.ts`;
- `web/src/test/quality-bar.test.tsx`;
- `web/src/routes/$org/$project/traces/index.test.tsx`;
- the `Status:` line of `docs/plans/c-fixes/README.md`.

**Commit:** `web: make the test suite reliable under load`

## Why

On 2026-11-13, `just check-web` failed under CPU load in 7 of 10 loaded runs, with 1 to 7 failing tests each time. Review § Flaky tests has the details. There are two causes:

1. **Cold starts and large datasets.**
   - The first test of each file takes about 2 s at idle. Later tests take 0.5–0.9 s.
   - The tests that generate the 5 000-trace dataset take 2.5–3.8 s.
   - Under load these times grow 3–4×.
   - They then exceed Testing Library's default `findBy*`/`waitFor` timeout (1 000 ms) and Vitest's default test timeout (5 000 ms).
2. **Unmocked requests do not fail tests.**
   - `server.listen({ onUnhandledFrame: "error" })` prints an error and rejects the request, but the test still passes.
   - In MSW 3, `onUnhandledFrame` replaces `onUnhandledRequest` and covers HTTP requests too.
   - Today exactly one test makes an unmocked request: `traces/index.test.tsx` › "keeps the selected row in place when a trace is prepended". It calls `GET /api/v1/projects/acme%2Fsupport-bot/traces/a1000000000000000000000000000006/stream`.

## Steps

### 1. Timeouts — `web/vite.config.ts`

In the `test` object, after `restoreMocks: true`, add:

```ts
    testTimeout: 15_000,
    hookTimeout: 30_000,
```

Change nothing else in the file.

### 2. Testing Library timeout — `web/src/test/setup.ts`

1. Add `configure` to the imports from `@testing-library/react`, which already imports `cleanup`:

   ```ts
   import { cleanup, configure } from "@testing-library/react";
   ```

2. Right after the imports, before `installTestEventSource();`, add:

   ```ts
   // Under CPU load the first render of a file can take several seconds.
   configure({ asyncUtilTimeout: 5_000 });
   ```

### 3. Unhandled requests fail the test — `web/src/test/setup.ts`

1. Add the import:

   ```ts
   import { HttpNetworkFrame } from "msw/experimental";
   ```

   `web/src/main.tsx` imports the same class from the same module, dynamically.

2. Above `beforeAll`, add a module-level array:

   ```ts
   const unhandled: string[] = [];
   ```

3. Replace `server.listen({ onUnhandledFrame: "error" });` with:

   ```ts
   server.listen({
     onUnhandledFrame({ frame, defaults }) {
       if (frame instanceof HttpNetworkFrame) {
         const { request } = frame.data;
         unhandled.push(`${request.method} ${new URL(request.url).pathname}`);
       } else {
         unhandled.push("a non-HTTP frame");
       }
       defaults.error();
       // A callback only prints; on its own it lets the request through to the
       // real network. Throwing a network error rejects it, as "error" did.
       throw Response.error();
     },
   });
   ```

   Record the pathname only, never the query string. A query string can hold user text.

4. In the existing `afterEach`, add these lines as its **last** statements, after `resetMatchMedia();`:

   ```ts
   const found = unhandled.splice(0);
   if (found.length > 0) {
     throw new Error(`Unhandled requests: ${found.join(", ")}`);
   }
   ```

### 4. Fix the one test that hits an unmocked request

The test is `web/src/routes/$org/$project/traces/index.test.tsx` › `it("keeps the selected row in place when a trace is prepended", …)`.

1. At the top of the test body, before `openLive`, add:

   ```ts
   server.use(spanStreamHandler(() => undefined));
   ```

2. Import `spanStreamHandler` from `@/mocks/sse`, next to the file's existing `traceStreamHandler` import.

### 5. Build the 5 000-trace datasets outside the timed test

**`web/src/test/quality-bar.test.tsx`:**

1. Add `beforeAll` to the `vitest` import.
2. Add a module-level `let traces5k: Dataset;`. Import the `Dataset` type from `@/mocks/generated/dataset`, which exports it.
3. Add this hook:

   ```ts
   beforeAll(() => {
     traces5k = createDataset({ seed: 1, now: Date.now(), traces: 5_000 });
   });
   ```

4. In `it("keeps a 5 000-trace list under 60 rendered rows in the middle", …)`, replace `createDataset({ seed: 1, now: Date.now(), traces: 5_000 })` with `traces5k`.
5. Leave the two `traces: 1` datasets as they are.

**`web/src/routes/$org/$project/traces/index.test.tsx`,** in `it("virtualizes a large list and loads the next page on scroll", …)`:

1. The test creates `const now = Date.now();` and then `createDataset({ seed: 1, now, traces: 5000 })`. Apply the same pattern as above:
   - add `beforeAll` to the `vitest` import, which today imports `expect, it, vi`;
   - add `type Dataset` to the existing `createDataset` import from `@/mocks/generated/dataset`;
   - a module-level `let traces5k: Dataset;`;
   - a `beforeAll` that builds it with `now: Date.now()`;
   - the test uses `traces5k`.
2. Remove the test's local `now`. Only `createDataset` uses it.

Do not share one dataset between files.

## Tests

No new test files. Step 3 is itself the test for the unhandled-request rule. Check it by hand once:
1. Temporarily remove the `server.use(spanStreamHandler…)` line from step 4.
2. Run that file. The test must fail with `Unhandled requests: GET /api/v1/projects/acme%2Fsupport-bot/traces/a1000000000000000000000000000006/stream`. The ID may differ; if any other request is listed, report it.
3. Put the line back.

## Verification under load (required, report the results)

1. Run `just check-web` once at idle.
2. Run the suite three times under load. From the repository root, in one shell:

   ```bash
   export PATH=$HOME/.local/share/nvm/v26.13.0/bin:$PATH
   for j in $(seq 1 $(nproc)); do timeout 600 sh -c 'while :; do :; done' & done
   for i in 1 2 3; do pnpm --dir web exec vitest run > /tmp/cf1-load-$i.txt 2>&1; echo "run $i exit $?"; done
   pkill -f 'while :; do :; done'
   ```

3. All three runs must exit 0. Report each run's `Duration` line and its slowest test. Use `pnpm --dir web exec vitest run --reporter=verbose` if you need per-test times.
4. If a run fails, report the failing test and its error. Do not raise the timeouts further without asking.

## Docs

None. `docs/reviews/c-web-ui.md` already describes the cause.

## Done when

- `just check-web` is green.
- The three loaded runs pass.
- The single commit contains only the files listed under **Edit only**.
