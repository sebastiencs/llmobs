"""Tolerant payload serialization (F plan § Serialization).

``serialize`` converts any value into a ``("text" | "json")`` pair for span
payloads and never raises. ``attribute_value`` converts metadata values to
OpenTelemetry attribute scalars (§ Span attributes).
"""

import base64
import dataclasses
import datetime
import decimal
import enum
import json
import math
import pathlib
import uuid
from collections.abc import Mapping
from typing import Final, Literal

Format = Literal["text", "json"]
MAX_PAYLOAD_BYTES: Final = 1_048_576
MAX_DEPTH: Final = 32

_MAX_DEPTH_MARKER = "[llmobs: max depth]"
_CYCLE_MARKER = "[llmobs: cycle]"
_UNSERIALIZABLE = "<unserializable {}>"
_TRUNCATED = "…[llmobs: truncated, original {} bytes]"


def _text(s: str) -> str:
    """Replace unpaired surrogate code points with U+FFFD (rule 5)."""
    return s.encode("utf-16", "surrogatepass").decode("utf-16", "replace")


def _guard(value: object, depth: int, path: set[int]) -> str | None:
    """Return the rule 3/4 replacement marker for a container, if any."""
    if depth > MAX_DEPTH:
        return _MAX_DEPTH_MARKER
    if id(value) in path:
        return _CYCLE_MARKER
    return None


def _child(value: object, depth: int, path: set[int]) -> object:
    """One contained value: converted, or the in-place marker when it fails.

    A failure that escapes a child's own conversion (a raising ``str`` on one
    of its keys, a raising accessor, …) names the child, keeping the rest of
    the payload; only the top-level value itself falls to rule 7.
    """
    try:
        return _convert(value, depth, path)
    except Exception:  # noqa: BLE001  # never raise into the app (spec 10 rule 4)
        return _UNSERIALIZABLE.format(type(value).__name__)


def _convert(value: object, depth: int, path: set[int]) -> object:
    """Rules 2-4 conversion; ``depth`` is the slot the value sits in (top level = 1)."""
    if value is None:
        return None
    if isinstance(value, bool):
        return value
    if isinstance(value, int):
        return value
    if isinstance(value, float):
        if math.isfinite(value):
            return value
        if math.isnan(value):
            return "NaN"
        return "Infinity" if value > 0 else "-Infinity"
    if isinstance(value, str):
        return _text(value)
    if isinstance(value, (bytes, bytearray, memoryview)):
        b64 = base64.b64encode(value).decode("ascii")
        return f"data:application/octet-stream;base64,{b64}"
    if isinstance(value, (datetime.datetime, datetime.date, datetime.time)):
        return _text(value.isoformat())
    if isinstance(value, (uuid.UUID, decimal.Decimal, pathlib.PurePath)):
        return _text(str(value))
    if isinstance(value, enum.Enum):
        return _convert(value.value, depth, path)
    if dataclasses.is_dataclass(value) and not isinstance(value, type):
        marker = _guard(value, depth, path)
        if marker is not None:
            return marker
        path.add(id(value))
        try:
            fields = dataclasses.fields(value)
            return {f.name: _child(getattr(value, f.name), depth + 1, path) for f in fields}
        finally:
            path.discard(id(value))
    model_dump = getattr(value, "model_dump", None)
    if callable(model_dump):
        marker = _guard(value, depth, path)
        if marker is not None:
            return marker
        try:
            dumped = model_dump(mode="json")
        except Exception:  # noqa: BLE001
            # never raise into the app (spec 10 rule 4); a broken model_dump falls to repr
            dumped = None
        else:
            path.add(id(value))
            try:
                return _convert(dumped, depth, path)
            finally:
                path.discard(id(value))
    if type(value).__module__ == "numpy":
        item = getattr(value, "item", None)
        if callable(item) and getattr(value, "ndim", 0) == 0:
            if id(value) in path:
                return _CYCLE_MARKER
            path.add(id(value))
            try:
                return _convert(item(), depth, path)
            finally:
                path.discard(id(value))
        tolist = getattr(value, "tolist", None)
        if callable(tolist) and callable(item):
            marker = _guard(value, depth, path)
            if marker is not None:
                return marker
            path.add(id(value))
            try:
                return _convert(tolist(), depth, path)
            finally:
                path.discard(id(value))
    if isinstance(value, BaseException):
        return {"type": type(value).__name__, "message": _text(str(value))}
    if isinstance(value, Mapping):
        marker = _guard(value, depth, path)
        if marker is not None:
            return marker
        path.add(id(value))
        try:
            return {_text(str(key)): _child(val, depth + 1, path) for key, val in value.items()}
        finally:
            path.discard(id(value))
    if isinstance(value, (list, tuple, set, frozenset)):
        marker = _guard(value, depth, path)
        if marker is not None:
            return marker
        path.add(id(value))
        try:
            return [_child(item, depth + 1, path) for item in value]
        finally:
            path.discard(id(value))
    try:
        return _text(repr(value))
    except Exception:
        # never raise into the app (spec 10 rule 4); at the top level the
        # failure belongs to the whole value, so it escapes to serialize's
        # rule-7 tuple instead of the in-place marker.
        if depth == 1:
            raise
        return _UNSERIALIZABLE.format(type(value).__name__)


def to_jsonable(value: object) -> object:
    """Convert ``value`` to JSON-safe types (§ rules 2-4); no truncation."""
    return _convert(value, 1, set())


def truncate(text: str, fmt: Format) -> tuple[str, Format]:
    """Cut ``text`` at MAX_PAYLOAD_BYTES UTF-8 bytes and mark the cut (rule 6)."""
    data = text.encode("utf-8")
    if len(data) <= MAX_PAYLOAD_BYTES:
        return (text, fmt)
    cut = data[:MAX_PAYLOAD_BYTES].decode("utf-8", "ignore")
    return (f"{cut}{_TRUNCATED.format(len(data))}", "text")


def serialize(value: object) -> tuple[str, Format]:
    """Serialize a payload value; never raises (§ Serialization rules 1-7)."""
    try:
        if isinstance(value, str):
            return truncate(_text(value), "text")
        text = json.dumps(
            to_jsonable(value), separators=(",", ":"), ensure_ascii=False, allow_nan=False
        )
        return truncate(text, "json")
    except Exception:  # noqa: BLE001
        # never raise into the app (spec 10 rule 4)
        return (_UNSERIALIZABLE.format(type(value).__name__), "text")


def attribute_value(value: object) -> str | bool | int | float:
    """Convert a metadata value to an OTel attribute scalar (§ Span attributes)."""
    if isinstance(value, str):
        return truncate(_text(value), "text")[0]
    if isinstance(value, bool):
        return value
    if isinstance(value, int) and abs(value) <= 2**53 - 1:
        return value
    if isinstance(value, float) and math.isfinite(value):
        return value
    return serialize(value)[0]
