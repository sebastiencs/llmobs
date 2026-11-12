"""Traced generator and async generator wrappers (plan F § P6, spec 10).

``observe`` detects generator functions at decoration time
(``inspect.isgeneratorfunction`` / ``inspect.isasyncgenfunction``) and its
wrapper returns a ``TracedGenerator`` / ``TracedAsyncGenerator`` around the
inner generator. The span starts at the first ``next()`` / ``__anext__()``
— the context current then is the parent — and is the current span only
while the body runs: the wrapper's context is attached before every step
and detached in a ``finally``.

Exhaustion ends the span with the status unset and the output of § P6
(joined text when every item is a ``str``, else a JSON list,
``reduce_output``'s result when given); closing early ends it with status
``ok`` and the items so far; an exception records the § Errors event and
re-raises. ``inspect.isasyncgenfunction`` (and ``isgeneratorfunction``) is
false on a wrapped generator function: the wrapper is a plain function
returning the traced generator.

An abandoned async generator — never exhausted, never ``aclose()``d —
leaves its span unended, so it is never exported; it has no ``__del__``
because an async generator cannot be closed from a finalizer.
"""

import functools
import inspect
from collections.abc import AsyncGenerator, Callable, Coroutine, Generator
from contextvars import Token
from typing import Any, ParamSpec, TypeVar, cast

from opentelemetry import trace as trace_api
from opentelemetry.context import Context, attach
from opentelemetry.trace import Status, StatusCode

from llmobs._log import logger
from llmobs._runtime import Runtime, current
from llmobs._spans import (
    SpanKind,
    _bound_input,
    _detach,
    _record_error,
    _record_payload,
    _serialize_payload,
)

P = ParamSpec("P")
R = TypeVar("R")
Y = TypeVar("Y")
S = TypeVar("S")


def _end(otel_span: trace_api.Span) -> None:
    """End the span; never raises (P5's ``_end_span`` without the token)."""
    try:
        otel_span.end()
    except Exception as error:  # noqa: BLE001  # never raise into the app (spec 10 rule 4)
        logger.debug("llmobs: could not end the span (%s)", type(error).__name__)


class _TracedSteps:
    """State and finish logic shared by the sync and async wrappers (plan F § P6)."""

    def __init__(
        self,
        runtime: Runtime,
        name: str,
        kind: SpanKind,
        serialized_input: tuple[str, str] | None,
        capture_output: bool,
        reduce_output: Callable[[list[object]], object] | None,
    ) -> None:
        """``serialized_input`` is the D-F-11 input, serialized at call time."""
        self._runtime = runtime
        self._name = name
        self._kind = kind
        self._input = serialized_input
        self._capture_output = capture_output
        self._reduce_output = reduce_output
        self._items: list[object] = []
        self._span: trace_api.Span | None = None
        self._ctx: Context | None = None
        # _started stays true after a failed _open, so the span start is not
        # retried on every step; _span None then means "run untraced".
        self._started = False
        self._done = False

    @property
    def _keep_items(self) -> bool:
        """Whether yielded items are kept: capture_content and capture_output both on."""
        return self._capture_output and self._runtime.config.capture_content

    def _open(self) -> None:
        """Start the span on the first step; the context current then is the parent."""
        try:
            otel_span = self._runtime.tracer.start_span(
                self._name, record_exception=False, set_status_on_exception=False
            )
            otel_span.set_attribute("llmobs.span.kind", self._kind)
            if self._input is not None:
                otel_span.set_attribute("llmobs.input", self._input[0])
                otel_span.set_attribute("llmobs.input.format", self._input[1])
            self._ctx = trace_api.set_span_in_context(otel_span)
            self._span = otel_span
        except Exception as error:  # noqa: BLE001  # never raise into the app (spec 10 rule 4)
            logger.debug("llmobs: could not start the span (%s)", type(error).__name__)
            self._span = None
            self._ctx = None

    def _finish(self, error: Exception | None = None, *, closed: bool = False) -> None:
        """End the span: the § Errors event on ``error``, status ok when ``closed``,
        the output from the items otherwise; never raises.
        """
        otel_span, self._span = self._span, None
        self._ctx = None
        self._done = True
        if otel_span is None:
            return
        try:
            if error is not None:
                _record_error(otel_span, error)
            else:
                if closed:
                    otel_span.set_status(Status(StatusCode.OK))
                self._record_output(otel_span)
            otel_span.end()
        except Exception as failure:  # noqa: BLE001  # never raise into the app (spec 10 rule 4)
            logger.debug("llmobs: could not end the span (%s)", type(failure).__name__)

    def _record_output(self, otel_span: trace_api.Span) -> None:
        """Record the output from the items (§ P6); never raises."""
        if not self._keep_items or not self._items:
            return
        if self._reduce_output is not None:
            try:
                reduced: object = self._reduce_output(list(self._items))
            except Exception as error:  # noqa: BLE001  # never raise into the app (spec 10 rule 4)
                logger.debug(
                    "llmobs: reduce_output raised %s; recording the items", type(error).__name__
                )
            else:
                _record_payload(otel_span, "output", reduced)
                return
        if all(isinstance(item, str) for item in self._items):
            _record_payload(otel_span, "output", "".join(cast("list[str]", self._items)))
        else:
            _record_payload(otel_span, "output", self._items)


class TracedGenerator(_TracedSteps, Generator[Y, S, R]):
    """A generator wrapper: one generator is one span (spec 10, plan F § P6).

    The span starts at the first ``send`` / ``throw`` and is current only
    while the body runs. ``__del__`` closes a started, unfinished generator
    so its span is exported with status ok (spec 10 § Abandoned generators).
    """

    def __init__(
        self,
        inner: Generator[Y, S, R],
        runtime: Runtime,
        name: str,
        kind: SpanKind,
        serialized_input: tuple[str, str] | None,
        capture_output: bool,
        reduce_output: Callable[[list[object]], object] | None,
    ) -> None:
        """Wrap ``inner``; the input pair was serialized at call time (D-F-11)."""
        super().__init__(runtime, name, kind, serialized_input, capture_output, reduce_output)
        self._inner = inner

    def _step(self, advance: Callable[[], Y]) -> Y:
        """Run one step of the body with our span current (§ P6); never swallows."""
        if not self._started:
            self._started = True
            self._open()
        token: Token[Context] | None = None if self._ctx is None else attach(self._ctx)
        try:
            try:
                item = advance()
            except StopIteration:
                self._finish()
                raise
            except Exception as error:
                self._finish(error)
                raise
            if self._keep_items:
                self._items.append(item)
            return item
        finally:
            _detach(token)

    def send(self, value: S | None) -> Y:
        """Advance the generator one step; the first send starts the span."""
        send = cast("Callable[..., Y]", self._inner.send)
        return self._step(lambda: send(value))

    def throw(self, *args: object) -> Y:
        """Raise in the generator, like ``generator.throw``; a first throw starts the span."""
        throw = cast("Callable[..., Y]", self._inner.throw)
        return self._step(lambda: throw(*args))

    def close(self) -> None:
        """Close the inner generator; a started span ends with status ok and the
        items yielded so far (§ Errors: a ``GeneratorExit`` is not an error).
        """
        if self._done or not self._started:
            self._inner.close()
            return
        token: Token[Context] | None = None if self._ctx is None else attach(self._ctx)
        try:
            self._inner.close()
        finally:
            self._finish(closed=True)
            _detach(token)

    def __del__(self) -> None:
        """Close a started, unfinished generator so its span is exported; never raises."""
        try:
            if self._started and not self._done:
                self.close()
        except Exception:  # noqa: BLE001, S110  # never raise from a finalizer; logging may be gone at shutdown
            pass


class TracedAsyncGenerator(_TracedSteps, AsyncGenerator[Y, S]):
    """An async generator wrapper: one async generator is one span (spec 10, plan F § P6).

    Like the sync wrapper the span starts at the first ``asend`` / ``athrow``
    and is current only while the body runs. There is no ``__del__``: an
    async generator cannot be closed from a finalizer, so one abandoned
    half-way — never exhausted, never ``aclose()``d — leaves its span
    unended and it is never exported (spec 10 § Abandoned generators).
    """

    def __init__(
        self,
        inner: AsyncGenerator[Y, S],
        runtime: Runtime,
        name: str,
        kind: SpanKind,
        serialized_input: tuple[str, str] | None,
        capture_output: bool,
        reduce_output: Callable[[list[object]], object] | None,
    ) -> None:
        """Wrap ``inner``; the input pair was serialized at call time (D-F-11)."""
        super().__init__(runtime, name, kind, serialized_input, capture_output, reduce_output)
        self._inner = inner

    async def _astep(self, advance: Callable[[], Coroutine[Any, Any, Y]]) -> Y:
        """Run one step of the body with our span current (§ P6); never swallows."""
        if not self._started:
            self._started = True
            self._open()
        token: Token[Context] | None = None if self._ctx is None else attach(self._ctx)
        try:
            try:
                item = await advance()
            except StopAsyncIteration:
                self._finish()
                raise
            except Exception as error:
                self._finish(error)
                raise
            if self._keep_items:
                self._items.append(item)
            return item
        finally:
            _detach(token)

    def __aiter__(self) -> "TracedAsyncGenerator[Y, S]":
        """Return self, like an async generator."""
        return self

    async def __anext__(self) -> Y:
        """``asend(None)``, like an async generator."""
        return await self.asend(None)

    async def asend(self, value: S | None) -> Y:
        """Advance the async generator one step; the first asend starts the span."""
        asend = cast("Callable[..., Coroutine[Any, Any, Y]]", self._inner.asend)
        return await self._astep(lambda: asend(value))

    async def athrow(self, *args: object) -> Y:
        """Raise in the async generator, like ``athrow``; a first athrow starts the span."""
        athrow = cast("Callable[..., Coroutine[Any, Any, Y]]", self._inner.athrow)
        return await self._astep(lambda: athrow(*args))

    async def aclose(self) -> None:
        """Close the inner async generator; a started span ends with status ok
        and the items yielded so far (§ Errors: a ``GeneratorExit`` is not an error).
        """
        if self._done or not self._started:
            await self._inner.aclose()
            return
        token: Token[Context] | None = None if self._ctx is None else attach(self._ctx)
        try:
            await self._inner.aclose()
        finally:
            self._finish(closed=True)
            _detach(token)


def _serialized_input(
    runtime: Runtime,
    capture_input: bool,
    signature: inspect.Signature | None,
    args: tuple[object, ...],
    kwargs: dict[str, object],
) -> tuple[str, str] | None:
    """The D-F-11 bound input serialized at call time; ``None`` when nothing is captured."""
    if not capture_input or not runtime.config.capture_content:
        return None
    return _serialize_payload(_bound_input(signature, args, kwargs))


def _wrap_generator(
    func: Callable[P, R],
    signature: inspect.Signature | None,
    *,
    span_name: str,
    kind: SpanKind,
    capture_input: bool,
    capture_output: bool,
    reduce_output: Callable[[list[object]], object] | None,
) -> Callable[P, R]:
    """The generator wrapper: the span starts at the generator's first step.

    The wrapper is a plain function — ``inspect.isgeneratorfunction`` is
    false on it — that binds the input at call time (D-F-11) and returns a
    ``TracedGenerator``; without a running runtime the inner generator is
    returned unchanged.
    """

    @functools.wraps(func)
    def wrapper(*args: P.args, **kwargs: P.kwargs) -> R:
        inner = func(*args, **kwargs)
        runtime = current()
        if runtime is None:
            return inner
        return cast(
            "R",
            TracedGenerator(
                cast("Generator[Any, Any, Any]", inner),
                runtime,
                span_name,
                kind,
                _serialized_input(runtime, capture_input, signature, args, kwargs),
                capture_output,
                reduce_output,
            ),
        )

    return wrapper


def _wrap_async_generator(
    func: Callable[P, R],
    signature: inspect.Signature | None,
    *,
    span_name: str,
    kind: SpanKind,
    capture_input: bool,
    capture_output: bool,
    reduce_output: Callable[[list[object]], object] | None,
) -> Callable[P, R]:
    """The async generator wrapper: the span starts at the first ``__anext__()``.

    The wrapper is a plain function, so ``inspect.isasyncgenfunction`` on it
    is false; calling it returns a ``TracedAsyncGenerator`` around the inner
    async generator. Without a running runtime the inner async generator is
    returned unchanged.
    """

    @functools.wraps(func)
    def wrapper(*args: P.args, **kwargs: P.kwargs) -> R:
        inner = func(*args, **kwargs)
        runtime = current()
        if runtime is None:
            return inner
        return cast(
            "R",
            TracedAsyncGenerator(
                cast("AsyncGenerator[Any, Any]", inner),
                runtime,
                span_name,
                kind,
                _serialized_input(runtime, capture_input, signature, args, kwargs),
                capture_output,
                reduce_output,
            ),
        )

    return wrapper
