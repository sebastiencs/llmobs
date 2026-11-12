"""Python SDK for llmobs.

Tracing, scores and configuration follow ``docs/spec/10-sdk.md``.
"""

import importlib.metadata

# __version__ is set before the submodule imports: _runtime imports it from here.
__version__ = importlib.metadata.version("llmobs")

from llmobs._config import ConfigError
from llmobs._context import context
from llmobs._runtime import flush, init, shutdown, span_processor, tracer_provider

__all__ = [
    "ConfigError",
    "__version__",
    "context",
    "flush",
    "init",
    "shutdown",
    "span_processor",
    "tracer_provider",
]
