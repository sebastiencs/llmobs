# B2 · Vercel AI SDK (K14–K15)

See [README.md](README.md) and the common rules in [03-genai-openinference.md](03-genai-openinference.md#common-rules-for-capture-sub-phases). The Vercel AI SDK emits its own spans (`ai.*`, plus `gen_ai.*` on the provider calls). It's the only TypeScript instrumentation in the matrix (D117), so K14 also creates the TypeScript project.

## K14 — TypeScript project and AI SDK v5

**Commit:** `capture: record Vercel AI SDK v5`

**Edit only:**
- `tools/capture/ts/**`;
- `tools/capture/src/**` (fixes only);
- `fixtures/otlp/vercel/<cases below>/{case.json,input.json}`;
- `justfile`;
- `biome.json` (add `tools/capture/ts/**` to `files.includes`).

### The project

`tools/capture/ts/` is a standalone pnpm project, **not** a member of the root workspace (D115).

- **`pnpm-workspace.yaml`:**
  - makes the directory its own workspace root;
  - `minimumReleaseAge: 10080`, as at the root;
  - `allowBuilds` only if a dependency needs an install script (report it).

  Check that `pnpm install` in this directory doesn't touch the root lockfile, and that the root `just js-install` doesn't install this project.
- **`package.json`:**
  - `"private": true`, `"type": "module"`, `"engines": {"node": ">=26"}`;
  - `"packageManager"` equal to the root's;
  - script `"typecheck": "tsc --noEmit"`;
  - exact versions throughout;
  - devDependencies: `typescript` (the same 5.x as `sdks/typescript`, per AGENTS.md) and `@types/node` (the same as `sdks/typescript`).
- **Dependencies.** The three AI SDK majors are installed side by side with npm aliases, so one project covers K14 and K15:
  - `"ai-v5": "npm:ai@<5.x>"`;
  - `"@ai-sdk/openai-v5": "npm:@ai-sdk/openai@<2.x>"`;
  - `"@ai-sdk/anthropic-v5": "npm:@ai-sdk/anthropic@<2.x>"`;
  - plus `@opentelemetry/api`, `@opentelemetry/sdk-trace-node`, `@opentelemetry/exporter-trace-otlp-proto`, `@opentelemetry/resources`.

  On 2026-11-10 the latest were `ai` 5.0.288 (dist-tag `ai-v5`), `@ai-sdk/openai` 2.0.142, `@ai-sdk/anthropic` 2.0.115, `@opentelemetry/sdk-trace-node` 2.12.1, `@opentelemetry/exporter-trace-otlp-proto` 0.301.1 and `@opentelemetry/api` 1.10.0. Take the newest release older than 7 days in the matching major. The provider majors that pair with each `ai` major are the ones its `ai-v5`/`ai-v6` dist-tags point to.
- **`tsconfig.json`:**
  - strict, `noEmit`;
  - `"module": "NodeNext"`, `"allowImportingTsExtensions": true`;
  - `"erasableSyntaxOnly": true`, so Node's type stripping can run every file. If TypeScript 5.9 lacks the option, say so and keep to erasable syntax by hand: no enums, namespaces or parameter properties.
  - It includes `harness.ts` and `scenarios/**/*.ts`.
- **`harness.ts`:**
  ```ts
  export function setup(): NodeTracerProvider;
  export function mockUrl(path?: string): string;
  export async function finish(provider: NodeTracerProvider, aliases?: Record<string, string>): Promise<void>;
  ```
  - `setup()` registers a `NodeTracerProvider` globally, with one `SimpleSpanProcessor(new OTLPTraceExporter())`. The exporter reads the environment set by the runner.
  - `finish()`:
    - force-flushes and shuts down the provider;
    - for each name in `LLMOBS_CAPTURE_LIBRARIES`, reads `node_modules/<aliases[name] ?? name>/package.json`;
    - checks that its `name` field equals the library name;
    - writes `{"libraries": {name: version}}` to `LLMOBS_CAPTURE_META`.

    So `scenario.json` lists real package names (`ai`, `@ai-sdk/openai`), and the program passes `{ai: "ai-v5", "@ai-sdk/openai": "@ai-sdk/openai-v5"}`.

### Scenarios

**Common to every case:**
- `instrumentation: "ai"`.
- Telemetry is enabled on every call: `experimental_telemetry: {isEnabled: true, functionId: "<case>"}`. Check the option's name in each major, since it may have changed.
- The provider is created with `baseURL: mockUrl("/v1")` and `apiKey: process.env.OPENAI_API_KEY`. For Anthropic, use the base URL that client expects; check whether it appends `/v1`.
- **Which OpenAI API:** in v5 and later, `openai(model)` may default to the Responses API. Use `openai.chat(model)` where the table says Chat, and `openai.responses(model)` where it says Responses, so the turns' `api` is explicit.

| Case | Scenario |
|---|---|
| `v5-generate-text-tool` | `generateText` with one tool that has an `execute`, two steps (via `stopWhen`/`maxSteps`, depending on the major), Responses API. `experimental_telemetry.metadata` holds `sessionId`, `userId` and one more key (spec 02 rule 11). |
| `v5-stream-text` | `streamText` with Chat, streamed with usage. The spans hold `ai.response.msToFirstChunk` and similar values, which go into `timing_attributes`. |
| `v5-generate-object` | `generateObject` with a small zod-free JSON schema (`jsonSchema(...)`), Chat. The scripted text is the JSON object. |
| `v5-embed-many` | `embedMany` with two values, OpenAI embeddings, `dims` 8. |
| `v5-anthropic-cache` | `generateText` with the Anthropic provider and a cached system prompt; usage `{input: 2100, output: 40, cache_read: 1800, cache_write: 200}`. Covers D111 (Vercel ≤ v5 + Anthropic: the exclusive input, and cache write only in `ai.response.providerMetadata`). |

The existing hand-written `vercel/generate-text` stays as it is.

### `justfile`

```
# Install the TypeScript capture project (tools/capture/ts)
capture-ts-sync:
    pnpm --dir tools/capture/ts install --frozen-lockfile
```

- `capture`, `capture-all` and `check-capture` depend on it too.
- `check-capture` adds `pnpm --dir tools/capture/ts run typecheck` and `pnpm exec biome check tools/capture/ts` before the recapture.

### Checks

`just check`, `just fixtures-check`, `just check-capture`.

## K15 — AI SDK v6 and v7

**Commit:** `capture: record Vercel AI SDK v6 and v7`

**Edit only:** `tools/capture/ts/**`, `fixtures/otlp/vercel/<cases below>/{case.json,input.json}`, `tools/capture/src/**` (fixes only).

**Dependencies:** add aliases `ai-v6`, `@ai-sdk/openai-v6`, `@ai-sdk/anthropic-v6`, `ai-v7`, `@ai-sdk/openai-v7`, `@ai-sdk/anthropic-v7`. On 2026-11-10 those were `ai` 6.0.329 and 7.0.169, `@ai-sdk/openai` 3.0.140 and 4.0.114, `@ai-sdk/anthropic` 3.0.142 and 4.0.102. Read each major's migration guide for:
- the telemetry option;
- the multi-step option;
- `generateObject` (it may be deprecated in favour of `generateText` with an output spec; if so, use the replacement and say so in `notes`).

**Cases**, the same scenarios as K14 and the same scripts, so B1 can diff the majors:

| Case | As |
|---|---|
| `v6-generate-text-tool`, `v7-generate-text-tool` | `v5-generate-text-tool` |
| `v6-stream-text`, `v7-stream-text` | `v5-stream-text` |
| `v6-generate-object`, `v7-generate-object` | `v5-generate-object` |
| `v6-anthropic-cache`, `v7-anthropic-cache` | `v5-anthropic-cache`. The report says whether v6+ still needs `providerMetadata` for the cache write (D111). |

**Report:** for each attribute, whether it differs between v5, v6 and v7 (renamed, added, removed, value shape). This table is B1's main input for N19.
