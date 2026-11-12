"""Tests for the masking helper and the export-time masking pass (plan F § P4, P8)."""

import json
import logging
from collections.abc import Callable, Mapping, Sequence

import pytest
from conftest import TEST_API_KEY, TEST_BASE_URL, SequentialIds
from opentelemetry.context import Context
from opentelemetry.sdk.trace import ReadableSpan, Span, SpanProcessor
from opentelemetry.sdk.trace.export import SpanExportResult
from opentelemetry.sdk.trace.export.in_memory_span_exporter import InMemorySpanExporter
from opentelemetry.util import types as otel_types

from llmobs._config import resolve_config
from llmobs._mask import MaskingExporter, apply
from llmobs._runtime import Runtime, flush, install, reset

Memory = tuple[InMemorySpanExporter, Callable[..., Runtime]]
Spans = Callable[[InMemorySpanExporter], list[ReadableSpan]]

OPENINFERENCE_SCOPE = "openinference.instrumentation.openai"


def _upper(value: object) -> object:
    return value.upper() if isinstance(value, str) else value


def _boom(value: object) -> object:
    raise ValueError("nope")


class _ExplodingSpan(ReadableSpan):
    """A span whose attributes blow up, to reach the fail-closed path."""

    @property
    def attributes(self) -> Mapping[str, otel_types.AttributeValue] | None:
        raise RuntimeError("attributes exploded")


def test_apply_without_runtime_returns_value() -> None:
    assert apply({"a": 1}) == {"a": 1}


def test_apply_without_mask_returns_value(memory: Memory) -> None:
    _, start = memory
    start()
    value = {"a": 1}
    assert apply(value) is value


def test_apply_runs_the_mask(memory: Memory) -> None:
    _, start = memory
    start(mask=_upper)
    assert apply("secret") == "SECRET"
    assert apply(3) == 3


def test_apply_fails_closed_when_mask_raises(
    memory: Memory, caplog: pytest.LogCaptureFixture
) -> None:
    _, start = memory
    start(mask=_boom)
    with caplog.at_level(logging.DEBUG, logger="llmobs"):
        assert apply("anything") == "[llmobs: mask failed]"
    messages = [record.getMessage() for record in caplog.records if record.name == "llmobs"]
    assert messages == ["llmobs: mask raised ValueError"]


def _redact(value: object) -> object:
    """Replace every occurrence of the test address, recursively."""
    if isinstance(value, str):
        return value.replace("alice@example.com", "[redacted]")
    if isinstance(value, list):
        return [_redact(item) for item in value]
    if isinstance(value, dict):
        return {key: _redact(item) for key, item in value.items()}
    return value


def test_instrumentor_span_payloads_are_masked(memory: Memory, spans: Spans) -> None:
    seen: list[object] = []

    def mask(value: object) -> object:
        seen.append(value)
        return _redact(value)

    exporter, start = memory
    runtime = start(mask=mask)
    tracer = runtime.provider.get_tracer(OPENINFERENCE_SCOPE, "0.1.69")
    with tracer.start_as_current_span("chat") as span:
        span.set_attribute("input.value", "email alice@example.com")
        span.set_attribute("llm.input_messages.0.message.content", "hi alice@example.com")
        span.set_attribute(
            "gen_ai.input.messages",
            json.dumps([{"role": "user", "content": "hi alice@example.com"}]),
        )
        span.set_attribute("openinference.span.kind", "LLM")
        span.add_event(
            "gen_ai.content.completion",
            {
                "gen_ai.output.messages": json.dumps(
                    [{"role": "assistant", "content": "yo alice@example.com"}]
                )
            },
        )
    finished = spans(exporter)
    assert [span.name for span in finished] == ["chat"]
    attributes = finished[0].attributes
    assert attributes is not None
    assert attributes["input.value"] == "email [redacted]"
    assert attributes["llm.input_messages.0.message.content"] == "hi [redacted]"
    assert attributes["gen_ai.input.messages"] == '[{"role":"user","content":"hi [redacted]"}]'
    assert attributes["openinference.span.kind"] == "LLM"  # not a payload key: untouched
    events = finished[0].events
    assert len(events) == 1
    assert events[0].attributes is not None
    assert events[0].attributes["gen_ai.output.messages"] == (
        '[{"role":"assistant","content":"yo [redacted]"}]'
    )
    # A JSON payload was decoded before the mask saw it.
    assert any(isinstance(value, list) for value in seen)


def test_llmobs_scope_spans_are_not_remasked(memory: Memory, spans: Spans) -> None:
    calls = 0

    def mask(value: object) -> object:
        nonlocal calls
        calls += 1
        return value

    exporter, start = memory
    runtime = start(mask=mask)
    with runtime.tracer.start_as_current_span("work") as span:
        span.set_attribute("llmobs.input", "already masked")
    finished = spans(exporter)
    assert finished[0].attributes is not None
    assert finished[0].attributes["llmobs.input"] == "already masked"
    assert calls == 0


def test_capture_content_false_drops_payload_keys(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    runtime = start(capture_content=False)
    tracer = runtime.provider.get_tracer(OPENINFERENCE_SCOPE, "0.1.69")
    with tracer.start_as_current_span("chat") as span:
        span.set_attribute("input.value", "hello")
        span.set_attribute("gen_ai.input.messages", "[1]")
        span.set_attribute("llm.token_count.prompt_tokens", 5)
        span.set_attribute("model.name", "gpt")
        span.add_event("e", {"output.value": "x"})
    finished = spans(exporter)
    attributes = finished[0].attributes
    assert attributes is not None
    assert set(attributes) == {"llm.token_count.prompt_tokens", "model.name"}
    events = finished[0].events
    assert len(events) == 1
    assert events[0].attributes is not None
    assert "output.value" not in events[0].attributes


def test_non_string_payload_values_are_dropped(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    runtime = start(mask=_redact)
    tracer = runtime.provider.get_tracer(OPENINFERENCE_SCOPE, "0.1.69")
    with tracer.start_as_current_span("chat") as span:
        span.set_attribute("input.value", 42)
        span.set_attribute("lmnr.span.output", [1, 2])
        span.set_attribute("gen_ai.usage.input_tokens", 3)
    attributes = spans(exporter)[0].attributes
    assert attributes is not None
    assert set(attributes) == {"gen_ai.usage.input_tokens"}
    assert attributes["gen_ai.usage.input_tokens"] == 3


def test_unparsable_json_goes_to_the_mask_as_a_string(memory: Memory, spans: Spans) -> None:
    seen: list[object] = []

    def mask(value: object) -> object:
        seen.append(value)
        return "MASKED"

    exporter, start = memory
    runtime = start(mask=mask)
    tracer = runtime.provider.get_tracer(OPENINFERENCE_SCOPE, "0.1.69")
    with tracer.start_as_current_span("chat") as span:
        span.set_attribute("gen_ai.input.messages", "{not json")
        span.set_attribute("input.value", '{"a": 1}')
    attributes = spans(exporter)[0].attributes
    assert attributes is not None
    assert attributes["gen_ai.input.messages"] == "MASKED"
    assert attributes["input.value"] == "MASKED"
    assert seen == ["{not json", {"a": 1}]


def test_mask_raising_marks_the_value_at_export(
    memory: Memory, spans: Spans, caplog: pytest.LogCaptureFixture
) -> None:
    def boom(value: object) -> object:
        raise ValueError("nope")

    exporter, start = memory
    runtime = start(mask=boom)
    tracer = runtime.provider.get_tracer(OPENINFERENCE_SCOPE, "0.1.69")
    with tracer.start_as_current_span("chat") as span:
        span.set_attribute("input.value", "a@b")
    with caplog.at_level(logging.DEBUG, logger="llmobs"):
        attributes = spans(exporter)[0].attributes
    assert attributes is not None
    assert attributes["input.value"] == "[llmobs: mask failed]"
    messages = [record.getMessage() for record in caplog.records if record.name == "llmobs"]
    assert messages == ["llmobs: mask raised ValueError"]


def test_masking_pass_not_installed_without_mask_or_capture_off() -> None:
    exported: list[ReadableSpan] = []

    class Recorder(InMemorySpanExporter):
        def export(self, spans: Sequence[ReadableSpan]) -> SpanExportResult:
            exported.extend(spans)
            return super().export(spans)

    forwarded: list[ReadableSpan] = []

    class Witness(SpanProcessor):
        def on_start(self, span: Span, parent_context: Context | None = None) -> None: ...

        def on_end(self, span: ReadableSpan) -> None:
            forwarded.append(span)

        def shutdown(self) -> None: ...

        def force_flush(self, timeout_millis: int = 30_000) -> bool:
            return True

    exporter = Recorder()
    config = resolve_config(api_key=TEST_API_KEY, base_url=TEST_BASE_URL, env={})
    runtime = Runtime.create(config, exporter=exporter, id_generator=SequentialIds())
    install(runtime)
    runtime.provider.add_span_processor(Witness())
    tracer = runtime.provider.get_tracer(OPENINFERENCE_SCOPE, "0.1.69")
    with tracer.start_as_current_span("chat") as span:
        span.set_attribute("input.value", "secret")
    flush()
    assert [span.name for span in exported] == ["chat"]
    # The very same span object the processor forwarded reaches the exporter:
    # without a mask (or capture_content=false) no pass rebuilds spans.
    assert exported[0] is forwarded[0]
    assert exported[0].attributes is not None
    assert exported[0].attributes["input.value"] == "secret"
    reset()


def test_span_without_instrumentation_scope_is_masked() -> None:
    inner = InMemorySpanExporter()
    exporter = MaskingExporter(inner, mask=_redact, capture_content=True)
    span = ReadableSpan(name="s", attributes={"input.value": "alice@example.com"})
    assert exporter.export([span]) == SpanExportResult.SUCCESS
    (finished,) = inner.get_finished_spans()
    assert finished.attributes is not None
    assert dict(finished.attributes) == {"input.value": "[redacted]"}


def test_span_without_payload_keys_is_returned_unchanged() -> None:
    inner = InMemorySpanExporter()
    exporter = MaskingExporter(inner, mask=_redact, capture_content=True)
    span = ReadableSpan(name="s", attributes={"http.method": "GET"})
    assert exporter.export([span]) == SpanExportResult.SUCCESS
    (finished,) = inner.get_finished_spans()
    assert finished is span


def test_masking_failure_drops_the_batch(caplog: pytest.LogCaptureFixture) -> None:
    inner = InMemorySpanExporter()
    exporter = MaskingExporter(inner, mask=_redact, capture_content=True)
    with caplog.at_level(logging.WARNING, logger="llmobs"):
        result = exporter.export([_ExplodingSpan("s")])
    assert result == SpanExportResult.FAILURE
    assert inner.get_finished_spans() == ()
    messages = [record.getMessage() for record in caplog.records if record.name == "llmobs"]
    assert messages == ["llmobs: masking failed; dropped 1 spans"]


def test_shutdown_and_flush_delegate_to_the_inner_exporter() -> None:
    inner = InMemorySpanExporter()
    exporter = MaskingExporter(inner, mask=None, capture_content=False)
    assert exporter.force_flush() is True
    exporter.shutdown()
    assert inner.export([]) == SpanExportResult.FAILURE
