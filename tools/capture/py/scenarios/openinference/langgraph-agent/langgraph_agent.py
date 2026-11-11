"""LangChain agent graph with one tool, traced by OpenInference."""

from __future__ import annotations

import json
import os

from langchain.agents import create_agent
from langchain.tools import tool
from langchain_core.messages import HumanMessage
from langchain_openai import ChatOpenAI
from openinference.instrumentation.langchain import LangChainInstrumentor
from pydantic import SecretStr

from capture_py.harness import finish, mock_url, setup

MODEL = "gpt-4o-mini-2024-07-18"


def _pin_framework_ids() -> None:
    """Replace LangChain and LangGraph random ids with a counter."""
    import uuid

    import langgraph.checkpoint.base as checkpoint_base
    from langchain_core.callbacks import manager as callback_manager
    from langchain_core.tracers import event_stream
    from langchain_core.utils import uuid as lc_uuid
    from langgraph.checkpoint.base import id as checkpoint_id
    from langgraph.pregel import _checkpoint, _messages

    counter = 0

    def next_id() -> uuid.UUID:
        nonlocal counter
        counter += 1
        return uuid.UUID(int=counter)

    def uuid7(nanoseconds: int | None = None) -> uuid.UUID:
        del nanoseconds
        return next_id()

    def uuid4() -> uuid.UUID:
        return next_id()

    def uuid6(node: int | None = None, clock_seq: int | None = None) -> uuid.UUID:
        del node, clock_seq
        return next_id()

    lc_uuid.uuid7 = uuid7
    callback_manager.uuid7 = uuid7  # type: ignore[attr-defined]
    event_stream.uuid7 = uuid7  # type: ignore[attr-defined]
    _messages.uuid4 = uuid4  # type: ignore[attr-defined]
    # add_messages stamps a missing tool-message id through the uuid module.
    uuid.uuid4 = uuid4
    checkpoint_id.uuid6 = uuid6  # type: ignore[assignment]
    checkpoint_base.uuid6 = uuid6  # type: ignore[attr-defined, assignment]
    _checkpoint.uuid6 = uuid6  # type: ignore[attr-defined, assignment]


@tool
def get_weather(city: str) -> str:
    """Look up the weather for a city."""
    return json.dumps({"temperature_c": 18, "conditions": "sunny", "city": city})


def main() -> None:
    """Run the agent through one tool call and the answer."""
    _pin_framework_ids()
    provider = setup()
    LangChainInstrumentor().instrument(tracer_provider=provider)
    model = ChatOpenAI(
        model=MODEL,
        api_key=SecretStr(os.environ["OPENAI_API_KEY"]),
        base_url=mock_url("/v1"),
        max_retries=0,
    )
    agent = create_agent(
        model,
        tools=[get_weather],
        system_prompt="You are a weather assistant.",
    )
    agent.invoke({"messages": [HumanMessage("What's the weather in Paris?", id="msg_user")]})
    finish(provider)


if __name__ == "__main__":
    main()
