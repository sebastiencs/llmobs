"""Masking: the capture-time helper and the export-time pass (D-F-6).

``apply`` routes a value through the ``mask`` callable of the running
runtime, when one is configured. ``MaskingExporter`` wraps the real exporter
and rewrites payload attributes of instrumentor spans before export.
"""

import json
from collections.abc import Callable, Mapping, Sequence
from typing import Any

from opentelemetry.sdk.trace import Event, ReadableSpan
from opentelemetry.sdk.trace.export import SpanExporter, SpanExportResult
from opentelemetry.util import types as otel_types

from llmobs._log import logger
from llmobs._serialize import serialize

_MASK_FAILED = "[llmobs: mask failed]"

# § Payload keys (plan F README): exact keys, then the prefixes.
_PAYLOAD_KEYS = frozenset(
    {
        "llmobs.input",
        "llmobs.output",
        "gen_ai.input.messages",
        "gen_ai.output.messages",
        "gen_ai.system_instructions",
        "gen_ai.tool.call.arguments",
        "gen_ai.tool.call.result",
        "input.value",
        "output.value",
        "traceloop.entity.input",
        "traceloop.entity.output",
        "ai.prompt",
        "ai.prompt.messages",
        "ai.response.text",
        "ai.response.object",
        "ai.response.toolCalls",
        "ai.toolCall.args",
        "ai.toolCall.result",
        "langfuse.observation.input",
        "langfuse.observation.output",
        "langfuse.trace.input",
        "langfuse.trace.output",
        "lmnr.span.input",
        "lmnr.span.output",
    }
)
_PAYLOAD_PREFIXES = (
    "llm.input_messages.",
    "llm.output_messages.",
    "gen_ai.prompt.",
    "gen_ai.completion.",
)

Attributes = Mapping[str, otel_types.AttributeValue]


def _is_payload_key(key: str) -> bool:
    return key in _PAYLOAD_KEYS or key.startswith(_PAYLOAD_PREFIXES)


def _run_mask(mask: Callable[[Any], Any], value: object) -> object:
    """Run ``mask`` fail closed: a raise becomes the marker string (D-F-6)."""
    try:
        return mask(value)
    except Exception as error:  # noqa: BLE001  # never raise into the app (spec 10 rule 4)
        logger.debug("llmobs: mask raised %s", type(error).__name__)
        return _MASK_FAILED


def apply(value: object) -> object:
    """Pass ``value`` through the running ``mask``, when there is one (D-F-6).

    Without a runtime, or without a configured mask, the value is returned
    unchanged. A mask that raises fails closed: the value becomes the marker
    string and one debug line names the exception type, never the value.
    """
    # Deferred: _runtime imports the processor, which imports _context, which
    # imports this module.
    from llmobs._runtime import current

    runtime = current()
    if runtime is None:
        return value
    mask = runtime.config.mask
    if mask is None:
        return value
    return _run_mask(mask, value)


def _mask_text(mask: Callable[[Any], Any], text: str) -> str:
    """Decode, mask and re-encode one payload string (D-F-6).

    A string that looks like a JSON object or array and parses is decoded so
    the mask sees the structure; the masked result is a string as is, anything
    else compact JSON.
    """
    value: object = text
    stripped = text.lstrip()
    if stripped[:1] in ("{", "["):
        try:
            value = json.loads(text)
        except ValueError:
            value = text
    masked = _run_mask(mask, value)
    if isinstance(masked, str):
        return masked
    return serialize(masked)[0]


class MaskingExporter(SpanExporter):
    """Rewrites payload attributes of instrumentor spans, then exports (D-F-6).

    Installed only when a ``mask`` is configured or ``capture_content`` is
    false. Spans whose instrumentation scope is named ``llmobs`` are already
    masked at capture time and pass through untouched, as do spans without a
    payload key. The pass fails closed: on an unexpected error the whole batch
    is dropped (never exported unmasked) and one warning counts the spans.
    """

    def __init__(
        self,
        inner: SpanExporter,
        *,
        mask: Callable[[Any], Any] | None,
        capture_content: bool,
    ) -> None:
        self._inner = inner
        self._mask = mask
        self._capture_content = capture_content

    def export(self, spans: Sequence[ReadableSpan]) -> SpanExportResult:
        try:
            batch = [self._rewrite(span) for span in spans]
        except Exception:  # noqa: BLE001  # never raise into the app (spec 10 rule 4)
            logger.warning("llmobs: masking failed; dropped %d spans", len(spans))
            return SpanExportResult.FAILURE
        return self._inner.export(batch)

    def shutdown(self) -> None:
        self._inner.shutdown()

    def force_flush(self, timeout_millis: int = 30_000) -> bool:
        return self._inner.force_flush(timeout_millis)

    def _rewrite(self, span: ReadableSpan) -> ReadableSpan:
        """A new span with masked payload attributes, or the span itself."""
        scope = span.instrumentation_scope
        if scope is not None and scope.name == "llmobs":
            return span
        if not _has_payload_key(span.attributes) and not any(
            _has_payload_key(event.attributes) for event in span.events
        ):
            return span
        return ReadableSpan(
            name=span.name,
            context=span.context,
            parent=span.parent,
            resource=span.resource,
            attributes=self._mask_attributes(span.attributes),
            events=[
                Event(event.name, self._mask_attributes(event.attributes), event.timestamp)
                for event in span.events
            ],
            links=span.links,
            kind=span.kind,
            status=span.status,
            start_time=span.start_time,
            end_time=span.end_time,
            instrumentation_scope=span.instrumentation_scope,
        )

    def _mask_attributes(
        self, attributes: Attributes | None
    ) -> dict[str, otel_types.AttributeValue] | None:
        """Payload keys are masked (or dropped); every other key is kept."""
        if attributes is None:
            return None
        rewritten: dict[str, otel_types.AttributeValue] = {}
        for key, value in attributes.items():
            if not _is_payload_key(key):
                rewritten[key] = value
                continue
            # Only strings are masked; any other value is dropped, and so is
            # every payload key when capture_content is false.
            if isinstance(value, str) and self._capture_content:
                rewritten[key] = _mask_text(self._mask, value) if self._mask is not None else value
        return rewritten


def _has_payload_key(attributes: Attributes | None) -> bool:
    if attributes is None:
        return False
    return any(_is_payload_key(key) for key in attributes)
