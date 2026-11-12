"""One ADK answer that includes a reasoning part and text."""

from __future__ import annotations

import asyncio
import itertools

from google.adk.agents import Agent
from google.adk.models import Gemini
from google.adk.platform.uuid import set_id_provider
from google.adk.runners import InMemoryRunner
from google.genai import types
from opentelemetry.sdk.trace import TracerProvider

from capture_py.harness import finish, mock_url, setup

MODEL = "gemini-2.5-flash"
APP_NAME = "capture-app"


def _pin_ids() -> None:
    counter = itertools.count(1)

    def provider() -> str:
        return f"capture-{next(counter)}"

    set_id_provider(provider)


def _agent() -> Agent:
    return Agent(
        name="weather_agent",
        model=Gemini(model=MODEL, base_url=mock_url()),
        instruction="Answer weather questions in one short sentence.",
        generate_content_config=types.GenerateContentConfig(
            thinking_config=types.ThinkingConfig(include_thoughts=True, thinking_budget=64),
        ),
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
    """Run one thinking answer and export its spans."""
    _pin_ids()
    provider = setup()
    asyncio.run(_run(provider))


if __name__ == "__main__":
    main()
