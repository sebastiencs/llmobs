"""The OpenInference instrumentor registry and its wiring (D-F-7).

``instrument=["openai", ...]`` names come from ``REGISTRY``; each maps to an
OpenInference instrumentor class and the extra that installs it. The
``openinference`` and ``opentelemetry.instrumentation`` imports live inside
:func:`enable_for`: without an extra none of it is installed, and an empty
``instrument`` list must import nothing.
"""

import importlib
from collections.abc import Sequence
from dataclasses import dataclass
from typing import TYPE_CHECKING, Any

from llmobs._config import ConfigError
from llmobs._log import logger

if TYPE_CHECKING:
    from llmobs._runtime import Runtime


@dataclass(frozen=True, slots=True)
class Instrumentor:
    """One registry row: a name, the extra that installs it, and its class."""

    name: str
    extra: str
    module: str
    cls: str


# Fixed by § Python instrumentors (plan F). A plain dict so tests can patch a
# fake entry into it.
REGISTRY: dict[str, Instrumentor] = {
    entry.name: entry
    for entry in (
        Instrumentor(
            name="openai",
            extra="openai",
            module="openinference.instrumentation.openai",
            cls="OpenAIInstrumentor",
        ),
        Instrumentor(
            name="anthropic",
            extra="anthropic",
            module="openinference.instrumentation.anthropic",
            cls="AnthropicInstrumentor",
        ),
        Instrumentor(
            name="langchain",
            extra="langchain",
            module="openinference.instrumentation.langchain",
            cls="LangChainInstrumentor",
        ),
        Instrumentor(
            name="llama-index",
            extra="llama-index",
            module="openinference.instrumentation.llama_index",
            cls="LlamaIndexInstrumentor",
        ),
        Instrumentor(
            name="litellm",
            extra="litellm",
            module="openinference.instrumentation.litellm",
            cls="LiteLLMInstrumentor",
        ),
        Instrumentor(
            name="google-genai",
            extra="google-genai",
            module="openinference.instrumentation.google_genai",
            cls="GoogleGenAIInstrumentor",
        ),
        Instrumentor(
            name="bedrock",
            extra="bedrock",
            module="openinference.instrumentation.bedrock",
            cls="BedrockInstrumentor",
        ),
        Instrumentor(
            name="openai-agents",
            extra="openai-agents",
            module="openinference.instrumentation.openai_agents",
            cls="OpenAIAgentsInstrumentor",
        ),
        Instrumentor(
            name="crewai",
            extra="crewai",
            module="openinference.instrumentation.crewai",
            cls="CrewAIInstrumentor",
        ),
        Instrumentor(
            name="dspy",
            extra="dspy",
            module="openinference.instrumentation.dspy",
            cls="DSPyInstrumentor",
        ),
    )
}


def resolve(names: Sequence[str]) -> list[type]:
    """Import each named instrumentor class; a bad name raises ``ConfigError``.

    Runs before the runtime exists, so a failure leaves nothing installed.
    Instrumentor names are not secret (F1), so the errors name them.
    """
    classes: list[type] = []
    for name in names:
        entry = REGISTRY.get(name)
        if entry is None:
            raise ConfigError(
                f"instrument: unknown instrumentor {name!r}; known: {', '.join(sorted(REGISTRY))}"
            )
        try:
            module = importlib.import_module(entry.module)
        except ImportError:
            raise ConfigError(_extra_hint(entry)) from None
        cls = getattr(module, entry.cls, None)
        if cls is None:
            raise ConfigError(_extra_hint(entry))
        classes.append(cls)
    return classes


def enable_for(runtime: "Runtime", classes: Sequence[type]) -> None:
    """Instrument each class on the runtime's provider (D-F-7).

    Only the instances instrumented here are kept on the runtime, and only
    those are uninstrumented at shutdown. Instances are ``Any``: their classes
    come from optional extras.
    """
    if not classes:
        return
    from openinference.instrumentation import TraceConfig
    from opentelemetry.instrumentation.dependencies import DependencyConflictError

    hide = not runtime.config.capture_content
    # None (not False) keeps the user's OPENINFERENCE_HIDE_* env settings.
    config = TraceConfig(
        hide_inputs=True if hide else None,
        hide_outputs=True if hide else None,
    )
    for cls in classes:
        entry = _entry_of(cls)
        instance: Any = cls()
        if instance.is_instrumented_by_opentelemetry:
            # The app instrumented this library itself; leave it alone.
            logger.debug("llmobs: instrumentor %s already active; skipped", entry.name)
            continue
        try:
            instance.instrument(
                tracer_provider=runtime.provider,
                config=config,
                raise_exception_on_conflict=True,
            )
        except (DependencyConflictError, ImportError):
            raise ConfigError(_extra_hint(entry)) from None
        runtime.instrumentors.append(instance)


def _extra_hint(entry: Instrumentor) -> str:
    return f"instrument: {entry.name!r} needs `pip install llmobs[{entry.extra}]`"


def _entry_of(cls: type) -> Instrumentor:
    """The registry entry whose class is ``cls`` (class names are unique)."""
    name = cls.__name__
    for entry in REGISTRY.values():
        if entry.cls == name:
            return entry
    # Unreachable: every class passed here came from resolve(), which read
    # REGISTRY (tests patch their fake entries into it too).
    raise ConfigError(f"instrument: unknown instrumentor class {name!r}")
