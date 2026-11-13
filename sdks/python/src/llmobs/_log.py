"""The ``llmobs`` logger and the once-per-key warning helper.

Messages logged here never contain values, keys or user payload (AGENTS.md § Privacy).
"""

import logging
import threading

logger = logging.getLogger("llmobs")
# Library convention: no handlers of our own, only a NullHandler so a missing
# configuration does not print "no handlers could be found".
logger.addHandler(logging.NullHandler())

_lock = threading.Lock()
_warned: set[str] = set()


def warn_once(key: str, message: str) -> None:
    """Log ``message`` at WARNING the first time ``key`` is seen in the process."""
    with _lock:
        if key in _warned:
            return
        _warned.add(key)
    logger.warning(message)


def reset_warnings() -> None:
    """Forget every seen key. Tests only."""
    with _lock:
        _warned.clear()
