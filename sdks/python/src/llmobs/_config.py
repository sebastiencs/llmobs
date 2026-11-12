"""SDK configuration resolution.

The rules are spec 10 § Configuration, items 1-11. ``init()`` will be the caller.
"""

import math
import os
import re
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass, field
from typing import Any

from llmobs._log import warn_once

_ENV_PREFIX = "LLMOBS_"
ENV_API_KEY = f"{_ENV_PREFIX}API_KEY"
ENV_BASE_URL = f"{_ENV_PREFIX}BASE_URL"
ENV_ENVIRONMENT = f"{_ENV_PREFIX}ENVIRONMENT"
ENV_RELEASE = f"{_ENV_PREFIX}RELEASE"
ENV_CAPTURE_CONTENT = f"{_ENV_PREFIX}CAPTURE_CONTENT"
ENV_SAMPLE_RATE = f"{_ENV_PREFIX}SAMPLE_RATE"
ENV_ENABLED = f"{_ENV_PREFIX}ENABLED"
ENV_GIT_COMMIT = "GIT_COMMIT"
ENV_SOURCE_VERSION = "SOURCE_VERSION"

_KEY_PREFIX = "sk-llmobs-"
# Reserved evaluation environment. llmobs_core::names::environment::EVAL
_RESERVED_ENVIRONMENT = "llmobs-eval"
_DEFAULT_BASE_URL = "http://localhost:8080"
_TRUE = frozenset({"true", "1", "yes", "on"})
_FALSE = frozenset({"false", "0", "no", "off"})
# D-F-3: exact text, pinned by the tests and spec 10 § Configuration rule 11.
_MISSING_KEY_WARNING = (
    "llmobs: no API key; set LLMOBS_API_KEY, or LLMOBS_ENABLED=false to silence this; "
    "tracing is disabled"
)


class ConfigError(ValueError):
    """An SDK option is invalid. The message names the option, never its value."""


@dataclass(frozen=True, slots=True, kw_only=True)
class Config:
    """Resolved SDK configuration.

    Never log ``asdict(config)``; it contains the API key.
    """

    enabled: bool
    api_key: str | None = field(repr=False)
    base_url: str
    environment: str | None
    release: str | None
    capture_content: bool
    sample_rate: float
    instrument: tuple[str, ...]
    mask: Callable[[Any], Any] | None


def resolve_config(
    *,
    api_key: str | None = None,
    base_url: str | None = None,
    environment: str | None = None,
    release: str | None = None,
    capture_content: bool | None = None,
    sample_rate: float | int | None = None,
    instrument: Sequence[str] | None = None,
    mask: Callable[[Any], Any] | None = None,
    enabled: bool | None = None,
    env: Mapping[str, str] | None = None,
) -> Config:
    """Resolve SDK options. An explicit argument wins, then the env var, then the default."""

    source = os.environ if env is None else env
    resolved_enabled = _resolve_bool("enabled", ENV_ENABLED, enabled, source, default=True)
    if not resolved_enabled:
        return _disabled()
    api_key = _resolve_api_key(api_key, source)
    if api_key is None:
        # D-F-3: a missing key warns once and runs the SDK disabled; nothing else is
        # validated. A key with the wrong prefix still raises, in _resolve_api_key.
        warn_once("missing-api-key", _MISSING_KEY_WARNING)
        return _disabled()
    return Config(
        enabled=True,
        api_key=api_key,
        base_url=_resolve_base_url(base_url, source),
        environment=_resolve_environment(environment, source),
        release=_resolve_release(release, source),
        capture_content=_resolve_bool(
            "capture_content",
            ENV_CAPTURE_CONTENT,
            capture_content,
            source,
            default=True,
        ),
        sample_rate=_resolve_sample_rate(sample_rate, source),
        instrument=_resolve_instrument(instrument),
        mask=_resolve_mask(mask),
    )


def _disabled() -> Config:
    return Config(
        enabled=False,
        api_key=None,
        base_url=_DEFAULT_BASE_URL,
        environment=None,
        release=None,
        capture_content=True,
        sample_rate=1.0,
        instrument=(),
        mask=None,
    )


def _read_env(source: Mapping[str, str], name: str) -> str | None:
    raw = source.get(name)
    if raw is None:
        return None
    trimmed = raw.strip()
    if trimmed == "":
        return None
    return trimmed


def _resolve_bool(
    argument: str,
    env_name: str,
    value: object,
    source: Mapping[str, str],
    *,
    default: bool,
) -> bool:
    if value is not None:
        if not isinstance(value, bool):
            raise ConfigError(f"{argument}: must be a boolean")
        return value
    raw = _read_env(source, env_name)
    if raw is None:
        return default
    parsed = raw.lower()
    if parsed in _TRUE:
        return True
    if parsed in _FALSE:
        return False
    raise ConfigError(f"{env_name}: must be a boolean")


def _resolve_api_key(value: object, source: Mapping[str, str]) -> str | None:
    """Return the key, or ``None`` when it is missing (D-F-3: not an error)."""
    if value is not None:
        if not isinstance(value, str) or not value.startswith(_KEY_PREFIX):
            raise ConfigError(f"api_key: must start with {_KEY_PREFIX}")
        return value
    raw = _read_env(source, ENV_API_KEY)
    if raw is None:
        return None
    if not raw.startswith(_KEY_PREFIX):
        raise ConfigError(f"{ENV_API_KEY}: must start with {_KEY_PREFIX}")
    return raw


def _resolve_base_url(value: object, source: Mapping[str, str]) -> str:
    if value is not None:
        return _check_base_url(value, "base_url")
    raw = _read_env(source, ENV_BASE_URL)
    if raw is None:
        return _DEFAULT_BASE_URL
    return _check_base_url(raw, ENV_BASE_URL)


_BASE_URL = re.compile(r"^(https?)://([^/?#]+)", re.IGNORECASE)
_BRACKET_HOST = re.compile(r"^\[[0-9A-Fa-f:.]+\](?::[0-9]*)?$")
_SAMPLE_RATE_ENV = re.compile(r"^(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)$")


def _check_base_url(value: object, name: str) -> str:
    if not isinstance(value, str) or not _http_authority(value):
        raise ConfigError(f"{name}: must be an absolute http or https URL")
    hash_at = value.find("#")
    before_hash = value if hash_at == -1 else value[:hash_at]
    query_at = before_hash.find("?")
    query = "" if query_at == -1 else before_hash[query_at + 1 :]
    fragment = "" if hash_at == -1 else value[hash_at + 1 :]
    if query != "" or fragment != "":
        raise ConfigError(f"{name}: must not include a query or fragment")
    return value.rstrip("/")


def _http_authority(value: str) -> bool:
    match = _BASE_URL.match(value)
    if match is None:
        return False
    host = match.group(2).rsplit("@", 1)[-1]
    if host.startswith("["):
        return _BRACKET_HOST.fullmatch(host) is not None
    if "[" in host or "]" in host:
        return False
    return host.split(":", 1)[0] != ""


def _resolve_environment(value: object, source: Mapping[str, str]) -> str | None:
    if value is not None:
        return _check_environment(value, "environment")
    raw = _read_env(source, ENV_ENVIRONMENT)
    if raw is None:
        return None
    return _check_environment(raw, ENV_ENVIRONMENT)


def _check_environment(value: object, name: str) -> str:
    if not isinstance(value, str):
        raise ConfigError(f"{name}: must be a string")
    if value == _RESERVED_ENVIRONMENT:
        raise ConfigError(f"{name}: is reserved")
    return value


def _resolve_release(value: object, source: Mapping[str, str]) -> str | None:
    if value is not None:
        if not isinstance(value, str):
            raise ConfigError("release: must be a string")
        return value
    for name in (ENV_RELEASE, ENV_GIT_COMMIT, ENV_SOURCE_VERSION):
        raw = _read_env(source, name)
        if raw is not None:
            return raw
    return None


def _resolve_sample_rate(value: object, source: Mapping[str, str]) -> float:
    if value is not None:
        return _check_sample_rate(value, "sample_rate")
    raw = _read_env(source, ENV_SAMPLE_RATE)
    if raw is None:
        return 1.0
    if _SAMPLE_RATE_ENV.fullmatch(raw) is None:
        raise ConfigError(f"{ENV_SAMPLE_RATE}: must be a number between 0 and 1")
    try:
        number = float(raw)
    except (ValueError, OverflowError):
        raise ConfigError(
            f"{ENV_SAMPLE_RATE}: must be a number between 0 and 1",
        ) from None
    return _check_unit(number, ENV_SAMPLE_RATE)


def _check_sample_rate(value: object, name: str) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ConfigError(f"{name}: must be a number between 0 and 1")
    try:
        number = float(value)
    except OverflowError:
        raise ConfigError(f"{name}: must be a number between 0 and 1") from None
    return _check_unit(number, name)


def _check_unit(number: float, name: str) -> float:
    if not math.isfinite(number) or not 0 <= number <= 1:
        raise ConfigError(f"{name}: must be a number between 0 and 1")
    return number


def _resolve_instrument(value: object) -> tuple[str, ...]:
    if value is None:
        return ()
    if isinstance(value, (str, bytes)) or not isinstance(value, Sequence):
        raise ConfigError("instrument: must be a list of non-empty strings")
    items: list[str] = []
    for item in value:
        if not isinstance(item, str) or item == "":
            raise ConfigError("instrument: must be a list of non-empty strings")
        items.append(item)
    return tuple(items)


def _resolve_mask(value: object) -> Callable[[Any], Any] | None:
    if value is None:
        return None
    if not callable(value):
        raise ConfigError("mask: must be callable")
    return value
