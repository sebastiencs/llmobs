"""OpenAI chat completion with a tool call, traced by OpenInference."""

from __future__ import annotations

import json
import os

from openai import OpenAI
from openai.types.chat import (
    ChatCompletionMessage,
    ChatCompletionMessageFunctionToolCall,
    ChatCompletionMessageParam,
    ChatCompletionToolParam,
)
from openinference.instrumentation.openai import OpenAIInstrumentor

from capture_py.harness import finish, mock_url, setup

MODEL = "gpt-4o-mini-2024-07-18"


def main() -> None:
    """Run two chat completions: a tool call, then the answer."""
    provider = setup()
    OpenAIInstrumentor().instrument(tracer_provider=provider)
    client = OpenAI(
        base_url=mock_url("/v1"),
        api_key=os.environ["OPENAI_API_KEY"],
        max_retries=0,
    )
    tools = [_weather_tool()]
    messages: list[ChatCompletionMessageParam] = [
        {"role": "system", "content": "You are a weather assistant."},
        {"role": "user", "content": "What's the weather in Paris?"},
    ]
    first = client.chat.completions.create(model=MODEL, messages=messages, tools=tools)
    call = _function_call(first.choices[0].message)
    messages.append(
        {
            "role": "assistant",
            "tool_calls": [
                {
                    "id": call.id,
                    "type": "function",
                    "function": {"name": call.function.name, "arguments": call.function.arguments},
                }
            ],
        }
    )
    messages.append(
        {
            "role": "tool",
            "tool_call_id": call.id,
            "content": json.dumps({"temperature_c": 18, "conditions": "sunny"}),
        }
    )
    client.chat.completions.create(model=MODEL, messages=messages, tools=tools)
    finish(provider)


def _weather_tool() -> ChatCompletionToolParam:
    return {
        "type": "function",
        "function": {
            "name": "get_weather",
            "description": "Look up the weather for a city.",
            "parameters": {
                "type": "object",
                "properties": {"city": {"type": "string"}},
                "required": ["city"],
            },
        },
    }


def _function_call(message: ChatCompletionMessage) -> ChatCompletionMessageFunctionToolCall:
    for call in message.tool_calls or []:
        if isinstance(call, ChatCompletionMessageFunctionToolCall):
            return call
    raise RuntimeError("tool call missing")


if __name__ == "__main__":
    main()
