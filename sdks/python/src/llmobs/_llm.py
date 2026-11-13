"""The manual LLM span (plan F § P7, shared contract § LLM span, D-F-13).

``llm_span`` is a sync and async context manager like ``span`` (P5) that
writes the GenAI attributes: provider, request and response models, request
params, input and output messages (converted from the short form), system
instructions, usage token counts, a reported cost and the time to first
token. An invalid ``provider`` or ``model`` raises ``ValueError`` at
construction (§ "Programming errors raise"); every other failure after
``init()`` is a no-op (spec 10 rule 4).
"""

import math
import time
from collections.abc import Mapping, Sequence
from typing import TypeGuard, cast

from opentelemetry import trace as trace_api
from opentelemetry.sdk.trace import Span as SdkSpan

from llmobs._log import logger
from llmobs._mask import apply
from llmobs._runtime import current
from llmobs._serialize import serialize
from llmobs._spans import _UNSET, Span, _detach, _start_span, span


def to_canonical_messages(messages: Sequence[object]) -> list[object]:
    """Convert messages to the canonical shape (§ Messages JSON).

    An object with a ``parts`` key is kept as is; an object with ``role``
    and a string ``content`` becomes ``{"role": …, [name], [finish_reason],
    "parts": [one text part]}`` in that key order; anything else is kept
    for the server to judge (spec 02 records a conversion error).
    """
    return [_canonical_message(message) for message in messages]


def _canonical_message(message: object) -> object:
    """One message's canonical shape, or the message itself (§ Messages JSON)."""
    if not isinstance(message, Mapping):
        return message
    if "parts" in message:
        return message
    if "role" not in message or not isinstance(message.get("content"), str):
        return message
    canonical: dict[str, object] = {"role": message["role"]}
    if "name" in message:
        canonical["name"] = message["name"]
    if "finish_reason" in message:
        canonical["finish_reason"] = message["finish_reason"]
    canonical["parts"] = [{"type": "text", "content": message["content"]}]
    return canonical


def _record_messages(otel_span: trace_api.Span, key: str, messages: Sequence[object]) -> None:
    """Mask the whole list, canonicalize it, set the JSON attribute; never raises."""
    try:
        masked = apply(messages)
        converted: object = (
            to_canonical_messages(masked) if isinstance(masked, (list, tuple)) else masked
        )
        otel_span.set_attribute(key, serialize(converted)[0])
    except Exception as error:  # noqa: BLE001  # never raise into the app (spec 10 rule 4)
        logger.debug("llmobs: could not record %s (%s)", key, type(error).__name__)


def _record_system_instructions(otel_span: trace_api.Span, value: str | Sequence[object]) -> None:
    """Mask the instructions and wrap a string as one text part; never raises."""
    try:
        masked = apply(value)
        converted: object = (
            [{"type": "text", "content": masked}] if isinstance(masked, str) else masked
        )
        otel_span.set_attribute("gen_ai.system_instructions", serialize(converted)[0])
    except Exception as error:  # noqa: BLE001  # never raise into the app (spec 10 rule 4)
        logger.debug(
            "llmobs: could not record gen_ai.system_instructions (%s)", type(error).__name__
        )


def _check_non_empty(which: str, value: str) -> None:
    """Raise ``ValueError`` on an empty or non-string provider/model value."""
    if not isinstance(value, str) or not value:
        raise ValueError(f"{which}: must be a non-empty string")


def _param_ok(value: object) -> TypeGuard[str | bool | int | float]:
    """Whether a request param value is a recordable scalar (§ LLM span)."""
    if isinstance(value, (str, int)):  # bool is an int; OTel keeps its actual type
        return True
    return isinstance(value, float) and math.isfinite(value)


class LlmSpan(Span):
    """Handle on one of our LLM spans (§ LLM span, D-F-13).

    Every method is a no-op after the span ended and never raises. The
    inherited ``set_input``/``set_output`` remain available for non-chat
    payloads; with ``capture_content=false`` no message attribute is
    written, while the models, params, usage, cost and metadata are kept.
    """

    def __init__(self, otel_span: trace_api.Span | None, *, capture: bool) -> None:
        """``otel_span`` is ``None`` for a no-op handle (no running runtime)."""
        super().__init__(otel_span, capture=capture)
        self._first_token_done = False

    def set_input_messages(self, messages: Sequence[object]) -> None:
        """Record ``messages`` as ``gen_ai.input.messages`` (§ Messages JSON)."""
        otel_span = self._otel_span
        if otel_span is None or self._ended or not self._capture:
            return
        _record_messages(otel_span, "gen_ai.input.messages", messages)

    def set_output_messages(self, messages: Sequence[object]) -> None:
        """Record ``messages`` as ``gen_ai.output.messages`` (§ Messages JSON)."""
        otel_span = self._otel_span
        if otel_span is None or self._ended or not self._capture:
            return
        _record_messages(otel_span, "gen_ai.output.messages", messages)

    def set_response_model(self, model: str) -> None:
        """Record the model that answered as ``gen_ai.response.model``."""
        otel_span = self._otel_span
        if otel_span is None or self._ended:
            return
        try:
            otel_span.set_attribute("gen_ai.response.model", model)
        except Exception as error:  # noqa: BLE001  # never raise into the app (spec 10 rule 4)
            logger.debug("llmobs: could not record the response model (%s)", type(error).__name__)

    def set_usage(
        self,
        *,
        input_tokens: int | None = None,
        output_tokens: int | None = None,
        cache_read_tokens: int | None = None,
        cache_write_tokens: int | None = None,
        reasoning_tokens: int | None = None,
    ) -> None:
        """Record token counts as the ``gen_ai.usage.*`` attributes (§ LLM span).

        ``input_tokens`` includes cache tokens (GenAI semantics). Ints >= 0
        only: ``None`` means not given; a ``bool``, negative or non-int
        value is skipped with one debug line of counts only.
        """
        otel_span = self._otel_span
        if otel_span is None or self._ended:
            return
        counts: tuple[tuple[str, int | None], ...] = (
            ("gen_ai.usage.input_tokens", input_tokens),
            ("gen_ai.usage.output_tokens", output_tokens),
            ("gen_ai.usage.cache_read.input_tokens", cache_read_tokens),
            ("gen_ai.usage.cache_creation.input_tokens", cache_write_tokens),
            ("gen_ai.usage.reasoning.output_tokens", reasoning_tokens),
        )
        skipped = 0
        try:
            for key, value in counts:
                if value is None:
                    continue
                if isinstance(value, int) and not isinstance(value, bool) and value >= 0:
                    otel_span.set_attribute(key, value)
                else:
                    skipped += 1
            if skipped:
                logger.debug("llmobs: skipped %d usage values", skipped)
        except Exception as error:  # noqa: BLE001  # never raise into the app (spec 10 rule 4)
            logger.debug("llmobs: could not record usage (%s)", type(error).__name__)

    def set_cost(self, total_usd: float) -> None:
        """Record the call's total cost as ``llmobs.cost.total`` USD.

        The value is converted with ``float()`` and written only when the
        result is finite and >= 0.
        """
        otel_span = self._otel_span
        if otel_span is None or self._ended:
            return
        try:
            cost = float(total_usd)
        except Exception as error:  # noqa: BLE001  # never raise into the app (spec 10 rule 4)
            logger.debug("llmobs: cost skipped (%s)", type(error).__name__)
            return
        if not math.isfinite(cost) or cost < 0:
            logger.debug("llmobs: cost skipped (negative or not finite)")
            return
        try:
            otel_span.set_attribute("llmobs.cost.total", cost)
        except Exception as error:  # noqa: BLE001  # never raise into the app (spec 10 rule 4)
            logger.debug("llmobs: could not record the cost (%s)", type(error).__name__)

    def record_first_token(self) -> None:
        """Record the time to first token as ``llmobs.ttft_ns`` (int ns).

        The elapsed time is measured from the OTel span's start; only the
        first call counts.
        """
        otel_span = self._otel_span
        if otel_span is None or self._ended or self._first_token_done:
            return
        self._first_token_done = True
        try:
            # Our spans come from the SDK tracer; start_time is int ns once started.
            start_time = cast("SdkSpan", otel_span).start_time
            if start_time is not None:
                otel_span.set_attribute("llmobs.ttft_ns", time.time_ns() - start_time)
        except Exception as error:  # noqa: BLE001  # never raise into the app (spec 10 rule 4)
            logger.debug("llmobs: could not record the first token (%s)", type(error).__name__)


class llm_span(span):  # noqa: N801  # public API name from spec 10
    """Manual LLM span, as a sync and async context manager (§ LLM span, D-F-13).

    The body's return value is never recorded (§ Context managers); the
    ``LlmSpan`` handle's setters write the GenAI attributes. An invalid
    ``provider`` or ``model`` raises ``ValueError`` at construction;
    without a running runtime the block yields a no-op handle.
    """

    def __init__(
        self,
        name: str,
        *,
        provider: str,
        model: str,
        input_messages: Sequence[object] | None = None,
        system_instructions: str | Sequence[object] | None = None,
        params: Mapping[str, object] | None = None,
        metadata: Mapping[str, object] | None = None,
    ) -> None:
        """Validate ``provider`` and ``model`` now: bad ones are programming errors."""
        _check_non_empty("provider", provider)
        _check_non_empty("model", model)
        super().__init__(name, kind="llm", metadata=metadata)
        self._provider = provider
        self._model = model
        self._input_messages = input_messages
        self._system_instructions = system_instructions
        self._params = params

    def _enter(self) -> LlmSpan:
        runtime = current()
        if runtime is None:
            self._handle = LlmSpan(None, capture=False)
            return self._handle
        otel_span, self._token = _start_span(runtime, self._name, "llm", _UNSET)
        self._otel_span = otel_span
        self._write_request_attributes(otel_span)
        handle = LlmSpan(otel_span, capture=runtime.config.capture_content)
        self._handle = handle
        if self._metadata is not None:
            handle._write_metadata(self._metadata)
        if self._input_messages is not None:
            handle.set_input_messages(self._input_messages)
        if self._system_instructions is not None and runtime.config.capture_content:
            _record_system_instructions(otel_span, self._system_instructions)
        return handle

    def _write_request_attributes(self, otel_span: trace_api.Span) -> None:
        """Set the always-on GenAI attributes and the request params; never raises."""
        try:
            otel_span.set_attribute("gen_ai.operation.name", "chat")
            otel_span.set_attribute("gen_ai.provider.name", self._provider)
            otel_span.set_attribute("gen_ai.request.model", self._model)
            params = self._params
            if params is not None:
                skipped = 0
                for key, value in params.items():
                    if _param_ok(value):
                        otel_span.set_attribute(f"gen_ai.request.{key}", value)
                    else:
                        skipped += 1
                if skipped:
                    logger.debug("llmobs: skipped %d request params", skipped)
        except Exception as error:  # noqa: BLE001  # never raise into the app (spec 10 rule 4)
            logger.debug(
                "llmobs: could not record the request attributes (%s)", type(error).__name__
            )

    def __enter__(self) -> LlmSpan:
        try:
            return self._enter()
        except Exception as error:  # noqa: BLE001  # never raise into the app (spec 10 rule 4)
            logger.debug("llmobs: llm_span entry failed (%s)", type(error).__name__)
            token, self._token = self._token, None
            self._otel_span, self._handle = None, None
            _detach(token)
            return LlmSpan(None, capture=False)

    async def __aenter__(self) -> LlmSpan:
        """Async variant of ``__enter__``."""
        return self.__enter__()
