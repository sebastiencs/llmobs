"""Python SDK for llmobs.

Tracing, scores and configuration follow ``docs/spec/10-sdk.md``.
"""

import importlib.metadata

from llmobs._config import ConfigError

__version__ = importlib.metadata.version("llmobs")
__all__ = ["ConfigError", "__version__"]
