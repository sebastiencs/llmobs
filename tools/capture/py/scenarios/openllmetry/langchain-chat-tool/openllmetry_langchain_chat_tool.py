"""LangChain chat model with one tool, traced by OpenLLMetry."""

from __future__ import annotations

import json
import os

from langchain.tools import tool
from langchain_core.messages import AIMessage, HumanMessage, SystemMessage, ToolMessage
from langchain_openai import ChatOpenAI
from opentelemetry.instrumentation.langchain import LangchainInstrumentor
from pydantic import SecretStr

from capture_py.harness import finish, mock_url, setup

MODEL = "gpt-4o-mini-2024-07-18"


def _pin_framework_ids() -> None:
    """Replace LangChain's clock-based run ids with a counter."""
    import uuid

    from langchain_core.callbacks import manager as callback_manager
    from langchain_core.tracers import event_stream
    from langchain_core.utils import uuid as lc_uuid

    counter = 0

    def next_id() -> uuid.UUID:
        nonlocal counter
        counter += 1
        return uuid.UUID(int=counter)

    def uuid7(nanoseconds: int | None = None) -> uuid.UUID:
        del nanoseconds
        return next_id()

    lc_uuid.uuid7 = uuid7
    callback_manager.uuid7 = uuid7  # type: ignore[attr-defined]
    event_stream.uuid7 = uuid7  # type: ignore[attr-defined]


@tool
def get_weather(city: str) -> str:
    """Look up the weather for a city."""
    return json.dumps({"temperature_c": 18, "conditions": "sunny", "city": city})


def main() -> None:
    """Run a tool call, the tool, then the answer."""
    _pin_framework_ids()
    provider = setup()
    LangchainInstrumentor().instrument(tracer_provider=provider)
    model = ChatOpenAI(
        model=MODEL,
        api_key=SecretStr(os.environ["OPENAI_API_KEY"]),
        base_url=mock_url("/v1"),
        max_retries=0,
    )
    bound = model.bind_tools([get_weather])
    messages: list[SystemMessage | HumanMessage | AIMessage | ToolMessage] = [
        SystemMessage("You are a weather assistant."),
        HumanMessage("What's the weather in Paris?"),
    ]
    first = bound.invoke(messages)
    if not isinstance(first, AIMessage) or not first.tool_calls:
        raise RuntimeError("tool call missing")
    call = first.tool_calls[0]
    result = get_weather.invoke(call["args"])
    text = result if isinstance(result, str) else json.dumps(result)
    messages.extend([first, ToolMessage(content=text, tool_call_id=call["id"])])
    bound.invoke(messages)
    finish(provider)


if __name__ == "__main__":
    main()
