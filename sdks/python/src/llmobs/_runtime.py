"""The SDK runtime: ``init()``, the tracer provider, the exporter and the lifecycle.

Own-provider mode only in this sub-phase (D-F-4): the provider is never
registered globally. Global mode arrives with P8.
"""

import atexit
from collections.abc import Callable, Sequence
from dataclasses import dataclass, field
from typing import Any

from opentelemetry import trace
from opentelemetry.exporter.otlp.proto.http import Compression
from opentelemetry.exporter.otlp.proto.http.trace_exporter import OTLPSpanExporter
from opentelemetry.sdk.resources import Resource
from opentelemetry.sdk.trace import SpanLimits, SpanProcessor, TracerProvider
from opentelemetry.sdk.trace.export import SpanExporter
from opentelemetry.sdk.trace.id_generator import IdGenerator, RandomIdGenerator
from opentelemetry.sdk.trace.sampling import ParentBased, TraceIdRatioBased
from opentelemetry.trace import NoOpTracerProvider

from llmobs import __version__
from llmobs._config import Config, resolve_config
from llmobs._log import logger, reset_warnings, warn_once
from llmobs._processor import LlmobsSpanProcessor

_INIT_TWICE_WARNING = "llmobs: init() was already called; this call is ignored"


@dataclass(slots=True)
class Runtime:
    """The installed SDK runtime (spec 10 § Lifecycle)."""

    config: Config
    provider: TracerProvider
    owns_provider: bool
    processor: LlmobsSpanProcessor
    tracer: trace.Tracer
    _shutdown_done: bool = field(default=False, repr=False)

    @classmethod
    def create(
        cls,
        config: Config,
        *,
        exporter: SpanExporter | None = None,
        id_generator: IdGenerator | None = None,
    ) -> "Runtime":
        """Build the own-provider runtime of D-F-4 (global mode arrives in P8)."""
        if exporter is None:
            exporter = OTLPSpanExporter(
                endpoint=f"{config.base_url}/v1/traces",
                headers={"Authorization": f"Bearer {config.api_key}"},
                timeout=10,
                compression=Compression.Gzip,
            )
        processor = LlmobsSpanProcessor(
            exporter,
            environment=config.environment,
            release=config.release,
            export_filter=None,
            default_filter=False,
        )
        provider = TracerProvider(
            resource=Resource.create(
                {
                    "telemetry.distro.name": "llmobs",
                    "telemetry.distro.version": __version__,
                }
            ),
            sampler=ParentBased(root=TraceIdRatioBased(config.sample_rate)),
            span_limits=SpanLimits(max_span_attributes=10_000),
            id_generator=id_generator or RandomIdGenerator(),
        )
        provider.add_span_processor(processor)
        return cls(
            config=config,
            provider=provider,
            owns_provider=True,
            processor=processor,
            tracer=provider.get_tracer("llmobs", __version__),
        )

    def flush(self, timeout_millis: int = 30_000) -> bool:
        return self.processor.force_flush(timeout_millis)

    def shutdown(self) -> None:
        """Flush, then stop the processor and, when owned, the provider. Idempotent."""
        if self._shutdown_done:
            return
        self._shutdown_done = True
        self.flush()
        self.processor.shutdown()
        if self.owns_provider:
            self.provider.shutdown()


_runtime: Runtime | None = None
_init_called = False
_atexit_registered = False


def init(
    *,
    api_key: str | None = None,
    base_url: str | None = None,
    environment: str | None = None,
    release: str | None = None,
    capture_content: bool | None = None,
    sample_rate: float | None = None,
    instrument: Sequence[str] | None = None,
    mask: Callable[[Any], Any] | None = None,
    enabled: bool | None = None,
) -> None:
    """Initialize the SDK, once per process (D-F-15).

    Raises ``ConfigError`` on an invalid option.
    """
    global _init_called, _atexit_registered
    if _init_called:
        warn_once("init-twice", _INIT_TWICE_WARNING)
        return
    # Set before anything else: a disabled first call also blocks a second one.
    _init_called = True
    config = resolve_config(
        api_key=api_key,
        base_url=base_url,
        environment=environment,
        release=release,
        capture_content=capture_content,
        sample_rate=sample_rate,
        instrument=instrument,
        mask=mask,
        enabled=enabled,
    )
    if not config.enabled:
        return
    install(Runtime.create(config))
    if not _atexit_registered:
        _atexit_registered = True
        atexit.register(shutdown)


def flush() -> None:
    """Flush pending spans. A no-op when not running; never raises."""
    runtime = current()
    if runtime is None:
        return
    try:
        runtime.flush()
    except Exception as error:  # noqa: BLE001  # never raise into the app (spec 10 rule 4)
        logger.debug("llmobs: flush failed (%s)", type(error).__name__)


def shutdown() -> None:
    """Flush and stop the SDK; idempotent, and a no-op after the first call."""
    global _runtime
    runtime, _runtime = _runtime, None
    if runtime is None:
        return
    try:
        runtime.shutdown()
    except Exception as error:  # noqa: BLE001  # never raise into the app (spec 10 rule 4)
        logger.debug("llmobs: shutdown failed (%s)", type(error).__name__)


def tracer_provider() -> trace.TracerProvider:
    """The provider in use: a no-op provider before ``init()``, when disabled or after shutdown."""
    runtime = current()
    if runtime is None:
        return NoOpTracerProvider()
    return runtime.provider


def span_processor() -> SpanProcessor | None:
    """Our span processor, for apps building their own OTel pipeline; ``None`` when not running."""
    runtime = current()
    if runtime is None:
        return None
    return runtime.processor


def current() -> Runtime | None:
    """The running runtime; ``None`` when absent, disabled or shut down. Internal."""
    return _runtime


def install(runtime: Runtime) -> None:
    """Install ``runtime`` as the process runtime. Internal; also the test seam."""
    global _runtime
    _runtime = runtime


def reset() -> None:
    """Shut down and forget the runtime, clear the init flag and the warnings. Tests only.

    The atexit flag is never cleared: a second registration would be wrong (D-F-15).
    """
    global _runtime, _init_called
    runtime, _runtime = _runtime, None
    _init_called = False
    reset_warnings()
    if runtime is None:
        return
    try:
        runtime.shutdown()
    except Exception as error:  # noqa: BLE001  # never raise into the app (spec 10 rule 4)
        logger.debug("llmobs: shutdown during reset failed (%s)", type(error).__name__)
