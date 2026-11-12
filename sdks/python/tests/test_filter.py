"""Tests for the default export filter and the on_end wiring (plan F § P8)."""

import logging
from collections.abc import Callable
from typing import Any

import pytest
from opentelemetry.sdk.trace import ReadableSpan
from opentelemetry.sdk.trace.export.in_memory_span_exporter import InMemorySpanExporter
from opentelemetry.sdk.util.instrumentation import InstrumentationScope
from opentelemetry.trace import SpanContext, TraceFlags

from llmobs._filter import default_filter
from llmobs._processor import LlmobsSpanProcessor


def _span(
    *,
    name: str = "s",
    scope: str | None = None,
    attributes: dict[str, Any] | None = None,
) -> ReadableSpan:
    return ReadableSpan(
        name=name,
        # The batch processor only queues sampled spans.
        context=SpanContext(
            trace_id=1,
            span_id=1,
            is_remote=False,
            trace_flags=TraceFlags(TraceFlags.SAMPLED),
        ),
        attributes=attributes,
        instrumentation_scope=InstrumentationScope(scope) if scope is not None else None,
    )


@pytest.mark.parametrize("scope", ["llmobs", "ai"])
def test_scope_names_export(scope: str) -> None:
    assert default_filter(_span(scope=scope)) is True


@pytest.mark.parametrize("scope", [None, "httpx", "openinference.instrumentation.openai"])
def test_other_or_missing_scope_needs_a_matching_attribute(scope: str | None) -> None:
    assert default_filter(_span(scope=scope)) is False


@pytest.mark.parametrize(
    "key",
    [
        "llmobs.input",
        "llmobs.span.kind",
        "gen_ai.input.messages",
        "llm.token_count.prompt_tokens",
        "openinference.span.kind",
        "ai.prompt",
        "traceloop.entity.name",
        "langfuse.session.id",
        "lmnr.span.type",
        "gcp.vertex.agent.tool_name",
    ],
)
def test_attribute_prefixes_export(key: str) -> None:
    assert default_filter(_span(scope="httpx", attributes={key: "v"})) is True


@pytest.mark.parametrize("key", ["input.value", "output.value"])
def test_exact_attribute_keys_export(key: str) -> None:
    assert default_filter(_span(scope=None, attributes={key: "v"})) is True


@pytest.mark.parametrize(
    "key",
    ["inputs.value", "input.values", "myllmobs.x", "llmobs", "openai.input.value", "http.method"],
)
def test_near_miss_keys_do_not_export(key: str) -> None:
    assert default_filter(_span(scope="httpx", attributes={key: "v"})) is False


def _drain(processor: LlmobsSpanProcessor, exporter: InMemorySpanExporter) -> list[str]:
    processor.shutdown()
    return [span.name for span in exporter.get_finished_spans()]


_HTTP_ATTRIBUTES: dict[str, Any] = {"http.request.method": "GET", "url.full": "https://example.com"}


def _processor(
    exporter: InMemorySpanExporter,
    *,
    export_filter: Callable[[ReadableSpan], bool] | None = None,
    default_filter_on: bool,
) -> LlmobsSpanProcessor:
    return LlmobsSpanProcessor(
        exporter,
        environment=None,
        release=None,
        export_filter=export_filter,
        default_filter=default_filter_on,
    )


def test_http_span_dropped_with_the_default_filter() -> None:
    exporter = InMemorySpanExporter()
    processor = _processor(exporter, default_filter_on=True)
    processor.on_end(_span(name="GET /x", scope="httpx", attributes=_HTTP_ATTRIBUTES))
    assert _drain(processor, exporter) == []


def test_http_span_kept_in_own_mode() -> None:
    exporter = InMemorySpanExporter()
    processor = _processor(exporter, default_filter_on=False)
    processor.on_end(_span(name="GET /x", scope="httpx", attributes=_HTTP_ATTRIBUTES))
    assert _drain(processor, exporter) == ["GET /x"]


def test_llm_span_kept_by_the_default_filter() -> None:
    exporter = InMemorySpanExporter()
    processor = _processor(exporter, default_filter_on=True)
    processor.on_end(_span(name="chat", scope="httpx", attributes={"input.value": "hi"}))
    assert _drain(processor, exporter) == ["chat"]


def test_export_filter_replaces_the_default_rule() -> None:
    def mine(span: ReadableSpan) -> bool:
        return span.name == "mine"

    exporter = InMemorySpanExporter()
    # Global mode (default filter on): only the explicit filter decides.
    processor = _processor(exporter, export_filter=mine, default_filter_on=True)
    processor.on_end(_span(name="mine", scope="httpx", attributes=_HTTP_ATTRIBUTES))
    processor.on_end(_span(name="chat", scope="httpx", attributes={"input.value": "hi"}))
    assert _drain(processor, exporter) == ["mine"]


def test_export_filter_applies_in_own_mode() -> None:
    def never(span: ReadableSpan) -> bool:
        return False

    exporter = InMemorySpanExporter()
    processor = _processor(exporter, export_filter=never, default_filter_on=False)
    processor.on_end(_span(name="chat", scope="llmobs", attributes={"llmobs.input": "hi"}))
    assert _drain(processor, exporter) == []


def test_raising_export_filter_exports(
    caplog: pytest.LogCaptureFixture,
) -> None:
    def broken(span: ReadableSpan) -> bool:
        raise RuntimeError("no")

    exporter = InMemorySpanExporter()
    processor = _processor(exporter, export_filter=broken, default_filter_on=False)
    with caplog.at_level(logging.DEBUG, logger="llmobs"):
        processor.on_end(_span(name="GET /x", scope="httpx", attributes=_HTTP_ATTRIBUTES))
    assert _drain(processor, exporter) == ["GET /x"]
    debug = [record.getMessage() for record in caplog.records if record.name == "llmobs"]
    assert debug == ["llmobs: export_filter raised RuntimeError; span exported"]
