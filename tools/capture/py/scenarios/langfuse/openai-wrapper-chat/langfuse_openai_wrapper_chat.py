"""One chat completion through the Langfuse OpenAI drop-in."""

from __future__ import annotations

import importlib
import os

import openai
from langfuse import Langfuse, propagate_attributes
from openai.types.chat import ChatCompletionMessageParam
from opentelemetry import trace
from opentelemetry.sdk.trace import TracerProvider

from capture_py.harness import finish, mock_url, receiver_base_url

MODEL = "gpt-4o-mini-2024-07-18"


def main() -> None:
    """Record one chat with session, user, tags, and one metadata key."""
    importlib.import_module("langfuse.openai")
    langfuse = _langfuse()
    try:
        client = openai.OpenAI(
            base_url=mock_url("/v1"),
            api_key=os.environ["OPENAI_API_KEY"],
            max_retries=0,
        )
        messages: list[ChatCompletionMessageParam] = [
            {"role": "system", "content": "You are a weather assistant."},
            {"role": "user", "content": "What's the weather in Paris?"},
        ]
        with propagate_attributes(
            user_id="user-1",
            session_id="session-1",
            tags=["weather"],
            metadata={"feature": "weather"},
        ):
            client.chat.completions.create(model=MODEL, messages=messages)
    finally:
        _finish(langfuse)


def _langfuse() -> Langfuse:
    return Langfuse(base_url=receiver_base_url(), otel_compression="none")


def _finish(langfuse: Langfuse) -> None:
    langfuse.flush()
    provider = trace.get_tracer_provider()
    if not isinstance(provider, TracerProvider):
        raise RuntimeError("langfuse tracer provider was not installed")
    finish(provider)
