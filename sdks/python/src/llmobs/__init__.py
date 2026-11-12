"""Python SDK for llmobs.

Tracing, scores and configuration follow ``docs/spec/10-sdk.md``.
"""

import importlib.metadata

# __version__ is set before the submodule imports: _runtime imports it from here.
__version__ = importlib.metadata.version("llmobs")

from llmobs._config import ConfigError
from llmobs._context import context
from llmobs._llm import LlmSpan, llm_span
from llmobs._runtime import flush, init, shutdown, span_processor, tracer_provider
from llmobs._scores import score, uuid7
from llmobs._spans import Span, current_span_id, current_trace_id, observe, span

__all__ = [
    "ConfigError",
    "LlmSpan",
    "Span",
    "__version__",
    "context",
    "current_span_id",
    "current_trace_id",
    "flush",
    "init",
    "llm_span",
    "observe",
    "score",
    "shutdown",
    "span",
    "span_processor",
    "tracer_provider",
    "uuid7",
]
