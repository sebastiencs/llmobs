"""One streamed Anthropic message with text and a tool call, traced by OpenLLMetry."""

from __future__ import annotations

import os

from anthropic import Anthropic
from anthropic.types import ToolParam
from opentelemetry.instrumentation.anthropic import AnthropicInstrumentor

from capture_py.harness import finish, mock_url, setup

MODEL = "claude-sonnet-4-5"


def main() -> None:
    """Stream one message and read every event."""
    provider = setup()
    AnthropicInstrumentor().instrument(tracer_provider=provider)
    client = Anthropic(
        base_url=mock_url(),
        api_key=os.environ["ANTHROPIC_API_KEY"],
        max_retries=0,
    )
    stream = client.messages.create(
        model=MODEL,
        max_tokens=64,
        stream=True,
        tools=[_weather_tool()],
        messages=[{"role": "user", "content": "What's the weather in Paris?"}],
    )
    for _event in stream:
        pass
    finish(provider)


def _weather_tool() -> ToolParam:
    return {
        "name": "get_weather",
        "description": "Look up the weather for a city.",
        "input_schema": {
            "type": "object",
            "properties": {"city": {"type": "string"}},
            "required": ["city"],
        },
    }


if __name__ == "__main__":
    main()
