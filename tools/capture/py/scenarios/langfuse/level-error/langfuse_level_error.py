"""An observation marked ERROR whose OTLP status stays unset."""

from __future__ import annotations

import os

from langfuse import Langfuse
from openai import OpenAI
from openai.types.chat import ChatCompletionMessageParam
from opentelemetry import trace
from opentelemetry.sdk.trace import TracerProvider

from capture_py.harness import finish, mock_url, receiver_base_url

MODEL = "gpt-4o-mini-2024-07-18"
STATUS_MESSAGE = "weather lookup failed"


def main() -> None:
    """Record level ERROR and a status message on a span with no OTLP error status."""
    langfuse = _langfuse()
    try:
        _lookup(langfuse)
    finally:
        _finish(langfuse)


def _lookup(langfuse: Langfuse) -> None:
    client = OpenAI(
        base_url=mock_url("/v1"),
        api_key=os.environ["OPENAI_API_KEY"],
        max_retries=0,
    )
    messages: list[ChatCompletionMessageParam] = [
        {"role": "user", "content": "What's the weather in Paris?"}
    ]
    with langfuse.start_as_current_observation(name="failed-lookup", as_type="span"):
        current = trace.get_current_span()
        current.set_attribute("langfuse.observation.level", "ERROR")
        current.set_attribute("langfuse.observation.status_message", STATUS_MESSAGE)
        client.chat.completions.create(model=MODEL, messages=messages)


def _langfuse() -> Langfuse:
    return Langfuse(base_url=receiver_base_url(), otel_compression="none")


def _finish(langfuse: Langfuse) -> None:
    langfuse.flush()
    provider = trace.get_tracer_provider()
    if not isinstance(provider, TracerProvider):
        raise RuntimeError("langfuse tracer provider was not installed")
    finish(provider)
