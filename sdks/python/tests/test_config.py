import logging
import math
from collections.abc import Callable, Sequence

import pytest

from llmobs._config import (
    ENV_API_KEY,
    ENV_BASE_URL,
    ENV_CAPTURE_CONTENT,
    ENV_ENABLED,
    ENV_ENVIRONMENT,
    ENV_GIT_COMMIT,
    ENV_RELEASE,
    ENV_SAMPLE_RATE,
    ENV_SOURCE_VERSION,
    Config,
    ConfigError,
    resolve_config,
)
from llmobs._log import reset_warnings

KEY = "sk-llmobs-test"
STORED_KEY = "sk-llmobs-super-secret-key-42"
BAD_KEY = "not-a-key-DISTINCTIVE"
MISSING_KEY_MESSAGE = (
    "llmobs: no API key; set LLMOBS_API_KEY, or LLMOBS_ENABLED=false to silence this; "
    "tracing is disabled"
)


@pytest.fixture(autouse=True)
def _fresh_warnings() -> None:
    reset_warnings()


def test_defaults() -> None:
    assert resolve_config(api_key=KEY, env={}) == Config(
        enabled=True,
        api_key=KEY,
        base_url="http://localhost:8080",
        environment=None,
        release=None,
        capture_content=True,
        sample_rate=1.0,
        instrument=(),
        mask=None,
        use_global_provider=False,
        export_filter=None,
    )


def _arg_base() -> Config:
    return resolve_config(
        api_key=KEY,
        base_url="http://arg.example",
        env={ENV_BASE_URL: "http://env.example"},
    )


def _env_base() -> Config:
    return resolve_config(api_key=KEY, env={ENV_BASE_URL: "http://env.example"})


def _default_base() -> Config:
    return resolve_config(api_key=KEY, env={})


def _arg_environment() -> Config:
    return resolve_config(api_key=KEY, environment="prod", env={ENV_ENVIRONMENT: "staging"})


def _env_environment() -> Config:
    return resolve_config(api_key=KEY, env={ENV_ENVIRONMENT: "staging"})


def _default_environment() -> Config:
    return resolve_config(api_key=KEY, env={})


def _arg_release() -> Config:
    return resolve_config(
        api_key=KEY,
        release="arg",
        env={ENV_RELEASE: "env", ENV_GIT_COMMIT: "git", ENV_SOURCE_VERSION: "source"},
    )


def _env_release() -> Config:
    return resolve_config(
        api_key=KEY,
        env={ENV_RELEASE: "env", ENV_GIT_COMMIT: "git", ENV_SOURCE_VERSION: "source"},
    )


def _default_release() -> Config:
    return resolve_config(api_key=KEY, env={})


def _arg_capture() -> Config:
    return resolve_config(api_key=KEY, capture_content=False, env={ENV_CAPTURE_CONTENT: "true"})


def _env_capture() -> Config:
    return resolve_config(api_key=KEY, env={ENV_CAPTURE_CONTENT: "false"})


def _default_capture() -> Config:
    return resolve_config(api_key=KEY, env={})


def _arg_rate() -> Config:
    return resolve_config(api_key=KEY, sample_rate=0.25, env={ENV_SAMPLE_RATE: "0.5"})


def _env_rate() -> Config:
    return resolve_config(api_key=KEY, env={ENV_SAMPLE_RATE: "0.5"})


def _default_rate() -> Config:
    return resolve_config(api_key=KEY, env={})


def _arg_enabled() -> Config:
    return resolve_config(api_key=KEY, enabled=False, env={ENV_ENABLED: "true"})


def _env_enabled() -> Config:
    return resolve_config(api_key=KEY, env={ENV_ENABLED: "false"})


def _default_enabled() -> Config:
    return resolve_config(api_key=KEY, env={})


def _arg_key() -> Config:
    return resolve_config(api_key=STORED_KEY, env={ENV_API_KEY: KEY})


def _env_key() -> Config:
    return resolve_config(env={ENV_API_KEY: KEY})


@pytest.mark.parametrize(
    ("resolve", "field", "expected"),
    [
        (_arg_base, "base_url", "http://arg.example"),
        (_env_base, "base_url", "http://env.example"),
        (_default_base, "base_url", "http://localhost:8080"),
        (_arg_environment, "environment", "prod"),
        (_env_environment, "environment", "staging"),
        (_default_environment, "environment", None),
        (_arg_release, "release", "arg"),
        (_env_release, "release", "env"),
        (_default_release, "release", None),
        (_arg_capture, "capture_content", False),
        (_env_capture, "capture_content", False),
        (_default_capture, "capture_content", True),
        (_arg_rate, "sample_rate", 0.25),
        (_env_rate, "sample_rate", 0.5),
        (_default_rate, "sample_rate", 1.0),
        (_arg_enabled, "enabled", False),
        (_env_enabled, "enabled", False),
        (_default_enabled, "enabled", True),
        (_arg_key, "api_key", STORED_KEY),
        (_env_key, "api_key", KEY),
    ],
)
def test_argument_beats_env_beats_default(
    resolve: Callable[[], Config],
    field: str,
    expected: object,
) -> None:
    assert getattr(resolve(), field) == expected


@pytest.mark.parametrize("blank", ["", "   ", "\n\t"])
def test_blank_env_values_are_unset(blank: str) -> None:
    config = resolve_config(
        api_key=KEY,
        env={ENV_BASE_URL: blank, ENV_ENVIRONMENT: blank, ENV_SAMPLE_RATE: blank},
    )
    assert config.base_url == "http://localhost:8080"
    assert config.environment is None
    assert config.sample_rate == 1.0


def test_disabled_env_skips_invalid_options() -> None:
    config = resolve_config(
        env={
            ENV_ENABLED: "false",
            ENV_SAMPLE_RATE: "nope",
            ENV_GIT_COMMIT: "abc",
        },
    )
    assert config == Config(
        enabled=False,
        api_key=None,
        base_url="http://localhost:8080",
        environment=None,
        release=None,
        capture_content=True,
        sample_rate=1.0,
        instrument=(),
        mask=None,
        use_global_provider=False,
        export_filter=None,
    )


def test_disabled_argument_overrides_enabled_env() -> None:
    config = resolve_config(
        enabled=False,
        env={ENV_ENABLED: "true", ENV_SAMPLE_RATE: "nope", ENV_GIT_COMMIT: "abc"},
    )
    assert config.enabled is False
    assert config.api_key is None
    assert config.release is None
    assert config.sample_rate == 1.0


def test_invalid_enabled_stops_before_the_api_key() -> None:
    with pytest.raises(ConfigError, match=f"{ENV_ENABLED}: must be a boolean"):
        resolve_config(env={ENV_ENABLED: "maybe"})


@pytest.mark.parametrize(
    ("text", "expected"),
    [
        ("true", True),
        ("TRUE", True),
        ("false", False),
        ("FALSE", False),
        ("1", True),
        ("0", False),
        ("yes", True),
        ("YES", True),
        ("no", False),
        ("NO", False),
        ("on", True),
        ("ON", True),
        ("off", False),
        ("OFF", False),
    ],
)
def test_boolean_spellings(text: str, expected: bool) -> None:
    config = resolve_config(api_key=KEY, env={ENV_CAPTURE_CONTENT: text})
    assert config.capture_content is expected


def test_boolean_env_rejects_unknown_text() -> None:
    with pytest.raises(ConfigError, match=f"{ENV_CAPTURE_CONTENT}: must be a boolean"):
        resolve_config(api_key=KEY, env={ENV_CAPTURE_CONTENT: "maybe"})


def test_boolean_arguments_must_be_bools() -> None:
    with pytest.raises(ConfigError, match="capture_content: must be a boolean"):
        resolve_config(api_key=KEY, capture_content=1, env={})  # type: ignore[arg-type]


def test_api_key_errors_do_not_echo_the_value() -> None:
    config = resolve_config(api_key=STORED_KEY, env={})
    assert STORED_KEY not in repr(config)
    assert STORED_KEY not in str(config)

    with pytest.raises(ConfigError, match="api_key: must start with sk-llmobs-") as argument_error:
        resolve_config(api_key=BAD_KEY, env={})
    assert BAD_KEY not in str(argument_error.value)
    with pytest.raises(
        ConfigError, match=f"{ENV_API_KEY}: must start with sk-llmobs-"
    ) as env_error:
        resolve_config(env={ENV_API_KEY: BAD_KEY})
    assert BAD_KEY not in str(env_error.value)


@pytest.mark.parametrize("source", [{}, {ENV_API_KEY: "  "}])
def test_missing_api_key_warns_once_and_disables(
    source: dict[str, str], caplog: pytest.LogCaptureFixture
) -> None:
    with caplog.at_level(logging.WARNING, logger="llmobs"):
        config = resolve_config(env=source)
        again = resolve_config(env=source)
    assert config.enabled is False
    assert again.enabled is False
    assert config == Config(
        enabled=False,
        api_key=None,
        base_url="http://localhost:8080",
        environment=None,
        release=None,
        capture_content=True,
        sample_rate=1.0,
        instrument=(),
        mask=None,
        use_global_provider=False,
        export_filter=None,
    )
    warnings = [record for record in caplog.records if record.name == "llmobs"]
    assert len(warnings) == 1
    assert warnings[0].levelno == logging.WARNING
    assert warnings[0].getMessage() == MISSING_KEY_MESSAGE


def test_missing_api_key_skips_other_validation() -> None:
    config = resolve_config(env={ENV_BASE_URL: "ftp://x", ENV_SAMPLE_RATE: "nope"})
    assert config.enabled is False


def test_base_url_strips_trailing_slashes() -> None:
    config = resolve_config(api_key=KEY, base_url="https://host/prefix/", env={})
    assert config.base_url == "https://host/prefix"


@pytest.mark.parametrize(
    ("url", "expected"),
    [
        ("http://[::1]:8080", "http://[::1]:8080"),
        ("HTTPS://X.example/", "HTTPS://X.example"),
    ],
)
def test_base_url_accepts_bracketed_hosts_and_keeps_scheme_case(url: str, expected: str) -> None:
    assert resolve_config(api_key=KEY, base_url=url, env={}).base_url == expected


@pytest.mark.parametrize(
    "url",
    ["http://[::1", "http://::1]", "http://[zz]", "http://a]b", "http://", "http://:8080"],
)
def test_base_url_rejects_a_bad_host(url: str) -> None:
    with pytest.raises(ConfigError, match="must be an absolute http or https URL") as caught:
        resolve_config(api_key=KEY, base_url=url, env={})
    assert url not in str(caught.value)


def test_base_url_unclosed_ipv6_is_a_config_error() -> None:
    with pytest.raises(ConfigError, match="base_url: must be an absolute http or https URL"):
        resolve_config(api_key=KEY, base_url="http://[::1", env={})


def test_base_url_allows_an_empty_query_or_fragment() -> None:
    query = resolve_config(api_key=KEY, base_url="http://host/?", env={})
    fragment = resolve_config(api_key=KEY, base_url="http://host/#", env={})
    assert query.base_url == "http://host/?"
    assert fragment.base_url == "http://host/#"


@pytest.mark.parametrize(
    "url",
    ["ftp://example.com/traces", "localhost:8080", "http://h/?q=1", "http://h/#f"],
)
def test_base_url_rejects_bad_urls(url: str) -> None:
    with pytest.raises(ConfigError, match="base_url:") as caught:
        resolve_config(api_key=KEY, base_url=url, env={})
    assert url not in str(caught.value)


@pytest.mark.parametrize("rate", [0, 1, 0.25])
def test_sample_rate_accepts_a_unit_interval(rate: float) -> None:
    assert resolve_config(api_key=KEY, sample_rate=rate, env={}).sample_rate == float(rate)


@pytest.mark.parametrize("rate", [-0.1, 1.5, math.nan, math.inf, True])
def test_sample_rate_rejects_bad_numbers(rate: float) -> None:
    with pytest.raises(ConfigError, match="sample_rate: must be a number between 0 and 1"):
        resolve_config(api_key=KEY, sample_rate=rate, env={})


@pytest.mark.parametrize("raw", ["1", "0", "0.25", ".5", "1.", "0.0", " 0.5 "])
def test_sample_rate_env_accepts_a_plain_decimal(raw: str) -> None:
    assert resolve_config(api_key=KEY, env={ENV_SAMPLE_RATE: raw}).sample_rate == float(raw)


@pytest.mark.parametrize(
    "raw",
    ["+0.5", "-0", "5e-1", "0x1", "0b1", "0o1", "0.2_5", "\u0661", "Infinity", "NaN", "1.5"],
)
def test_sample_rate_env_rejects_other_spellings(raw: str) -> None:
    with pytest.raises(ConfigError, match=f"{ENV_SAMPLE_RATE}: must be a number between 0 and 1"):
        resolve_config(api_key=KEY, env={ENV_SAMPLE_RATE: raw})


def test_sample_rate_rejects_an_overflowing_integer() -> None:
    with pytest.raises(ConfigError, match="sample_rate: must be a number between 0 and 1"):
        resolve_config(api_key=KEY, sample_rate=10**400, env={})


def test_sample_rate_rejects_a_bad_env_value() -> None:
    with pytest.raises(ConfigError, match=f"{ENV_SAMPLE_RATE}: must be a number between 0 and 1"):
        resolve_config(api_key=KEY, env={ENV_SAMPLE_RATE: "abc"})


def test_environment_rejects_the_reserved_value() -> None:
    with pytest.raises(ConfigError, match="environment: is reserved") as argument_error:
        resolve_config(api_key=KEY, environment="llmobs-eval", env={})
    assert "llmobs-eval" not in str(argument_error.value)
    with pytest.raises(ConfigError, match=f"{ENV_ENVIRONMENT}: is reserved") as env_error:
        resolve_config(api_key=KEY, env={ENV_ENVIRONMENT: "llmobs-eval"})
    assert "llmobs-eval" not in str(env_error.value)


def test_release_falls_back_to_git_then_source() -> None:
    assert resolve_config(api_key=KEY, env={ENV_GIT_COMMIT: "abc"}).release == "abc"
    assert resolve_config(api_key=KEY, env={ENV_SOURCE_VERSION: "source"}).release == "source"
    both = resolve_config(api_key=KEY, env={ENV_GIT_COMMIT: "abc", ENV_SOURCE_VERSION: "source"})
    assert both.release == "abc"
    named = resolve_config(
        api_key=KEY,
        env={ENV_RELEASE: "named", ENV_GIT_COMMIT: "abc", ENV_SOURCE_VERSION: "source"},
    )
    assert named.release == "named"
    argument = resolve_config(
        api_key=KEY,
        release="argument",
        env={ENV_RELEASE: "named", ENV_GIT_COMMIT: "abc"},
    )
    assert argument.release == "argument"


def test_instrument_is_a_tuple_of_names() -> None:
    config = resolve_config(api_key=KEY, instrument=["openai"], env={})
    assert config.instrument == ("openai",)


def test_instrument_rejects_a_bare_string_and_empty_items() -> None:
    with pytest.raises(ConfigError, match="instrument: must be a list of non-empty strings"):
        resolve_config(api_key=KEY, instrument="openai", env={})
    with pytest.raises(ConfigError, match="instrument: must be a list of non-empty strings"):
        resolve_config(api_key=KEY, instrument=b"openai", env={})  # type: ignore[arg-type]
    with pytest.raises(ConfigError, match="instrument: must be a list of non-empty strings"):
        resolve_config(api_key=KEY, instrument=["", "x"], env={})


def test_instrument_rejects_non_strings() -> None:
    values: Sequence[object] = ["openai", 1]
    with pytest.raises(ConfigError, match="instrument: must be a list of non-empty strings"):
        resolve_config(api_key=KEY, instrument=values, env={})  # type: ignore[arg-type]


def test_mask_must_be_callable() -> None:
    def mask(value: object) -> object:
        return value

    config = resolve_config(api_key=KEY, mask=mask, env={})
    assert config.mask is mask
    with pytest.raises(ConfigError, match="mask: must be callable"):
        resolve_config(api_key=KEY, mask=1, env={})  # type: ignore[arg-type]


def test_use_global_provider_defaults_to_false() -> None:
    config = resolve_config(api_key=KEY, env={})
    assert config.use_global_provider is False


def test_use_global_provider_accepts_true() -> None:
    config = resolve_config(api_key=KEY, use_global_provider=True, env={})
    assert config.use_global_provider is True


@pytest.mark.parametrize("value", ["yes", 1, "0"])
def test_use_global_provider_rejects_non_bools(value: object) -> None:
    with pytest.raises(ConfigError, match="use_global_provider: must be a boolean") as caught:
        resolve_config(api_key=KEY, use_global_provider=value, env={})  # type: ignore[arg-type]
    assert str(value) not in str(caught.value)


def test_export_filter_defaults_to_none() -> None:
    config = resolve_config(api_key=KEY, env={})
    assert config.export_filter is None


def test_export_filter_accepts_a_callable() -> None:
    def keep(span: object) -> bool:
        return True

    config = resolve_config(api_key=KEY, export_filter=keep, env={})
    assert config.export_filter is keep


def test_export_filter_rejects_non_callables() -> None:
    with pytest.raises(ConfigError, match="export_filter: must be callable") as caught:
        resolve_config(api_key=KEY, export_filter=1, env={})  # type: ignore[arg-type]
    assert "1" not in str(caught.value)


def test_disabled_ignores_the_new_options() -> None:
    config = resolve_config(
        use_global_provider="yes",  # type: ignore[arg-type]
        export_filter=1,  # type: ignore[arg-type]
        env={ENV_ENABLED: "false"},
    )
    assert config.enabled is False
    assert config.use_global_provider is False
    assert config.export_filter is None


def test_env_none_reads_os_environ(monkeypatch: pytest.MonkeyPatch) -> None:
    for name in (
        ENV_API_KEY,
        ENV_BASE_URL,
        ENV_ENVIRONMENT,
        ENV_RELEASE,
        ENV_CAPTURE_CONTENT,
        ENV_SAMPLE_RATE,
        ENV_ENABLED,
        ENV_GIT_COMMIT,
        ENV_SOURCE_VERSION,
    ):
        monkeypatch.delenv(name, raising=False)
    monkeypatch.setenv(ENV_API_KEY, KEY)
    assert resolve_config().api_key == KEY
