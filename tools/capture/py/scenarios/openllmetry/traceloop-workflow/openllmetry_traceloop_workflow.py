"""A Traceloop workflow and task around one OpenAI chat completion."""

from __future__ import annotations

import os

from openai import OpenAI
from opentelemetry import trace
from opentelemetry.sdk.trace import TracerProvider
from traceloop.sdk import Traceloop
from traceloop.sdk.decorators import task, workflow
from traceloop.sdk.instruments import Instruments

from capture_py.harness import finish, mock_url, receiver_base_url

MODEL = "gpt-4o-mini-2024-07-18"


@task(name="lookup_weather")
def lookup_weather() -> str:
    """Ask the model for the weather."""
    client = OpenAI(
        base_url=mock_url("/v1"),
        api_key=os.environ["OPENAI_API_KEY"],
        max_retries=0,
    )
    completion = client.chat.completions.create(
        model=MODEL,
        messages=[
            {"role": "system", "content": "You are a weather assistant."},
            {"role": "user", "content": "What's the weather in Paris?"},
        ],
    )
    content = completion.choices[0].message.content
    if content is None:
        raise RuntimeError("answer missing")
    return content


@workflow(name="weather_report")
def answer_weather() -> str:
    """Record association properties, then run the task."""
    Traceloop.set_association_properties(
        {
            "session_id": "session-1",
            "user_id": "user-1",
            "feature": "weather",
        }
    )
    return lookup_weather()


def main() -> None:
    """Export one workflow span through the Traceloop SDK."""
    Traceloop.init(
        app_name=os.environ["OTEL_SERVICE_NAME"],
        api_endpoint=receiver_base_url(),
        disable_batch=True,
        telemetry_enabled=False,
        instruments={Instruments.OPENAI},
    )
    provider = trace.get_tracer_provider()
    if not isinstance(provider, TracerProvider):
        raise RuntimeError("tracer provider missing")
    try:
        answer_weather()
    finally:
        finish(provider)


if __name__ == "__main__":
    main()
