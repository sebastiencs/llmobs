"""The llmobs span processor.

Stamps the ``context()`` values, then the configured attributes, on span
start, and batch-exports on end through the export filter of D-F-5
(spec 10 § Design rules 1, D-F-4, D-F-12).
"""

import re
import threading
from collections.abc import Callable, Mapping

from opentelemetry.baggage import get_baggage
from opentelemetry.context import Context
from opentelemetry.sdk.trace import ReadableSpan, Span, SpanProcessor
from opentelemetry.sdk.trace.export import BatchSpanProcessor, SpanExporter

from llmobs._context import current_values
from llmobs._filter import default_filter
from llmobs._log import logger
from llmobs._serialize import attribute_value

# § Span attributes: empty or outside [A-Za-z0-9_.-] fails the key rule.
_METADATA_KEY_OK = re.compile(r"^[A-Za-z0-9_.-]+$")
_METADATA_PREFIX = "llmobs.metadata."

_skip_lock = threading.Lock()
_skipped_keys: set[str] = set()


def _metadata_attribute(key: str) -> str | None:
    """The attribute name for a metadata key, or ``None`` when it fails the key rule.

    A skipped key is logged once per process at debug level, with its length
    only, never the key itself.
    """
    if _METADATA_KEY_OK.fullmatch(key) is not None:
        return f"{_METADATA_PREFIX}{key}"
    with _skip_lock:
        first = key not in _skipped_keys
        if first:
            _skipped_keys.add(key)
    if first:
        logger.debug("llmobs: metadata key skipped (length %d)", len(key))
    return None


def _baggage_string(key: str, ctx: Context | None) -> str | None:
    """The baggage value for ``key`` in ``ctx``, when it is a string."""
    value = get_baggage(key, ctx)
    return value if isinstance(value, str) else None


class LlmobsSpanProcessor(SpanProcessor):
    """Our span processor: stamp on start, batch-export on end.

    Every span of our provider is exported in own-provider mode; an explicit
    ``export_filter`` replaces that rule, and the ``default_filter`` of D-F-5
    runs in global mode.
    """

    def __init__(
        self,
        exporter: SpanExporter,
        *,
        environment: str | None,
        release: str | None,
        export_filter: Callable[[ReadableSpan], bool] | None,
        default_filter: bool,
    ) -> None:
        self._environment = environment
        self._release = release
        self._export_filter = export_filter
        self._default_filter = default_filter
        # Typed as the interface: we only forward, and BatchSpanProcessor's
        # shutdown is untyped in the installed package.
        self._batch: SpanProcessor = BatchSpanProcessor(
            exporter,
            schedule_delay_millis=1000,
            export_timeout_millis=10_000,
        )

    def _stamp(self, span: Span, parent_context: Context | None) -> None:
        values = current_values(parent_context)
        session_id = values.session_id
        if session_id is None:
            session_id = _baggage_string("llmobs.session.id", parent_context)
        if session_id is not None:
            span.set_attribute("llmobs.session.id", session_id)
        user_id = values.user_id
        if user_id is None:
            user_id = _baggage_string("llmobs.user.id", parent_context)
        if user_id is not None:
            span.set_attribute("llmobs.user.id", user_id)
        if values.tags:
            span.set_attribute("llmobs.tags", list(values.tags))
        self._stamp_metadata(span, values.metadata)
        if self._environment is not None:
            span.set_attribute("llmobs.environment", self._environment)
        if self._release is not None:
            span.set_attribute("llmobs.release", self._release)

    def _stamp_metadata(self, span: Span, metadata: Mapping[str, object]) -> None:
        for key, value in metadata.items():
            name = _metadata_attribute(key)
            if name is not None:
                span.set_attribute(name, attribute_value(value))

    def on_start(self, span: Span, parent_context: Context | None = None) -> None:
        try:
            self._stamp(span, parent_context)
        except Exception as error:  # noqa: BLE001  # never raise into the app (spec 10 rule 4)
            logger.debug("llmobs: could not stamp span attributes (%s)", type(error).__name__)

    def on_end(self, span: ReadableSpan) -> None:
        if self._keep(span):
            self._batch.on_end(span)

    def _keep(self, span: ReadableSpan) -> bool:
        """D-F-5: the configured filter decides; a raising filter exports."""
        if self._export_filter is not None:
            try:
                return bool(self._export_filter(span))
            except Exception as error:  # noqa: BLE001  # never raise into the app (spec 10 rule 4)
                logger.debug("llmobs: export_filter raised %s; span exported", type(error).__name__)
                return True
        # Own-provider mode exports everything; the default filter runs only
        # in global mode (D-F-4, D-F-5).
        return not self._default_filter or default_filter(span)

    def force_flush(self, timeout_millis: int = 30_000) -> bool:
        return self._batch.force_flush(timeout_millis)

    def shutdown(self) -> None:
        self._batch.shutdown()
