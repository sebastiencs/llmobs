# F · Python track (P1–P12)

See [README.md](README.md) for the decisions and the shared contract; every "§" below refers to it unless it names another file. Every P-sub-phase edits only `sdks/python/` and runs `just check-py`, then `just check`.

**Package layout at the end of the track** (`sdks/python/src/llmobs/`):

| Module | Content | Sub-phase |
|---|---|---|
| `__init__.py` | public API re-exports, `__version__`, `__all__` | P1, then each sub-phase adds its names |
| `_config.py` | configuration resolution (exists) | P1 |
| `_log.py` | the `llmobs` logger and `warn_once` | P1 |
| `_serialize.py` | § Serialization, attribute value conversion | P2 |
| `_runtime.py` | `Runtime`, `init`, `flush`, `shutdown`, `tracer_provider`, `span_processor`, test seam | P3 |
| `_processor.py` | `LlmobsSpanProcessor` (stamping on start, filtered batch export on end) | P3, P4, P8 |
| `_context.py` | `context()`, the context key, merge rules, baggage | P4 |
| `_spans.py` | `Span` handle, `span()`, `observe`, `current_trace_id`, `current_span_id` | P5 |
| `_generators.py` | traced generator and async generator wrappers | P6 |
| `_llm.py` | `LlmSpan`, `llm_span`, messages conversion | P7 |
| `_mask.py` | capture-time masking helper and the export-time masking exporter | P4 (helper), P8 (exporter) |
| `_filter.py` | default export filter | P8 |
| `_scores.py` | `score()`, UUIDv7, the score queue and transport | P9 |
| `_instrument.py` | the instrumentor registry and wiring | P10 |

Tests live in `sdks/python/tests/`, one file per module (`test_<module>.py`), plus `conftest.py` (P3) and `_conformance.py` + `test_conformance.py` (P11). Tests use only the standard library, `pytest`, OTel's `InMemorySpanExporter`, and (P10) the instrumentation dev dependencies. Async tests call `asyncio.run(...)` inside a sync test function; do not add `pytest-asyncio`.

**Rules for every P-sub-phase:**
- Public functions and classes have docstrings and full type hints; `mypy --strict` passes without `type: ignore` except where a sub-phase allows one.
- Anything that can fail after `init()` is wrapped so it never raises into the app (spec 10 rule 4): catch `Exception` (never `BaseException`) at the API boundary, log at debug level with the exception type name only, and continue with a no-op. Ruff's `BLE001` is suppressed with `# noqa: BLE001` and a comment `# never raise into the app (spec 10 rule 4)` on exactly those lines.
- Log through `_log.logger` (`logging.getLogger("llmobs")`). Never log values, keys, ids of user data (trace ids are fine), or exception messages from user code.

## P1 — Dependencies, missing-key change, logging

**Commit:** `sdk-py: add OpenTelemetry dependencies and warn on a missing key`

**Edit:** `sdks/python/pyproject.toml`, `sdks/python/uv.lock`, `sdks/python/src/llmobs/_config.py`, `sdks/python/src/llmobs/_log.py` (new), `sdks/python/src/llmobs/__init__.py`, `sdks/python/tests/test_config.py`, `sdks/python/tests/test_log.py` (new).

1. **Dependencies.** In `[project] dependencies` add, in this order: `"opentelemetry-api>=1.45.2,<2"`, `"opentelemetry-sdk>=1.45.2,<2"`, `"opentelemetry-exporter-otlp-proto-http>=1.45.2,<2"`, `"requests>=2.32"`. Add to the `dev` group `types-protobuf` and `types-requests` (`uv add --dev types-protobuf types-requests`; report the versions): protobuf and requests ship no type information and later sub-phases import them under `mypy --strict`. Run `uv lock` then `uv sync`. The instrumentor extras come in P10. Do not add dev dependencies here. Keep the `"Private :: Do Not Upload"` classifier; the package is not released.
2. **`_log.py`:**
   ```python
   logger = logging.getLogger("llmobs")
   def warn_once(key: str, message: str) -> None: ...
   def reset_warnings() -> None: ...  # tests only
   ```
   `warn_once` logs `message` at WARNING the first time a `key` is seen in the process (a module-level `set[str]` guarded by a `threading.Lock`). The logger gets no handlers (library convention: `logger.addHandler(logging.NullHandler())` once at import).
3. **`_config.py` change (D-F-3):**
   -  when the API key is missing (argument `None` and env unset/empty), `resolve_config` no longer raises. It calls `warn_once("missing-api-key", "llmobs: no API key; set LLMOBS_API_KEY, or LLMOBS_ENABLED=false to silence this; tracing is disabled")` and returns the disabled config (`_disabled()`). It still validates nothing else in that case. A key without the prefix still raises as today.
   - Every other rule stays as written.
4. **`__init__.py`:** export `ConfigError` (from `_config`) in addition to `__version__`.
5. **Tests:**
   - `test_config.py`: replace the missing-key test(s) with: missing key → `config.enabled is False`, one WARNING record on logger `llmobs` with the exact message, and a second call logs nothing more (call `reset_warnings()` in a fixture before each test); an invalid prefix still raises. The error messages contain no values (existing helper assertions).
   - `test_log.py`: `warn_once` logs once per key, different keys log separately, thread safety (10 threads × same key → one record).

**Checks:** `just check-py`, `just check`.

## P2 — Serialization

**Commit:** `sdk-py: add the tolerant payload serializer`

**Edit:** `src/llmobs/_serialize.py` (new), `tests/test_serialize.py` (new).

```python
Format = Literal["text", "json"]
MAX_PAYLOAD_BYTES: Final = 1_048_576
MAX_DEPTH: Final = 32

def serialize(value: object) -> tuple[str, Format]: ...
def to_jsonable(value: object) -> object: ...          # the § Serialization rule 2–4 conversion, no truncation
def truncate(text: str, fmt: Format) -> tuple[str, Format]: ...
def attribute_value(value: object) -> str | bool | int | float: ...   # metadata values, § Span attributes
```

- `to_jsonable` implements § Serialization rules 2–4 recursively and returns only `None`, `bool`, `int`, `float`, `str`, `list`, `dict[str, …]`. Track the current path with a `set[int]` of `id()`s of containers (dict, list, tuple, set, frozenset, dataclass instances, objects converted through `model_dump`/numpy); depth counts containers.
- Order of checks (first match wins): `None`; `bool` (before `int`); `int`; `float`; `str`; `bytes`/`bytearray`/`memoryview`; `datetime.datetime`/`date`/`time`; `uuid.UUID`/`decimal.Decimal`/`pathlib.PurePath`; `enum.Enum`; dataclass instance (`dataclasses.is_dataclass(v) and not isinstance(v, type)`); callable `model_dump`; numpy (`type(v).__module__ == "numpy"` and `hasattr(v, "item")` with `getattr(v, "ndim", 0) == 0` → `item()`, else `hasattr(v, "tolist")` → `tolist()`); `BaseException`; `Mapping`; `list`/`tuple`; `set`/`frozenset`; everything else → `repr`.
- `serialize`: a top-level `str` → `truncate(value, "text")`; else `json.dumps(to_jsonable(value), separators=(",", ":"), ensure_ascii=False, allow_nan=False)` → `truncate(text, "json")`. Any exception → `(f"<unserializable {type(value).__name__}>", "text")`.
- `truncate`: if `len(text.encode("utf-8")) <= MAX_PAYLOAD_BYTES` return unchanged; else encode, cut at `MAX_PAYLOAD_BYTES`, decode with `errors="ignore"` to drop a split character, append `f"…[llmobs: truncated, original {n} bytes]"`, format `"text"`.
- `attribute_value`: `str` → itself (truncated with `truncate(…, "text")[0]`); `bool` → itself; `int` with `abs(v) <= 2**53 - 1` → itself; finite `float` → itself; anything else → `serialize(v)[0]`.

**Tests** (table-driven where possible): every row of rule 2 including nested combinations; `NaN`/`inf`; a dataclass with a nested pydantic-like object (define a small class with `model_dump(mode=...)` in the test; do not install pydantic); a `model_dump` that raises → `repr`; a fake numpy scalar and array (classes whose `__module__` is set to `"numpy"`); a dict with non-str keys; a self-referencing list and dict → `"[llmobs: cycle]"`; the same list referenced twice (not a cycle) → serialized twice; depth 33 → marker at the right level, depth 32 → no marker; a `str` of exactly 1 048 576 bytes unchanged; 1 048 577 bytes → marker with `original 1048577 bytes`; a multi-byte character straddling the cut (`"é" * 600_000`) → no broken character and valid UTF-8; JSON over the limit → format `text`; an object whose `__repr__` raises → `"<unserializable Foo>"`; `attribute_value` for each branch including `2**53` → JSON string `"9007199254740992"`.

**Checks:** `just check-py`, `just check`.

## P3 — Runtime, provider, exporter, lifecycle

**Commit:** `sdk-py: add init, the provider and the OTLP exporter`

**Edit:** `src/llmobs/_runtime.py` (new), `src/llmobs/_processor.py` (new), `src/llmobs/__init__.py`, `tests/conftest.py` (new), `tests/test_runtime.py` (new).

**`_processor.py`** — `class LlmobsSpanProcessor(SpanProcessor)`:
- `__init__(self, exporter: SpanExporter, *, environment: str | None, release: str | None, export_filter: Callable[[ReadableSpan], bool] | None, default_filter: bool)`. It owns a `BatchSpanProcessor(exporter)` with OTel's default queue and batch sizes, `schedule_delay_millis=1000`, `export_timeout_millis=10_000`.
- `on_start(span, parent_context=None)`: sets `llmobs.environment` / `llmobs.release` when not `None`. (P4 adds the context values here.) Wrapped per the "never raise" rule.
- `on_end(span)`: forwards to the batch processor. (P8 adds the filter here.)
- `force_flush(timeout_millis=30_000) -> bool` and `shutdown() -> None`: delegate.

**`_runtime.py`:**

```python
class Runtime:
    config: Config
    provider: TracerProvider                 # ours, or the global SDK provider in global mode
    owns_provider: bool
    processor: LlmobsSpanProcessor
    tracer: Tracer                           # provider.get_tracer("llmobs", __version__)
    @classmethod
    def create(cls, config: Config, *, exporter: SpanExporter | None = None,
               id_generator: IdGenerator | None = None) -> "Runtime": ...
    def flush(self, timeout_millis: int = 30_000) -> bool: ...
    def shutdown(self) -> None: ...          # idempotent

def init(*, api_key: str | None = None, base_url: str | None = None, environment: str | None = None,
         release: str | None = None, capture_content: bool | None = None, sample_rate: float | None = None,
         instrument: Sequence[str] | None = None, mask: Callable[[Any], Any] | None = None,
         enabled: bool | None = None) -> None    # public; P8 adds use_global_provider and export_filter
def flush() -> None                           # public
def shutdown() -> None                        # public
def tracer_provider() -> trace.TracerProvider # public; NoOpTracerProvider() when not running
def span_processor() -> SpanProcessor | None  # public; our processor or None
def current() -> Runtime | None               # internal: the running runtime, None when absent/shut down/disabled
def install(runtime: Runtime) -> None         # internal + test seam
def reset() -> None                           # tests only: shut down and forget the runtime, clear the init-called flag and warn_once (never the atexit flag)
```

- `Runtime.create`, own-provider mode (`use_global_provider=False`):
  - exporter: `exporter` if given (tests), else `OTLPSpanExporter(endpoint=f"{base_url}/v1/traces", headers={"Authorization": f"Bearer {api_key}"}, timeout=10, compression=Compression.Gzip)`;
  - resource: `Resource.create({"telemetry.distro.name": "llmobs", "telemetry.distro.version": __version__})` (OTel's `Resource.create` adds `telemetry.sdk.*`, reads `OTEL_SERVICE_NAME` and `OTEL_RESOURCE_ATTRIBUTES`);
  - sampler: `ParentBased(root=TraceIdRatioBased(config.sample_rate))`;
  - limits: `SpanLimits(max_span_attributes=10_000)`;
  - `TracerProvider(resource=…, sampler=…, span_limits=…, id_generator=id_generator or RandomIdGenerator())`, then `add_span_processor(processor)`. Never call `trace.set_tracer_provider`.
- Global mode is added in P8; until then `create` ignores `use_global_provider`.
- `init(...)`:
  1. If the module-level `_init_called` flag is set, `warn_once("init-twice", "llmobs: init() was already called; this call is ignored")` and return (D-F-15). Otherwise set it, before anything else, so a disabled first `init()` also counts.
  2. `config = resolve_config(**options)` (raises `ConfigError`).
  3. If `config.enabled` is false, install nothing and return (no-op mode).
  4. `install(Runtime.create(config))`, then `atexit.register(shutdown)` guarded by a module-level `_atexit_registered` flag that `reset()` never clears.
- `flush()`: `current()` → `runtime.flush()`; no-op otherwise. Never raises.
- `shutdown()`: idempotent; flushes, then `processor.shutdown()` and, when `owns_provider`, `provider.shutdown()`. After it, `current()` returns `None` (so the API is a no-op); `_init_called` stays set (D-F-15).
- `Runtime.flush` returns `processor.force_flush(timeout_millis)` (P9 adds the score queue).

**`tests/conftest.py`:**
- An autouse fixture that calls `_runtime.reset()` before and after every test and sets `OTEL_SERVICE_NAME=conformance` with `monkeypatch`.
- A fixture `memory` returning `(exporter: InMemorySpanExporter, start: Callable[..., Runtime])` where `start(**options)` builds `resolve_config(**{"api_key": "sk-llmobs-test", "base_url": "http://127.0.0.1:9", **options}, env={})`, then `Runtime.create(config, exporter=exporter, id_generator=SequentialIds())`, then `install(...)`. `SequentialIds` (in `conftest.py`, subclassing `opentelemetry.sdk.trace.id_generator.IdGenerator`) returns trace ids and span ids from two counters starting at 1.
- A helper `spans(exporter)` that calls `_runtime.flush()` and returns `exporter.get_finished_spans()`.

**Tests (`test_runtime.py`):**
- `init(enabled=False)` installs nothing; `tracer_provider()` is a `NoOpTracerProvider`; `span_processor()` is `None`.
- `init()` with no `api_key` argument and `LLMOBS_API_KEY` removed from the environment (`monkeypatch.delenv`) → nothing installed, and the D-F-3 warning logged once.
- `init(api_key=..., base_url=...)` with a real exporter: `tracer_provider()` is an SDK `TracerProvider` that is **not** the global one (`trace.get_tracer_provider()` is unchanged); `span_processor()` is the `LlmobsSpanProcessor`.
- A span started with `runtime.tracer` is exported with scope name `llmobs` and version `__version__`; the resource contains `telemetry.distro.name`, `telemetry.distro.version` and `service.name = conformance`.
- `environment`/`release` stamped on every span; absent when not configured.
- `sample_rate=0` → no span exported; a child of a sampled remote parent context is still sampled (ParentBased).
- A second `init` warns once and keeps the first runtime; `init(enabled=False)` followed by `init(api_key=…)` also warns and installs nothing.
- `shutdown()` twice is fine; spans after shutdown are not exported; `flush()` after shutdown is a no-op.
- The OTLP exporter is built with the URL `<base_url>/v1/traces`, the bearer header and gzip: start a `http.server.ThreadingHTTPServer` on `127.0.0.1:0` in a thread that records method, path, headers and body; `init(base_url=f"http://127.0.0.1:{port}", api_key="sk-llmobs-test")`, create one span, `flush()`; assert `POST /v1/traces`, `Authorization: Bearer sk-llmobs-test`, `Content-Encoding: gzip`, `Content-Type: application/x-protobuf`, and that the gunzipped body parses as `ExportTraceServiceRequest` (from `opentelemetry.proto.collector.trace.v1.trace_service_pb2`) with one span. Put the server helper in `conftest.py` as fixture `http_recorder` (P9 and P10 extend it); it records each request as `(method, path, headers, body)` and answers `200` with an empty body by default; a test may queue responses as `(status, headers: dict[str, str], body: bytes)` tuples, answered in order.
- `atexit`: patch `atexit.register` and check it is called once with `shutdown` across two `init` calls.

**Checks:** `just check-py`, `just check`.

## P4 — `context()` and the context processor

**Commit:** `sdk-py: add context() and stamp context values on spans`

**Edit:** `src/llmobs/_context.py` (new), `src/llmobs/_mask.py` (new), `src/llmobs/_processor.py`, `src/llmobs/__init__.py`, `tests/test_context.py` (new), `tests/test_mask.py` (new).

**`_mask.py` (capture-time part):** `def apply(value: object) -> object` — when a runtime is running and its config has `mask`, return `mask(value)`; if `mask` raises, return `"[llmobs: mask failed]"` and log at debug `"llmobs: mask raised <ExceptionType>"`. Without a mask, return `value`. Tests in `test_mask.py`: no runtime, no mask, a mask, a raising mask.

```python
@dataclass(frozen=True, slots=True)
class ContextValues:
    session_id: str | None = None
    user_id: str | None = None
    tags: tuple[str, ...] = ()
    metadata: Mapping[str, object] = field(default_factory=dict)   # an immutable copy (MappingProxyType)

CONTEXT_KEY: Final = create_key("llmobs.context")

class context:  # noqa: N801 (public API name from spec 10)
    def __init__(self, *, session_id: str | None = None, user_id: str | None = None,
                 tags: Sequence[str] | None = None, metadata: Mapping[str, object] | None = None,
                 propagate: bool = False) -> None: ...
    def __enter__(self) -> None: ...
    def __exit__(self, *exc: object) -> None: ...
    async def __aenter__(self) -> None: ...
    async def __aexit__(self, *exc: object) -> None: ...

def current_values(ctx: Context | None = None) -> ContextValues: ...
```

- Arguments are validated in `__init__` **without raising**: a non-`str` `session_id`/`user_id` is ignored (debug log), `tags` must be a sequence of `str` (a bare `str` is treated as one tag; non-`str` items are skipped), `metadata` must be a `Mapping` with `str` keys (others skipped). Masking: each metadata value is passed through `_mask.apply` when the block is entered.
- `__enter__`: compute the merged values from `current_values()` and the new ones (D-F-12: session/user inner wins when given; tags union keeping order, outer first, duplicates dropped; metadata `{**outer, **inner}`); build `ctx = set_value(CONTEXT_KEY, merged)`; when `propagate=True`, also `baggage.set_baggage("llmobs.session.id", …, context=ctx)` and `"llmobs.user.id"` for the values that are set; `self._token = attach(ctx)`. `__exit__`: `detach(self._token)`. The async variants do the same. A `context` object is not reentrant: entering one that is already entered logs at debug level and does nothing (its `__exit__` then does nothing either); keep a per-object depth counter to tell them apart.
- `current_values(ctx)`: `get_value(CONTEXT_KEY, ctx)` or an empty `ContextValues`.
- **Processor (`LlmobsSpanProcessor.on_start`):** read `values = current_values(parent_context)`; set `llmobs.session.id` from `values.session_id`, else from `baggage.get_baggage("llmobs.session.id", parent_context)` when it is a `str`; the same for `llmobs.user.id`; `llmobs.tags` as a list when non-empty; each metadata entry as `llmobs.metadata.<key>` with `attribute_value(value)`, skipping keys that fail the key rule (§ Span attributes). Then environment and release.
- Export `context` from `__init__.py`.

**Tests:** values on a span inside a block, on its child, and on a span started with the runtime's tracer by "third-party" code (call `provider.get_tracer("thirdparty").start_as_current_span(...)` inside the block → stamped); nesting merge (session override, tag union order, metadata override); exit restores the outer values; async: two concurrent tasks under `asyncio.gather`, each in its own `context(...)`, never see each other's values; `contextvars.copy_context().run(fn)` in a `ThreadPoolExecutor` keeps the values (the documented pattern, D-F-14) while a plain `executor.submit(fn)` does not (skip that assertion when `getattr(sys.flags, "thread_inherit_context", False)` is true, as on free-threaded builds); `propagate=True` sets the two baggage keys and nothing else (tags/metadata never in baggage); a span started under a context that only has baggage (no `context()` block: build it with `baggage.set_baggage` and `attach`) gets session/user from baggage; invalid arguments are dropped silently; metadata `{"seats": 3, "obj": {"a": 1}, "bad key": 1}` → `llmobs.metadata.seats = 3`, `llmobs.metadata.obj = '{"a":1}'`, no attribute for `bad key`; the processor never raises when the context holds garbage (put a non-`ContextValues` object under the key).

**Checks:** `just check-py`, `just check`.

## P5 — `span()` and `observe` for functions and coroutines

**Commit:** `sdk-py: add span() and observe`

**Edit:** `src/llmobs/_spans.py` (new), `src/llmobs/__init__.py`, `tests/test_spans.py` (new), `tests/test_observe.py` (new).

**`_spans.py`:**

```python
SpanKind = Literal["llm", "embedding", "tool", "agent", "chain", "retriever", "reranker", "guardrail", "evaluator", "span"]

class Span:
    """Handle on one of our spans. Every method is a no-op after the span ended and never raises."""
    @property
    def trace_id(self) -> str | None: ...     # 32 lowercase hex, None for a no-op handle
    @property
    def span_id(self) -> str | None: ...      # 16 lowercase hex
    def set_input(self, value: object) -> None: ...
    def set_output(self, value: object) -> None: ...
    def set_metadata(self, **values: object) -> None: ...

class span:  # noqa: N801
    def __init__(self, name: str, *, kind: SpanKind = "chain", input: object = _UNSET,
                 metadata: Mapping[str, object] | None = None) -> None: ...
    def __enter__(self) -> Span: ...
    def __exit__(self, exc_type, exc, tb) -> None: ...   # returns None: never swallows
    async def __aenter__(self) -> Span: ...
    async def __aexit__(self, exc_type, exc, tb) -> None: ...

def observe(func=None, /, *, name: str | None = None, kind: SpanKind = "chain",
            capture_input: bool = True, capture_output: bool = True): ...   # P6 adds reduce_output
def current_trace_id() -> str | None: ...
def current_span_id() -> str | None: ...
```

Typing of `observe`: two `@overload`s (bare decorator taking `Callable[P, R]` and returning `Callable[P, R]`; and the factory form returning `Callable[[Callable[P, R]], Callable[P, R]]`), using `ParamSpec`/`TypeVar`.

Behaviour:
- **Kind check:** `kind` not in the `SpanKind` values → `ValueError("kind: must be one of …")` raised by `observe(...)` / `span(...)` itself (decoration time / construction time).
- **No runtime** (`_runtime.current()` is `None`): `span` yields a no-op `Span` whose properties are `None`; `observe` calls the function directly. The check happens at **call** time, so functions decorated before `init()` are traced after it.
- **Starting a span:** `runtime.tracer.start_span(name)` with the current context as parent, then `trace.use_span`-equivalent: `token = attach(set_span_in_context(otel_span))`, detached at the end. Set `llmobs.span.kind`. When `capture_content` is true and an input is given: `text, fmt = serialize(apply(value))`, set `llmobs.input` and `llmobs.input.format`. Same for output.
- `set_metadata(**values)`: for each key passing the key rule, `llmobs.metadata.<key> = attribute_value(apply(value))` (masked per value). Metadata is recorded even with `capture_content=false` (it is not a payload).
- **Errors:** § Span attributes "Errors", then re-raise.
- **observe, sync function:** `inspect.iscoroutinefunction(func)` false and not a generator function (P6). At call: start the span named `name or func.__qualname__`; if `capture_input`: the input is D-F-11's bound-arguments object (below); call `func`; if `capture_output`: record the return value (a `str` → text, `None` → `null` json, a generator or async generator object → **not recorded** (D-F-11), else json); end the span.
- **observe, coroutine function:** the wrapper is `async def`; the span is started when the coroutine is first awaited (inside the wrapper body), the context attached for the whole `await func(...)`, then detached.
- **Bound arguments (D-F-11):** `sig = inspect.signature(func)` computed once at decoration; at call `bound = sig.bind(*args, **kwargs)`; `bound.apply_defaults()`; build a dict from `bound.arguments` in parameter order: drop the first parameter when it is named `self` or `cls`; a `VAR_POSITIONAL` parameter becomes a list; a `VAR_KEYWORD` parameter stays a dict under its name. If `bind` raises `TypeError`, record `{"args": list(args), "kwargs": kwargs}` and still call the function (which will raise the real error).
- `functools.wraps` on every wrapper. Methods, `staticmethod` and `classmethod` work when `@observe` is the innermost decorator (test it).
- `current_trace_id()` / `current_span_id()`: from `trace.get_current_span().get_span_context()` when `is_valid`, formatted with `format(id, "032x")` / `format(id, "016x")`; else `None`. They work for any current span (ours or an instrumentor's).

**Tests (`test_spans.py`, `test_observe.py`):** use the `memory` fixture. Kind default and every valid kind; an invalid kind raises before the function runs; input/output for str/None/dict/list/bytes/dataclass; `capture_input=False`, `capture_output=False`, and `init(capture_content=False)` (no payload attributes, metadata kept); name default `Class.method` qualname; `self`/`cls` dropped; defaults applied; `*args`/`**kwargs` shapes; positional-only parameters; a bind failure records the fallback shape and propagates the `TypeError`; exception → status, event attributes, re-raise, and `exception.type == "ScenarioError"` (the class `__name__`) for a class defined inside the test function; nested `observe` → parent/child ids; `span` inside `observe` and vice versa; `async def` with two concurrent calls under `gather` → two siblings with the right parent; `async with span(...)`; `set_metadata` masked and key rule; mask applied to input and output (`mask=lambda v: "x"`) and a raising mask → `"[llmobs: mask failed]"`; decorated before `init()` then called after → traced; called before `init()` → not traced and returns the result; `current_trace_id()` inside and outside a span; the `Span` handle after the block ended ignores `set_output`.

**Checks:** `just check-py`, `just check`.

## P6 — Generators and async generators

**Commit:** `sdk-py: trace generators and async generators`

**Edit:** `src/llmobs/_generators.py` (new), `src/llmobs/_spans.py`, `tests/test_generators.py` (new), `tests/test_observe.py`.

`observe` gains the keyword `reduce_output: Callable[[list[object]], object] | None = None` (accepted for every function, used only for generator functions). It detects `inspect.isgeneratorfunction(func)` and `inspect.isasyncgenfunction(func)` at decoration. The wrapper is a plain function that binds the input at call time and returns a `TracedGenerator` / `TracedAsyncGenerator` around `func(*args, **kwargs)`. When no runtime is running at call time, it returns the inner generator unchanged.

`class TracedGenerator(Generator[Y, S, R])` — fields: the inner generator, the runtime, name, kind, the captured input (already serialized, or `None`), `capture_output`, `reduce_output`, `items: list[object]`, `span: otel Span | None`, `ctx: Context | None`, `done: bool`.
- `__iter__` returns self; `__next__()` = `send(None)`.
- `send(value)`, `throw(...)`: on the first step, start the span with the current context as parent, set kind and input, and keep `ctx = set_span_in_context(span, current context)`. Every step: `token = attach(ctx)`; call `inner.send(value)` / `inner.throw(...)`; `detach(token)` in `finally`. A yielded item is appended to `items` when `capture_content and capture_output`. `StopIteration` → end the span (status unset) with the output, then re-raise. Any other exception → error per § Span attributes, end, re-raise.
- `close()`: if the span started and not done → `inner.close()` (with `ctx` attached), set status ok, record the output from the items so far, end. If never started → `inner.close()` only.
- `__del__`: if started and not done, call `close()` inside `try/except Exception` (never raise from a finalizer).
- **Output:** no items → no output attribute; every item a `str` → `"".join(items)` with format text; otherwise the list through `serialize` (json). With `reduce_output`: `serialize(apply(reduce_output(items)))`; if `reduce_output` raises, fall back to the default rule and log at debug.
- `TracedAsyncGenerator(AsyncGenerator[Y, S])`: the same with `__aiter__`, `__anext__` = `asend(None)`, `asend`, `athrow`, `aclose`, and `StopAsyncIteration`. No `__del__`: an async generator cannot be closed from a finalizer. An async generator that is abandoned half-way (never exhausted, never `aclose()`d) leaves its span unended, so it is never exported. Document this in the docstring and in P12's README, and test that abandoning one raises nothing and exports nothing.

**Tests:** the five behaviours of spec 10's generator bullet for both kinds: span starts at first `next` (a span created before the first `next` is not its child; check with `start_time` ordering and that a span created between `gen = f()` and `next(gen)` is a sibling); the generator's span is current only while the body runs (code in the consumer's loop sees the outer span as current); exhaustion → status unset and joined text; `break` → status ok and partial items; exception inside the body → error; `throw()` from the consumer → error; non-str items → JSON list; `reduce_output`; `capture_content=false` → no items kept (check `items` stays empty with a generator of 10 000 items); the `generator-text`/`generator-json` scenario shapes; a plain function returning a generator → a call span with no output, and the generator is returned untouched (not consumed, not traced); document in the docstring that `inspect.isasyncgenfunction(wrapped)` is false for a wrapped async generator function; a generator dropped without iteration → no span, no warning; GC of a half-consumed sync generator ends the span with ok (`del gen; gc.collect()`).

**Checks:** `just check-py`, `just check`.

## P7 — `llm_span`

**Commit:** `sdk-py: add the manual LLM span`

**Edit:** `src/llmobs/_llm.py` (new), `src/llmobs/__init__.py`, `tests/test_llm.py` (new).

```python
class LlmSpan(Span):
    def set_input_messages(self, messages: Sequence[object]) -> None: ...
    def set_output_messages(self, messages: Sequence[object]) -> None: ...
    def set_response_model(self, model: str) -> None: ...
    def set_usage(self, *, input_tokens: int | None = None, output_tokens: int | None = None,
                  cache_read_tokens: int | None = None, cache_write_tokens: int | None = None,
                  reasoning_tokens: int | None = None) -> None: ...
    def set_cost(self, total_usd: float) -> None: ...
    def record_first_token(self) -> None: ...

class llm_span:  # noqa: N801 — context manager like span, yields LlmSpan
    def __init__(self, name: str, *, provider: str, model: str,
                 input_messages: Sequence[object] | None = None,
                 system_instructions: str | Sequence[object] | None = None,
                 params: Mapping[str, object] | None = None,
                 metadata: Mapping[str, object] | None = None) -> None: ...
```

- Writes exactly § LLM span. `provider`/`model` empty or not `str` → `ValueError` at construction (programming error, like `kind`).
- `set_input`/`set_output` inherited from `Span` remain available and write `llmobs.input`/`llmobs.output` (documented as "for non-chat payloads"); the README example uses messages.
- Message conversion is a pure function `to_canonical_messages(messages: Sequence[object]) -> list[object]` applied after `apply(mask)` on the whole list.
- `set_usage` skips `None`, `bool`, negative and non-`int` values (debug log, counts only).
- `record_first_token`: `time.time_ns() - start_time_ns` where the start is the OTel span's `start_time`; only the first call writes.

**Tests:** the `llm-span` scenario's attributes exactly; system instructions as a string and as parts; `params` filtering; usage edge cases; `set_cost`; `record_first_token` once; messages with `parts` kept, short form converted with `name`/`finish_reason` order, unknown shapes kept; `capture_content=false` → no message attributes but model/usage kept; mask over messages; invalid provider raises; exception in the body → error and re-raise; no runtime → no-op handle.

**Checks:** `just check-py`, `just check`.

## P8 — Masking pass, export filter, global mode

**Commit:** `sdk-py: mask instrumentor payloads, filter exports, support the global provider`

**Edit:** `src/llmobs/_config.py`, `src/llmobs/_mask.py`, `src/llmobs/_filter.py` (new), `src/llmobs/_processor.py`, `src/llmobs/_runtime.py`, `tests/test_config.py`, `tests/test_mask.py`, `tests/test_filter.py` (new), `tests/test_runtime.py`.

**Options.** `Config` gains `use_global_provider: bool` and `export_filter: Callable[[ReadableSpan], bool] | None` (import `ReadableSpan` from `opentelemetry.sdk.trace`). `resolve_config` and `init` gain the keywords `use_global_provider: bool | None = None` (default `False`; a non-`bool` → `ConfigError("use_global_provider: must be a boolean")`; no env var) and `export_filter: Callable[[ReadableSpan], bool] | None = None` (not callable → `ConfigError("export_filter: must be callable")`). Both are ignored when the config is disabled. Tests in `test_config.py`: defaults, accepted values, both errors without values in the message.

**Masking exporter** (`_mask.py`): `class MaskingExporter(SpanExporter)` wrapping the real exporter, installed by `Runtime.create` only when `config.mask is not None or not config.capture_content`.
- `export(spans)`: build a new list; for each span whose `instrumentation_scope` is `None` or has a name other than `"llmobs"` and that has at least one payload key (§ Payload keys) in its attributes or its events' attributes, replace it with a copy: `ReadableSpan(name=s.name, context=s.context, parent=s.parent, resource=s.resource, attributes=<new>, events=[Event(e.name, <new attrs>, e.timestamp) …], links=s.links, kind=s.kind, status=s.status, start_time=s.start_time, end_time=s.end_time, instrumentation_scope=s.instrumentation_scope)`. Other spans pass through unchanged. Then `inner.export(new_list)`.
- New attributes: for a payload key with a `str` value: with `capture_content=false` → drop; else decode (`json.loads` when the stripped text starts with `{` or `[` and parses, else the string), `apply(mask)`, then re-encode: a `str` result as is, anything else `serialize(...)[0]`. A non-`str` value under a payload key → drop. Other keys unchanged.
- `shutdown()`, `force_flush()` delegate. `export` never raises. On an unexpected error it fails closed: it drops the whole batch (never exports it unmasked), returns `SpanExportResult.FAILURE`, and logs at warning `"llmobs: masking failed; dropped <n> spans"`.

**Filter** (`_filter.py`): `def default_filter(span: ReadableSpan) -> bool` per D-F-5 (scope name `llmobs` or `ai`; or any attribute key equal to `input.value`/`output.value` or starting with one of the prefixes; a span without a scope or attributes is judged on what it has). In `LlmobsSpanProcessor.on_end`: if `export_filter` is set, export when it returns truthy (if it raises, export and log at debug); else if `default_filter` mode is on (global mode), export when `default_filter(span)`; else export.

**Global mode** (`Runtime.create` with `use_global_provider=True`):
- `existing = trace.get_tracer_provider()`, then three cases:
  1. an `opentelemetry.sdk.trace.TracerProvider`: `existing.add_span_processor(processor)`, `provider = existing`, `owns_provider = False`, processor built with `default_filter=True`; if `sample_rate < 1`, `warn_once("global-sample-rate", "llmobs: sample_rate is ignored with use_global_provider=True and an existing provider")`;
  2. an `opentelemetry.trace.ProxyTracerProvider` (nothing registered yet): build our provider as in P3 with `default_filter=True` and `trace.set_tracer_provider(provider)`; `owns_provider = True`;
  3. anything else (a non-SDK provider): `warn_once("global-provider-unsupported", "llmobs: the global tracer provider cannot take span processors; llmobs keeps its own provider")` and build our own provider as in own mode (`default_filter=False`, not registered).
- `shutdown()` in global mode with an existing provider: only `processor.shutdown()` (the app owns the provider).

**Tests:**
- `test_mask.py`: an "instrumentor" span (tracer scope `openinference.instrumentation.openai`) with `input.value`, `llm.input_messages.0.message.content`, `gen_ai.input.messages` (JSON array), a non-payload attribute and an event carrying `gen_ai.output.messages` → each payload masked (JSON decoded → mask sees a list), the other attribute untouched; our own `llmobs` span not re-masked (count mask calls); `capture_content=false` drops the keys; a non-string payload value dropped; mask raising → marker; the pass is not installed without mask/capture-off (the inner exporter receives the very same span objects).
- `test_filter.py`: every rule of D-F-5 (one span per rule), an HTTP-like span dropped in global mode, kept in own mode; `export_filter` replacing the rule in both modes; a raising `export_filter` exports.
- `test_runtime.py` additions: global mode with an existing SDK provider (set one with `trace.set_tracer_provider` in the test; OTel allows setting it once per process, so run global-mode tests in a subprocess with `subprocess.run([sys.executable, "-c", …])` printing a JSON result, or reset `opentelemetry.trace._TRACER_PROVIDER_SET_ONCE` — use the subprocess approach, never touch OTel private state); global mode without one registers ours; a non-SDK global provider (a minimal `trace.TracerProvider` subclass) → warning and own mode; the sample-rate warning; shutdown leaves the app's provider running.

**Checks:** `just check-py`, `just check`.

## P9 — Scores

**Commit:** `sdk-py: add score() with a background queue`

**Edit:** `src/llmobs/_scores.py` (new), `src/llmobs/_runtime.py`, `src/llmobs/__init__.py`, `tests/conftest.py`, `tests/test_scores.py` (new).

```python
def score(*, name: str, value: bool | int | float | str,
          trace_id: str | None = None, span_id: str | None = None, session_id: str | None = None,
          reason: str | None = None,
          data_type: Literal["numeric", "boolean", "categorical", "text"] | None = None,
          metadata: Mapping[str, object] | None = None, user_id: str | None = None,
          score_id: str | None = None) -> str | None: ...
def uuid7() -> str: ...
```

- **Validation and body:** § Score request, exactly. Ids must match `^[0-9a-f]{32}$` / `^[0-9a-f]{16}$`; `score_id` when given must be a UUID string (`uuid.UUID(score_id)` parses) and is sent lowercase. `user_id` defaults to `current_values().user_id`. `metadata` values: `str` kept, others `serialize(apply(v))[0]`; `reason` passes through `apply` (mask). Returns the id.
- **`uuid7()`** (RFC 9562 §5.7): 48-bit big-endian Unix milliseconds, version `0b0111`, 12 random bits, variant `0b10`, 62 random bits, all random bits from `secrets.randbits`. Return `str(uuid.UUID(int=…))`.
- **`ScoreQueue`** (owned by the `Runtime`, created in `Runtime.create`):
  - `put(item: dict) -> bool`: when 10 000 items are waiting, drop and count; else append and notify. Starts the worker thread lazily (daemon, name `llmobs-scores`).
  - Worker: waits up to 1 s or until ≥ 1000 items, takes up to 1000 items, sends them, repeats; exits when shut down and empty.
  - Sending: `requests.Session` created per queue; `session.post(f"{base_url}/api/v1/scores", json={"scores": batch}, headers={"Authorization": f"Bearer {api_key}"}, timeout=10)`; response handling and warning texts exactly as § Score request (waits through an `Event.wait`, so shutdown can interrupt them; tests replace the queue's `wait` function to record durations).
  - Dropped-on-full count: logged at warning at most once per minute: `"llmobs: score buffer full; dropped <n> scores in the last minute"`.
  - `flush(timeout_s: float) -> bool`: wakes the worker and waits until the queue is empty and no batch is in flight, or the timeout.
  - `shutdown()`: flush with the remaining part of a 30 s budget, then stop the worker.
  - Fork: `os.register_at_fork(after_in_child=…)` registered once per queue (through a `weakref` to the queue), which replaces the lock/condition/event, empties the queue, forgets the thread, and creates a new `requests.Session` (D-F-15).
- `Runtime.flush` also flushes the queue; `Runtime.shutdown` shuts it down after the span processor.
- A test seam: `Runtime.create(..., score_url: str | None = None)` is **not** needed; tests point `base_url` at the `http_recorder` fixture.

**Tests:** body shape for each target and value type (compare parsed JSON with literal dicts); `data_type` forcing and mismatch; invalid inputs → warning, `None`, nothing sent; `user_id` from `context()`; mask on reason/metadata; ids are valid UUIDv7 (version and variant bits, 1000 ids unique, ms prefix non-decreasing); batching (2500 scores → batches of 1000, 1000, 500); retry on 503 then 200 → sent once more with the same ids; `Retry-After: 1` honoured (patch `time.sleep`/the wait to record durations instead of sleeping); 400 → dropped with the warning; buffer full → drop count warning once; `flush()` waits for delivery; `shutdown()` flushes; after shutdown `score()` returns `None`; fork: decorate the test with `@pytest.mark.filterwarnings("ignore:This process .* is multi-threaded:DeprecationWarning")`; in a child created with `os.fork()` (skip on platforms without it), `score()` works and the parent's pending items are not sent twice (the child exits with `os._exit` after `flush()`; assert from the parent via the recorder that the child's item arrived once); no-runtime → `None`.

**Checks:** `just check-py`, `just check`.

## P10 — Instrumentation

**Commit:** `sdk-py: wire OpenInference instrumentors`

**Edit:** `pyproject.toml`, `uv.lock`, `src/llmobs/_instrument.py` (new), `src/llmobs/_runtime.py`, `tests/conftest.py`, `tests/test_instrument.py` (new).

- **Extras.** Add `[project.optional-dependencies]` with the ten extras of § Python instrumentors (names and lower bounds from the table) and `all` listing the ten distributions. If `uv lock` fails because an extra's packages do not support the whole `requires-python` range (`>=3.11`), add an environment marker to that extra's requirement limiting it to the Python versions its metadata supports (e.g. `; python_version < "3.14"`), and list every marker in the report. Do not drop an extra.

- Dev group additions: `uv add --dev "openinference-instrumentation-openai>=0.1.69" "openinference-instrumentation-anthropic>=3.3.0" "openai>=2" "anthropic>=0.70"`. Report the resolved versions.
- `_instrument.py`: a frozen registry `dict[str, Instrumentor]` with `Instrumentor(name, extra, module, cls)` from § Python instrumentors; `def resolve(names: Sequence[str]) -> list[type]` imports each module with `importlib.import_module` and gets the class: unknown name → `ConfigError(f"instrument: unknown instrumentor {name!r}; known: {', '.join(sorted(REGISTRY))}")`; `ImportError` or missing attribute → `ConfigError(f"instrument: {name!r} needs `pip install llmobs[{extra}]`")`. (Instrumentor names are not secret; F1 records that rule 11's "never its value" does not apply to them.) `def enable_for(runtime: Runtime, classes: Sequence[type]) -> None` creates each instance; if the instance is already instrumented (`instance.is_instrumented_by_opentelemetry` is true: the app did it), log at debug and skip it; else call `.instrument(tracer_provider=runtime.provider, config=TraceConfig(hide_inputs=True if not capture_content else None, hide_outputs=True if not capture_content else None), raise_exception_on_conflict=True)` with `TraceConfig` from `openinference.instrumentation` (passing `None` keeps the user's `OPENINFERENCE_HIDE_*` env settings). A `DependencyConflictError` (from `opentelemetry.instrumentation.dependencies`) or an exception saying the instrumented library is missing → `ConfigError(f"instrument: {name!r} needs `pip install llmobs[{extra}]`")`. The runtime keeps only the instances it instrumented itself, and `shutdown` uninstruments only those. Instances are `Any`-typed (`# type: ignore[...]` is allowed only on these import lines if mypy needs it, with a comment).
- `init()`: resolve the instrumentors **before** creating the runtime (a `ConfigError` leaves nothing installed); call `enable_for(runtime, classes)` after `install`. If `enable_for` raises `ConfigError`, shut the runtime down, uninstall it and re-raise. The `memory` fixture's `start(**options)` calls the same `resolve` + `enable_for` when `instrument` is given, so tests use the production path. `Runtime.shutdown()` calls `uninstrument()` on each instance first (errors swallowed per the rule).
- Mocked provider HTTP: tests use the `http_recorder` fixture extended with a response body per path: `/v1/chat/completions` → a minimal OpenAI chat completion JSON (`{"id":"chatcmpl-1","object":"chat.completion","created":0,"model":"gpt-4o-mini","choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}],"usage":{"prompt_tokens":5,"completion_tokens":1,"total_tokens":6}}`), `/v1/messages` → a minimal Anthropic message JSON (`{"id":"msg_1","type":"message","role":"assistant","model":"claude-sonnet-4-5","content":[{"type":"text","text":"hi"}],"stop_reason":"end_turn","stop_sequence":null,"usage":{"input_tokens":5,"output_tokens":1}}`).

**Tests:** unknown name → error listing the known names; a registered name whose module is missing (monkeypatch the registry with a fake entry pointing at `"llmobs_tests_missing_module"`) → the extra hint; `init` with a bad name installs nothing; `openai` end to end: `init(instrument=["openai"])` through the `memory` fixture (extend `start` to accept `instrument`), call `openai.OpenAI(base_url=f"http://127.0.0.1:{port}/v1", api_key="test").chat.completions.create(model="gpt-4o-mini", messages=[{"role": "user", "content": "hello"}])` inside `context(session_id="s1")` and `observe` → one OpenInference `LLM` span, child of our span, stamped with `llmobs.session.id = "s1"`; the same for `anthropic`; `capture_content=false` → the instrumentor span has no `input.value`/`llm.input_messages.*`; `shutdown` uninstruments (a call after shutdown creates no span); for the eight other names, a parametrized test that `pytest.importorskip(module)` then checks the class exists.

**Checks:** `just check-py`, `just check`.

## P11 — Conformance tests

**Commit:** `sdk-py: check the conformance fixtures`

**Edit:** `tests/_conformance.py` (new), `tests/test_conformance.py` (new).

- `_conformance.py`: `def canonical(spans: Sequence[ReadableSpan]) -> dict[str, object]` implementing § Conformance: `encode_spans(spans)` (from `opentelemetry.exporter.otlp.proto.common.trace_encoder`) → `json_format.MessageToDict(request, use_integers_for_enums=True)` (from `google.protobuf`) → rewrite: ids from base64 to hex then to the creation-order mapping (creation order = the sequential ids the test's `SequentialIds` produced: sort spans by span id), times per rule 2, resource and scope per rule 3, field filtering rule 4, attribute sorting rule 5 (`intValue` stays a string — `MessageToDict` already writes int64 as strings), exception attribute reduction rule 6, span order rule 7.
- `test_conformance.py`: one test function per scenario, named from the case directory by dropping `sdk-` and replacing `-` with `_` (`sdk-observe-tool` → `test_observe_tool`); it runs the scenario's calls through the `memory` fixture (with `OTEL_SERVICE_NAME=conformance`, the init options of the scenario row), `flush()`, and asserts `canonical(spans) == json.loads(<repo>/fixtures/otlp/llmobs/sdk-<case>/input.json)`. The repository root is `Path(__file__).parents[3]`. Define `class ScenarioError(Exception)` in the test module. A last test lists `fixtures/otlp/llmobs/sdk-*` and asserts that each directory's derived name is a function in the module (so a new case cannot be forgotten).
- On mismatch, the assertion message shows the canonical JSON (pretty-printed) so the diff is readable; never rewrite the fixture from the test.

**Checks:** `just check-py`, `just check`.

## P12 — README and examples

**Commit:** `sdk-py: document the SDK`

**Edit:** `sdks/python/README.md`, `tests/test_readme.py` (new).

- README sections: Install (extras); Quick start (`init`, `observe`, `flush`); Configuration (the table from spec 10 incl. `use_global_provider`, `export_filter`); Decorator rules (inputs, outputs, generators, errors); Manual spans and `llm_span`; Context and baggage (with the privacy note); Scores; Instrumentation (the registry table); Existing OpenTelemetry setups (`use_global_provider`, `span_processor()`, the filter); Masking and `capture_content`; Threads (D-F-14: `copy_context().run`, `opentelemetry-instrumentation-threading`); Serverless (`flush()` at the end of each invocation); Known limits (abandoned async generators are not exported; `inspect.isasyncgenfunction` is false on a wrapped async generator function; in global mode our processor also stamps `llmobs.*` context attributes on the app's own spans, which its other exporters see; `OTEL_EXPORTER_OTLP_HEADERS` are sent to llmobs too, § Language differences); What is never logged.
- `test_readme.py` extracts every ```` ```python ```` block marked with a preceding `<!-- test -->` line and `exec`s it against the `memory` fixture (with `api_key="sk-llmobs-test"`), so the documented examples keep working. Mark at least the quick start, `llm_span`, `context` and `score` examples.

**Checks:** `just check-py`, `just check`.
