# Developer tools

Tools that are not part of the product. Benchmark scripts live here too.

## Fixture capture (`tools/capture/`)

`llmobs-capture` records the spans a real instrumentation library emits. A short Python or TypeScript program calls a local mock of the OpenAI, Anthropic and Gemini HTTP APIs and exports OTLP to a receiver in the same tool. The tool then writes `fixtures/otlp/<group>/<case>/case.json` and `input.json`.

Scenarios live in `tools/capture/py/scenarios/<group>/<case>/` (uv) and `tools/capture/ts/scenarios/<group>/<case>/` (pnpm). Each directory holds `scenario.json` and the program.

The recipes arrive with the tool, in K9–K10 of [`docs/plans/b2-capture/`](../docs/plans/b2-capture/README.md):

- `just capture <group>/<case>` records one scenario.
- `just capture-all` records every scenario.
- `just check-capture` runs the scenario projects' lint and type checks, then recaptures every scenario and compares it with the committed `input.json` (timing excluded).

A captured `input.json` is never edited by hand. Change the scenario and recapture (D120).
