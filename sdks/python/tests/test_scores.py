"""Tests for ``score()`` and the background queue (plan F § P9)."""

import itertools
import json
import logging
import os
import threading
import time
import uuid
from collections.abc import Callable
from typing import Any, Protocol

import pytest

import llmobs
from llmobs._runtime import Runtime
from llmobs._scores import ScoreQueue, uuid7

TEST_API_KEY = "sk-llmobs-test"
# Fills the score buffer to capacity once one batch is stuck in a retry.
_MAX_BUFFER_FILL = 10_000


class Recorder(Protocol):
    """The surface of the ``http_recorder`` fixture used by the tests."""

    url: str
    requests: list[tuple[str, str, dict[str, str], bytes]]

    def queue_response(self, status: int, headers: dict[str, str], body: bytes) -> None: ...


ScoreSetup = tuple[Recorder, Runtime]


def score_batches(recorder: Recorder) -> list[list[dict[str, Any]]]:
    """The parsed ``{"scores": [...]}`` bodies of every recorded score request."""
    return [
        doc["scores"]
        for method, path, _, body in recorder.requests
        if method == "POST" and path == "/api/v1/scores"
        for doc in [json.loads(body)]
    ]


def wait_for_request(recorder: Recorder, count: int, timeout_s: float = 5.0) -> None:
    """Block until ``count`` requests were recorded (the worker's 1 s window)."""
    deadline = time.monotonic() + timeout_s
    while len(recorder.requests) < count and time.monotonic() < deadline:
        time.sleep(0.01)


def record_waits(waited: list[float]) -> Callable[[float], bool]:
    """A replacement for the queue's ``wait``: record durations, never sleep."""

    def wait(seconds: float) -> bool:
        waited.append(seconds)
        return False

    return wait


def test_body_span_target(score_runtime: ScoreSetup) -> None:
    recorder, _ = score_runtime
    the_id = llmobs.score(
        name="accuracy",
        value=0.8,
        trace_id="0" * 31 + "1",
        span_id="0" * 15 + "2",
    )
    assert the_id is not None
    llmobs.flush()
    method, path, headers, _ = recorder.requests[0]
    assert method == "POST"
    assert path == "/api/v1/scores"
    lower = {name.lower(): value for name, value in headers.items()}
    assert lower["authorization"] == f"Bearer {TEST_API_KEY}"
    assert lower["content-type"] == "application/json"
    assert score_batches(recorder)[0] == [
        {
            "score_id": the_id,
            "target": {
                "type": "span",
                "trace_id": "0" * 31 + "1",
                "span_id": "0" * 15 + "2",
            },
            "name": "accuracy",
            "value": {"data_type": "numeric", "value": 0.8},
            "source": "sdk",
        }
    ]


def test_body_trace_target(score_runtime: ScoreSetup) -> None:
    recorder, _ = score_runtime
    the_id = llmobs.score(name="quality", value=True, trace_id="f" * 32)
    assert the_id is not None
    llmobs.flush()
    assert score_batches(recorder)[0] == [
        {
            "score_id": the_id,
            "target": {"type": "trace", "trace_id": "f" * 32},
            "name": "quality",
            "value": {"data_type": "boolean", "value": True},
            "source": "sdk",
        }
    ]


def test_body_session_target(score_runtime: ScoreSetup) -> None:
    recorder, _ = score_runtime
    the_id = llmobs.score(name="thumb", value="good", session_id="chat-42")
    assert the_id is not None
    llmobs.flush()
    assert score_batches(recorder)[0] == [
        {
            "score_id": the_id,
            "target": {"type": "session", "session_id": "chat-42"},
            "name": "thumb",
            "value": {"data_type": "categorical", "value": {"label": "good"}},
            "source": "sdk",
        }
    ]


@pytest.mark.parametrize(
    ("value", "expected"),
    [
        (True, {"data_type": "boolean", "value": True}),
        (0.5, {"data_type": "numeric", "value": 0.5}),
        (3, {"data_type": "numeric", "value": 3}),
        ("good", {"data_type": "categorical", "value": {"label": "good"}}),
    ],
)
def test_value_encodings(
    score_runtime: ScoreSetup, value: bool | int | float | str, expected: dict[str, Any]
) -> None:
    recorder, _ = score_runtime
    assert llmobs.score(name="v", value=value, session_id="s") is not None
    llmobs.flush()
    assert score_batches(recorder)[0][0]["value"] == expected


@pytest.mark.parametrize(
    ("value", "data_type", "expected"),
    [
        (7, "numeric", {"data_type": "numeric", "value": 7}),
        (0.5, "numeric", {"data_type": "numeric", "value": 0.5}),
        (True, "boolean", {"data_type": "boolean", "value": True}),
        ("good", "categorical", {"data_type": "categorical", "value": {"label": "good"}}),
        ("a long text", "text", {"data_type": "text", "value": "a long text"}),
    ],
)
def test_data_type_forcing(
    score_runtime: ScoreSetup,
    value: bool | int | float | str,
    data_type: str,
    expected: dict[str, Any],
) -> None:
    recorder, _ = score_runtime
    the_id = llmobs.score(name="v", value=value, session_id="s", data_type=data_type)  # type: ignore[arg-type]
    assert the_id is not None
    llmobs.flush()
    assert score_batches(recorder)[0][0]["value"] == expected


def test_reason_metadata_and_user_id(score_runtime: ScoreSetup) -> None:
    recorder, _ = score_runtime
    the_id = llmobs.score(
        name="annotated",
        value=1,
        session_id="s",
        reason="looked right",
        metadata={"model": "bge", "seats": 3, "raw": {"a": [1, 2]}},
        user_id="u-99",
    )
    assert the_id is not None
    llmobs.flush()
    assert score_batches(recorder)[0][0] == {
        "score_id": the_id,
        "target": {"type": "session", "session_id": "s"},
        "name": "annotated",
        "value": {"data_type": "numeric", "value": 1},
        "source": "sdk",
        "reason": "looked right",
        "user_id": "u-99",
        "metadata": {"model": "bge", "seats": "3", "raw": '{"a":[1,2]}'},
    }


def test_user_id_from_context(score_runtime: ScoreSetup) -> None:
    recorder, _ = score_runtime
    with llmobs.context(user_id="u-17"):
        from_context = llmobs.score(name="a", value=1, session_id="s")
        explicit = llmobs.score(name="b", value=1, session_id="s", user_id="u-1")
    assert from_context is not None
    assert explicit is not None
    llmobs.flush()
    items = score_batches(recorder)[0]
    assert items[0]["user_id"] == "u-17"
    assert items[1]["user_id"] == "u-1"


def test_absent_keys_omitted(score_runtime: ScoreSetup) -> None:
    recorder, _ = score_runtime
    assert llmobs.score(name="bare", value=1, session_id="s") is not None
    llmobs.flush()
    assert score_batches(recorder)[0][0] == {
        "score_id": score_batches(recorder)[0][0]["score_id"],
        "target": {"type": "session", "session_id": "s"},
        "name": "bare",
        "value": {"data_type": "numeric", "value": 1},
        "source": "sdk",
    }


def test_score_id_normalized_lowercase(score_runtime: ScoreSetup) -> None:
    recorder, _ = score_runtime
    given = "0193DCEF-9C1A-7C2A-9C1A-9C1A9C1A9C1A"
    the_id = llmobs.score(name="update", value=1, session_id="s", score_id=given)
    assert the_id == given.lower()
    llmobs.flush()
    assert score_batches(recorder)[0][0]["score_id"] == given.lower()


INVALID_CASES: list[tuple[dict[str, Any], str]] = [
    ({"name": "", "value": 1, "session_id": "s"}, "empty name"),
    ({"name": "x", "value": 1}, "no target"),
    ({"name": "x", "value": 1, "span_id": "0" * 16}, "span_id without trace_id"),
    ({"name": "x", "value": 1, "trace_id": "0" * 31}, "invalid trace_id"),
    ({"name": "x", "value": 1, "trace_id": "F" * 32}, "invalid trace_id"),
    (
        {"name": "x", "value": 1, "trace_id": "0" * 31 + "1", "span_id": "0" * 15},
        "invalid span_id",
    ),
    ({"name": "x", "value": 1, "session_id": ""}, "invalid session_id"),
    ({"name": "x", "value": 1, "session_id": "s", "score_id": "0" * 32}, "invalid score_id"),
    ({"name": "x", "value": 1, "session_id": "s", "score_id": "not a uuid"}, "invalid score_id"),
    ({"name": "x", "value": float("nan"), "session_id": "s"}, "non-finite value"),
    ({"name": "x", "value": float("inf"), "session_id": "s"}, "non-finite value"),
    (
        {"name": "x", "value": "good", "session_id": "s", "data_type": "numeric"},
        "data_type mismatch",
    ),
    ({"name": "x", "value": 1, "session_id": "s", "data_type": "boolean"}, "data_type mismatch"),
    (
        {"name": "x", "value": True, "session_id": "s", "data_type": "categorical"},
        "data_type mismatch",
    ),
    ({"name": "x", "value": 1, "session_id": "s", "data_type": "text"}, "data_type mismatch"),
    ({"name": "x", "value": [1], "session_id": "s"}, "data_type mismatch"),
]


@pytest.mark.parametrize(("options", "reason"), INVALID_CASES)
def test_invalid_input_warns_and_queues_nothing(
    score_runtime: ScoreSetup,
    caplog: pytest.LogCaptureFixture,
    options: dict[str, Any],
    reason: str,
) -> None:
    recorder, _ = score_runtime
    with caplog.at_level(logging.WARNING, logger="llmobs"):
        assert llmobs.score(**options) is None
    llmobs.flush()
    messages = [record.getMessage() for record in caplog.records if record.name == "llmobs"]
    assert messages == [f"llmobs: score not queued: {reason}"]
    assert recorder.requests == []


def test_validation_reason_order(
    score_runtime: ScoreSetup, caplog: pytest.LogCaptureFixture
) -> None:
    """The first failing rule wins: empty name before no target, trace id before span id."""
    with caplog.at_level(logging.WARNING, logger="llmobs"):
        assert llmobs.score(name="", value=1) is None
        assert (
            llmobs.score(
                name="x",
                value=1,
                trace_id="zz" * 16,
                span_id="0" * 15,
                session_id="",
            )
            is None
        )
    messages = [record.getMessage() for record in caplog.records if record.name == "llmobs"]
    assert messages == [
        "llmobs: score not queued: empty name",
        "llmobs: score not queued: invalid trace_id",
    ]


def test_score_without_runtime_returns_none() -> None:
    assert llmobs.score(name="x", value=1, session_id="s") is None


def test_uuid7_layout_uniqueness_and_monotone_ms() -> None:
    ids = [uuid7() for _ in range(1000)]
    assert len(set(ids)) == 1000
    parsed = [uuid.UUID(value) for value in ids]
    assert all(value.version == 7 for value in parsed)
    assert all(value.variant == uuid.RFC_4122 for value in parsed)
    ms = [value.int >> 80 for value in parsed]
    assert all(a <= b for a, b in itertools.pairwise(ms))
    now_ms = time.time_ns() // 1_000_000
    assert abs(ms[-1] - now_ms) < 60_000


def test_mask_applies_to_reason_and_metadata(http_recorder: Recorder) -> None:
    def upper(value: object) -> object:
        return value.upper() if isinstance(value, str) else value

    llmobs.init(api_key=TEST_API_KEY, base_url=http_recorder.url, mask=upper)
    the_id = llmobs.score(
        name="masked",
        value="good",
        session_id="s",
        reason="thumbs up",
        metadata={"kept": "a", "count": 3},
    )
    assert the_id is not None
    llmobs.flush()
    item = score_batches(http_recorder)[0][0]
    assert item["reason"] == "THUMBS UP"
    assert item["metadata"] == {"kept": "A", "count": "3"}


def test_mask_non_string_reason_is_serialized(http_recorder: Recorder) -> None:
    def replace_reason(value: object) -> object:
        return 42 if value == "why" else value

    llmobs.init(api_key=TEST_API_KEY, base_url=http_recorder.url, mask=replace_reason)
    assert llmobs.score(name="m", value=1, session_id="s", reason="why") is not None
    llmobs.flush()
    assert score_batches(http_recorder)[0][0]["reason"] == "42"


def test_batching_2500_scores(score_runtime: ScoreSetup) -> None:
    recorder, _ = score_runtime
    for i in range(2500):
        assert llmobs.score(name="bulk", value=i, session_id="s") is not None
    llmobs.flush()
    sizes = [len(batch) for batch in score_batches(recorder)]
    assert sorted(sizes) == [500, 1000, 1000]


def test_retry_on_503_then_200_resends_the_same_batch(score_runtime: ScoreSetup) -> None:
    recorder, runtime = score_runtime
    waited: list[float] = []
    runtime.scores.wait = record_waits(waited)
    recorder.queue_response(503, {}, b"")
    assert llmobs.score(name="retry", value=0.5, session_id="s") is not None
    llmobs.flush()
    batches = score_batches(recorder)
    assert len(batches) == 2
    assert batches[0] == batches[1]
    assert waited == [1.0]


def test_backoff_sequence_1_2_4(score_runtime: ScoreSetup) -> None:
    recorder, runtime = score_runtime
    waited: list[float] = []
    runtime.scores.wait = record_waits(waited)
    for _ in range(3):
        recorder.queue_response(503, {}, b"")
    assert llmobs.score(name="retry", value=0.5, session_id="s") is not None
    llmobs.flush()
    assert waited == [1.0, 2.0, 4.0]
    assert len(score_batches(recorder)) == 4


def test_retry_after_header_honoured_and_capped(score_runtime: ScoreSetup) -> None:
    recorder, runtime = score_runtime
    waited: list[float] = []
    runtime.scores.wait = record_waits(waited)
    recorder.queue_response(429, {"Retry-After": "1"}, b"")
    assert llmobs.score(name="ra", value=1, session_id="s") is not None
    llmobs.flush()
    assert waited == [1.0]
    recorder.queue_response(429, {"Retry-After": "60"}, b"")
    assert llmobs.score(name="ra", value=1, session_id="s") is not None
    llmobs.flush()
    assert waited == [1.0, 30.0]


def test_http_date_retry_after_uses_the_default(score_runtime: ScoreSetup) -> None:
    recorder, runtime = score_runtime
    waited: list[float] = []
    runtime.scores.wait = record_waits(waited)
    recorder.queue_response(429, {"Retry-After": "Wed, 21 Oct 2015 07:28:00 GMT"}, b"")
    assert llmobs.score(name="ra", value=1, session_id="s") is not None
    llmobs.flush()
    assert waited == [1.0]


def test_400_drops_the_batch_at_once(
    score_runtime: ScoreSetup, caplog: pytest.LogCaptureFixture
) -> None:
    recorder, _ = score_runtime
    recorder.queue_response(400, {}, b"")
    with caplog.at_level(logging.WARNING, logger="llmobs"):
        assert llmobs.score(name="bad", value=1, session_id="s") is not None
        llmobs.flush()
    assert len(score_batches(recorder)) == 1
    messages = [record.getMessage() for record in caplog.records if record.name == "llmobs"]
    assert messages == ["llmobs: score batch rejected with status 400; dropped 1 scores"]


def test_retries_exhausted_drops_the_batch(
    score_runtime: ScoreSetup, caplog: pytest.LogCaptureFixture
) -> None:
    recorder, runtime = score_runtime
    waited: list[float] = []
    runtime.scores.wait = record_waits(waited)
    for _ in range(4):
        recorder.queue_response(503, {}, b"")
    with caplog.at_level(logging.WARNING, logger="llmobs"):
        assert llmobs.score(name="doomed", value=1, session_id="s") is not None
        llmobs.flush()
    assert len(score_batches(recorder)) == 4
    messages = [record.getMessage() for record in caplog.records if record.name == "llmobs"]
    assert messages == ["llmobs: score batch failed after retries; dropped 1 scores"]
    assert waited == [1.0, 2.0, 4.0]


def test_network_failure_retried_like_a_5xx(caplog: pytest.LogCaptureFixture) -> None:
    queue = ScoreQueue(base_url="http://127.0.0.1:1", api_key=TEST_API_KEY)
    waited: list[float] = []
    queue.wait = record_waits(waited)
    assert queue.put({"score_id": uuid7(), "name": "net", "source": "sdk"})
    with caplog.at_level(logging.WARNING, logger="llmobs"):
        assert queue.flush(10.0)
    assert waited == [1.0, 2.0, 4.0]
    messages = [record.getMessage() for record in caplog.records if record.name == "llmobs"]
    assert messages == ["llmobs: score batch failed after retries; dropped 1 scores"]
    queue.shutdown()


def test_buffer_full_warns_once(
    score_runtime: ScoreSetup, caplog: pytest.LogCaptureFixture
) -> None:
    recorder, runtime = score_runtime
    waited: list[float] = []
    blocked = threading.Event()

    def blocking_wait(seconds: float) -> bool:
        waited.append(seconds)
        blocked.wait(10.0)
        return True

    runtime.scores.wait = blocking_wait
    recorder.queue_response(503, {}, b"")
    assert llmobs.score(name="stuck", value=1, session_id="s") is not None
    # The worker holds that batch inside the retry wait, so the buffer fills.
    wait_for_request(recorder, 1)
    with caplog.at_level(logging.WARNING, logger="llmobs"):
        for i in range(_MAX_BUFFER_FILL):
            assert llmobs.score(name="fill", value=i, session_id="s") is not None
        assert llmobs.score(name="overflow", value=0, session_id="s") is None
    messages = [record.getMessage() for record in caplog.records if record.name == "llmobs"]
    assert messages == ["llmobs: score buffer full; dropped 1 scores in the last minute"]
    blocked.set()
    llmobs.flush()


def test_flush_waits_for_delivery(score_runtime: ScoreSetup) -> None:
    recorder, _ = score_runtime
    assert llmobs.score(name="delivered", value=1, session_id="s") is not None
    llmobs.flush()
    names = [item["name"] for batch in score_batches(recorder) for item in batch]
    assert names == ["delivered"]


def test_shutdown_flushes_the_queue(score_runtime: ScoreSetup) -> None:
    recorder, _ = score_runtime
    assert llmobs.score(name="last", value=True, session_id="s") is not None
    llmobs.shutdown()
    names = [item["name"] for batch in score_batches(recorder) for item in batch]
    assert names == ["last"]


def test_score_after_shutdown_returns_none(score_runtime: ScoreSetup) -> None:
    recorder, _ = score_runtime
    llmobs.shutdown()
    assert llmobs.score(name="late", value=1, session_id="s") is None
    assert recorder.requests == []


@pytest.mark.filterwarnings("ignore:This process .* is multi-threaded:DeprecationWarning")
@pytest.mark.skipif(not hasattr(os, "fork"), reason="os.fork is unavailable")
def test_fork_child_scores_and_parent_items_are_not_sent_twice(score_runtime: ScoreSetup) -> None:
    recorder, _ = score_runtime
    parent_id = llmobs.score(name="parent", value=1, session_id="s")
    assert parent_id is not None
    pid = os.fork()
    if pid == 0:
        # The child re-creates the worker (D-F-15), queues only its own item
        # and leaves through os._exit, so no atexit flush runs twice.
        queued = llmobs.score(name="child", value=2, session_id="s") is not None
        llmobs.flush()
        os._exit(0 if queued else 1)
    _, status = os.waitpid(pid, 0)
    assert status == 0
    llmobs.flush()
    ids = [item["score_id"] for batch in score_batches(recorder) for item in batch]
    assert ids.count(parent_id) == 1
    assert len([value for value in ids if value != parent_id]) == 1
