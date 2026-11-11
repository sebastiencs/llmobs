"""One streamed OpenAI chat answer, traced by the official OTel instrumentation."""

from __future__ import annotations

import os

from openai import OpenAI
from openai.types.chat import ChatCompletionMessageParam
from opentelemetry.instrumentation.openai_v2 import OpenAIInstrumentor

from capture_py.harness import finish, mock_url, setup

MODEL = "gpt-4o-mini-2024-07-18"


def main() -> None:
    """Stream one chat completion and read every chunk, including the usage chunk."""
    provider = setup()
    OpenAIInstrumentor().instrument(tracer_provider=provider)
    client = OpenAI(
        base_url=mock_url("/v1"),
        api_key=os.environ["OPENAI_API_KEY"],
        max_retries=0,
    )
    messages: list[ChatCompletionMessageParam] = [
        {"role": "user", "content": "Describe the harbor in a few words."},
    ]
    stream = client.chat.completions.create(
        model=MODEL,
        messages=messages,
        stream=True,
        stream_options={"include_usage": True},
    )
    for _chunk in stream:
        pass
    finish(provider)


if __name__ == "__main__":
    main()
