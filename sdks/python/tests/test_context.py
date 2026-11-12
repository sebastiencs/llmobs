"""Tests for ``context()`` and the context processor (plan F § P4)."""

import asyncio
import contextvars
import logging
import sys
from collections.abc import Callable, Mapping
from concurrent.futures import ThreadPoolExecutor
from typing import Any

import pytest
from opentelemetry.baggage import get_all, set_baggage
from opentelemetry.context import attach, detach, set_value
from opentelemetry.sdk.trace import ReadableSpan
from opentelemetry.sdk.trace.export.in_memory_span_exporter import InMemorySpanExporter
from opentelemetry.util.types import AnyValue

import llmobs
from llmobs._context import CONTEXT_KEY, ContextValues, current_values
from llmobs._runtime import Runtime

Memory = tuple[InMemorySpanExporter, Callable[..., Runtime]]
Spans = Callable[[InMemorySpanExporter], list[ReadableSpan]]


def attrs(span: ReadableSpan) -> Mapping[str, AnyValue]:
    """The span's attributes; every span these tests build carries them."""
    assert span.attributes is not None
    return span.attributes


def _redact(value: object) -> object:
    return "[redacted]" if value == "alice@example.com" else value


def test_values_on_span_child_and_third_party_tracer(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    runtime = start(environment="prod", release="1.2.3")
    with llmobs.context(
        session_id="chat-42", user_id="u-17", tags=["beta"], metadata={"plan": "pro"}
    ):
        with (
            runtime.tracer.start_as_current_span("outer"),
            runtime.tracer.start_as_current_span("child"),
        ):
            pass
        third_party = runtime.provider.get_tracer("thirdparty")
        with third_party.start_as_current_span("third"):
            pass
    by_name = {span.name: span for span in spans(exporter)}
    assert set(by_name) == {"outer", "child", "third"}
    for name, span in by_name.items():
        attributes = attrs(span)
        assert attributes["llmobs.session.id"] == "chat-42", name
        assert attributes["llmobs.user.id"] == "u-17", name
        assert attributes["llmobs.tags"] == ("beta",), name
        assert attributes["llmobs.metadata.plan"] == "pro", name
        assert attributes["llmobs.environment"] == "prod", name
        assert attributes["llmobs.release"] == "1.2.3", name


def test_nested_merge(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    runtime = start()
    with (
        llmobs.context(
            session_id="outer-s",
            user_id="outer-u",
            tags=["a", "b"],
            metadata={"k": "outer", "keep": 1},
        ),
        llmobs.context(session_id="inner-s", tags=["b", "c"], metadata={"k": "inner"}),
        runtime.tracer.start_as_current_span("inner"),
    ):
        pass
    attributes = attrs(spans(exporter)[0])
    assert attributes["llmobs.session.id"] == "inner-s"  # the inner session wins when given
    assert (
        attributes["llmobs.user.id"] == "outer-u"
    )  # the outer user is kept when the inner has none
    assert attributes["llmobs.tags"] == ("a", "b", "c")  # union, outer first, no duplicates
    assert attributes["llmobs.metadata.k"] == "inner"  # metadata merges by key, inner wins
    assert attributes["llmobs.metadata.keep"] == 1


def test_exit_restores_outer_values(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    runtime = start()
    with llmobs.context(session_id="outer-s", tags=["a"]):
        with llmobs.context(session_id="inner-s", tags=["b"]):
            pass
        assert current_values().session_id == "outer-s"
        assert current_values().tags == ("a",)
        with runtime.tracer.start_as_current_span("after"):
            pass
    assert current_values() == ContextValues()
    attributes = attrs(spans(exporter)[0])
    assert attributes["llmobs.session.id"] == "outer-s"
    assert attributes["llmobs.tags"] == ("a",)


def test_concurrent_async_tasks_never_share_values(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    runtime = start()

    async def task(session: str, delay: float) -> None:
        with llmobs.context(session_id=session, tags=[session]):
            await asyncio.sleep(delay)
            assert current_values().session_id == session
            with runtime.tracer.start_as_current_span(f"span-{session}"):
                pass

    async def main() -> None:
        await asyncio.gather(task("s1", 0.05), task("s2", 0.0))

    asyncio.run(main())
    by_name = {span.name: span for span in spans(exporter)}
    assert attrs(by_name["span-s1"])["llmobs.session.id"] == "s1"
    assert attrs(by_name["span-s1"])["llmobs.tags"] == ("s1",)
    assert attrs(by_name["span-s2"])["llmobs.session.id"] == "s2"
    assert attrs(by_name["span-s2"])["llmobs.tags"] == ("s2",)


def test_copy_context_in_executor_keeps_values(memory: Memory) -> None:
    _, start = memory
    start()

    def read() -> str | None:
        return current_values().session_id

    with llmobs.context(session_id="chat-42"), ThreadPoolExecutor(max_workers=2) as executor:
        copied = executor.submit(contextvars.copy_context().run, read).result()
        plain = executor.submit(read).result()
    assert copied == "chat-42"  # the documented pattern (D-F-14)
    if not getattr(sys.flags, "thread_inherit_context", False):
        assert plain is None


def test_propagate_true_sets_only_the_two_baggage_keys() -> None:
    with llmobs.context(
        session_id="chat-42",
        user_id="u-17",
        tags=["beta"],
        metadata={"plan": "pro"},
        propagate=True,
    ):
        assert dict(get_all()) == {"llmobs.session.id": "chat-42", "llmobs.user.id": "u-17"}
    assert not get_all()


def test_no_baggage_without_propagate() -> None:
    with llmobs.context(session_id="chat-42", tags=["beta"], metadata={"plan": "pro"}):
        assert not get_all()


def test_span_gets_session_user_from_baggage_only(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    runtime = start()
    ctx = set_baggage("llmobs.session.id", "from-baggage")
    ctx = set_baggage("llmobs.user.id", "u-9", context=ctx)
    token = attach(ctx)
    try:
        with runtime.tracer.start_as_current_span("bagged"):
            pass
    finally:
        detach(token)
    attributes = attrs(spans(exporter)[0])
    assert attributes["llmobs.session.id"] == "from-baggage"
    assert attributes["llmobs.user.id"] == "u-9"
    assert "llmobs.tags" not in attributes


def test_invalid_arguments_are_dropped() -> None:
    bad_ids: dict[str, Any] = {"session_id": 123, "user_id": object(), "tags": "beta"}
    with llmobs.context(**bad_ids):
        assert current_values() == ContextValues(tags=("beta",))  # a bare string is one tag
    bad_items: dict[str, Any] = {"tags": ["beta", 1, "gamma"], "metadata": {1: "x", "ok": 2}}
    with llmobs.context(**bad_items):
        assert current_values() == ContextValues(tags=("beta", "gamma"), metadata={"ok": 2})
    bad_types: dict[str, Any] = {"tags": 123, "metadata": ["a"]}
    with llmobs.context(**bad_types):
        assert current_values() == ContextValues()


def test_metadata_attributes_follow_the_key_rule(
    memory: Memory, spans: Spans, caplog: pytest.LogCaptureFixture
) -> None:
    exporter, start = memory
    runtime = start()
    with (
        caplog.at_level(logging.DEBUG, logger="llmobs"),
        llmobs.context(metadata={"seats": 3, "obj": {"a": 1}, "bad key": 1}),
    ):
        with runtime.tracer.start_as_current_span("one"):
            pass
        with runtime.tracer.start_as_current_span("two"):
            pass
    for span in spans(exporter):
        attributes = attrs(span)
        assert attributes["llmobs.metadata.seats"] == 3
        assert attributes["llmobs.metadata.obj"] == '{"a":1}'
        assert "llmobs.metadata.bad key" not in attributes
    skips = [
        r for r in caplog.records if r.name == "llmobs" and "metadata key skipped" in r.getMessage()
    ]
    assert len(skips) == 1  # logged once per key, not once per span


def test_processor_never_raises_on_garbage_under_the_key(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    runtime = start()
    token = attach(set_value(CONTEXT_KEY, object()))
    try:
        with runtime.tracer.start_as_current_span("garbage"):
            pass
    finally:
        detach(token)
    assert current_values() == ContextValues()
    assert [span.name for span in spans(exporter)] == ["garbage"]


def test_reentering_an_entered_context_is_a_noop(caplog: pytest.LogCaptureFixture) -> None:
    block = llmobs.context(session_id="s")
    with block:
        assert current_values().session_id == "s"
        with caplog.at_level(logging.DEBUG, logger="llmobs"), block:
            assert current_values().session_id == "s"
        assert current_values().session_id == "s"  # the inner exit detached nothing
    assert current_values() == ContextValues()  # the outer exit did
    messages = [record.getMessage() for record in caplog.records if record.name == "llmobs"]
    assert messages == ["llmobs: context re-entered; this entry is a no-op"]


def test_async_context_manager_stamps_and_restores(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    runtime = start()

    async def main() -> None:
        async with llmobs.context(session_id="chat-9"):
            assert current_values().session_id == "chat-9"
            with runtime.tracer.start_as_current_span("async"):
                pass
        assert current_values() == ContextValues()

    asyncio.run(main())
    assert attrs(spans(exporter)[0])["llmobs.session.id"] == "chat-9"


def test_metadata_masked_on_entry(memory: Memory, spans: Spans) -> None:
    exporter, start = memory
    runtime = start(mask=_redact)
    with (
        llmobs.context(metadata={"email": "alice@example.com", "n": 1}),
        runtime.tracer.start_as_current_span("masked"),
    ):
        pass
    attributes = attrs(spans(exporter)[0])
    assert attributes["llmobs.metadata.email"] == "[redacted]"
    assert attributes["llmobs.metadata.n"] == 1
