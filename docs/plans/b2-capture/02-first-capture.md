# B2 · The first capture (K10)

See [README.md](README.md). K10 adds the Python scenario project and records the first real case end to end: an OpenAI chat completion with a tool call, traced by OpenInference. It is the sub-phase where the harness meets a real instrumentation, so expect to fix small things in K2–K9. Those fixes go in this commit and are listed in the report.

**Prerequisite:** request R1 (B1) is on `main`, so the validation test accepts a captured case without `expected.json`. If it isn't, stop and report.

## K10 — Python project and OpenAI via OpenInference

**Commit:** `capture: record OpenAI via OpenInference`

**Edit only:**
- `tools/capture/py/**`;
- `tools/capture/src/**` (fixes only; list each one);
- `fixtures/otlp/openinference/openai-chat-tool/{case.json,input.json}`;
- `justfile`;
- `AGENTS.md` § Commands;
- `.gitignore` (only if the project needs an entry the root file lacks).

### The project

`tools/capture/py/`:

- **`.python-version`:** the newest CPython 3.13 patch older than 7 days, as an exact version (e.g. `3.13.9`). `process.runtime.version` ends up in fixtures, so it must not depend on the machine (D116).
- **`pyproject.toml`:**
  ```toml
  [project]
  name = "llmobs-capture-scenarios"
  version = "0.0.0"
  requires-python = "==3.13.*"
  dependencies = [
    "opentelemetry-sdk==<x>",
    "opentelemetry-exporter-otlp-proto-http==<x>",
    "openai==<x>",
    "openinference-instrumentation-openai==<x>",
  ]

  [dependency-groups]
  dev = ["ruff==<x>", "mypy==<x>"]

  [build-system]
  requires = ["uv_build>=0.13.6,<0.14"]
  build-backend = "uv_build"

  [tool.uv]
  exclude-newer = "7 days"   # D74/D117: no release younger than 7 days

  [tool.uv.build-backend]
  module-name = "capture_py"
  ```
  - Versions are exact (`==`), each the newest release older than 7 days (PyPI JSON API). On 2026-11-10 the latest were `opentelemetry-sdk` 1.45.2, `opentelemetry-exporter-otlp-proto-http` 1.45.2, `openai` 3.49.0 and `openinference-instrumentation-openai` 0.1.69. Use the same `ruff` and `mypy` versions as `sdks/python/uv.lock`.
  - Copy `sdks/python`'s `[tool.ruff]`/`[tool.ruff.lint]` selection and `[tool.mypy] strict = true`, with `files = ["src", "scenarios"]`. Third-party packages without type information get `[[tool.mypy.overrides]] ignore_missing_imports = true` per module, never a global ignore.
- **`uv.lock`:** generated with `uv lock`, committed.
- **`src/capture_py/__init__.py`** (empty, with a docstring) and **`src/capture_py/harness.py`:**
  ```python
  def setup() -> TracerProvider: ...
  def mock_url(path: str = "") -> str: ...
  def finish(provider: TracerProvider) -> None: ...
  ```
  - `setup()`:
    - builds `TracerProvider(resource=Resource.create({}))` (the service name comes from `OTEL_SERVICE_NAME`);
    - adds `SimpleSpanProcessor(OTLPSpanExporter())`, which reads its endpoint, protocol and compression from the environment the runner sets;
    - installs it with `trace.set_tracer_provider` and returns it.

    `SimpleSpanProcessor`, not batching: every span is exported as it ends, so nothing is lost at exit, and the merge in K7 makes the batching irrelevant.
  - `mock_url(path)` returns `os.environ["LLMOBS_CAPTURE_MOCK_URL"] + path`.
  - `finish(provider)` calls `force_flush()` and `shutdown()`, then writes `{"libraries": {name: importlib.metadata.version(name)}}` for each name in `LLMOBS_CAPTURE_LIBRARIES` to `LLMOBS_CAPTURE_META`.
  - Any missing environment variable raises immediately with the variable's name.
- **`scenarios/openinference/openai-chat-tool/`:**
  - **`scenario.json`:**
    - `instrumentation: "openinference-instrumentation-openai"`;
    - `libraries: ["openai", "openinference-instrumentation-openai", "opentelemetry-sdk"]`;
    - `spec_refs`:
      - `02 § Convention detection`;
      - `02 § Mapping tables / Span kind`;
      - `02 § Mapping tables / Input / output`;
      - `02 § Mapping tables / Model, provider, usage`;
      - `02 § Normalization rules`;
    - **two `openai.chat` turns:**
      1. a `get_weather` tool call with arguments `{"city": "Paris"}`, `finish: tool_calls`, usage `{input: 82, output: 17}`;
      2. text "It is 18 °C and sunny in Paris.", `finish: stop`, usage `{input: 131, output: 12, cache_read: 64}`. That puts a cached-token count in a real OpenInference span.

    Model `gpt-4o-mini-2024-07-18` for both. It must have a row in `fixtures/otlp/prices.json` so B1 can compute a cost when it blesses. If none matches, report it to B1 rather than editing `prices.json`, which belongs to B1.
  - **`main.py`:**
    1. `provider = harness.setup()`;
    2. `OpenAIInstrumentor().instrument(tracer_provider=provider)`;
    3. `client = OpenAI(base_url=harness.mock_url("/v1"), api_key=os.environ["OPENAI_API_KEY"])`;
    4. a system message, a user message ("What's the weather in Paris?"), and one tool definition (`get_weather`, a JSON schema with `city: string`);
    5. the first call, then the assistant message with the tool call plus a `tool` message with the synthetic result `{"temperature_c": 18, "conditions": "sunny"}`, then the second call;
    6. `harness.finish(provider)`.

    No retries (`max_retries=0`), so a mock error fails fast. The content is synthetic.

### The capture

1. `uv lock` in `tools/capture/py`, then `just capture openinference/openai-chat-tool`.
2. `just fixtures-check` must pass on the new case:
   - the layout and canonical JSON;
   - the OTLP/JSON walker;
   - the secret and email scan;
   - the 10-second window;
   - the case files.

   **Fix failures in the tool or the scenario, never in the files.**
3. Run the capture a second time: `just check-capture` must pass, which proves determinism (D116).
4. Read `input.json` yourself:
   - two LLM spans with the `llm.*` / `openinference.span.kind` attributes;
   - flattened `llm.input_messages.*`;
   - `llm.token_count.*`, including the cache-read detail if the instrumentation emits one;
   - `server.address` not present, or `api.openai.com`.

   Report every attribute that spec 02 § Mapping tables doesn't mention, and every spec row this capture contradicts (for example a different token attribute name). **Don't change the spec in this sub-phase:** the report goes to B1 (R2).

### `justfile`

```
# Install the Python capture project (tools/capture/py)
capture-py-sync:
    cd tools/capture/py && uv sync --locked

# Lint and type-check the capture scenarios, then recapture every scenario and compare (timing excluded)
check-capture: capture-py-sync
    cd tools/capture/py && uv run ruff format --check && uv run ruff check && uv run mypy
    cargo run -q -p llmobs-capture -- run --all --check
```

Make `capture` and `capture-all` depend on `capture-py-sync`. **Don't add `check-capture` to `check`** (D117).

### `AGENTS.md`

Add `just check-capture` to § Commands. In § Tests add one bullet: "Captured OTLP fixtures are recorded with `just capture` (tools/capture) and never edited by hand; `just check-capture` before committing one."

### Checks

- `just check`;
- `just fixtures-check`;
- `just check-capture`.

### Report

- the versions chosen (CPython included);
- the fixes made to K2–K9;
- the attributes and spec rows of step 4;
- the size of `input.json`;
- how many milliseconds the timings moved between the two captures.
