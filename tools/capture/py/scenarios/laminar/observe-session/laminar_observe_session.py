"""An @observe function with session, user, metadata, and tags around one OpenAI call."""

from __future__ import annotations

import os
from urllib.parse import urlparse

from lmnr import Instruments, Laminar, observe
from openai import OpenAI
from openai.types.chat import ChatCompletionMessageParam
from opentelemetry import trace
from opentelemetry.sdk.trace import TracerProvider

from capture_py.harness import finish, mock_url, prefer_identity_otlp_compression, receiver_base_url

MODEL = "gpt-4o-mini-2024-07-18"


def main() -> None:
    """Record the observed function and one chat completion."""
    prefer_identity_otlp_compression()
    _start()
    try:
        answer_weather("Paris")
    finally:
        _finish()


@observe(
    name="answer-weather",
    session_id="session-1",
    user_id="user-1",
    metadata={"feature": "weather"},
    tags=["weather"],
)
def answer_weather(city: str) -> str:
    """Look up the weather for one city."""
    client = OpenAI(
        base_url=mock_url("/v1"),
        api_key=os.environ["OPENAI_API_KEY"],
        max_retries=0,
    )
    messages: list[ChatCompletionMessageParam] = [
        {"role": "user", "content": f"What's the weather in {city}?"},
    ]
    completion = client.chat.completions.create(model=MODEL, messages=messages)
    return completion.choices[0].message.content or ""


def _start() -> None:
    host, port = _receiver()
    Laminar.initialize(
        base_url=host,
        http_port=port,
        force_http=True,
        disable_batch=True,
        instruments={Instruments.OPENAI},
        release="capture",
    )


def _receiver() -> tuple[str, int]:
    parsed = urlparse(receiver_base_url())
    if parsed.hostname is None or parsed.port is None:
        raise RuntimeError("receiver base url is missing a host or port")
    return f"{parsed.scheme}://{parsed.hostname}", parsed.port


def _finish() -> None:
    provider = trace.get_tracer_provider()
    if not isinstance(provider, TracerProvider):
        raise RuntimeError("laminar tracer provider was not installed")
    finish(provider)
