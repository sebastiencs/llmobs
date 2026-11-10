# Python SDK

The `llmobs` package (MIT) is the Python SDK. Spec: `docs/spec/10-sdk.md`.

What exists now is configuration resolution in `llmobs._config`. `init()` and tracing are not implemented yet.

Python ≥ 3.11 (D77). Develop on 3.11 (`.python-version`).

Checks: `just check-py` (ruff format, ruff, mypy, pytest).
