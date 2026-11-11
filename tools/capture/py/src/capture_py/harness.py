"""Start a tracer provider aimed at the capture receiver, then record library versions."""

from __future__ import annotations

import importlib.metadata
import json
import os

from opentelemetry import trace
from opentelemetry.exporter.otlp.proto.http.trace_exporter import OTLPSpanExporter
from opentelemetry.sdk.resources import Resource
from opentelemetry.sdk.trace import TracerProvider
from opentelemetry.sdk.trace.export import SimpleSpanProcessor


def setup() -> TracerProvider:
    """Install a provider that exports each span as it ends.

    The service name comes from ``OTEL_SERVICE_NAME``. The exporter reads its endpoint,
    protocol, and compression from the environment the runner sets.
    """
    provider = TracerProvider(resource=Resource.create({}))
    provider.add_span_processor(SimpleSpanProcessor(OTLPSpanExporter()))
    trace.set_tracer_provider(provider)
    return provider


def mock_url(path: str = "") -> str:
    """Return the mock base URL plus ``path``.

    A missing ``LLMOBS_CAPTURE_MOCK_URL`` raises ``KeyError`` naming that variable.
    """
    return os.environ["LLMOBS_CAPTURE_MOCK_URL"] + path


def receiver_base_url() -> str:
    """Return the receiver origin, without the ``/v1/traces`` path.

    A missing ``LLMOBS_CAPTURE_OTLP_ENDPOINT`` raises ``KeyError`` naming that variable.
    An endpoint that does not end in ``/v1/traces`` raises ``ValueError``.
    """
    endpoint = os.environ["LLMOBS_CAPTURE_OTLP_ENDPOINT"]
    suffix = "/v1/traces"
    if not endpoint.endswith(suffix):
        raise ValueError("LLMOBS_CAPTURE_OTLP_ENDPOINT has no /v1/traces suffix")
    return endpoint[: -len(suffix)]


def finish(provider: TracerProvider) -> None:
    """Flush and shut down ``provider``, then write the library versions the runner asked for.

    A missing ``LLMOBS_CAPTURE_LIBRARIES`` or ``LLMOBS_CAPTURE_META`` raises ``KeyError``
    naming that variable.
    """
    provider.force_flush()
    provider.shutdown()
    names = os.environ["LLMOBS_CAPTURE_LIBRARIES"].split(",")
    meta_path = os.environ["LLMOBS_CAPTURE_META"]
    libraries = {name: importlib.metadata.version(name) for name in names if name}
    with open(meta_path, "w", encoding="utf-8") as meta_file:
        json.dump({"libraries": libraries}, meta_file)


def prefer_identity_otlp_compression() -> None:
    """Make later HTTP OTLP exporters send an identity body.

    Laminar constructs its trace and log exporters with gzip and does not read
    ``OTEL_EXPORTER_OTLP_COMPRESSION``. The receiver accepts only an identity body.
    """
    from collections.abc import Callable
    from typing import cast

    from opentelemetry.exporter.otlp.proto.http import Compression
    from opentelemetry.exporter.otlp.proto.http._log_exporter import OTLPLogExporter
    from opentelemetry.exporter.otlp.proto.http.trace_exporter import OTLPSpanExporter

    def wrap(exporter: type[OTLPSpanExporter] | type[OTLPLogExporter]) -> None:
        original = cast("Callable[..., None]", exporter.__init__)

        def init(self: object, *args: object, **kwargs: object) -> None:
            kwargs["compression"] = Compression.NoCompression
            original(self, *args, **kwargs)

        exporter.__init__ = init  # type: ignore[method-assign]

    wrap(OTLPSpanExporter)
    wrap(OTLPLogExporter)
