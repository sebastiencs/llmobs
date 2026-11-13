"""Tests for ``llmobs._serialize`` (F plan § Serialization)."""

import asyncio
import dataclasses
import datetime
import decimal
import enum
import json
import pathlib
import types
import uuid

from llmobs._serialize import MAX_PAYLOAD_BYTES, attribute_value, serialize, truncate


def compact(value: object) -> str:
    """The exact JSON encoding ``serialize`` uses."""
    return json.dumps(value, separators=(",", ":"), ensure_ascii=False)


def as_json(value: object) -> str:
    """``serialize`` output for a non-string value, checked to be JSON."""
    text, fmt = serialize(value)
    assert fmt == "json"
    return text


class FakeModel:
    """Pydantic-like object: a callable ``model_dump``."""

    def __init__(self, dumped: dict[str, object]) -> None:
        self._dumped = dumped

    def model_dump(self, mode: str = "python") -> dict[str, object]:
        if mode != "json":
            raise AssertionError(mode)
        return self._dumped


class BrokenModel:
    """A ``model_dump`` that raises falls through to repr."""

    def model_dump(self, mode: str = "python") -> dict[str, object]:
        raise RuntimeError("boom")


class FakeNumpyScalar:
    """Numpy-like scalar: module ``numpy``, ``item``, ``ndim == 0``."""

    __module__ = "numpy"
    ndim = 0

    def __init__(self, value: object) -> None:
        self._value = value

    def item(self) -> object:
        return self._value


class FakeNumpyArray:
    """Numpy-like array: module ``numpy``, ``item``, ``ndim >= 1``, ``tolist``."""

    __module__ = "numpy"
    ndim = 1

    def __init__(self, value: list[object]) -> None:
        self._value = value

    def item(self) -> object:
        raise AssertionError("item() on an array")

    def tolist(self) -> list[object]:
        return self._value


class BadRepr:
    """An object whose ``__repr__`` raises."""

    def __repr__(self) -> str:
        raise RuntimeError("no repr for you")


class EvilKey:
    """A dict key whose ``__str__`` raises."""

    def __str__(self) -> str:
        raise RuntimeError("no str for you")


class Color(enum.Enum):
    RED = "red"
    NUM = 3


@dataclasses.dataclass
class Point:
    x: int
    y: str


def test_top_level_str_is_text() -> None:
    assert serialize("hello") == ("hello", "text")


def test_top_level_surrogates_replaced() -> None:
    assert serialize("\ud800abc\udfff") == ("�abc�", "text")


def test_scalar_rows() -> None:
    rows: list[tuple[object, str]] = [
        (None, "null"),
        (True, "true"),
        (False, "false"),
        (7, "7"),
        (-3, "-3"),
        (1.5, "1.5"),
        (-0.25, "-0.25"),
        (["hé"], '["hé"]'),
        (b"hi", '"data:application/octet-stream;base64,aGk="'),
        (bytearray(b"hi"), '"data:application/octet-stream;base64,aGk="'),
        (memoryview(b"hi"), '"data:application/octet-stream;base64,aGk="'),
        (datetime.datetime(2026, 1, 2, 3, 4, 5, 123456), '"2026-01-02T03:04:05.123456"'),
        (datetime.date(2026, 1, 2), '"2026-01-02"'),
        (datetime.time(3, 4, 5), '"03:04:05"'),
        (
            uuid.UUID("12345678-1234-5678-1234-567812345678"),
            '"12345678-1234-5678-1234-567812345678"',
        ),
        (decimal.Decimal("1.50"), '"1.50"'),
        (pathlib.PurePosixPath("/a/b"), '"/a/b"'),
        (pathlib.Path("/a/b"), '"/a/b"'),
        (Color.RED, '"red"'),
        (Color.NUM, "3"),
        (ValueError("oops"), '{"type":"ValueError","message":"oops"}'),
    ]
    for value, expected in rows:
        assert as_json(value) == expected


def test_nonfinite_floats() -> None:
    assert as_json(float("nan")) == '"NaN"'
    assert as_json(float("inf")) == '"Infinity"'
    assert as_json(float("-inf")) == '"-Infinity"'
    assert as_json([float("nan"), float("inf"), float("-inf")]) == '["NaN","Infinity","-Infinity"]'


def test_containers() -> None:
    assert as_json([1, "a", None]) == '[1,"a",null]'
    assert as_json((1, 2)) == "[1,2]"
    assert as_json({1, 2}) == compact([1, 2])
    assert as_json(frozenset({"a"})) == '["a"]'
    assert as_json(types.MappingProxyType({"a": 1})) == '{"a":1}'
    assert as_json({"a": {"b": [1, (2, 3)]}}) == '{"a":{"b":[1,[2,3]]}}'


def test_dict_keys_become_strings() -> None:
    text = as_json({1: "a", ("t",): "b", None: "c", 2.5: "e"})
    assert json.loads(text) == {"1": "a", "('t',)": "b", "None": "c", "2.5": "e"}


def test_dataclass_fields_in_declaration_order() -> None:
    assert as_json(Point(1, "a")) == '{"x":1,"y":"a"}'


def test_dataclass_with_nested_model() -> None:
    @dataclasses.dataclass
    class Nested:
        p: Point
        model: FakeModel
        tags: list[str]

    value = Nested(p=Point(2, "b"), model=FakeModel({"k": [1], "m": Point(3, "c")}), tags=["t"])
    assert (
        as_json(value) == '{"p":{"x":2,"y":"b"},"model":{"k":[1],"m":{"x":3,"y":"c"}},"tags":["t"]}'
    )


def test_model_dump_called_with_json_mode() -> None:
    assert as_json(FakeModel({"a": 1})) == '{"a":1}'


def test_model_dump_that_raises_falls_back_to_repr() -> None:
    broken = BrokenModel()
    assert as_json(broken) == compact(repr(broken))


def test_numpy_scalar_and_array() -> None:
    assert as_json(FakeNumpyScalar(5)) == "5"
    assert as_json(FakeNumpyScalar("x")) == '"x"'
    assert as_json(FakeNumpyArray([1, 2, 3])) == "[1,2,3]"
    assert (
        as_json(FakeNumpyArray([FakeNumpyScalar(4), {"a": FakeNumpyScalar("b")}]))
        == '[4,{"a":"b"}]'
    )


def test_functions_classes_modules_generators_coroutines_use_repr() -> None:
    values: list[object] = [len, int, json, BadRepr]
    gen = (i for i in range(2))
    values.append(gen)

    async def coro_fn() -> None:
        await asyncio.sleep(0)

    coro = coro_fn()
    try:
        for value in [*values, coro]:
            assert as_json(value) == compact(repr(value))
    finally:
        coro.close()


def test_object_repr_fallback() -> None:
    obj = object()
    assert as_json(obj) == compact(repr(obj))


def test_nested_combinations() -> None:
    value = {
        "bytes": b"hi",
        "when": datetime.date(2026, 1, 2),
        "nested": [Point(1, "a"), (FakeModel({"ok": True}), {2.5, 1.5})],
        "err": [ValueError("x")],
        "np": FakeNumpyArray([FakeNumpyScalar(1)]),
    }
    parsed = json.loads(as_json(value))
    assert parsed["bytes"] == "data:application/octet-stream;base64,aGk="
    assert parsed["when"] == "2026-01-02"
    assert parsed["nested"][0] == {"x": 1, "y": "a"}
    assert parsed["nested"][1][0] == {"ok": True}
    assert set(parsed["nested"][1][1]) == {1.5, 2.5}
    assert parsed["err"] == [{"type": "ValueError", "message": "x"}]
    assert parsed["np"] == [1]


def test_surrogates_replaced_inside_json() -> None:
    assert as_json({"k": "a\udfff"}) == '{"k":"a�"}'
    assert as_json({"\ud800": 1}) == '{"�":1}'


def test_cycles() -> None:
    lst: list[object] = [1]
    lst.append(lst)
    assert as_json(lst) == '[1,"[llmobs: cycle]"]'
    dct: dict[str, object] = {"a": 1}
    dct["self"] = dct
    assert as_json(dct) == '{"a":1,"self":"[llmobs: cycle]"}'


def test_shared_reference_is_not_a_cycle() -> None:
    shared = [1]
    assert as_json([shared, shared]) == "[[1],[1]]"


def nested_lists(depth: int) -> object:
    value: object = "x"
    for _ in range(depth):
        value = [value]
    return value


def unwrap(value: object, levels: int) -> object:
    for _ in range(levels):
        assert isinstance(value, list)
        value = value[0]
    return value


def test_depth_32_has_no_marker() -> None:
    parsed = json.loads(as_json(nested_lists(32)))
    assert unwrap(parsed, 31) == ["x"]


def test_depth_33_gets_marker() -> None:
    parsed = json.loads(as_json(nested_lists(33)))
    assert unwrap(parsed, 32) == "[llmobs: max depth]"
    assert "[llmobs: max depth]" not in json.loads(as_json(nested_lists(32)))


def test_never_raises() -> None:
    # rule 2 fallback: the marker names the object whose repr failed
    assert as_json({"k": BadRepr()}) == '{"k":"<unserializable BadRepr>"}'
    # a top-level value that fails to convert is the rule 7 tuple, not an
    # in-place JSON string
    assert serialize(BadRepr()) == ("<unserializable BadRepr>", "text")
    # a dict key whose str() raises escapes the rules: rule 7 fallback at the
    # top level, the in-place marker when the dict is itself nested
    assert serialize({EvilKey(): 1}) == ("<unserializable dict>", "text")
    assert as_json({"outer": {EvilKey(): 1}}) == '{"outer":"<unserializable dict>"}'


def marker(n: int) -> str:
    return f"…[llmobs: truncated, original {n} bytes]"


def test_text_exactly_at_limit_is_unchanged() -> None:
    text = "x" * MAX_PAYLOAD_BYTES
    assert serialize(text) == (text, "text")


def test_text_one_byte_over_limit_is_truncated() -> None:
    text = "x" * (MAX_PAYLOAD_BYTES + 1)
    assert serialize(text) == ("x" * MAX_PAYLOAD_BYTES + marker(1_048_577), "text")


def test_multibyte_character_at_the_cut() -> None:
    text = "é" * 600_000
    got, fmt = serialize(text)
    assert fmt == "text"
    assert got == "é" * 524_288 + marker(1_200_000)
    got.encode("utf-8")  # must not raise: no broken character


def test_multibyte_character_straddling_the_cut() -> None:
    text = "x" * (MAX_PAYLOAD_BYTES - 1) + "é" + "x" * 100
    got, fmt = serialize(text)
    assert fmt == "text"
    # the first byte of the é at the cut is dropped, not kept broken
    assert got == "x" * (MAX_PAYLOAD_BYTES - 1) + marker(1_048_677)


def test_json_over_limit_becomes_text() -> None:
    text, fmt = serialize({"k": "x" * 1_100_000})
    assert fmt == "text"
    assert text.startswith('{"k":"')
    assert text.endswith(marker(1_100_008))


def test_truncate_directly() -> None:
    assert truncate("abc", "json") == ("abc", "json")
    got, fmt = truncate("ab" * (MAX_PAYLOAD_BYTES // 2 + 1), "json")
    assert fmt == "text"
    assert got == "ab" * (MAX_PAYLOAD_BYTES // 2) + marker(MAX_PAYLOAD_BYTES + 2)


def test_attribute_value_scalars() -> None:
    assert attribute_value("a") == "a"
    assert attribute_value(True) is True
    assert attribute_value(False) is False
    assert attribute_value(5) == 5
    assert attribute_value(0) == 0
    assert attribute_value(-(2**53 - 1)) == -(2**53 - 1)
    assert attribute_value(2**53 - 1) == 2**53 - 1
    assert attribute_value(1.5) == 1.5
    assert attribute_value(-0.0) == -0.0


def test_attribute_value_int_beyond_2_53() -> None:
    assert attribute_value(2**53) == "9007199254740992"
    assert attribute_value(-(2**53)) == "-9007199254740992"
    assert attribute_value(2**60) == compact(2**60)


def test_attribute_value_other_becomes_json_string() -> None:
    assert attribute_value(None) == "null"
    assert attribute_value([1, 2]) == "[1,2]"
    assert attribute_value({"a": True}) == '{"a":true}'
    assert attribute_value(float("nan")) == '"NaN"'
    assert attribute_value(float("inf")) == '"Infinity"'
    assert attribute_value([BadRepr()]) == '["<unserializable BadRepr>"]'
    assert attribute_value(BadRepr()) == "<unserializable BadRepr>"


def test_attribute_value_truncates_strings() -> None:
    assert attribute_value("x" * (MAX_PAYLOAD_BYTES + 1)) == (
        "x" * MAX_PAYLOAD_BYTES + marker(1_048_577)
    )
