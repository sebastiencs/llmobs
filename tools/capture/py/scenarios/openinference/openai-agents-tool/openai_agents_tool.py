"""OpenAI Agents SDK with one function tool, traced by OpenInference."""

from __future__ import annotations

import json
import os

from agents import Agent, Runner, function_tool, set_default_openai_client
from openai import AsyncOpenAI
from openinference.instrumentation.openai_agents import OpenAIAgentsInstrumentor

from capture_py.harness import finish, mock_url, setup

MODEL = "gpt-4o-mini-2024-07-18"


@function_tool
def get_weather(city: str) -> str:
    """Look up the weather for a city."""
    return json.dumps({"temperature_c": 18, "conditions": "sunny", "city": city})


def main() -> None:
    """Run the agent through one tool call and the answer."""
    provider = setup()
    OpenAIAgentsInstrumentor().instrument(tracer_provider=provider)
    set_default_openai_client(
        AsyncOpenAI(
            base_url=mock_url("/v1"),
            api_key=os.environ["OPENAI_API_KEY"],
            max_retries=0,
        ),
        use_for_tracing=False,
    )
    agent = Agent(
        name="weather",
        instructions="You are a weather assistant.",
        tools=[get_weather],
        model=MODEL,
    )
    Runner.run_sync(agent, "What's the weather in Paris?")
    finish(provider)


if __name__ == "__main__":
    main()
