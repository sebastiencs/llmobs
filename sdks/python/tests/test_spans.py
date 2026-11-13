"""Tests for ``span()`` and the ``Span`` handle (plan F § P5)."""

import asyncio
from collections.abc import Callable, Mapping
from dataclasses import dataclass
from typing import cast, get_args

import pytest
from opentelemetry.sdk.trace import ReadableSpan
from opentelemetry.sdk.trace.export.in_memory_span_exporter import InMemorySpanExporter
from opentelemetry.trace import StatusCode
from opentelemetry.util.types import AnyValue

import llmobs
from llmobs._runtime import Runtime
from llmobs._spans import SpanKind

Memory = tuple[InMemorySpanExporter, Callable[..., Runtime]]
Spans = Callable[[InMemorySpanExporter], list[ReadableSpan]]


def attrs(span: ReadableSpan) -> Mapping[str, AnyValue]:
    """The span's attributes; every span these tests build carries them."""
    assert span.attributes is not None
    return span.attributes


@dataclass
class Point:
    x: int
    y: str


def test_kind_default_and_every_valid_kind(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start()
    with llmobs.span("plain"):
        pass
    for kind in get_args(SpanKind):
        with llmobs.span(f"sp-{kind}", kind=kind):
            pass
    finished = spans(exporter)
    assert len(finished) == 1 + len(get_args(SpanKind))
    assert attrs(finished[0])["llmobs.span.kind"] == "chain"
    by_name = {span.name: span for span in finished}
    for kind in get_args(SpanKind):
        assert attrs(by_name[f"sp-{kind}"])["llmobs.span.kind"] == kind


def test_invalid_kind_raises_at_construction() -> None:
    bad_kind = cast(SpanKind, "bogus")
    with pytest.raises(ValueError, match=r"^kind: must be one of"):
        llmobs.span("s", kind=bad_kind)


def test_payload_shapes(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start()
    cases: list[tuple[object, str, str]] = [
        ("hi", "hi", "text"),
        (None, "null", "json"),
        ({"a": 1, "b": [True, None]}, '{"a":1,"b":[true,null]}', "json"),
        ([1, 2], "[1,2]", "json"),
        (b"ab", '"data:application/octet-stream;base64,YWI="', "json"),
        (Point(x=1, y="z"), '{"x":1,"y":"z"}', "json"),
    ]
    for index, (value, _text, _fmt) in enumerate(cases):
        with llmobs.span(f"case-{index}", input=value) as handle:
            handle.set_output(value)
    finished = spans(exporter)
    assert len(finished) == len(cases)
    for span, (value, text, fmt) in zip(finished, cases, strict=True):
        attributes = attrs(span)
        assert attributes["llmobs.input"] == text, value
        assert attributes["llmobs.input.format"] == fmt, value
        assert attributes["llmobs.output"] == text, value
        assert attributes["llmobs.output.format"] == fmt, value


def test_capture_content_false_keeps_metadata(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start(capture_content=False)
    with llmobs.span("s", input="secret", metadata={"model": "bge"}) as handle:
        handle.set_output("out")
        handle.set_metadata(plan="pro")
    [span] = spans(exporter)
    assert set(attrs(span)) == {
        "llmobs.span.kind",
        "llmobs.metadata.model",
        "llmobs.metadata.plan",
    }


def _mask_value(value: object) -> object:
    return "x" if value == "secret" else value


def test_set_metadata_key_rule_and_mask(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start(mask=_mask_value)
    with llmobs.span("s", metadata={"good-key": "secret"}) as handle:
        handle.set_metadata(other=3, **{"bad key": "v"})
    [span] = spans(exporter)
    attributes = attrs(span)
    assert attributes["llmobs.metadata.good-key"] == "x"
    assert attributes["llmobs.metadata.other"] == 3
    assert "llmobs.metadata.bad key" not in attributes


def test_mask_applied_to_payloads(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start(mask=lambda value: "x")
    with llmobs.span("s", input={"q": "a"}) as handle:
        handle.set_output([1, 2])
    [span] = spans(exporter)
    attributes = attrs(span)
    assert attributes["llmobs.input"] == "x"
    assert attributes["llmobs.input.format"] == "text"
    assert attributes["llmobs.output"] == "x"
    assert attributes["llmobs.output.format"] == "text"


def test_failing_mask_fails_closed(memory: Memory, spans: Spans) -> None:
    def boom(value: object) -> object:
        raise RuntimeError("no")

    exporter, start = memory
    start(mask=boom)
    with llmobs.span("s", input="v") as handle:
        handle.set_output("w")
    [span] = spans(exporter)
    attributes = attrs(span)
    assert attributes["llmobs.input"] == "[llmobs: mask failed]"
    assert attributes["llmobs.output"] == "[llmobs: mask failed]"


def test_no_runtime_yields_noop_handle(memory: Memory, spans: Spans) -> None:
    exporter, _start = memory
    with llmobs.span("s", input="v", metadata={"k": 1}) as handle:
        assert handle.trace_id is None
        assert handle.span_id is None
        handle.set_output("out")
    assert spans(exporter) == []


def test_error_in_body_records_event_and_status(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start()

    class ScenarioError(Exception):
        pass

    with pytest.raises(ScenarioError, match="boom"), llmobs.span("s", input="v"):
        raise ScenarioError("boom")
    [span] = spans(exporter)
    attributes = attrs(span)
    assert attributes["llmobs.input"] == "v"
    assert "llmobs.output" not in attributes
    assert span.status.status_code is StatusCode.ERROR
    assert span.status.description == "ScenarioError: boom"
    [event] = span.events
    assert event.name == "exception"
    event_attributes = event.attributes
    assert event_attributes is not None
    assert event_attributes["exception.type"] == "ScenarioError"
    assert event_attributes["exception.message"] == "boom"
    assert isinstance(event_attributes["exception.stacktrace"], str)
    assert "ScenarioError" in event_attributes["exception.stacktrace"]


def test_base_exception_passes_through_without_error_status(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start()
    with pytest.raises(KeyboardInterrupt), llmobs.span("s"):
        raise KeyboardInterrupt
    [span] = spans(exporter)
    assert span.status.status_code is StatusCode.UNSET
    assert not span.events


def test_async_context_manager(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start()

    async def run() -> None:
        async with llmobs.span("a", input="v") as handle:
            handle.set_output("w")

    asyncio.run(run())
    [span] = spans(exporter)
    attributes = attrs(span)
    assert attributes["llmobs.input"] == "v"
    assert attributes["llmobs.output"] == "w"


def test_handle_ids_and_after_end(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start()
    with llmobs.span("s") as handle:
        assert handle.trace_id == llmobs.current_trace_id()
        assert handle.span_id == llmobs.current_span_id()
        assert handle.trace_id is not None
        assert handle.span_id is not None
    handle.set_output("late")
    handle.set_input("late")
    handle.set_metadata(k="late")
    [span] = spans(exporter)
    attributes = attrs(span)
    assert handle.trace_id == format(span.context.trace_id, "032x")
    assert handle.span_id == format(span.context.span_id, "016x")
    assert "llmobs.input" not in attributes
    assert "llmobs.output" not in attributes
    assert "llmobs.metadata.k" not in attributes


def test_current_ids_for_any_span(memory: Memory, spans: Spans) -> None:
    _exporter, start = memory
    runtime = start()
    assert llmobs.current_trace_id() is None
    assert llmobs.current_span_id() is None
    third_party = runtime.provider.get_tracer("thirdparty")
    with third_party.start_as_current_span("tp"):
        assert llmobs.current_span_id() == format(1, "016x")
        assert llmobs.current_trace_id() == format(1, "032x")
    with llmobs.span("ours"):
        assert llmobs.current_span_id() == format(2, "016x")
    assert llmobs.current_trace_id() is None
    assert llmobs.current_span_id() is None
