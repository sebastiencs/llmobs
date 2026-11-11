"""An @observe call that runs an @observe tool and then the OpenAI drop-in."""

from __future__ import annotations

import importlib
import os

import openai
from langfuse import Langfuse, observe
from openai.types.chat import ChatCompletionMessageParam
from opentelemetry import trace
from opentelemetry.sdk.trace import TracerProvider

from capture_py.harness import finish, mock_url, receiver_base_url

MODEL = "gpt-4o-mini-2024-07-18"


def main() -> None:
    """Record the nested observation and one chat completion."""
    importlib.import_module("langfuse.openai")
    langfuse = _langfuse()
    try:
        answer_weather("Paris")
    finally:
        _finish(langfuse)


@observe(name="answer-weather")
def answer_weather(city: str) -> str:
    """Look up the weather for one city."""
    return lookup_weather(city)


@observe(name="lookup_weather", as_type="tool")
def lookup_weather(city: str) -> str:
    """Call the model for one city's weather."""
    client = openai.OpenAI(
        base_url=mock_url("/v1"),
        api_key=os.environ["OPENAI_API_KEY"],
        max_retries=0,
    )
    messages: list[ChatCompletionMessageParam] = [
        {"role": "user", "content": f"What's the weather in {city}?"}
    ]
    completion = client.chat.completions.create(model=MODEL, messages=messages)
    return completion.choices[0].message.content or ""


def _langfuse() -> Langfuse:
    return Langfuse(base_url=receiver_base_url(), otel_compression="none")


def _finish(langfuse: Langfuse) -> None:
    langfuse.flush()
    provider = trace.get_tracer_provider()
    if not isinstance(provider, TracerProvider):
        raise RuntimeError("langfuse tracer provider was not installed")
    finish(provider)
