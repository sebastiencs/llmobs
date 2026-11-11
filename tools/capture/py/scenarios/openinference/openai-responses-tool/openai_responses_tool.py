"""OpenAI Responses API with a tool call, traced by OpenInference."""

from __future__ import annotations

import json
import os

from openai import OpenAI
from openai.types.responses import (
    FunctionToolParam,
    Response,
    ResponseFunctionToolCall,
    ResponseInputParam,
)
from openinference.instrumentation.openai import OpenAIInstrumentor

from capture_py.harness import finish, mock_url, setup

MODEL = "gpt-4o-mini-2024-07-18"


def main() -> None:
    """Run two Responses calls: a tool call, then the answer."""
    provider = setup()
    OpenAIInstrumentor().instrument(tracer_provider=provider)
    client = OpenAI(
        base_url=mock_url("/v1"),
        api_key=os.environ["OPENAI_API_KEY"],
        max_retries=0,
    )
    tools = [_weather_tool()]
    first = client.responses.create(
        model=MODEL,
        input="What's the weather in Paris?",
        tools=tools,
    )
    call = _function_call(first)
    follow_up: ResponseInputParam = [
        {"role": "user", "content": "What's the weather in Paris?"},
        {
            "type": "function_call",
            "call_id": call.call_id,
            "name": call.name,
            "arguments": call.arguments,
        },
        {
            "type": "function_call_output",
            "call_id": call.call_id,
            "output": json.dumps({"temperature_c": 18, "conditions": "sunny"}),
        },
    ]
    client.responses.create(model=MODEL, input=follow_up, tools=tools)
    finish(provider)


def _weather_tool() -> FunctionToolParam:
    return {
        "type": "function",
        "name": "get_weather",
        "strict": None,
        "description": "Look up the weather for a city.",
        "parameters": {
            "type": "object",
            "properties": {"city": {"type": "string"}},
            "required": ["city"],
        },
    }


def _function_call(response: Response) -> ResponseFunctionToolCall:
    for item in response.output:
        if isinstance(item, ResponseFunctionToolCall):
            return item
    raise RuntimeError("tool call missing")


if __name__ == "__main__":
    main()
