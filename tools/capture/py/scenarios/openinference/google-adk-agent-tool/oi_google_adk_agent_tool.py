"""The agent-tool run with OpenInference's ADK instrumentation enabled."""

from __future__ import annotations

import asyncio
import itertools

from google.adk.agents import Agent
from google.adk.models import Gemini
from google.adk.platform.time import set_time_provider
from google.adk.platform.uuid import set_id_provider
from google.adk.runners import InMemoryRunner
from google.genai import types
from openinference.instrumentation.google_adk import GoogleADKInstrumentor
from opentelemetry.sdk.trace import TracerProvider

from capture_py.harness import finish, mock_url, setup

MODEL = "gemini-2.5-flash"
APP_NAME = "capture-app"


def get_weather(city: str) -> dict[str, str]:
    """Return the weather for a city."""
    return {"city": city, "report": "It is 18 °C and sunny."}


def _pin_ids() -> None:
    counter = itertools.count(1)

    def provider() -> str:
        return f"capture-{next(counter)}"

    set_id_provider(provider)


def _pin_time() -> None:
    ticks = itertools.count(0)

    def provider() -> float:
        return 1_767_225_590.0 + next(ticks) * 0.001

    set_time_provider(provider)


def _agent() -> Agent:
    return Agent(
        name="weather_agent",
        model=Gemini(model=MODEL, base_url=mock_url()),
        instruction="Use get_weather for weather questions.",
        tools=[get_weather],
    )


async def _run(provider: TracerProvider) -> None:
    runner = InMemoryRunner(agent=_agent(), app_name=APP_NAME)
    await runner.session_service.create_session(
        app_name=APP_NAME,
        user_id="user-1",
        session_id="session-1",
    )
    message = types.Content(
        role="user",
        parts=[types.Part(text="What's the weather in Paris?")],
    )
    async for _event in runner.run_async(
        user_id="user-1",
        session_id="session-1",
        invocation_id="invocation-1",
        new_message=message,
    ):
        pass
    await runner.close()
    finish(provider)


def main() -> None:
    """Run the agent once under the OpenInference instrumentor."""
    _pin_ids()
    _pin_time()
    provider = setup()
    GoogleADKInstrumentor().instrument(tracer_provider=provider)
    asyncio.run(_run(provider))


if __name__ == "__main__":
    main()
