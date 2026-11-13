"""The canonical form of an exported OTLP request (plan F § Conformance, D-F-9).

Test code shared by the conformance suite: it rewrites what the OTLP encoder
produced into the canonical document a fixture's ``input.json`` holds, so the
Python SDK, the TypeScript SDK and the Rust golden test compare one shape.
"""

from __future__ import annotations

import base64
from collections.abc import Sequence
from typing import Any

from google.protobuf.json_format import MessageToDict
from opentelemetry.exporter.otlp.proto.common.trace_encoder import encode_spans
from opentelemetry.sdk.trace import ReadableSpan

# Rule 2: 2025-12-31T23:59:50Z plus one millisecond per creation rank.
_EPOCH = 1767225590000000000
_MILLI = 1_000_000
# Rule 3: the only resource attributes the canonical document keeps.
_RESOURCE_KEYS = ("service.name", "telemetry.distro.name")
# Rule 6: an exception event keeps only these attributes.
_EXCEPTION_KEYS = ("exception.message", "exception.type")


def canonical(spans: Sequence[ReadableSpan]) -> dict[str, Any]:
    """The canonical OTLP document of ``spans`` (§ Conformance rules 1-7)."""
    # Rules 7 and 1: the tests use sequential ids, so sorting by span id
    # lists the spans in creation order.
    ordered = sorted(spans, key=lambda span: span.context.span_id)
    document = MessageToDict(encode_spans(ordered), use_integers_for_enums=True)
    resources: list[dict[str, Any]] = document["resourceSpans"]
    raw: list[dict[str, Any]] = [
        span
        for resource in resources
        for scoped in resource.get("scopeSpans", [])
        for span in scoped.get("spans", [])
    ]
    trace_ranks: dict[str, int] = {}
    span_ranks: dict[str, int] = {}
    for span in raw:
        trace_ranks.setdefault(_hex_id(span["traceId"]), len(trace_ranks) + 1)
        span_ranks.setdefault(_hex_id(span["spanId"]), len(span_ranks) + 1)
    return {
        "resourceSpans": [
            {
                "resource": {"attributes": _resource_attributes(resources)},
                "scopeSpans": [
                    {
                        "scope": {"name": _scope_name(resources), "version": "0.0.0"},
                        "spans": [
                            _canonical_span(span, rank, trace_ranks, span_ranks)
                            for rank, span in enumerate(raw, start=1)
                        ],
                    }
                ],
            }
        ]
    }


def _canonical_span(
    span: dict[str, Any],
    rank: int,
    trace_ranks: dict[str, int],
    span_ranks: dict[str, int],
) -> dict[str, Any]:
    """One span under rules 1 (ids), 2 (times) and 4 (fields kept)."""
    end = str(_EPOCH + (1000 - rank) * _MILLI)
    canonical: dict[str, Any] = {
        "traceId": f"{trace_ranks[_hex_id(span['traceId'])]:032x}",
        "spanId": f"{span_ranks[_hex_id(span['spanId'])]:016x}",
    }
    if span.get("parentSpanId"):
        parent = span_ranks.get(_hex_id(span["parentSpanId"]))
        if parent is not None:
            canonical["parentSpanId"] = f"{parent:016x}"
    canonical["name"] = span["name"]
    canonical["kind"] = span["kind"]
    canonical["startTimeUnixNano"] = str(_EPOCH + rank * _MILLI)
    canonical["endTimeUnixNano"] = end
    attributes = _attributes(span.get("attributes", []))
    if attributes:
        canonical["attributes"] = attributes
    events = _events(span.get("events", []), end)
    if events:
        canonical["events"] = events
    status = span.get("status", {})
    if status.get("code", 0) != 0:
        canonical_status: dict[str, Any] = {"code": status["code"]}
        if status.get("message"):
            canonical_status["message"] = status["message"]
        canonical["status"] = canonical_status
    return canonical


def _events(events: list[dict[str, Any]], end: str) -> list[dict[str, Any]]:
    """Events under rules 2, 5 and 6: the span's end time, reduced exceptions."""
    canonical = []
    for event in events:
        attributes = event.get("attributes", [])
        if event["name"] == "exception":
            attributes = [a for a in attributes if a["key"] in _EXCEPTION_KEYS]
        canonical_event: dict[str, Any] = {"timeUnixNano": end, "name": event["name"]}
        canonical_attributes = _attributes(attributes)
        if canonical_attributes:
            canonical_event["attributes"] = canonical_attributes
        canonical.append(canonical_event)
    return canonical


def _attributes(attributes: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Rule 5: sorted by key, one ``key``/``value`` pair each."""
    return [
        {"key": attribute["key"], "value": _value(attribute["value"])}
        for attribute in sorted(attributes, key=lambda attribute: attribute["key"])
    ]


def _value(value: dict[str, Any]) -> dict[str, Any]:
    """Rule 5: OTLP/JSON encoding; ``intValue`` as a decimal string, arrays recursed."""
    if "intValue" in value:
        return {"intValue": str(value["intValue"])}
    if "arrayValue" in value:
        items = value["arrayValue"].get("values", [])
        return {"arrayValue": {"values": [_value(item) for item in items]}}
    return value


def _resource_attributes(resources: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Rule 3: only ``service.name`` and ``telemetry.distro.name`` survive."""
    attributes = resources[0].get("resource", {}).get("attributes", [])
    return _attributes([a for a in attributes if a["key"] in _RESOURCE_KEYS])


def _scope_name(resources: list[dict[str, Any]]) -> str:
    """Rule 3: the scope name of the exported spans."""
    scoped = resources[0].get("scopeSpans", [])
    if scoped:
        return str(scoped[0].get("scope", {}).get("name", "llmobs"))
    return "llmobs"


def _hex_id(encoded: str) -> str:
    """A base64 id from the OTLP/JSON encoding, as lowercase hex."""
    return base64.b64decode(encoded).hex()
