"""The llmobs span processor.

Stamps the configured attributes on span start, then batch-exports
(spec 10 § Design rules 1, D-F-4). P4 adds the ``context()`` values to
``on_start``; P8 adds the export filter to ``on_end``.
"""

from collections.abc import Callable

from opentelemetry.context import Context
from opentelemetry.sdk.trace import ReadableSpan, Span, SpanProcessor
from opentelemetry.sdk.trace.export import BatchSpanProcessor, SpanExporter

from llmobs._log import logger


class LlmobsSpanProcessor(SpanProcessor):
    """Our span processor: stamp on start, batch-export on end.

    Every span of our provider is exported in own-provider mode (D-F-5);
    ``export_filter`` and ``default_filter`` are stored for P8's global mode.
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

    def on_start(self, span: Span, parent_context: Context | None = None) -> None:
        try:
            if self._environment is not None:
                span.set_attribute("llmobs.environment", self._environment)
            if self._release is not None:
                span.set_attribute("llmobs.release", self._release)
        except Exception as error:  # noqa: BLE001  # never raise into the app (spec 10 rule 4)
            logger.debug("llmobs: could not stamp span attributes (%s)", type(error).__name__)

    def on_end(self, span: ReadableSpan) -> None:
        self._batch.on_end(span)

    def force_flush(self, timeout_millis: int = 30_000) -> bool:
        return self._batch.force_flush(timeout_millis)

    def shutdown(self) -> None:
        self._batch.shutdown()
