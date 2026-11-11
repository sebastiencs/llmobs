"""A plain OpenAI call inside a manual generation that records usage and cost."""

from __future__ import annotations

import os
from datetime import UTC, datetime

from langfuse import Langfuse
from openai import OpenAI
from openai.types.chat import ChatCompletionMessageParam
from opentelemetry import trace
from opentelemetry.sdk.trace import TracerProvider

from capture_py.harness import finish, mock_url, receiver_base_url

MODEL = "gpt-4o-mini-2024-07-18"


def main() -> None:
    """Record usage, cost, model parameters, and completion_start_time on one generation."""
    langfuse = _langfuse()
    try:
        _generate(langfuse)
    finally:
        _finish(langfuse)


def _generate(langfuse: Langfuse) -> None:
    client = OpenAI(
        base_url=mock_url("/v1"),
        api_key=os.environ["OPENAI_API_KEY"],
        max_retries=0,
    )
    messages: list[ChatCompletionMessageParam] = [
        {"role": "user", "content": "What's the weather in Paris?"}
    ]
    with langfuse.start_as_current_observation(
        name="weather-generation",
        as_type="generation",
        model=MODEL,
        input=[{"role": "user", "content": "What's the weather in Paris?"}],
        model_parameters={"temperature": 0, "max_tokens": 64},
    ) as generation:
        completion = client.chat.completions.create(model=MODEL, messages=messages)
        generation.update(
            output=completion.choices[0].message.content or "",
            usage_details={
                "input": 24,
                "output": 12,
                "input_cached_tokens": 8,
                "output_reasoning_tokens": 3,
            },
            cost_details={"input": 1.0, "output": 2.0, "total": 3.0},
            completion_start_time=datetime.now(UTC),
        )


def _langfuse() -> Langfuse:
    return Langfuse(base_url=receiver_base_url(), otel_compression="none")


def _finish(langfuse: Langfuse) -> None:
    langfuse.flush()
    provider = trace.get_tracer_provider()
    if not isinstance(provider, TracerProvider):
        raise RuntimeError("langfuse tracer provider was not installed")
    finish(provider)
