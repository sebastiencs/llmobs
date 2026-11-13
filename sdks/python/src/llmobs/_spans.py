"""Manual spans and ``observe`` (plan F § P5, spec 10).

``span()`` is a sync and async context manager, ``observe`` wraps functions
and coroutine functions, and generator functions (detected at decoration
time) through the wrappers of ``_generators`` (P6). Payloads are masked
then serialized (§ Serialization, D-F-6, D-F-10), inputs follow D-F-11, and
errors are recorded as one ``exception`` event with the status set to error
(§ Errors). Nothing raises into the app after ``init()`` except an invalid
``kind``, a programming error (§ "Programming errors raise").
"""

import functools
import inspect
import traceback
from collections.abc import Awaitable, Callable, Coroutine, Mapping
from contextvars import Token
from types import TracebackType
from typing import Any, Final, Literal, ParamSpec, TypeVar, cast, overload

from opentelemetry import trace as trace_api
from opentelemetry.context import Context, attach, detach
from opentelemetry.trace import Status, StatusCode

from llmobs._log import logger
from llmobs._mask import apply
from llmobs._processor import _metadata_attribute
from llmobs._runtime import Runtime, current
from llmobs._serialize import attribute_value, serialize

SpanKind = Literal[
    "llm",
    "embedding",
    "tool",
    "agent",
    "chain",
    "retriever",
    "reranker",
    "guardrail",
    "evaluator",
    "span",
]

_KINDS: Final = (
    "llm",
    "embedding",
    "tool",
    "agent",
    "chain",
    "retriever",
    "reranker",
    "guardrail",
    "evaluator",
    "span",
)
_KIND_ERROR: Final = "kind: must be one of " + ", ".join(_KINDS)
_UNSET: Final = object()

P = ParamSpec("P")
R = TypeVar("R")


def _check_kind(kind: SpanKind) -> None:
    """Raise ``ValueError`` on a kind outside ``SpanKind`` (§ Programming errors raise)."""
    if kind not in _KINDS:
        raise ValueError(_KIND_ERROR)


def _serialize_payload(value: object) -> tuple[str, str] | None:
    """Mask and serialize one payload value; ``None`` on failure (never raises)."""
    try:
        pair = serialize(apply(value))
    except Exception as error:  # noqa: BLE001  # never raise into the app (spec 10 rule 4)
        logger.debug("llmobs: could not serialize the payload (%s)", type(error).__name__)
        return None
    return pair[0], pair[1]


def _record_payload(otel_span: trace_api.Span, which: str, value: object) -> None:
    """Mask, serialize and set ``llmobs.<which>`` with its format; never raises."""
    serialized = _serialize_payload(value)
    if serialized is None:
        return
    try:
        otel_span.set_attribute(f"llmobs.{which}", serialized[0])
        otel_span.set_attribute(f"llmobs.{which}.format", serialized[1])
    except Exception as error:  # noqa: BLE001  # never raise into the app (spec 10 rule 4)
        logger.debug("llmobs: could not record the %s (%s)", which, type(error).__name__)


def _record_error(otel_span: trace_api.Span, error: Exception) -> None:
    """Add the ``exception`` event and set the error status (§ Errors); never raises."""
    try:
        exception_type = type(error).__name__
        message = str(error)
        otel_span.add_event(
            "exception",
            {
                "exception.type": exception_type,
                "exception.message": message,
                "exception.stacktrace": "".join(traceback.format_exception(error)),
            },
        )
        otel_span.set_status(Status(StatusCode.ERROR, f"{exception_type}: {message}"))
    except Exception as failure:  # noqa: BLE001  # never raise into the app (spec 10 rule 4)
        logger.debug("llmobs: could not record the exception (%s)", type(failure).__name__)


def _detach(token: Token[Context] | None) -> None:
    """Restore the context attached before ``token``; never raises."""
    if token is None:
        return
    try:
        detach(token)
    except Exception as error:  # noqa: BLE001  # never raise into the app (spec 10 rule 4)
        logger.debug("llmobs: context detach failed (%s)", type(error).__name__)


def _end_span(otel_span: trace_api.Span, token: Token[Context] | None) -> None:
    """End the span, then restore the previous context; never raises."""
    try:
        otel_span.end()
    except Exception as error:  # noqa: BLE001  # never raise into the app (spec 10 rule 4)
        logger.debug("llmobs: could not end the span (%s)", type(error).__name__)
    _detach(token)


def _start_span(
    runtime: Runtime, name: str, kind: SpanKind, input_value: object
) -> tuple[trace_api.Span, Token[Context] | None]:
    """Start one of our spans and attach it as the current one; never raises.

    The span parents on the current context (§ "Starting a span"). On failure
    the caller gets the invalid no-op span and no token, so it runs without
    tracing instead of the error reaching the app.
    """
    token: Token[Context] | None = None
    try:
        otel_span = runtime.tracer.start_span(
            name, record_exception=False, set_status_on_exception=False
        )
        token = attach(trace_api.set_span_in_context(otel_span))
        otel_span.set_attribute("llmobs.span.kind", kind)
        if input_value is not _UNSET and runtime.config.capture_content:
            _record_payload(otel_span, "input", input_value)
        return otel_span, token
    except Exception as error:  # noqa: BLE001  # never raise into the app (spec 10 rule 4)
        logger.debug("llmobs: could not start the span (%s)", type(error).__name__)
        _detach(token)
        return trace_api.INVALID_SPAN, None


class Span:
    """Handle on one of our spans. Every method is a no-op after the span
    ended and never raises.
    """

    def __init__(self, otel_span: trace_api.Span | None, *, capture: bool) -> None:
        """``otel_span`` is ``None`` for a no-op handle (no running runtime)."""
        self._otel_span = otel_span
        self._capture = capture
        self._ended = False

    @property
    def trace_id(self) -> str | None:
        """The trace id, 32 lowercase hex characters; ``None`` for a no-op handle."""
        context = self._span_context()
        if context is None:
            return None
        return format(context.trace_id, "032x")

    @property
    def span_id(self) -> str | None:
        """The span id, 16 lowercase hex characters; ``None`` for a no-op handle."""
        context = self._span_context()
        if context is None:
            return None
        return format(context.span_id, "016x")

    def _span_context(self) -> trace_api.SpanContext | None:
        if self._otel_span is None:
            return None
        context = self._otel_span.get_span_context()
        return context if context.is_valid else None

    def _close(self) -> None:
        self._ended = True

    def set_input(self, value: object) -> None:
        """Record ``value`` as ``llmobs.input``; a no-op after the end or without capture."""
        if self._otel_span is None or self._ended or not self._capture:
            return
        _record_payload(self._otel_span, "input", value)

    def set_output(self, value: object) -> None:
        """Record ``value`` as ``llmobs.output``; a no-op after the end or without capture."""
        if self._otel_span is None or self._ended or not self._capture:
            return
        _record_payload(self._otel_span, "output", value)

    def set_metadata(self, **values: object) -> None:
        """Attach metadata attributes, masked per value; kept without payload capture too.

        Keys follow the processor's key rule: an empty key or one with a
        character outside ``[A-Za-z0-9_.-]`` is skipped with one debug line.
        """
        self._write_metadata(values)

    def _write_metadata(self, metadata: Mapping[str, object]) -> None:
        otel_span = self._otel_span
        if otel_span is None or self._ended:
            return
        for key, value in metadata.items():
            if not isinstance(key, str):
                continue
            name = _metadata_attribute(key)
            if name is not None:
                # apply and attribute_value never raise (spec 10 rule 4), and an
                # OTel span takes a scalar attribute without raising.
                otel_span.set_attribute(name, attribute_value(apply(value)))


class span:  # noqa: N801  # public API name from spec 10
    """Manual span, as a sync and async context manager (spec 10 § Manual spans).

    The body's return value is never recorded (§ Context managers); the
    ``Span`` handle's ``set_output`` writes the output. An invalid ``kind``
    raises ``ValueError`` at construction; without a running runtime the
    block yields a no-op handle.
    """

    def __init__(
        self,
        name: str,
        *,
        kind: SpanKind = "chain",
        input: object = _UNSET,
        metadata: Mapping[str, object] | None = None,
    ) -> None:
        """Validate ``kind`` now: a bad one is a programming error."""
        _check_kind(kind)
        self._name = name
        self._kind = kind
        self._input = input
        self._metadata = metadata
        self._otel_span: trace_api.Span | None = None
        self._token: Token[Context] | None = None
        self._handle: Span | None = None

    def _enter(self) -> Span:
        runtime = current()
        if runtime is None:
            self._handle = Span(None, capture=False)
            return self._handle
        otel_span, self._token = _start_span(runtime, self._name, self._kind, self._input)
        self._otel_span = otel_span
        handle = Span(otel_span, capture=runtime.config.capture_content)
        if self._metadata is not None:
            handle._write_metadata(self._metadata)
        self._handle = handle
        return handle

    def _exit(self, exc: BaseException | None) -> None:
        """End the span; ``exc`` is recorded per § Errors and never swallowed."""
        otel_span, self._otel_span = self._otel_span, None
        token, self._token = self._token, None
        handle, self._handle = self._handle, None
        if handle is not None:
            handle._close()
        if otel_span is None:
            return
        # Only Exception is an error: a GeneratorExit is explicitly not one
        # (§ Errors), and other BaseExceptions pass through untouched too.
        if isinstance(exc, Exception):
            _record_error(otel_span, exc)
        _end_span(otel_span, token)

    def __enter__(self) -> Span:
        try:
            return self._enter()
        except Exception as error:  # noqa: BLE001  # never raise into the app (spec 10 rule 4)
            logger.debug("llmobs: span entry failed (%s)", type(error).__name__)
            self._otel_span, self._token, self._handle = None, None, None
            return Span(None, capture=False)

    def __exit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        tb: TracebackType | None,
    ) -> None:
        """Return ``None`` so an exception from the body is never swallowed."""
        self._exit(exc)

    async def __aenter__(self) -> Span:
        """Async variant of ``__enter__``."""
        return self.__enter__()

    async def __aexit__(
        self,
        exc_type: type[BaseException] | None,
        exc: BaseException | None,
        tb: TracebackType | None,
    ) -> None:
        """Async variant of ``__exit__``."""
        self._exit(exc)


def _bound_input(
    signature: inspect.Signature | None, args: tuple[object, ...], kwargs: dict[str, object]
) -> dict[str, object]:
    """The D-F-11 bound-arguments input.

    Defaults are applied, a first parameter named ``self`` or ``cls`` is
    dropped, ``*args`` becomes a list and ``**kwargs`` a dict under their
    parameter names, in parameter order. A ``bind`` failure — and a callable
    whose signature could not be computed at decoration — falls back to
    ``{"args": list(args), "kwargs": kwargs}`` while the call itself raises
    the real error.
    """
    if signature is None:
        return {"args": list(args), "kwargs": kwargs}
    try:
        bound = signature.bind(*args, **kwargs)
    except TypeError:
        return {"args": list(args), "kwargs": kwargs}
    bound.apply_defaults()
    parameters = signature.parameters
    drop_first = bool(parameters) and next(iter(parameters)) in ("self", "cls")
    values: dict[str, object] = {}
    for index, (parameter_name, parameter) in enumerate(parameters.items()):
        if index == 0 and drop_first:
            continue
        # apply_defaults leaves every parameter bound: a missing one without a
        # default already failed bind above.
        argument = bound.arguments[parameter_name]
        if parameter.kind is inspect.Parameter.VAR_POSITIONAL:
            argument = list(argument)
        elif parameter.kind is inspect.Parameter.VAR_KEYWORD:
            argument = dict(argument)
        values[parameter_name] = argument
    return values


def _recordable(result: object) -> bool:
    """Whether a call's return value is its recorded output (D-F-11).

    A generator or async generator object returned by a plain function is
    passed through untouched, never consumed and never recorded.
    """
    return not (inspect.isgenerator(result) or inspect.isasyncgen(result))


def _wrap_function(
    func: Callable[P, R],
    signature: inspect.Signature | None,
    *,
    span_name: str,
    kind: SpanKind,
    capture_input: bool,
    capture_output: bool,
) -> Callable[P, R]:
    """The sync wrapper: one call is one span, started at call time."""

    @functools.wraps(func)
    def wrapper(*args: P.args, **kwargs: P.kwargs) -> R:
        runtime = current()
        if runtime is None:
            return func(*args, **kwargs)
        input_value = _UNSET if not capture_input else _bound_input(signature, args, kwargs)
        otel_span, token = _start_span(runtime, span_name, kind, input_value)
        try:
            result = func(*args, **kwargs)
        except Exception as error:
            _record_error(otel_span, error)
            _end_span(otel_span, token)
            raise
        except BaseException:
            # KeyboardInterrupt, asyncio.CancelledError, …: the span ends with
            # the status unset (like span()/llm_span) and the raise goes through.
            _end_span(otel_span, token)
            raise
        if capture_output and runtime.config.capture_content and _recordable(result):
            _record_payload(otel_span, "output", result)
        _end_span(otel_span, token)
        return result

    return wrapper


def _span_name(func: object, name: str | None) -> str:
    """The span name: the given one, else the function's qualname, else its name."""
    if name is not None:
        return name
    qualname = getattr(func, "__qualname__", "")
    if isinstance(qualname, str) and qualname:
        return qualname
    short_name = getattr(func, "__name__", "")
    if isinstance(short_name, str) and short_name:
        return short_name
    return "call"


def _wrap_coroutine(
    func: Callable[P, Awaitable[R]],
    signature: inspect.Signature | None,
    *,
    span_name: str,
    kind: SpanKind,
    capture_input: bool,
    capture_output: bool,
) -> Callable[P, Coroutine[Any, Any, R]]:
    """The async wrapper: the span starts when the coroutine is first awaited."""

    @functools.wraps(func)
    async def wrapper(*args: P.args, **kwargs: P.kwargs) -> R:
        runtime = current()
        if runtime is None:
            return await func(*args, **kwargs)
        input_value = _UNSET if not capture_input else _bound_input(signature, args, kwargs)
        otel_span, token = _start_span(runtime, span_name, kind, input_value)
        try:
            result = await func(*args, **kwargs)
        except Exception as error:
            _record_error(otel_span, error)
            _end_span(otel_span, token)
            raise
        except BaseException:
            # KeyboardInterrupt, asyncio.CancelledError, …: the span ends with
            # the status unset (like span()/llm_span) and the raise goes through.
            _end_span(otel_span, token)
            raise
        if capture_output and runtime.config.capture_content and _recordable(result):
            _record_payload(otel_span, "output", result)
        _end_span(otel_span, token)
        return result

    return wrapper


def _wrap(
    func: Callable[P, R],
    *,
    name: str | None,
    kind: SpanKind,
    capture_input: bool,
    capture_output: bool,
    reduce_output: Callable[[list[object]], object] | None,
) -> Callable[P, R]:
    """Build the traced wrapper of ``func`` (functions, coroutine functions,
    generators and async generators, P5-P6).
    """
    span_name = _span_name(func, name)
    try:
        signature = inspect.signature(func)
    except (TypeError, ValueError):
        signature = None
    if inspect.isgeneratorfunction(func) or inspect.isasyncgenfunction(func):
        # Imported here: _generators builds on this module's helpers. The
        # is*function checks narrow func's return type, so the R-typed view
        # of the same callable is passed through a cast.
        from llmobs._generators import _wrap_async_generator, _wrap_generator

        traced = cast("Callable[P, R]", func)
        if inspect.isasyncgenfunction(func):
            return _wrap_async_generator(
                traced,
                signature,
                span_name=span_name,
                kind=kind,
                capture_input=capture_input,
                capture_output=capture_output,
                reduce_output=reduce_output,
            )
        return _wrap_generator(
            traced,
            signature,
            span_name=span_name,
            kind=kind,
            capture_input=capture_input,
            capture_output=capture_output,
            reduce_output=reduce_output,
        )
    if inspect.iscoroutinefunction(func):
        # For a coroutine function R binds to Coroutine[Any, Any, X], so the
        # async wrapper (Callable[P, Coroutine[Any, Any, X]]) is a Callable[P, R].
        return cast(
            "Callable[P, R]",
            _wrap_coroutine(
                cast("Callable[P, Awaitable[R]]", func),
                signature,
                span_name=span_name,
                kind=kind,
                capture_input=capture_input,
                capture_output=capture_output,
            ),
        )
    return _wrap_function(
        func,
        signature,
        span_name=span_name,
        kind=kind,
        capture_input=capture_input,
        capture_output=capture_output,
    )


@overload
def observe(
    func: Callable[P, R],
    /,
    *,
    name: str | None = ...,
    kind: SpanKind = ...,
    capture_input: bool = ...,
    capture_output: bool = ...,
    reduce_output: Callable[[list[object]], object] | None = ...,
) -> Callable[P, R]: ...


@overload
def observe(
    func: None = ...,
    /,
    *,
    name: str | None = ...,
    kind: SpanKind = ...,
    capture_input: bool = ...,
    capture_output: bool = ...,
    reduce_output: Callable[[list[object]], object] | None = ...,
) -> Callable[[Callable[P, R]], Callable[P, R]]: ...


def observe(
    func: Callable[P, R] | None = None,
    /,
    *,
    name: str | None = None,
    kind: SpanKind = "chain",
    capture_input: bool = True,
    capture_output: bool = True,
    reduce_output: Callable[[list[object]], object] | None = None,
) -> Callable[P, R] | Callable[[Callable[P, R]], Callable[P, R]]:
    """Trace a function, coroutine function, generator or async generator as
    one span (spec 10, D-F-11).

    ``@observe`` keeps the function's qualname and the default kind; the
    factory form ``@observe(name=..., kind=...)`` overrides them. Generator
    functions are detected at decoration time; their span starts at the
    generator's first step, and ``reduce_output`` (accepted on every
    function, used only for generator functions) records its result on the
    items instead of the joined-text / JSON-list default. An invalid
    ``kind`` raises ``ValueError`` now, at decoration time. Every other
    failure after ``init()`` is a no-op, and without a running runtime the
    function is called unchanged: the runtime is checked at call time, so a
    function decorated before ``init()`` is traced after it.
    """
    _check_kind(kind)

    def decorate(function: Callable[P, R]) -> Callable[P, R]:
        return _wrap(
            function,
            name=name,
            kind=kind,
            capture_input=capture_input,
            capture_output=capture_output,
            reduce_output=reduce_output,
        )

    if func is None:
        return decorate
    return decorate(func)


def _current_span_context() -> trace_api.SpanContext:
    return trace_api.get_current_span().get_span_context()


def current_trace_id() -> str | None:
    """The current span's trace id, ours or any tracer's, as 32 lowercase hex
    characters; ``None`` outside a span.
    """
    context = _current_span_context()
    if context.is_valid:
        return format(context.trace_id, "032x")
    return None


def current_span_id() -> str | None:
    """The current span's span id, ours or any tracer's, as 16 lowercase hex
    characters; ``None`` outside a span.
    """
    context = _current_span_context()
    if context.is_valid:
        return format(context.span_id, "016x")
    return None
