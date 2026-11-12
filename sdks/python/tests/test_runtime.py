"""Tests for the runtime: init, provider, exporter wiring, lifecycle (plan F § P3, P8)."""

import atexit
import gzip
import json
import logging
import os
import subprocess
import sys
from collections.abc import Callable
from typing import Any, Protocol

import pytest
from opentelemetry import trace
from opentelemetry.proto.collector.trace.v1.trace_service_pb2 import (
    ExportTraceServiceRequest,
)
from opentelemetry.sdk.trace import ReadableSpan, TracerProvider
from opentelemetry.sdk.trace.export.in_memory_span_exporter import InMemorySpanExporter
from opentelemetry.trace import (
    NonRecordingSpan,
    NoOpTracerProvider,
    SpanContext,
    TraceFlags,
    set_span_in_context,
)

import llmobs
from llmobs import __version__
from llmobs._config import ConfigError
from llmobs._processor import LlmobsSpanProcessor
from llmobs._runtime import Runtime, current

Memory = tuple[InMemorySpanExporter, Callable[..., Runtime]]
Spans = Callable[[InMemorySpanExporter], list[ReadableSpan]]


class Recorder(Protocol):
    """The surface of the ``http_recorder`` fixture used by the tests."""

    url: str
    requests: list[tuple[str, str, dict[str, str], bytes]]


INIT_TWICE_MESSAGE = "llmobs: init() was already called; this call is ignored"
MISSING_KEY_MESSAGE = (
    "llmobs: no API key; set LLMOBS_API_KEY, or LLMOBS_ENABLED=false to silence this; "
    "tracing is disabled"
)


def test_init_disabled_installs_nothing() -> None:
    llmobs.init(enabled=False)
    assert isinstance(llmobs.tracer_provider(), NoOpTracerProvider)
    assert llmobs.span_processor() is None
    assert current() is None


def test_init_without_api_key_warns_and_runs_disabled(
    caplog: pytest.LogCaptureFixture, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.delenv("LLMOBS_API_KEY", raising=False)
    with caplog.at_level(logging.WARNING, logger="llmobs"):
        llmobs.init()
    records = [record for record in caplog.records if record.name == "llmobs"]
    assert [record.getMessage() for record in records] == [MISSING_KEY_MESSAGE]
    assert isinstance(llmobs.tracer_provider(), NoOpTracerProvider)
    assert llmobs.span_processor() is None
    assert current() is None


def test_init_installs_own_provider_not_global(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("LLMOBS_API_KEY", raising=False)
    global_before = trace.get_tracer_provider()
    llmobs.init(api_key="sk-llmobs-test", base_url="http://127.0.0.1:9")
    provider = llmobs.tracer_provider()
    assert isinstance(provider, TracerProvider)
    assert trace.get_tracer_provider() is global_before
    assert trace.get_tracer_provider() is not provider
    assert isinstance(llmobs.span_processor(), LlmobsSpanProcessor)


def test_span_carries_scope_and_resource(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    runtime = start()
    with runtime.tracer.start_as_current_span("work"):
        pass
    span = spans(exporter)[0]
    assert span.instrumentation_scope is not None
    assert span.instrumentation_scope.name == "llmobs"
    assert span.instrumentation_scope.version == __version__
    resource = span.resource.attributes
    assert resource["telemetry.distro.name"] == "llmobs"
    assert resource["telemetry.distro.version"] == __version__
    assert resource["service.name"] == "conformance"


def test_environment_and_release_stamped(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    runtime = start(environment="staging", release="1.2.3")
    with runtime.tracer.start_as_current_span("work"):
        pass
    attributes = spans(exporter)[0].attributes
    assert attributes is not None
    assert attributes["llmobs.environment"] == "staging"
    assert attributes["llmobs.release"] == "1.2.3"


def test_no_environment_attributes_by_default(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    runtime = start()
    with runtime.tracer.start_as_current_span("work"):
        pass
    attributes = spans(exporter)[0].attributes
    assert attributes is not None
    assert "llmobs.environment" not in attributes
    assert "llmobs.release" not in attributes


def test_sample_rate_zero_drops_roots(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    runtime = start(sample_rate=0)
    with runtime.tracer.start_as_current_span("dropped"):
        pass
    assert spans(exporter) == []


def test_sampled_remote_parent_is_recorded(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    runtime = start(sample_rate=0)
    parent = SpanContext(
        trace_id=1,
        span_id=2,
        is_remote=True,
        trace_flags=TraceFlags(TraceFlags.SAMPLED),
    )
    context = set_span_in_context(NonRecordingSpan(parent))
    with runtime.tracer.start_as_current_span("child", context=context):
        pass
    finished = spans(exporter)
    assert [span.name for span in finished] == ["child"]
    child = finished[0]
    assert child.context.trace_id == 1
    assert child.parent is not None
    assert child.parent.span_id == 2


def test_second_init_warns_once_and_keeps_the_first(
    caplog: pytest.LogCaptureFixture,
) -> None:
    llmobs.init(api_key="sk-llmobs-test", base_url="http://127.0.0.1:9")
    first = llmobs.tracer_provider()
    with caplog.at_level(logging.WARNING, logger="llmobs"):
        llmobs.init(api_key="sk-llmobs-test", base_url="http://127.0.0.1:9")
    records = [record for record in caplog.records if record.name == "llmobs"]
    assert [record.getMessage() for record in records] == [INIT_TWICE_MESSAGE]
    assert llmobs.tracer_provider() is first


def test_disabled_first_init_blocks_the_second(
    caplog: pytest.LogCaptureFixture,
) -> None:
    llmobs.init(enabled=False)
    with caplog.at_level(logging.WARNING, logger="llmobs"):
        llmobs.init(api_key="sk-llmobs-test", base_url="http://127.0.0.1:9")
    records = [record for record in caplog.records if record.name == "llmobs"]
    assert [record.getMessage() for record in records] == [INIT_TWICE_MESSAGE]
    assert isinstance(llmobs.tracer_provider(), NoOpTracerProvider)
    assert llmobs.span_processor() is None
    assert current() is None


def test_shutdown_is_idempotent_and_drops_after(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    runtime = start()
    with runtime.tracer.start_as_current_span("one"):
        pass
    llmobs.shutdown()
    llmobs.shutdown()
    assert current() is None
    with runtime.tracer.start_as_current_span("two"):
        pass
    assert [span.name for span in spans(exporter)] == ["one"]


def test_otlp_exporter_request(http_recorder: Recorder) -> None:
    llmobs.init(api_key="sk-llmobs-test", base_url=http_recorder.url)
    runtime = current()
    assert runtime is not None
    with runtime.tracer.start_as_current_span("hello"):
        pass
    llmobs.flush()
    assert len(http_recorder.requests) == 1
    method, path, headers, body = http_recorder.requests[0]
    assert method == "POST"
    assert path == "/v1/traces"
    lower = {name.lower(): value for name, value in headers.items()}
    assert lower["authorization"] == "Bearer sk-llmobs-test"
    assert lower["content-encoding"] == "gzip"
    assert lower["content-type"] == "application/x-protobuf"
    request = ExportTraceServiceRequest()
    request.ParseFromString(gzip.decompress(body))
    scope_spans = request.resource_spans[0].scope_spans[0]
    assert scope_spans.scope.name == "llmobs"
    assert len(scope_spans.spans) == 1
    assert scope_spans.spans[0].name == "hello"


def test_atexit_registered_once(monkeypatch: pytest.MonkeyPatch) -> None:
    registered: list[tuple[Any, ...]] = []

    def fake_register(*args: object) -> object:
        registered.append(args)
        return args[0]

    monkeypatch.setattr(atexit, "register", fake_register)
    # The atexit flag is process-global (reset() never clears it, D-F-15);
    # simulate a fresh process so this test does not depend on test order.
    monkeypatch.setattr(llmobs._runtime, "_atexit_registered", False)
    llmobs.init(api_key="sk-llmobs-test", base_url="http://127.0.0.1:9")
    llmobs.init(api_key="sk-llmobs-test", base_url="http://127.0.0.1:9")
    ours = [call for call in registered if call and call[0] is llmobs.shutdown]
    assert len(ours) == 1


GLOBAL_SAMPLE_RATE_MESSAGE = (
    "llmobs: sample_rate is ignored with use_global_provider=True and an existing provider"
)
GLOBAL_PROVIDER_MESSAGE = (
    "llmobs: the global tracer provider cannot take span processors; llmobs keeps its own provider"
)

# OTel allows setting the global tracer provider once per process, so every
# global-mode scenario runs in a subprocess and prints one JSON result line.
_GLOBAL_SETUP = """
import json, logging
from opentelemetry import trace
from opentelemetry.sdk.trace import TracerProvider
from opentelemetry.sdk.trace.export.in_memory_span_exporter import InMemorySpanExporter
from llmobs._config import resolve_config
from llmobs._runtime import Runtime, flush, install, shutdown

class _H(logging.Handler):
    def __init__(self):
        super().__init__()
        self.records = []
    def emit(self, record):
        self.records.append(record.getMessage())

_handler = _H()
logging.getLogger("llmobs").addHandler(_handler)
exporter = InMemorySpanExporter()

def make_runtime(**options):
    config = resolve_config(
        api_key="sk-llmobs-test",
        base_url="http://127.0.0.1:9",
        use_global_provider=True,
        env={},
        **options,
    )
    runtime = Runtime.create(config, exporter=exporter)
    install(runtime)
    return runtime

def result(**extra):
    out = {"warnings": _handler.records,
           "exported": [s.name for s in exporter.get_finished_spans()]}
    out.update(extra)
    print(json.dumps(out))
"""


def _run_script(code: str) -> dict[str, Any]:
    """Run ``code`` in a subprocess with a clean OTEL/LLMOBS environment."""
    env = {
        name: value
        for name, value in os.environ.items()
        if not name.startswith(("OTEL_", "LLMOBS_"))
    }
    env["OTEL_SERVICE_NAME"] = "conformance"
    run = subprocess.run(  # noqa: S603  # the command is test-authored, not input
        [sys.executable, "-c", code],
        capture_output=True,
        text=True,
        env=env,
        timeout=60,
    )
    assert run.returncode == 0, run.stderr
    return dict(json.loads(run.stdout.splitlines()[-1]))


def test_init_accepts_the_global_mode_options(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("LLMOBS_API_KEY", raising=False)
    llmobs.init(
        api_key="sk-llmobs-test",
        base_url="http://127.0.0.1:9",
        use_global_provider=False,
        export_filter=lambda span: True,
    )
    assert isinstance(llmobs.span_processor(), LlmobsSpanProcessor)
    assert current() is not None


def test_init_rejects_a_non_bool_use_global_provider(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    monkeypatch.delenv("LLMOBS_API_KEY", raising=False)
    with pytest.raises(ConfigError, match="use_global_provider: must be a boolean"):
        llmobs.init(api_key="sk-llmobs-test", use_global_provider="yes")  # type: ignore[arg-type]
    assert current() is None


def test_global_mode_joins_an_existing_sdk_provider() -> None:
    out = _run_script(
        _GLOBAL_SETUP
        + """
app = TracerProvider()
trace.set_tracer_provider(app)
runtime = make_runtime()
with trace.get_tracer("httpx").start_as_current_span("GET /x") as span:
    span.set_attribute("http.request.method", "GET")
with runtime.tracer.start_as_current_span("ours"):
    pass
flush()
result(owns_provider=runtime.owns_provider,
       provider_is_app=runtime.provider is app,
       global_is_app=trace.get_tracer_provider() is app)
"""
    )
    assert out["owns_provider"] is False
    assert out["provider_is_app"] is True
    assert out["global_is_app"] is True
    # The default filter drops the HTTP span of the app's provider.
    assert out["exported"] == ["ours"]
    assert out["warnings"] == []


def test_global_mode_warns_on_a_sample_rate_with_an_existing_provider() -> None:
    out = _run_script(
        _GLOBAL_SETUP
        + """
app = TracerProvider()
trace.set_tracer_provider(app)
runtime = make_runtime(sample_rate=0.5)
flush()
result(owns_provider=runtime.owns_provider)
"""
    )
    assert out["owns_provider"] is False
    assert out["warnings"] == [GLOBAL_SAMPLE_RATE_MESSAGE]


def test_global_mode_registers_our_provider_when_none_is_set() -> None:
    out = _run_script(
        _GLOBAL_SETUP
        + """
proxy = trace.get_tracer_provider()
runtime = make_runtime()
with trace.get_tracer("httpx").start_as_current_span("GET /x") as span:
    span.set_attribute("http.request.method", "GET")
with runtime.tracer.start_as_current_span("ours"):
    pass
flush()
result(owns_provider=runtime.owns_provider,
       global_is_ours=trace.get_tracer_provider() is runtime.provider,
       proxy_replaced=trace.get_tracer_provider() is not proxy)
"""
    )
    assert out["owns_provider"] is True
    assert out["global_is_ours"] is True
    assert out["proxy_replaced"] is True
    assert out["exported"] == ["ours"]
    assert out["warnings"] == []


def test_global_mode_with_a_non_sdk_provider_stays_private() -> None:
    out = _run_script(
        _GLOBAL_SETUP
        + """
from opentelemetry.trace import NoOpTracerProvider, TracerProvider as ApiProvider

class OtherProvider(ApiProvider):
    def get_tracer(self, instrumenting_module_name, *args, **kwargs):
        return NoOpTracerProvider().get_tracer(instrumenting_module_name, *args, **kwargs)

other = OtherProvider()
trace.set_tracer_provider(other)
runtime = make_runtime()
with runtime.provider.get_tracer("httpx").start_as_current_span("GET /x") as span:
    span.set_attribute("http.request.method", "GET")
flush()
result(owns_provider=runtime.owns_provider,
       provider_is_ours=runtime.provider is not other,
       global_is_other=trace.get_tracer_provider() is other)
"""
    )
    assert out["owns_provider"] is True
    assert out["provider_is_ours"] is True
    assert out["global_is_other"] is True
    # Own-mode rules: the default filter is off, everything exports.
    assert out["exported"] == ["GET /x"]
    assert out["warnings"] == [GLOBAL_PROVIDER_MESSAGE]


def test_global_mode_shutdown_leaves_the_app_provider_running() -> None:
    out = _run_script(
        _GLOBAL_SETUP
        + """
from opentelemetry.sdk.trace.export import SimpleSpanProcessor

app = TracerProvider()
app_exporter = InMemorySpanExporter()
app.add_span_processor(SimpleSpanProcessor(app_exporter))
trace.set_tracer_provider(app)
runtime = make_runtime()
app_tracer = trace.get_tracer("app")
with app_tracer.start_as_current_span("before"):
    pass
with runtime.tracer.start_as_current_span("ours"):
    pass
shutdown()
with app_tracer.start_as_current_span("after"):
    pass
result(app_exported=[s.name for s in app_exporter.get_finished_spans()])
"""
    )
    # The app's processor saw every span, ours included (branch 1 adds our
    # processor to the app's provider), and kept working after our shutdown.
    assert out["app_exported"] == ["before", "ours", "after"]
    # Our export stopped at shutdown: "after" never reaches our exporter.
    assert out["exported"] == ["ours"]
    assert out["warnings"] == []
