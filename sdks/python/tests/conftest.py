"""Shared fixtures: runtime isolation, the in-memory pipeline, the HTTP recorder (plan F § P3)."""

import threading
from collections.abc import Callable, Iterator
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from itertools import count
from typing import Any, ClassVar

import pytest
from opentelemetry.sdk.trace import ReadableSpan
from opentelemetry.sdk.trace.export.in_memory_span_exporter import InMemorySpanExporter
from opentelemetry.sdk.trace.id_generator import IdGenerator

import llmobs._runtime as runtime_module
from llmobs._config import ConfigError, resolve_config
from llmobs._instrument import enable_for, resolve
from llmobs._runtime import Runtime, install, uninstall

TEST_API_KEY = "sk-llmobs-test"
TEST_BASE_URL = "http://127.0.0.1:9"

# Minimal provider responses per request path (plan F § P10); the exact JSON
# bodies the instrumentor tests replay, byte for byte.
PATH_BODIES: dict[str, bytes] = {
    "/v1/chat/completions": (
        b'{"id":"chatcmpl-1","object":"chat.completion","created":0,"model":"gpt-4o-mini",'
        b'"choices":[{"index":0,"message":{"role":"assistant","content":"hi"},'
        b'"finish_reason":"stop"}],"usage":{"prompt_tokens":5,"completion_tokens":1,'
        b'"total_tokens":6}}'
    ),
    "/v1/messages": (
        b'{"id":"msg_1","type":"message","role":"assistant","model":"claude-sonnet-4-5",'
        b'"content":[{"type":"text","text":"hi"}],"stop_reason":"end_turn",'
        b'"stop_sequence":null,"usage":{"input_tokens":5,"output_tokens":1}}'
    ),
}


class SequentialIds(IdGenerator):
    """Trace ids and span ids from two counters starting at 1 (deterministic tests)."""

    def __init__(self) -> None:
        self._trace_ids = count(1)
        self._span_ids = count(1)

    def generate_trace_id(self) -> int:
        return next(self._trace_ids)

    def generate_span_id(self) -> int:
        return next(self._span_ids)


class HttpRecorder:
    """Records requests as (method, path, headers, body); answers queued responses in order.

    Without a queued response, a request gets 200 with the JSON body registered
    for its path (``PATH_BODIES``), or an empty body.
    """

    def __init__(self, url: str) -> None:
        self.url = url
        self.requests: list[tuple[str, str, dict[str, str], bytes]] = []
        self._responses: list[tuple[int, dict[str, str], bytes]] = []
        self._bodies: dict[str, bytes] = dict(PATH_BODIES)
        self._lock = threading.Lock()

    def queue_response(self, status: int, headers: dict[str, str], body: bytes) -> None:
        """Queue a response for the next request; without one, 200 with the path body."""
        with self._lock:
            self._responses.append((status, headers, body))

    def record(self, method: str, path: str, headers: dict[str, str], body: bytes) -> None:
        with self._lock:
            self.requests.append((method, path, headers, body))

    def take_response(self, path: str) -> tuple[int, dict[str, str], bytes]:
        with self._lock:
            if self._responses:
                return self._responses.pop(0)
            body = self._bodies.get(path, b"")
            headers = {"Content-Type": "application/json"} if body else {}
            return (200, headers, body)


class _RecordingHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    recorder: ClassVar[HttpRecorder]

    def do_POST(self) -> None:
        length = int(self.headers.get("Content-Length", "0"))
        body = self.rfile.read(length)
        headers = {name: value for name, value in self.headers.items()}
        self.recorder.record(self.command, self.path, headers, body)
        status, response_headers, response_body = self.recorder.take_response(self.path)
        self.send_response(status)
        for name, value in response_headers.items():
            self.send_header(name, value)
        self.send_header("Content-Length", str(len(response_body)))
        self.end_headers()
        self.wfile.write(response_body)

    def log_message(self, format: str, *args: object) -> None:
        return  # keep test output quiet


@pytest.fixture(autouse=True)
def isolated_runtime(monkeypatch: pytest.MonkeyPatch) -> Iterator[None]:
    """Reset the runtime state before and after every test; stable service name."""
    monkeypatch.setenv("OTEL_SERVICE_NAME", "conformance")
    runtime_module.reset()
    yield
    runtime_module.reset()


@pytest.fixture
def memory() -> tuple[InMemorySpanExporter, Callable[..., Runtime]]:
    """The in-memory pipeline: ``(exporter, start)``; ``start(**options)`` installs a runtime."""
    exporter = InMemorySpanExporter()

    def start(**options: object) -> Runtime:
        merged: dict[str, Any] = {
            "api_key": TEST_API_KEY,
            "base_url": TEST_BASE_URL,
            **options,
        }
        config = resolve_config(**merged, env={})
        # The production instrumentor path (plan F § P10): resolve before the
        # runtime exists, enable after install.
        classes = resolve(config.instrument)
        runtime = Runtime.create(config, exporter=exporter, id_generator=SequentialIds())
        install(runtime)
        try:
            enable_for(runtime, classes)
        except ConfigError:
            uninstall(runtime)
            raise
        return runtime

    return exporter, start


@pytest.fixture
def spans() -> Callable[[InMemorySpanExporter], list[ReadableSpan]]:
    """Flush the runtime, then return the exporter's finished spans."""

    def _spans(exporter: InMemorySpanExporter) -> list[ReadableSpan]:
        runtime_module.flush()
        return list(exporter.get_finished_spans())

    return _spans


@pytest.fixture
def score_runtime(http_recorder: HttpRecorder) -> tuple[HttpRecorder, Runtime]:
    """A running runtime whose score queue posts to the recorder (plan F § P9).

    The autouse ``isolated_runtime`` fixture resets it after the test.
    """
    config = resolve_config(api_key=TEST_API_KEY, base_url=http_recorder.url, env={})
    runtime = Runtime.create(config, id_generator=SequentialIds())
    install(runtime)
    return http_recorder, runtime


@pytest.fixture
def http_recorder() -> Iterator[HttpRecorder]:
    """A local HTTP server that records requests and serves queued responses."""
    server = ThreadingHTTPServer(("127.0.0.1", 0), _RecordingHandler)
    recorder = HttpRecorder(f"http://127.0.0.1:{server.server_port}")
    _RecordingHandler.recorder = recorder
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield recorder
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)
