"""Tests for ``observe`` on functions and coroutines (plan F § P5)."""

import asyncio
import inspect
from collections.abc import AsyncIterator, Callable, Iterator, Mapping
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


def test_default_and_every_valid_kind(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start()

    @llmobs.observe
    def plain() -> None:
        return None

    plain()
    for kind in get_args(SpanKind):

        def make(kind: SpanKind) -> Callable[[], None]:
            @llmobs.observe(kind=kind)
            def call() -> None:
                return None

            return call

        make(kind)()
    finished = spans(exporter)
    assert len(finished) == 1 + len(get_args(SpanKind))
    assert attrs(finished[0])["llmobs.span.kind"] == "chain"
    assert {attrs(span)["llmobs.span.kind"] for span in finished[1:]} == set(get_args(SpanKind))


def test_invalid_kind_raises_at_decoration() -> None:
    calls: list[int] = []
    bad_kind = cast(SpanKind, "bogus")
    with pytest.raises(ValueError, match=r"^kind: must be one of"):

        @llmobs.observe(kind=bad_kind)
        def never() -> None:
            calls.append(1)

    assert calls == []


def test_name_default_and_self_cls_dropped(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start()

    class Widget:
        @llmobs.observe
        def run(self, x: int, y: str = "z") -> str:
            return f"{x}{y}"

        @classmethod
        @llmobs.observe
        def make(cls, x: int) -> int:
            return x

        @staticmethod
        @llmobs.observe
        def add(a: int, b: int) -> int:
            return a + b

    assert Widget().run(1) == "1z"
    assert Widget.make(2) == 2
    assert Widget.add(3, 4) == 7
    finished = spans(exporter)
    # The default name is the qualname; a class in a test function carries the
    # enclosing scope, so "Widget.run" is "...<locals>.Widget.run" here.
    prefix = "test_name_default_and_self_cls_dropped.<locals>."
    by_name = {span.name.removeprefix(prefix): span for span in finished}
    assert set(by_name) == {"Widget.run", "Widget.make", "Widget.add"}
    assert attrs(by_name["Widget.run"])["llmobs.input"] == '{"x":1,"y":"z"}'
    assert attrs(by_name["Widget.make"])["llmobs.input"] == '{"x":2}'
    assert attrs(by_name["Widget.add"])["llmobs.input"] == '{"a":3,"b":4}'


def test_input_shapes(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start()

    @llmobs.observe(name="defaults")
    def defaults(a: int, b: int = 2) -> None:
        return None

    @llmobs.observe(name="var")
    def var(a: int, *args: int, key: str = "k", **rest: int) -> None:
        return None

    @llmobs.observe(name="posonly")
    def posonly(a: int, b: int, /, c: int = 3) -> None:
        return None

    defaults(1)
    var(1, 2, 3, key="v", other=4)
    posonly(1, 2)
    finished = spans(exporter)
    by_name = {span.name: span for span in finished}
    var_input = attrs(by_name["var"])["llmobs.input"]
    assert attrs(by_name["defaults"])["llmobs.input"] == '{"a":1,"b":2}'
    assert var_input == '{"a":1,"args":[2,3],"key":"v","rest":{"other":4}}'
    assert attrs(by_name["posonly"])["llmobs.input"] == '{"a":1,"b":2,"c":3}'


def test_bind_failure_records_fallback_and_raises(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start()

    @llmobs.observe(name="strict")
    def strict(a: int) -> None:
        return None

    arguments = [1, 2]
    with pytest.raises(TypeError, match="takes 1 positional argument but 2 were given"):
        strict(*arguments)
    [span] = spans(exporter)
    attributes = attrs(span)
    assert attributes["llmobs.input"] == '{"args":[1,2],"kwargs":{}}'
    assert span.status.status_code is StatusCode.ERROR
    assert span.status.description is not None
    assert span.status.description.startswith("TypeError:")


@dataclass
class Row:
    n: int


def test_output_shapes(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start()

    @llmobs.observe(name="text")
    def text(value: str) -> str:
        return value

    @llmobs.observe(name="none")
    def none_() -> None:
        return None

    @llmobs.observe(name="zero")
    def zero() -> None:
        return None

    @llmobs.observe(name="data")
    def data() -> Row:
        return Row(n=1)

    @llmobs.observe(name="gen")
    def gen() -> Iterator[int]:
        return (i for i in range(3))

    async def numbers() -> AsyncIterator[int]:
        yield 1

    @llmobs.observe(name="agen")
    def agen() -> AsyncIterator[int]:
        return numbers()

    assert text("hi") == "hi"
    assert none_() is None
    assert zero() is None
    assert data() == Row(n=1)
    generator = gen()
    assert inspect.isgenerator(generator)
    async_generator = agen()
    assert inspect.isasyncgen(async_generator)

    finished = spans(exporter)
    by_name = {span.name: span for span in finished}
    text_attributes = attrs(by_name["text"])
    assert text_attributes["llmobs.input"] == '{"value":"hi"}'
    assert text_attributes["llmobs.output"] == "hi"
    assert text_attributes["llmobs.output.format"] == "text"
    none_attributes = attrs(by_name["none"])
    assert none_attributes["llmobs.input"] == "{}"
    assert none_attributes["llmobs.output"] == "null"
    assert none_attributes["llmobs.output.format"] == "json"
    zero_attributes = attrs(by_name["zero"])
    assert zero_attributes["llmobs.input"] == "{}"
    assert zero_attributes["llmobs.output"] == "null"
    data_attributes = attrs(by_name["data"])
    assert data_attributes["llmobs.input"] == "{}"
    assert data_attributes["llmobs.output"] == '{"n":1}'
    gen_attributes = attrs(by_name["gen"])
    assert gen_attributes["llmobs.input"] == "{}"
    assert "llmobs.output" not in gen_attributes
    agen_attributes = attrs(by_name["agen"])
    assert agen_attributes["llmobs.input"] == "{}"
    assert "llmobs.output" not in agen_attributes
    assert list(generator) == [0, 1, 2]  # the generator was returned untouched


def test_capture_input_output_toggles(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start()

    @llmobs.observe(name="no-in", capture_input=False)
    def no_in(a: int) -> int:
        return a

    @llmobs.observe(name="no-out", capture_output=False)
    def no_out(a: int) -> int:
        return a

    assert no_in(1) == 1
    assert no_out(2) == 2
    finished = spans(exporter)
    by_name = {span.name: span for span in finished}
    no_in_attributes = attrs(by_name["no-in"])
    assert "llmobs.input" not in no_in_attributes
    assert no_in_attributes["llmobs.output"] == "1"
    no_out_attributes = attrs(by_name["no-out"])
    assert no_out_attributes["llmobs.input"] == '{"a":2}'
    assert "llmobs.output" not in no_out_attributes


def test_capture_content_false_records_only_kind(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start(capture_content=False)

    @llmobs.observe(kind="tool", name="lookup")
    def lookup(query: str) -> str:
        return "x"

    assert lookup("q") == "x"
    [span] = spans(exporter)
    assert dict(attrs(span)) == {"llmobs.span.kind": "tool"}


def test_exception_recorded_and_reraised(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start()

    class ScenarioError(Exception):
        pass

    @llmobs.observe(name="fails")
    def fails(x: int) -> None:
        raise ScenarioError("boom")

    with pytest.raises(ScenarioError, match="boom"):
        fails(1)
    [span] = spans(exporter)
    attributes = attrs(span)
    assert attributes["llmobs.input"] == '{"x":1}'
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


def test_span_inside_observe(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start()

    @llmobs.observe(name="inner")
    def inner(q: str) -> list[dict[str, str]]:
        return [{"id": "d1"}]

    @llmobs.observe(name="outer")
    def outer(q: str) -> str:
        inner(q)
        with llmobs.span("manual"):
            pass
        return "done"

    assert outer("why") == "done"
    finished = spans(exporter)
    by_name = {span.name: span for span in finished}
    outer_span = by_name["outer"]
    inner_span = by_name["inner"]
    manual_span = by_name["manual"]
    assert outer_span.parent is None
    assert inner_span.parent is not None
    assert inner_span.parent.span_id == outer_span.context.span_id
    assert manual_span.parent is not None
    assert manual_span.parent.span_id == outer_span.context.span_id
    assert inner_span.context.trace_id == outer_span.context.trace_id


def test_observe_inside_span(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start()

    @llmobs.observe(name="leaf")
    def leaf() -> None:
        return None

    with llmobs.span("root", input="v") as handle:
        leaf()
        assert handle.trace_id is not None
    finished = spans(exporter)
    by_name = {span.name: span for span in finished}
    leaf_span = by_name["leaf"]
    assert leaf_span.parent is not None
    assert leaf_span.parent.span_id == by_name["root"].context.span_id


def test_async_gather_two_siblings(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start()

    @llmobs.observe(name="inner")
    async def inner(i: int) -> int:
        await asyncio.sleep(0)
        return i

    @llmobs.observe(name="outer")
    async def outer(i: int) -> int:
        return await inner(i)

    async def main() -> list[int]:
        return list(await asyncio.gather(outer(1), outer(2)))

    assert asyncio.run(main()) == [1, 2]
    # The exporter lists spans by end time; the sequential ids give creation order.
    finished = sorted(spans(exporter), key=lambda span: span.context.span_id)
    assert [span.name for span in finished] == ["outer", "inner", "outer", "inner"]
    outers = [finished[0], finished[2]]
    inners = [finished[1], finished[3]]
    for outer_span, inner_span in zip(outers, inners, strict=True):
        assert outer_span.parent is None
        assert inner_span.parent is not None
        assert inner_span.parent.span_id == outer_span.context.span_id
    assert [attrs(inner)["llmobs.input"] for inner in inners] == ['{"i":1}', '{"i":2}']


def test_async_exception_recorded_and_reraised(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start()

    class ScenarioError(Exception):
        pass

    @llmobs.observe(name="async-fails")
    async def async_fails(x: int) -> None:
        await asyncio.sleep(0)
        raise ScenarioError("boom")

    with pytest.raises(ScenarioError, match="boom"):
        asyncio.run(async_fails(1))
    [span] = spans(exporter)
    assert span.status.status_code is StatusCode.ERROR
    assert span.status.description == "ScenarioError: boom"
    [event] = span.events
    assert event.name == "exception"


def test_mask_applied_to_input_and_output(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start(mask=lambda value: "x")

    @llmobs.observe(name="m")
    def m(a: str) -> str:
        return a

    assert m("q") == "q"
    [span] = spans(exporter)
    attributes = attrs(span)
    assert attributes["llmobs.input"] == "x"
    assert attributes["llmobs.input.format"] == "text"
    assert attributes["llmobs.output"] == "x"
    assert attributes["llmobs.output.format"] == "text"


def test_decorated_before_init_traced_after(memory: Memory, spans: Spans) -> None:
    exporter, start = memory

    @llmobs.observe(name="early")
    def early(x: int) -> int:
        return x

    start()
    assert early(1) == 1
    [span] = spans(exporter)
    assert span.name == "early"
    assert attrs(span)["llmobs.input"] == '{"x":1}'


def test_called_without_runtime(memory: Memory, spans: Spans) -> None:
    exporter, _start = memory

    @llmobs.observe(name="off")
    def off(x: int) -> int:
        return x

    assert off(5) == 5
    assert llmobs.current_trace_id() is None
    assert llmobs.current_span_id() is None
    assert spans(exporter) == []


def test_current_ids_inside_observe(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start()
    seen: dict[str, str | None] = {}

    @llmobs.observe(name="id")
    def read() -> None:
        seen["trace"] = llmobs.current_trace_id()
        seen["span"] = llmobs.current_span_id()

    read()
    [span] = spans(exporter)
    assert seen["trace"] == format(span.context.trace_id, "032x")
    assert seen["span"] == format(span.context.span_id, "016x")
    assert llmobs.current_trace_id() is None
    assert llmobs.current_span_id() is None


def test_wraps_keeps_function_metadata() -> None:
    @llmobs.observe(name="w")
    def documented(a: int) -> int:
        """Doc."""
        return a

    assert documented.__name__ == "documented"
    assert documented.__qualname__ == ("test_wraps_keeps_function_metadata.<locals>.documented")
    assert documented.__doc__ == "Doc."
