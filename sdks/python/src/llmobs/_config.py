"""SDK configuration resolution.

The rules are spec 10 § Configuration, items 1-11. ``init()`` will be the caller.
"""

import math
import os
from collections.abc import Callable, Mapping, Sequence
from dataclasses import dataclass, field
from typing import Any
from urllib.parse import urlsplit

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
    return Config(
        enabled=True,
        api_key=_resolve_api_key(api_key, source),
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


def _resolve_api_key(value: object, source: Mapping[str, str]) -> str:
    if value is not None:
        if not isinstance(value, str) or not value.startswith(_KEY_PREFIX):
            raise ConfigError(f"api_key: must start with {_KEY_PREFIX}")
        return value
    raw = _read_env(source, ENV_API_KEY)
    if raw is None:
        raise ConfigError("api_key: required")
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


def _check_base_url(value: object, name: str) -> str:
    if not isinstance(value, str):
        raise ConfigError(f"{name}: must be an absolute http or https URL")
    parsed = urlsplit(value)
    if parsed.scheme not in {"http", "https"} or parsed.netloc == "":
        raise ConfigError(f"{name}: must be an absolute http or https URL")
    if parsed.query != "" or parsed.fragment != "":
        raise ConfigError(f"{name}: must not include a query or fragment")
    return value.rstrip("/")


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
    try:
        number = float(raw)
    except ValueError:
        raise ConfigError(
            f"{ENV_SAMPLE_RATE}: must be a number between 0 and 1",
        ) from None
    return _check_unit(number, ENV_SAMPLE_RATE)


def _check_sample_rate(value: object, name: str) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ConfigError(f"{name}: must be a number between 0 and 1")
    return _check_unit(float(value), name)


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
