"""Tests for the instrumentor registry and wiring (plan F § P10, D-F-7)."""

import sys
import types
from collections.abc import Callable, Mapping
from typing import Any, Protocol

import anthropic
import openai
import pytest
from anthropic.types import TextBlock
from opentelemetry.sdk.trace import ReadableSpan
from opentelemetry.sdk.trace.export.in_memory_span_exporter import InMemorySpanExporter
from opentelemetry.util.types import AnyValue

import llmobs
import llmobs._runtime as runtime_module
from llmobs._config import ConfigError
from llmobs._instrument import REGISTRY, Instrumentor, resolve
from llmobs._runtime import Runtime

Memory = tuple[InMemorySpanExporter, Callable[..., Runtime]]
Spans = Callable[[InMemorySpanExporter], list[ReadableSpan]]


class Recorder(Protocol):
    """The surface of the ``http_recorder`` fixture used by the tests."""

    url: str


OPENAI_SPAN = "ChatCompletion"
ANTHROPIC_SPAN = "messages.create"


def attrs(span: ReadableSpan) -> Mapping[str, AnyValue]:
    """The span's attributes; every span these tests create carries them."""
    assert span.attributes is not None
    return span.attributes


def one(finished: list[ReadableSpan], name: str) -> ReadableSpan:
    """The single span named ``name``."""
    matches = [span for span in finished if span.name == name]
    assert len(matches) == 1
    return matches[0]


def llm_spans(finished: list[ReadableSpan], name: str) -> list[ReadableSpan]:
    """The OpenInference spans of kind LLM among ``finished``."""
    return [
        span
        for span in finished
        if attrs(span).get("openinference.span.kind") == "LLM" and span.name == name
    ]


def test_resolve_unknown_name_lists_known_names() -> None:
    with pytest.raises(ConfigError) as info:
        resolve(["nope"])
    message = str(info.value)
    assert "instrument: unknown instrumentor 'nope'" in message
    for name in REGISTRY:
        assert name in message


def test_resolve_missing_module_names_the_extra(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    entry = Instrumentor(
        name="fake", extra="fake", module="llmobs_tests_missing_module", cls="FakeInstrumentor"
    )
    monkeypatch.setitem(REGISTRY, entry.name, entry)
    with pytest.raises(ConfigError) as info:
        resolve(["fake"])
    assert str(info.value) == "instrument: 'fake' needs `pip install llmobs[fake]`"


def test_init_with_bad_name_installs_nothing(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("LLMOBS_API_KEY", raising=False)
    with pytest.raises(ConfigError):
        llmobs.init(api_key="sk-llmobs-test", instrument=["nope"])
    assert runtime_module.current() is None


def test_conflicting_dependency_maps_to_the_extra_hint(
    monkeypatch: pytest.MonkeyPatch, memory: Memory
) -> None:
    """A DependencyConflictError becomes the extra hint and leaves nothing installed."""
    from opentelemetry.instrumentation.dependencies import (
        DependencyConflict,
        DependencyConflictError,
    )

    class ConflictInstrumentor:
        def __init__(self) -> None:
            self._is_instrumented_by_opentelemetry = False

        @property
        def is_instrumented_by_opentelemetry(self) -> bool:
            return self._is_instrumented_by_opentelemetry

        def instrument(self, **kwargs: object) -> None:
            conflict = DependencyConflict(required="openai>=99", found="openai 1.0.0")
            raise DependencyConflictError(conflict)

    module = types.ModuleType("llmobs_tests_conflict")
    module.ConflictInstrumentor = ConflictInstrumentor  # type: ignore[attr-defined]
    monkeypatch.setitem(sys.modules, "llmobs_tests_conflict", module)
    monkeypatch.setitem(
        REGISTRY,
        "conflict",
        Instrumentor(
            name="conflict",
            extra="conflict",
            module="llmobs_tests_conflict",
            cls="ConflictInstrumentor",
        ),
    )
    _, start = memory
    with pytest.raises(ConfigError) as info:
        start(instrument=["conflict"])
    assert str(info.value) == "instrument: 'conflict' needs `pip install llmobs[conflict]`"
    assert runtime_module.current() is None


def test_openai_end_to_end(memory: Memory, spans: Spans, http_recorder: Recorder) -> None:
    exporter, start = memory
    start(instrument=["openai"])
    client = openai.OpenAI(base_url=f"{http_recorder.url}/v1", api_key="test")

    @llmobs.observe(name="ask")
    def ask(question: str) -> str:
        completion = client.chat.completions.create(
            model="gpt-4o-mini", messages=[{"role": "user", "content": question}]
        )
        return completion.choices[0].message.content or ""

    with llmobs.context(session_id="s1"):
        answer = ask("hello")
    assert answer == "hi"

    finished = spans(exporter)
    assert {span.name for span in finished} == {"ask", OPENAI_SPAN}
    ours = one(finished, "ask")
    instrumented = one(finished, OPENAI_SPAN)
    assert instrumented.parent is not None
    assert instrumented.parent.span_id == ours.context.span_id
    assert attrs(instrumented)["openinference.span.kind"] == "LLM"
    assert attrs(instrumented)["llmobs.session.id"] == "s1"


def test_anthropic_end_to_end(memory: Memory, spans: Spans, http_recorder: Recorder) -> None:
    exporter, start = memory
    start(instrument=["anthropic"])
    # The Anthropic SDK prepends /v1 itself; base_url is the recorder root.
    client = anthropic.Anthropic(base_url=http_recorder.url, api_key="test")

    @llmobs.observe(name="ask")
    def ask(question: str) -> str:
        message = client.messages.create(
            model="claude-opus-4-6",
            max_tokens=8,
            messages=[{"role": "user", "content": question}],
        )
        if message.content and isinstance(message.content[0], TextBlock):
            return message.content[0].text
        return ""

    with llmobs.context(session_id="s1"):
        answer = ask("hello")
    assert answer == "hi"

    finished = spans(exporter)
    assert {span.name for span in finished} == {"ask", ANTHROPIC_SPAN}
    ours = one(finished, "ask")
    instrumented = one(finished, ANTHROPIC_SPAN)
    assert instrumented.parent is not None
    assert instrumented.parent.span_id == ours.context.span_id
    assert attrs(instrumented)["openinference.span.kind"] == "LLM"
    assert attrs(instrumented)["llmobs.session.id"] == "s1"


def test_capture_content_false_hides_instrumentor_payloads(
    memory: Memory, spans: Spans, http_recorder: Recorder
) -> None:
    exporter, start = memory
    start(instrument=["openai"], capture_content=False)
    client = openai.OpenAI(base_url=f"{http_recorder.url}/v1", api_key="test")
    client.chat.completions.create(
        model="gpt-4o-mini", messages=[{"role": "user", "content": "hello"}]
    )

    (instrumented,) = llm_spans(spans(exporter), OPENAI_SPAN)
    keys = set(attrs(instrumented))
    assert not keys & {"input.value", "output.value"}
    assert not any(key.startswith("llm.input_messages.") for key in keys)
    assert not any(key.startswith("llm.output_messages.") for key in keys)


def test_shutdown_uninstruments(memory: Memory, spans: Spans, http_recorder: Recorder) -> None:
    exporter, start = memory
    original = openai.OpenAI.request
    start(instrument=["openai"])
    assert openai.OpenAI.request is not original  # wrapped
    client = openai.OpenAI(base_url=f"{http_recorder.url}/v1", api_key="test")
    client.chat.completions.create(
        model="gpt-4o-mini", messages=[{"role": "user", "content": "hello"}]
    )
    assert len(spans(exporter)) == 1

    runtime_module.shutdown()
    assert openai.OpenAI.request is original  # uninstrumented

    # A call after shutdown creates no span, even under a fresh runtime.
    start()
    client.chat.completions.create(
        model="gpt-4o-mini", messages=[{"role": "user", "content": "hello"}]
    )
    assert len(spans(exporter)) == 1  # no new span


OTHER_ENTRIES = [entry for entry in REGISTRY.values() if entry.name not in ("openai", "anthropic")]


@pytest.mark.parametrize("entry", OTHER_ENTRIES, ids=[entry.name for entry in OTHER_ENTRIES])
def test_other_registry_entries_import(entry: Instrumentor) -> None:
    """The eight other names: module and class exist when the extra is installed."""
    module: Any = pytest.importorskip(entry.module, reason=f"llmobs[{entry.extra}] not installed")
    assert getattr(module, entry.cls, None) is not None
