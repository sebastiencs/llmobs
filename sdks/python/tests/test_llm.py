"""Tests for ``llm_span`` (plan F § P7)."""

import asyncio
import logging
import math
from collections.abc import Callable, Mapping
from itertools import count
from types import SimpleNamespace
from typing import cast

import pytest
from opentelemetry.sdk.trace import ReadableSpan
from opentelemetry.sdk.trace.export.in_memory_span_exporter import InMemorySpanExporter
from opentelemetry.trace import StatusCode
from opentelemetry.util.types import AnyValue

import llmobs
from llmobs import _llm
from llmobs._llm import to_canonical_messages
from llmobs._runtime import Runtime

Memory = tuple[InMemorySpanExporter, Callable[..., Runtime]]
Spans = Callable[[InMemorySpanExporter], list[ReadableSpan]]

INPUT_JSON = (
    '[{"role":"system","parts":[{"type":"text","content":"Be brief."}]}'
    ',{"role":"user","parts":[{"type":"text","content":"hi"}]}]'
)
OUTPUT_JSON = (
    '[{"role":"assistant","finish_reason":"stop","parts":[{"type":"text","content":"hello"}]}]'
)


def attrs(span: ReadableSpan) -> Mapping[str, AnyValue]:
    """The span's attributes; every span these tests build carries them."""
    assert span.attributes is not None
    return span.attributes


def test_llm_span_scenario_attributes(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start()
    with llmobs.llm_span(
        "chat",
        provider="openai",
        model="gpt-4o-mini",
        input_messages=[
            {"role": "system", "content": "Be brief."},
            {"role": "user", "content": "hi"},
        ],
        params={"temperature": 0.2},
    ) as handle:
        handle.set_output_messages(
            [{"role": "assistant", "content": "hello", "finish_reason": "stop"}]
        )
        handle.set_response_model("gpt-4o-mini-2024-07-18")
        handle.set_usage(input_tokens=12, output_tokens=3)
    [span] = spans(exporter)
    assert dict(attrs(span)) == {
        "llmobs.span.kind": "llm",
        "gen_ai.operation.name": "chat",
        "gen_ai.provider.name": "openai",
        "gen_ai.request.model": "gpt-4o-mini",
        "gen_ai.request.temperature": 0.2,
        "gen_ai.response.model": "gpt-4o-mini-2024-07-18",
        "gen_ai.input.messages": INPUT_JSON,
        "gen_ai.output.messages": OUTPUT_JSON,
        "gen_ai.usage.input_tokens": 12,
        "gen_ai.usage.output_tokens": 3,
    }


def test_system_instructions_string_and_parts(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start()
    with llmobs.llm_span("a", provider="openai", model="m", system_instructions="Be kind."):
        pass
    with llmobs.llm_span(
        "b",
        provider="openai",
        model="m",
        system_instructions=[{"type": "text", "content": "Be kind."}],
    ):
        pass
    span_a, span_b = spans(exporter)
    wrapped = '[{"type":"text","content":"Be kind."}]'
    assert attrs(span_a)["gen_ai.system_instructions"] == wrapped
    assert attrs(span_b)["gen_ai.system_instructions"] == wrapped


def test_params_filtering(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start()
    params: Mapping[str, object] = {
        "temperature": 0.2,
        "stream": True,
        "n": 2,
        "response_format": "json",
        "stop": ["a"],
        "user": None,
        "seed": math.nan,
        "penalty": math.inf,
    }
    with llmobs.llm_span("s", provider="openai", model="gpt", params=params):
        pass
    [span] = spans(exporter)
    assert dict(attrs(span)) == {
        "llmobs.span.kind": "llm",
        "gen_ai.operation.name": "chat",
        "gen_ai.provider.name": "openai",
        "gen_ai.request.model": "gpt",
        "gen_ai.request.temperature": 0.2,
        "gen_ai.request.stream": True,
        "gen_ai.request.n": 2,
        "gen_ai.request.response_format": "json",
    }


def test_usage_all_keys(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start()
    with llmobs.llm_span("s", provider="p", model="m") as handle:
        handle.set_usage(
            input_tokens=100,
            output_tokens=50,
            cache_read_tokens=10,
            cache_write_tokens=20,
            reasoning_tokens=5,
        )
    [span] = spans(exporter)
    assert dict(attrs(span)) == {
        "llmobs.span.kind": "llm",
        "gen_ai.operation.name": "chat",
        "gen_ai.provider.name": "p",
        "gen_ai.request.model": "m",
        "gen_ai.usage.input_tokens": 100,
        "gen_ai.usage.output_tokens": 50,
        "gen_ai.usage.cache_read.input_tokens": 10,
        "gen_ai.usage.cache_creation.input_tokens": 20,
        "gen_ai.usage.reasoning.output_tokens": 5,
    }


def test_usage_edge_cases(memory: Memory, spans: Spans, caplog: pytest.LogCaptureFixture) -> None:
    exporter, start = memory
    start()
    with (
        caplog.at_level(logging.DEBUG, logger="llmobs"),
        llmobs.llm_span("s", provider="p", model="m") as handle,
    ):
        handle.set_usage(
            input_tokens=10,
            output_tokens=0,
            cache_read_tokens=True,
            cache_write_tokens=-1,
            reasoning_tokens=cast("int", 2.5),
        )
    [span] = spans(exporter)
    attributes = attrs(span)
    assert attributes["gen_ai.usage.input_tokens"] == 10
    assert attributes["gen_ai.usage.output_tokens"] == 0
    assert "gen_ai.usage.cache_read.input_tokens" not in attributes
    assert "gen_ai.usage.cache_creation.input_tokens" not in attributes
    assert "gen_ai.usage.reasoning.output_tokens" not in attributes
    assert "llmobs: skipped 3 usage values" in caplog.text


def test_set_cost(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start()
    with llmobs.llm_span("a", provider="p", model="m") as handle:
        handle.set_cost(0.0015)
        handle.set_cost(-1.0)
        handle.set_cost(math.nan)
        handle.set_cost(math.inf)
    with llmobs.llm_span("b", provider="p", model="m") as handle:
        handle.set_cost(2)
    span_a, span_b = spans(exporter)
    assert attrs(span_a)["llmobs.cost.total"] == 0.0015
    cost_b = attrs(span_b)["llmobs.cost.total"]
    assert cost_b == 2.0
    assert isinstance(cost_b, float)


def test_record_first_token_once(
    memory: Memory, spans: Spans, monkeypatch: pytest.MonkeyPatch
) -> None:
    exporter, start = memory
    start()
    now = count(1)  # an increasing fake clock: a second write would use 2
    monkeypatch.setattr(_llm, "time", SimpleNamespace(time_ns=lambda: next(now)))
    with llmobs.llm_span("s", provider="p", model="m") as handle:
        handle.record_first_token()
        handle.record_first_token()
    [span] = spans(exporter)
    ttft = attrs(span)["llmobs.ttft_ns"]
    assert isinstance(ttft, int)
    assert span.start_time is not None
    assert ttft == 1 - span.start_time


def test_to_canonical_messages_shapes() -> None:
    with_parts = [{"role": "user", "parts": [{"type": "text", "content": "hi"}]}]
    assert to_canonical_messages(with_parts)[0] is with_parts[0]
    converted = to_canonical_messages(
        [
            {"role": "user", "content": "hi"},
            {
                "role": "assistant",
                "name": "bot",
                "finish_reason": "stop",
                "content": "hey",
                "extra": 1,
            },
        ]
    )
    first = cast("Mapping[str, object]", converted[0])
    assert list(first) == ["role", "parts"]
    second = cast("Mapping[str, object]", converted[1])
    assert list(second) == ["role", "name", "finish_reason", "parts"]
    assert second["parts"] == [{"type": "text", "content": "hey"}]
    unknown: list[object] = [
        "plain",
        {"content": "no role"},
        {"role": "user", "content": 5},
        {"role": "user"},
        None,
    ]
    assert to_canonical_messages(unknown) == unknown


def test_capture_content_false_keeps_model_and_usage(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start(capture_content=False)
    with llmobs.llm_span(
        "chat",
        provider="openai",
        model="gpt-4o-mini",
        input_messages=[{"role": "user", "content": "hi"}],
        system_instructions="Be brief.",
        params={"temperature": 0.5},
    ) as handle:
        handle.set_input("raw")
        handle.set_output_messages([{"role": "assistant", "content": "yo"}])
        handle.set_usage(input_tokens=5)
        handle.set_cost(0.01)
    [span] = spans(exporter)
    assert dict(attrs(span)) == {
        "llmobs.span.kind": "llm",
        "gen_ai.operation.name": "chat",
        "gen_ai.provider.name": "openai",
        "gen_ai.request.model": "gpt-4o-mini",
        "gen_ai.request.temperature": 0.5,
        "gen_ai.usage.input_tokens": 5,
        "llmobs.cost.total": 0.01,
    }


def _redact(value: object) -> object:
    """Replace every exact ``"secret"`` string, recursively."""
    if isinstance(value, str):
        return "[redacted]" if value == "secret" else value
    if isinstance(value, list):
        return [_redact(item) for item in value]
    if isinstance(value, dict):
        return {key: _redact(item) for key, item in value.items()}
    return value


def test_mask_over_messages(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    seen: list[type] = []

    def mask(value: object) -> object:
        seen.append(type(value))
        return _redact(value)

    start(mask=mask)
    with llmobs.llm_span(
        "s",
        provider="p",
        model="m",
        input_messages=[{"role": "user", "content": "secret"}],
        system_instructions="secret",
    ) as handle:
        handle.set_output_messages([{"role": "assistant", "content": "hello"}])
    [span] = spans(exporter)
    attributes = attrs(span)
    assert attributes["gen_ai.input.messages"] == (
        '[{"role":"user","parts":[{"type":"text","content":"[redacted]"}]}]'
    )
    assert attributes["gen_ai.system_instructions"] == '[{"type":"text","content":"[redacted]"}]'
    assert attributes["gen_ai.output.messages"] == (
        '[{"role":"assistant","parts":[{"type":"text","content":"hello"}]}]'
    )
    # The mask sees each whole value once: the input list, the instructions
    # string, then the output list.
    assert seen == [list, str, list]


def test_invalid_provider_or_model_raises() -> None:
    with pytest.raises(ValueError, match=r"^provider: must be a non-empty string$"):
        llmobs.llm_span("s", provider="", model="gpt")
    with pytest.raises(ValueError, match=r"^model: must be a non-empty string$"):
        llmobs.llm_span("s", provider="openai", model="")
    with pytest.raises(ValueError, match=r"^provider:"):
        llmobs.llm_span("s", provider=cast("str", 3), model="gpt")
    with pytest.raises(ValueError, match=r"^model:"):
        llmobs.llm_span("s", provider="openai", model=cast("str", None))


def test_exception_in_body_recorded_and_reraised(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start()

    class ScenarioError(Exception):
        pass

    def call() -> None:
        with llmobs.llm_span("s", provider="p", model="m") as handle:
            handle.set_usage(input_tokens=1)
            raise ScenarioError("boom")

    with pytest.raises(ScenarioError, match="boom"):
        call()
    [span] = spans(exporter)
    assert span.status.status_code is StatusCode.ERROR
    assert span.status.description == "ScenarioError: boom"
    [event] = span.events
    assert event.name == "exception"
    event_attributes = event.attributes
    assert event_attributes is not None
    assert event_attributes["exception.type"] == "ScenarioError"
    assert event_attributes["exception.message"] == "boom"
    assert attrs(span)["gen_ai.usage.input_tokens"] == 1


def test_no_runtime_yields_noop_handle() -> None:
    with llmobs.llm_span(
        "s", provider="p", model="m", input_messages=[{"role": "user", "content": "hi"}]
    ) as handle:
        assert handle.trace_id is None
        assert handle.span_id is None
        handle.set_usage(input_tokens=3)
        handle.set_cost(1.0)
        handle.record_first_token()
        handle.set_output_messages([{"role": "assistant", "content": "yo"}])


def test_async_llm_span_with_metadata(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start()

    async def run() -> None:
        async with llmobs.llm_span(
            "s", provider="p", model="m", metadata={"plan": "pro"}
        ) as handle:
            handle.set_usage(input_tokens=2)

    asyncio.run(run())
    [span] = spans(exporter)
    assert dict(attrs(span)) == {
        "llmobs.span.kind": "llm",
        "gen_ai.operation.name": "chat",
        "gen_ai.provider.name": "p",
        "gen_ai.request.model": "m",
        "gen_ai.usage.input_tokens": 2,
        "llmobs.metadata.plan": "pro",
    }


def test_handle_after_end_ignores_setters(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start()
    with llmobs.llm_span("s", provider="p", model="m") as handle:
        pass
    handle.set_usage(input_tokens=1)
    handle.set_output_messages([{"role": "assistant", "content": "yo"}])
    handle.set_cost(1.0)
    handle.record_first_token()
    handle.set_metadata(plan="pro")
    [span] = spans(exporter)
    assert dict(attrs(span)) == {
        "llmobs.span.kind": "llm",
        "gen_ai.operation.name": "chat",
        "gen_ai.provider.name": "p",
        "gen_ai.request.model": "m",
    }


def test_inherited_set_input_and_set_output(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start()
    with llmobs.llm_span("s", provider="p", model="m") as handle:
        handle.set_input("raw-in")
        handle.set_output("raw-out")
    [span] = spans(exporter)
    attributes = attrs(span)
    assert attributes["llmobs.input"] == "raw-in"
    assert attributes["llmobs.input.format"] == "text"
    assert attributes["llmobs.output"] == "raw-out"
    assert attributes["llmobs.output.format"] == "text"


def test_body_return_value_not_recorded(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start()

    def call() -> str:
        with llmobs.llm_span("s", provider="p", model="m"):
            return "the-answer"

    assert call() == "the-answer"
    [span] = spans(exporter)
    assert "llmobs.output" not in attrs(span)
    assert "gen_ai.output.messages" not in attrs(span)
