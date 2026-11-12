"""One Anthropic message with a thinking block, traced by OpenInference."""

from __future__ import annotations

import os

from anthropic import Anthropic
from anthropic.types import ThinkingConfigEnabledParam
from openinference.instrumentation.anthropic import AnthropicInstrumentor

from capture_py.harness import finish, mock_url, setup

MODEL = "claude-sonnet-4-5"


def main() -> None:
    """Request thinking and read the message."""
    provider = setup()
    AnthropicInstrumentor().instrument(tracer_provider=provider)
    client = Anthropic(
        base_url=mock_url(),
        api_key=os.environ["ANTHROPIC_API_KEY"],
        max_retries=0,
    )
    thinking: ThinkingConfigEnabledParam = {"type": "enabled", "budget_tokens": 32}
    client.messages.create(
        model=MODEL,
        max_tokens=64,
        thinking=thinking,
        messages=[{"role": "user", "content": "Describe the harbor in a few words."}],
    )
    finish(provider)


if __name__ == "__main__":
    main()
