"""One Anthropic message with cache read and cache write, traced by OpenLLMetry."""

from __future__ import annotations

import os

from anthropic import Anthropic
from anthropic.types import TextBlockParam
from opentelemetry.instrumentation.anthropic import AnthropicInstrumentor

from capture_py.harness import finish, mock_url, setup

MODEL = "claude-sonnet-4-5"


def main() -> None:
    """Send a system prompt marked for caching and read the text answer."""
    provider = setup()
    AnthropicInstrumentor().instrument(tracer_provider=provider)
    client = Anthropic(
        base_url=mock_url(),
        api_key=os.environ["ANTHROPIC_API_KEY"],
        max_retries=0,
    )
    system: list[TextBlockParam] = [
        {
            "type": "text",
            "text": "You are a weather assistant.",
            "cache_control": {"type": "ephemeral"},
        }
    ]
    client.messages.create(
        model=MODEL,
        max_tokens=64,
        system=system,
        messages=[{"role": "user", "content": "What's the weather in Paris?"}],
    )
    finish(provider)


if __name__ == "__main__":
    main()
