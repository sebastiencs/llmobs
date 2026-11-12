"""One streamed OpenAI chat completion auto-instrumented by Laminar."""

from __future__ import annotations

import os
from urllib.parse import urlparse

from lmnr import Instruments, Laminar
from openai import OpenAI
from openai.types.chat import ChatCompletionMessageParam
from opentelemetry import trace
from opentelemetry.sdk.trace import TracerProvider

from capture_py.harness import finish, mock_url, prefer_identity_otlp_compression, receiver_base_url

MODEL = "gpt-4o-mini-2024-07-18"


def main() -> None:
    """Stream one chat completion and read every chunk, including the usage chunk."""
    prefer_identity_otlp_compression()
    _start()
    try:
        client = OpenAI(
            base_url=mock_url("/v1"),
            api_key=os.environ["OPENAI_API_KEY"],
            max_retries=0,
        )
        messages: list[ChatCompletionMessageParam] = [
            {"role": "user", "content": "What's the weather in Paris?"},
        ]
        stream = client.chat.completions.create(
            model=MODEL,
            messages=messages,
            stream=True,
            stream_options={"include_usage": True},
        )
        for _chunk in stream:
            pass
    finally:
        _finish()


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
