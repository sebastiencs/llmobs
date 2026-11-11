"""One OpenAI embeddings call, traced by the official OTel instrumentation."""

from __future__ import annotations

import os

from openai import OpenAI
from opentelemetry.instrumentation.openai_v2 import OpenAIInstrumentor

from capture_py.harness import finish, mock_url, setup

MODEL = "text-embedding-3-small"


def main() -> None:
    """Embed two short strings."""
    provider = setup()
    OpenAIInstrumentor().instrument(tracer_provider=provider)
    client = OpenAI(
        base_url=mock_url("/v1"),
        api_key=os.environ["OPENAI_API_KEY"],
        max_retries=0,
    )
    client.embeddings.create(model=MODEL, input=["alpha", "beta"], dimensions=8)
    finish(provider)


if __name__ == "__main__":
    main()
