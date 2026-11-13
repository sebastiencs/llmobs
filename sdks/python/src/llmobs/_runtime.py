"""The SDK runtime: ``init()``, the tracer provider, the exporter and the lifecycle.

Own-provider mode by default (D-F-4): the provider is never registered
globally. With ``use_global_provider=True`` the runtime joins the global
SDK provider when there is one, or registers its own.
"""

import atexit
from collections.abc import Callable, Sequence
from dataclasses import dataclass, field
from typing import Any

from opentelemetry import trace
from opentelemetry.exporter.otlp.proto.http import Compression
from opentelemetry.exporter.otlp.proto.http.trace_exporter import OTLPSpanExporter
from opentelemetry.sdk.resources import Resource
from opentelemetry.sdk.trace import ReadableSpan, SpanLimits, SpanProcessor, TracerProvider
from opentelemetry.sdk.trace.export import SpanExporter
from opentelemetry.sdk.trace.id_generator import IdGenerator, RandomIdGenerator
from opentelemetry.sdk.trace.sampling import ParentBased, TraceIdRatioBased
from opentelemetry.trace import NoOpTracerProvider, ProxyTracerProvider

from llmobs import __version__
from llmobs._config import Config, ConfigError, resolve_config
from llmobs._instrument import enable_for, resolve
from llmobs._log import logger, reset_warnings, warn_once
from llmobs._mask import MaskingExporter
from llmobs._processor import LlmobsSpanProcessor
from llmobs._scores import ScoreQueue

_INIT_TWICE_WARNING = "llmobs: init() was already called; this call is ignored"
_GLOBAL_SAMPLE_RATE_WARNING = (
    "llmobs: sample_rate is ignored with use_global_provider=True and an existing provider"
)
_GLOBAL_PROVIDER_WARNING = (
    "llmobs: the global tracer provider cannot take span processors; llmobs keeps its own provider"
)


@dataclass(slots=True)
class Runtime:
    """The installed SDK runtime (spec 10 § Lifecycle)."""

    config: Config
    provider: TracerProvider
    owns_provider: bool
    processor: LlmobsSpanProcessor
    tracer: trace.Tracer
    scores: ScoreQueue
    instrumentors: list[Any] = field(default_factory=list, repr=False)
    _shutdown_done: bool = field(default=False, repr=False)

    @classmethod
    def create(
        cls,
        config: Config,
        *,
        exporter: SpanExporter | None = None,
        id_generator: IdGenerator | None = None,
    ) -> "Runtime":
        """Build the runtime of D-F-4: our own provider, or the global one."""
        if exporter is None:
            exporter = OTLPSpanExporter(
                endpoint=f"{config.base_url}/v1/traces",
                headers={"Authorization": f"Bearer {config.api_key}"},
                timeout=10,
                compression=Compression.Gzip,
            )
        if config.mask is not None or not config.capture_content:
            exporter = MaskingExporter(
                exporter, mask=config.mask, capture_content=config.capture_content
            )
        global_provider = trace.get_tracer_provider() if config.use_global_provider else None
        # The default export filter runs only in global mode (D-F-5): own mode
        # and the unsupported-provider branch export every span.
        use_default_filter = isinstance(global_provider, (TracerProvider, ProxyTracerProvider))
        provider: TracerProvider
        if isinstance(global_provider, TracerProvider):
            # D-F-4 branch 1: the app's SDK provider takes our processor. The
            # app keeps ownership — and its sampler, hence the warning.
            if config.sample_rate < 1:
                warn_once("global-sample-rate", _GLOBAL_SAMPLE_RATE_WARNING)
            provider = global_provider
            owns_provider = False
        else:
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
            owns_provider = True
            if isinstance(global_provider, ProxyTracerProvider):
                # D-F-4 branch 2: nothing is registered yet; ours becomes the
                # global provider.
                trace.set_tracer_provider(provider)
            elif global_provider is not None:
                # D-F-4 branch 3: a non-SDK provider cannot take processors.
                warn_once("global-provider-unsupported", _GLOBAL_PROVIDER_WARNING)
        processor = LlmobsSpanProcessor(
            exporter,
            environment=config.environment,
            release=config.release,
            export_filter=config.export_filter,
            default_filter=use_default_filter,
        )
        provider.add_span_processor(processor)
        # A disabled config never reaches Runtime.create, so the key is set;
        # ``or ""`` only satisfies the type checker.
        scores = ScoreQueue(base_url=config.base_url, api_key=config.api_key or "")
        return cls(
            config=config,
            provider=provider,
            owns_provider=owns_provider,
            processor=processor,
            tracer=provider.get_tracer("llmobs", __version__),
            scores=scores,
        )

    def flush(self, timeout_millis: int = 30_000) -> bool:
        spans_ok = self.processor.force_flush(timeout_millis)
        scores_ok = self.scores.flush(timeout_millis / 1000)
        return spans_ok and scores_ok

    def shutdown(self) -> None:
        """Flush, then stop the processor and, when owned, the provider. Idempotent.

        The instrumentors the runtime enabled are uninstrumented first, and
        only those (D-F-7). In global mode with the app's provider (D-F-4
        branch 1) only our processor stops: the app owns the provider. The
        score queue stops after the span pipeline.
        """
        if self._shutdown_done:
            return
        self._shutdown_done = True
        for instance in self.instrumentors:
            try:
                instance.uninstrument()
            except Exception as error:  # noqa: BLE001  # never raise into the app (spec 10 rule 4)
                logger.debug("llmobs: uninstrument failed (%s)", type(error).__name__)
        self.instrumentors.clear()
        self.flush()
        self.processor.shutdown()
        if self.owns_provider:
            self.provider.shutdown()
        self.scores.shutdown()


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
    use_global_provider: bool | None = None,
    export_filter: Callable[[ReadableSpan], bool] | None = None,
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
        use_global_provider=use_global_provider,
        export_filter=export_filter,
        enabled=enabled,
    )
    if not config.enabled:
        return
    # D-F-7: resolve before the runtime exists, so a bad name leaves nothing
    # installed.
    classes = resolve(config.instrument)
    runtime = Runtime.create(config)
    install(runtime)
    try:
        enable_for(runtime, classes)
    except ConfigError:
        uninstall(runtime)
        raise
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


def uninstall(runtime: Runtime) -> None:
    """Shut ``runtime`` down and remove it when installed. Internal; test seam.

    Used when enabling instrumentors fails after install (D-F-7): the process
    is left with nothing installed and the error is re-raised.
    """
    global _runtime
    runtime.shutdown()
    if _runtime is runtime:
        _runtime = None


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
