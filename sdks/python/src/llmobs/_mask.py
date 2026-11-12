"""Capture-time masking helper (D-F-6).

``apply`` routes a value through the ``mask`` callable of the running
runtime, when one is configured. P8 adds the export-time masking pass here.
"""

from llmobs._log import logger

_MASK_FAILED = "[llmobs: mask failed]"


def apply(value: object) -> object:
    """Pass ``value`` through the running ``mask``, when there is one (D-F-6).

    Without a runtime, or without a configured mask, the value is returned
    unchanged. A mask that raises fails closed: the value becomes the marker
    string and one debug line names the exception type, never the value.
    """
    # Deferred: _runtime imports the processor, which imports _context, which
    # imports this module.
    from llmobs._runtime import current

    runtime = current()
    if runtime is None:
        return value
    mask = runtime.config.mask
    if mask is None:
        return value
    try:
        masked: object = mask(value)
    except Exception as error:  # noqa: BLE001  # never raise into the app (spec 10 rule 4)
        logger.debug("llmobs: mask raised %s", type(error).__name__)
        return _MASK_FAILED
    return masked
