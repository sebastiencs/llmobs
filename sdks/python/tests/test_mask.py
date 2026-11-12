"""Tests for the capture-time masking helper (plan F § P4)."""

import logging
from collections.abc import Callable

import pytest
from opentelemetry.sdk.trace.export.in_memory_span_exporter import InMemorySpanExporter

from llmobs._mask import apply
from llmobs._runtime import Runtime

Memory = tuple[InMemorySpanExporter, Callable[..., Runtime]]


def _upper(value: object) -> object:
    return value.upper() if isinstance(value, str) else value


def _boom(value: object) -> object:
    raise ValueError("nope")


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
