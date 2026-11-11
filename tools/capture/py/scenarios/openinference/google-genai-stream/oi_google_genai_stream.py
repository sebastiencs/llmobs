"""One streamed Gemini answer with a reasoning part, traced by OpenInference."""

from __future__ import annotations

import os

from google import genai
from google.genai.types import HttpOptions
from openinference.instrumentation.google_genai import GoogleGenAIInstrumentor

from capture_py.harness import finish, mock_url, setup

MODEL = "gemini-2.5-flash"


def main() -> None:
    """Stream one generateContent response and read every chunk."""
    provider = setup()
    GoogleGenAIInstrumentor().instrument(tracer_provider=provider)
    client = genai.Client(
        api_key=os.environ["GOOGLE_API_KEY"],
        http_options=HttpOptions(base_url=mock_url()),
    )
    for _chunk in client.models.generate_content_stream(
        model=MODEL,
        contents="Describe the harbor in a few words.",
    ):
        pass
    finish(provider)


if __name__ == "__main__":
    main()
