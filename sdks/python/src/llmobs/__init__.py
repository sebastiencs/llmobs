"""Python SDK for llmobs.

Tracing, scores and configuration follow ``docs/spec/10-sdk.md``.
"""

import importlib.metadata

__version__ = importlib.metadata.version("llmobs")
__all__ = ["__version__"]
