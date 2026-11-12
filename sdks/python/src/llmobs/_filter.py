"""The default export filter of global mode (D-F-5).

With our own provider every span is exported: only our tracer and the
instrumentors we bind create spans there. In global mode other libraries
create spans too, so the default filter keeps the LLM-related ones.
"""

from opentelemetry.sdk.trace import ReadableSpan

# A span is exported when its instrumentation scope has one of these names.
_SCOPE_NAMES = frozenset({"llmobs", "ai"})

# Or when one of its attribute keys starts with one of these prefixes.
_KEY_PREFIXES = (
    "llmobs.",
    "gen_ai.",
    "llm.",
    "openinference.",
    "ai.",
    "traceloop.",
    "langfuse.",
    "lmnr.",
    "gcp.vertex.agent.",
)

# Or when one of its attribute keys is exactly one of these.
_EXACT_KEYS = frozenset({"input.value", "output.value"})


def default_filter(span: ReadableSpan) -> bool:
    """D-F-5: export LLM-related spans in global mode.

    A span counts when its instrumentation scope is named ``llmobs`` or
    ``ai``, or when one of its attribute keys starts with a known prefix or
    is exactly ``input.value``/``output.value``. A span without a scope or
    without attributes is judged on what it has.
    """
    scope = span.instrumentation_scope
    if scope is not None and scope.name in _SCOPE_NAMES:
        return True
    attributes = span.attributes
    if attributes is None:
        return False
    return any(key in _EXACT_KEYS or key.startswith(_KEY_PREFIXES) for key in attributes)
