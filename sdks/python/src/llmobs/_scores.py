"""``score()`` and the background score queue (plan F § P9, D-F-1, D-F-2).

Every call mints a UUIDv7 id (or normalizes the given ``score_id``),
validates the request (§ Score request) and queues it on a daemon worker
that posts batches of up to 1000 scores every second to
``<base_url>/api/v1/scores``, retrying 429/5xx/network failures.
"""

import math
import os
import re
import secrets
import threading
import time
import uuid
import weakref
from collections.abc import Callable, Mapping
from typing import Any, Final, Literal

import requests

from llmobs._context import current_values
from llmobs._log import logger
from llmobs._mask import apply
from llmobs._serialize import serialize

DataType = Literal["numeric", "boolean", "categorical", "text"]

ScoreItem = dict[str, Any]

_TRACE_ID: Final = re.compile(r"[0-9a-f]{32}")
_SPAN_ID: Final = re.compile(r"[0-9a-f]{16}")

_NOT_QUEUED_WARNING: Final = "llmobs: score not queued: %s"


def uuid7() -> str:
    """A UUIDv7 string (RFC 9562 §5.7): 48-bit ms, version 7, variant 10, random rest."""
    value = (time.time_ns() // 1_000_000 & 0xFFFF_FFFF_FFFF) << 80
    value |= 0x7 << 76
    value |= secrets.randbits(12) << 64
    value |= 0b10 << 62
    value |= secrets.randbits(62)
    return str(uuid.UUID(int=value))


def score(
    *,
    name: str,
    value: bool | int | float | str,
    trace_id: str | None = None,
    span_id: str | None = None,
    session_id: str | None = None,
    reason: str | None = None,
    data_type: DataType | None = None,
    metadata: Mapping[str, object] | None = None,
    user_id: str | None = None,
    score_id: str | None = None,
) -> str | None:
    """Queue one score (§ Score request); return its id, or ``None`` when not queued.

    Does nothing before ``init()`` or after ``shutdown()``. Invalid input
    logs one warning naming the reason (never the values) and returns
    ``None``; a full buffer also returns ``None``.
    """
    # Deferred: _runtime imports this module.
    from llmobs._runtime import current

    runtime = current()
    if runtime is None:
        return None
    invalid = _invalid_reason(
        name=name,
        value=value,
        trace_id=trace_id,
        span_id=span_id,
        session_id=session_id,
        score_id=score_id,
        data_type=data_type,
    )
    if invalid is not None:
        logger.warning(_NOT_QUEUED_WARNING, invalid)
        return None
    the_id, item = _build_item(
        name=name,
        value=value,
        trace_id=trace_id,
        span_id=span_id,
        session_id=session_id,
        reason=reason,
        data_type=data_type,
        metadata=metadata,
        user_id=user_id,
        score_id=score_id,
    )
    if runtime.scores.put(item):
        return the_id
    return None


def _invalid_reason(
    *,
    name: object,
    value: object,
    trace_id: object,
    span_id: object,
    session_id: object,
    score_id: object,
    data_type: object,
) -> str | None:
    """The § Score request validation reason, or ``None`` when valid.

    The reasons are checked in the order of the shared contract; each is one
    of the exact strings the warning text embeds.
    """
    if not isinstance(name, str) or name == "":
        return "empty name"
    if trace_id is None and span_id is None and session_id is None:
        return "no target"
    if span_id is not None and trace_id is None:
        return "span_id without trace_id"
    if trace_id is not None and (
        not isinstance(trace_id, str) or _TRACE_ID.fullmatch(trace_id) is None
    ):
        return "invalid trace_id"
    if span_id is not None and (
        not isinstance(span_id, str) or _SPAN_ID.fullmatch(span_id) is None
    ):
        return "invalid span_id"
    if session_id is not None and (not isinstance(session_id, str) or session_id == ""):
        return "invalid session_id"
    if score_id is not None and not _dashed_uuid(score_id):
        return "invalid score_id"
    if isinstance(value, float) and not math.isfinite(value):
        return "non-finite value"
    if not _value_matches(value, data_type):
        return "data_type mismatch"
    return None


def _dashed_uuid(value: object) -> bool:
    """A dashed UUID string, case-insensitive; undashed hex and urn/brace forms are not."""
    if not isinstance(value, str):
        return False
    try:
        parsed = uuid.UUID(value)
    except (ValueError, TypeError):
        return False
    return str(parsed) == value.lower()


def _value_matches(value: object, data_type: object) -> bool:
    """Whether ``value`` has one of the four encodings, optionally forced by ``data_type``."""
    if data_type is None:
        return isinstance(value, (bool, int, float, str))
    if data_type == "boolean":
        return isinstance(value, bool)
    if data_type == "numeric":
        return isinstance(value, (int, float)) and not isinstance(value, bool)
    if data_type in ("categorical", "text"):
        return isinstance(value, str)
    return False


def _build_item(
    *,
    name: str,
    value: bool | int | float | str,
    trace_id: str | None,
    span_id: str | None,
    session_id: str | None,
    reason: str | None,
    data_type: DataType | None,
    metadata: Mapping[str, object] | None,
    user_id: str | None,
    score_id: str | None,
) -> tuple[str, ScoreItem]:
    """The JSON body item of one score; inputs are already validated."""
    the_id = (
        str(uuid.UUID(score_id)) if score_id is not None else uuid7()
    )  # a valid score_id re-serializes lowercase
    target: dict[str, str]
    if span_id is not None and trace_id is not None:
        target = {"type": "span", "trace_id": trace_id, "span_id": span_id}
    elif trace_id is not None:
        target = {"type": "trace", "trace_id": trace_id}
    else:
        # Validation already rejected every-target-absent, so a session
        # target here implies session_id is set.
        target = {"type": "session", "session_id": session_id if session_id is not None else ""}
    item: ScoreItem = {
        "score_id": the_id,
        "target": target,
        "name": name,
        "value": _value_body(value, data_type),
        "source": "sdk",
    }
    if reason is not None:
        masked = apply(reason)
        item["reason"] = masked if isinstance(masked, str) else serialize(masked)[0]
    resolved_user = user_id if user_id is not None else current_values().user_id
    if resolved_user is not None:
        item["user_id"] = resolved_user
    if metadata is not None:
        encoded: dict[str, str] = {}
        for key, raw in metadata.items():
            masked = apply(raw)
            encoded[key] = masked if isinstance(masked, str) else serialize(masked)[0]
        item["metadata"] = encoded
    return the_id, item


def _value_body(value: bool | int | float | str, data_type: DataType | None) -> dict[str, Any]:
    """The ``ScoreValue`` JSON of one of the four encodings (spec 01 § Score)."""
    if data_type == "text":
        return {"data_type": "text", "value": value}
    if data_type == "boolean" or (data_type is None and isinstance(value, bool)):
        return {"data_type": "boolean", "value": value}
    if data_type == "categorical" or (data_type is None and isinstance(value, str)):
        return {"data_type": "categorical", "value": {"label": value}}
    return {"data_type": "numeric", "value": value}


_MAX_BUFFER: Final = 10_000
_BATCH_LIMIT: Final = 1000
_INTERVAL_S: Final = 1.0
_REQUEST_TIMEOUT_S: Final = 10
_MAX_RETRIES: Final = 3
_RETRY_BACKOFF_S: Final = (1.0, 2.0, 4.0)
_RETRY_AFTER_CAP_S: Final = 30.0
_SHUTDOWN_BUDGET_S: Final = 30.0
_WARN_EVERY_S: Final = 60.0

_BUFFER_FULL_WARNING: Final = "llmobs: score buffer full; dropped %d scores in the last minute"
_REJECTED_WARNING: Final = "llmobs: score batch rejected with status %d; dropped %d scores"
_FAILED_WARNING: Final = "llmobs: score batch failed after retries; dropped %d scores"


class ScoreQueue:
    """The background score sender (§ Score request, spec 10 § Lifecycle).

    ``put`` appends to a buffer of at most ``_MAX_BUFFER`` items (beyond
    which items are dropped and counted) and lazily starts the daemon
    worker thread ``llmobs-scores``. The worker posts batches of up to
    ``_BATCH_LIMIT`` items, retrying 429/5xx/network failures.
    """

    def __init__(self, *, base_url: str, api_key: str) -> None:
        self._url = f"{base_url}/api/v1/scores"
        self._api_key = api_key
        self._condition = threading.Condition()
        self._items: list[ScoreItem] = []
        self._stop = threading.Event()
        # The retry sleep is an Event.wait, so shutdown interrupts it; tests
        # replace this attribute to record durations instead of sleeping.
        self.wait: Callable[[float], bool] = self._stop.wait
        self._thread: threading.Thread | None = None
        self._session: requests.Session | None = None
        self._shutdown = False
        self._flush_requested = False
        self._collect_deadline: float | None = None
        self._in_flight = 0
        self._dropped_since_warning = 0
        self._last_warning: float | None = None
        if hasattr(os, "register_at_fork"):
            self._register_at_fork()

    def put(self, item: ScoreItem) -> bool:
        """Queue one score; ``False`` when the buffer is full or the queue stopped."""
        with self._condition:
            if self._shutdown:
                return False
            if len(self._items) >= _MAX_BUFFER:
                self._note_drop()
                return False
            self._items.append(item)
            self._condition.notify_all()
            start_worker = self._thread is None
        if start_worker:
            self._start_worker()
        return True

    def _start_worker(self) -> None:
        """Start the daemon worker once; the re-check under the lock is race-free."""
        with self._condition:
            if self._thread is not None or self._shutdown:
                return
            self._thread = threading.Thread(target=self._run, name="llmobs-scores", daemon=True)
            self._thread.start()

    def _note_drop(self) -> None:
        """Count one dropped score; warn at most once per minute. The lock is held."""
        now = time.monotonic()
        self._dropped_since_warning += 1
        if self._last_warning is None or now - self._last_warning >= _WARN_EVERY_S:
            logger.warning(_BUFFER_FULL_WARNING, self._dropped_since_warning)
            self._dropped_since_warning = 0
            self._last_warning = now

    def _run(self) -> None:
        """The worker loop: take a batch, send it, repeat until shut down and empty."""
        while True:
            batch = self._next_batch()
            if batch is None:
                return
            try:
                self._send(batch)
            except Exception as error:  # noqa: BLE001  # a dead worker would strand the buffer
                logger.debug("llmobs: score batch send failed (%s)", type(error).__name__)
            finally:
                with self._condition:
                    self._in_flight -= 1
                    self._condition.notify_all()

    def _next_batch(self) -> list[ScoreItem] | None:
        """The next batch to send, or ``None`` when shut down and empty.

        A batch window opens with the first queued item and closes after
        ``_INTERVAL_S`` or once ``_BATCH_LIMIT`` items are waiting, so
        trickle traffic batches per second and bursts go out full; a flush
        or shutdown request closes the window early.
        """
        with self._condition:
            while True:
                if self._items:
                    if self._collect_deadline is None:
                        self._collect_deadline = time.monotonic() + _INTERVAL_S
                    if (
                        len(self._items) >= _BATCH_LIMIT
                        or self._flush_requested
                        or self._shutdown
                        or time.monotonic() >= self._collect_deadline
                    ):
                        self._collect_deadline = None
                        self._flush_requested = False
                        batch = self._items[:_BATCH_LIMIT]
                        del self._items[:_BATCH_LIMIT]
                        self._in_flight += 1
                        return batch
                    remaining = max(0.0, self._collect_deadline - time.monotonic())
                    self._condition.wait(remaining)
                else:
                    self._collect_deadline = None
                    self._flush_requested = False
                    if self._shutdown:
                        return None
                    self._condition.wait(_INTERVAL_S)

    def _send(self, batch: list[ScoreItem]) -> None:
        """Post one batch with the § Score request retry policy."""
        attempt = 0
        while True:
            response = self._post(batch)
            if response is not None and 200 <= response.status_code < 300:
                return
            if response is None or response.status_code == 429 or response.status_code >= 500:
                if attempt >= _MAX_RETRIES:
                    logger.warning(_FAILED_WARNING, len(batch))
                    return
                self.wait(self._retry_delay(response, attempt))
                attempt += 1
                continue
            logger.warning(_REJECTED_WARNING, response.status_code, len(batch))
            return

    def _retry_delay(self, response: requests.Response | None, attempt: int) -> float:
        """``Retry-After`` seconds when a non-negative integer (capped), else 1/2/4 s."""
        if response is not None:
            header = response.headers.get("Retry-After")
            if header is not None:
                try:
                    seconds = int(header)
                except ValueError:
                    seconds = None  # an HTTP-date or any other value uses the default
                if seconds is not None and seconds >= 0:
                    return min(float(seconds), _RETRY_AFTER_CAP_S)
        return _RETRY_BACKOFF_S[attempt]

    def _post(self, batch: list[ScoreItem]) -> requests.Response | None:
        """One POST; ``None`` on a network error or timeout (retried like a 5xx)."""
        if self._session is None:
            self._session = requests.Session()
        try:
            return self._session.post(
                self._url,
                json={"scores": batch},
                headers={"Authorization": f"Bearer {self._api_key}"},
                timeout=_REQUEST_TIMEOUT_S,
            )
        except requests.RequestException:
            return None

    def flush(self, timeout_s: float) -> bool:
        """Close the batch window and wait until the queue is empty and no batch is
        in flight, or the timeout expires (``False``)."""
        deadline = time.monotonic() + timeout_s
        with self._condition:
            self._flush_requested = True
            self._condition.notify_all()
            while self._items or self._in_flight:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    return False
                # Re-assert: taking a batch consumed the earlier request while
                # items were still waiting behind it.
                self._flush_requested = True
                self._condition.wait(remaining)
        return True

    def shutdown(self) -> None:
        """Flush with the remaining part of a 30 s budget, then stop the worker."""
        self.flush(_SHUTDOWN_BUDGET_S)
        with self._condition:
            self._shutdown = True
            self._condition.notify_all()
        self._stop.set()  # interrupts any retry wait

    def _register_at_fork(self) -> None:
        """Re-create this queue's state in a forked child (D-F-15).

        The callback holds only a weakref, so a collected queue is a no-op.
        """
        ref = weakref.ref(self)

        def after_in_child() -> None:
            child = ref()
            if child is not None:
                child._reinit()

        os.register_at_fork(after_in_child=after_in_child)

    def _reinit(self) -> None:
        """Forked-child state: fresh lock and event, empty buffer, no worker (D-F-15).

        The parent's pending items are dropped, its lock (possibly held by a
        thread that does not exist here) replaced, and a new Session is made
        on the next send.
        """
        self._condition = threading.Condition()
        self._items = []
        self._stop = threading.Event()
        self.wait = self._stop.wait
        self._thread = None
        self._session = None
        self._shutdown = False
        self._flush_requested = False
        self._collect_deadline = None
        self._in_flight = 0
