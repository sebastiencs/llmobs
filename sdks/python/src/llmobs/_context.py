"""``context()``: session, user, tags and metadata in the OTel context (D-F-12).

The values live under one OTel context key, so they follow ``contextvars``
and ``copy_context()``; the span processor stamps them on every span of our
provider. ``propagate=True`` also writes the session and user ids to W3C
baggage, so they cross service boundaries; tags and metadata never do.
"""

from collections.abc import Mapping, Sequence
from contextvars import Token
from dataclasses import dataclass, field
from types import MappingProxyType
from typing import Final

from opentelemetry.baggage import set_baggage
from opentelemetry.context import (
    Context,
    attach,
    create_key,
    detach,
    get_value,
    set_value,
)

from llmobs._log import logger
from llmobs._mask import apply

CONTEXT_KEY: Final = create_key("llmobs.context")
_BAGGAGE_SESSION_KEY: Final = "llmobs.session.id"
_BAGGAGE_USER_KEY: Final = "llmobs.user.id"

_REENTERED_MESSAGE = "llmobs: context re-entered; this entry is a no-op"


@dataclass(frozen=True, slots=True)
class ContextValues:
    """The context values the processor stamps on spans (D-F-12)."""

    session_id: str | None = None
    user_id: str | None = None
    tags: tuple[str, ...] = ()
    metadata: Mapping[str, object] = field(
        default_factory=dict
    )  # an immutable copy (MappingProxyType)


def current_values(ctx: Context | None = None) -> ContextValues:
    """The values stored under ``CONTEXT_KEY`` in ``ctx``, or empty values.

    Anything other than a ``ContextValues`` under the key (empty or garbage)
    reads as empty values, so callers never see a foreign object.
    """
    values = get_value(CONTEXT_KEY, ctx)
    if isinstance(values, ContextValues):
        return values
    return ContextValues()


def _valid_id(name: str, value: object) -> str | None:
    """A string id, or ``None`` when absent or not a string (debug log)."""
    if value is None:
        return None
    if isinstance(value, str):
        return value
    logger.debug("llmobs: context %s ignored: not a string", name)
    return None


def _valid_tags(tags: object) -> tuple[str, ...]:
    """Tags as a tuple of strings; a bare string is one tag, other items are skipped."""
    if tags is None:
        return ()
    if isinstance(tags, str):
        return (tags,)
    if isinstance(tags, Sequence):
        return tuple(item for item in tags if isinstance(item, str))
    logger.debug("llmobs: context tags ignored: not a sequence of strings")
    return ()


def _valid_metadata(metadata: object) -> dict[str, object]:
    """The metadata entries with string keys; every other key is skipped."""
    if metadata is None:
        return {}
    if not isinstance(metadata, Mapping):
        logger.debug("llmobs: context metadata ignored: not a mapping")
        return {}
    return {key: value for key, value in metadata.items() if isinstance(key, str)}


class context:  # noqa: N801  # public API name from spec 10
    """Context manager applying session/user/tags/metadata to every span inside.

    Nested blocks merge (D-F-12): the inner session and user win when given,
    tags are the union (outer first, no duplicates), metadata merges by key
    with the inner value winning. Metadata values are masked when the block
    is entered. Re-entering an already entered ``context`` object is a no-op
    logged at debug level.
    """

    def __init__(
        self,
        *,
        session_id: str | None = None,
        user_id: str | None = None,
        tags: Sequence[str] | None = None,
        metadata: Mapping[str, object] | None = None,
        propagate: bool = False,
    ) -> None:
        """Validate the arguments without raising; invalid values are dropped."""
        self._propagate = propagate
        self._depth = 0
        self._token: Token[Context] | None = None
        try:
            self._session_id = _valid_id("session_id", session_id)
            self._user_id = _valid_id("user_id", user_id)
            self._tags = _valid_tags(tags)
            self._metadata = _valid_metadata(metadata)
        except Exception as error:  # noqa: BLE001  # never raise into the app (spec 10 rule 4)
            logger.debug("llmobs: context() arguments ignored (%s)", type(error).__name__)
            self._session_id = None
            self._user_id = None
            self._tags = ()
            self._metadata = {}

    def _enter(self) -> None:
        """Merge with the current values, mask metadata, attach (D-F-12)."""
        outer = current_values()
        inner_metadata = {key: apply(value) for key, value in self._metadata.items()}
        merged = ContextValues(
            session_id=self._session_id if self._session_id is not None else outer.session_id,
            user_id=self._user_id if self._user_id is not None else outer.user_id,
            tags=tuple(dict.fromkeys(outer.tags + self._tags)),
            metadata=MappingProxyType({**outer.metadata, **inner_metadata}),
        )
        ctx = set_value(CONTEXT_KEY, merged)
        if self._propagate:
            if merged.session_id is not None:
                ctx = set_baggage(_BAGGAGE_SESSION_KEY, merged.session_id, context=ctx)
            if merged.user_id is not None:
                ctx = set_baggage(_BAGGAGE_USER_KEY, merged.user_id, context=ctx)
        self._token = attach(ctx)

    def _exit(self) -> None:
        """Detach the context attached by the matching entry, when there is one."""
        if self._depth == 0:
            return
        self._depth -= 1
        if self._depth > 0:
            return
        token, self._token = self._token, None
        if token is None:
            return
        try:
            detach(token)
        except Exception as error:  # noqa: BLE001  # never raise into the app (spec 10 rule 4)
            logger.debug("llmobs: context exit failed (%s)", type(error).__name__)

    def __enter__(self) -> None:
        """Enter the block; a re-entry of this object is a logged no-op."""
        self._depth += 1
        if self._depth > 1:
            logger.debug(_REENTERED_MESSAGE)
            return
        try:
            self._enter()
        except Exception as error:  # noqa: BLE001  # never raise into the app (spec 10 rule 4)
            logger.debug("llmobs: context entry failed (%s)", type(error).__name__)
            self._depth = 0
            self._token = None

    def __exit__(self, *exc: object) -> None:
        """Exit the block and restore the outer context."""
        self._exit()

    async def __aenter__(self) -> None:
        """Async variant of ``__enter__``."""
        self.__enter__()

    async def __aexit__(self, *exc: object) -> None:
        """Async variant of ``__exit__``."""
        self._exit()
