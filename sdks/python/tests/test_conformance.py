"""Conformance tests: the SDK must emit the shared fixtures (plan F § P11, D-F-9).

One test per ``fixtures/otlp/llmobs/sdk-*`` case: each runs its scenario row
through the in-memory pipeline, flushes, and compares the canonical export
with the case's ``input.json`` as parsed JSON (never rewritten from here).
"""

import base64
import json
from collections.abc import Callable, Iterator, Sequence
from pathlib import Path

import pytest
from _conformance import canonical
from opentelemetry.sdk.trace import ReadableSpan
from opentelemetry.sdk.trace.export.in_memory_span_exporter import InMemorySpanExporter

import llmobs
from llmobs._runtime import Runtime

Memory = tuple[InMemorySpanExporter, Callable[..., Runtime]]
Spans = Callable[[InMemorySpanExporter], list[ReadableSpan]]

_FIXTURES = Path(__file__).parents[3] / "fixtures" / "otlp" / "llmobs"

# The 70-byte 1x1 PNG of § Scenarios.
_PNG_B64 = (
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJ"
    "AAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg=="
)
PNG = base64.b64decode(_PNG_B64)


class ScenarioError(Exception):
    """The error class the scenarios raise (§ Conformance rule 6)."""


def redact(value: object) -> object:
    """Replace every string equal to the scenario address with ``[redacted]``."""
    if isinstance(value, str):
        return "[redacted]" if value == "alice@example.com" else value
    if isinstance(value, dict):
        return {key: redact(item) for key, item in value.items()}
    if isinstance(value, list):
        return [redact(item) for item in value]
    return value


def assert_canonical(finished: Sequence[ReadableSpan], case: str) -> None:
    """Compare the canonical form of ``finished`` with the case's ``input.json``."""
    actual = canonical(finished)
    fixture = _FIXTURES / case / "input.json"
    expected = json.loads(fixture.read_text(encoding="utf-8"))
    assert actual == expected, (
        f"{case}: canonical export does not match {fixture}:\n{json.dumps(actual, indent=2)}"
    )


def test_observe_tool(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start()

    @llmobs.observe(kind="tool", name="search_docs")
    def search_docs(query: str, limit: int = 2) -> list[str]:
        return ["doc-1", "doc-2"]

    search_docs("pricing")
    assert_canonical(spans(exporter), "sdk-observe-tool")


def test_observe_nested(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start()

    @llmobs.observe(kind="retriever", name="retrieve")
    def retrieve(question: str) -> list[dict[str, str]]:
        return [{"id": "d1"}]

    @llmobs.observe(name="answer")
    def answer(question: str) -> str:
        retrieve(question)
        return "It is 42."

    answer("why?")
    assert_canonical(spans(exporter), "sdk-observe-nested")


def test_observe_error(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start()

    @llmobs.observe(name="fails")
    def fails(x: int) -> None:
        raise ScenarioError("boom")

    with pytest.raises(ScenarioError):
        fails(1)
    assert_canonical(spans(exporter), "sdk-observe-error")


def test_generator_text(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start()

    @llmobs.observe(name="stream")
    def stream(prompt: str) -> Iterator[str]:
        yield "Hel"
        yield "lo"

    assert list(stream("hi")) == ["Hel", "lo"]
    assert_canonical(spans(exporter), "sdk-generator-text")


def test_generator_json(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start()

    @llmobs.observe(name="numbers")
    def numbers() -> Iterator[dict[str, int]]:
        yield {"n": 1}
        yield {"n": 2}

    assert list(numbers()) == [{"n": 1}, {"n": 2}]
    assert_canonical(spans(exporter), "sdk-generator-json")


def test_span_manual(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start()
    with llmobs.span("rerank", kind="reranker", input=["a", "b"]) as s:
        s.set_output(["b", "a"])
        s.set_metadata(model="bge")
    assert_canonical(spans(exporter), "sdk-span-manual")


def test_llm_span(memory: Memory, spans: Spans) -> None:
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
    ) as s:
        s.set_output_messages([{"role": "assistant", "content": "hello", "finish_reason": "stop"}])
        s.set_response_model("gpt-4o-mini-2024-07-18")
        s.set_usage(input_tokens=12, output_tokens=3)
    assert_canonical(spans(exporter), "sdk-llm-span")


def test_context(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start(environment="staging", release="1.2.3")

    @llmobs.observe(name="lookup")
    def lookup() -> None:
        return None

    @llmobs.observe(name="turn")
    def turn() -> None:
        lookup()
        return None

    with llmobs.context(
        session_id="chat-42",
        user_id="u-17",
        tags=["beta"],
        metadata={"plan": "pro", "seats": 3},
    ):
        turn()
    assert_canonical(spans(exporter), "sdk-context")


def test_capture_off(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start(capture_content=False)

    @llmobs.observe(kind="tool", name="lookup")
    def lookup(query: str) -> str:
        return "x"

    lookup("q")
    assert_canonical(spans(exporter), "sdk-capture-off")


def test_mask(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start(mask=redact)

    @llmobs.observe(name="notify")
    def notify(email: str) -> str:
        return "sent to alice@example.com"

    notify("alice@example.com")
    assert_canonical(spans(exporter), "sdk-mask")


def test_media_bytes(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    start()

    @llmobs.observe(name="upload")
    def upload(data: bytes) -> str:
        return "ok"

    upload(PNG)
    assert_canonical(spans(exporter), "sdk-media-bytes")


def test_every_case_has_a_function() -> None:
    cases = sorted(path.name for path in _FIXTURES.glob("sdk-*") if path.is_dir())
    assert cases, "no sdk-* fixture directories found"
    for case in cases:
        name = f"test_{case.removeprefix('sdk-').replace('-', '_')}"
        assert name in globals(), f"{case} has no test function {name}"
