"""Gemini generateContent with text and a synthetic PNG, traced by OpenLLMetry."""

from __future__ import annotations

import os
import struct
import zlib

from google import genai
from google.genai.types import HttpOptions, Part
from opentelemetry.instrumentation.google_generativeai import GoogleGenerativeAiInstrumentor

from capture_py.harness import finish, mock_url, setup

MODEL = "gemini-2.5-flash"


def main() -> None:
    """Send a text part and a 64x64 PNG, then read the text answer."""
    provider = setup()
    GoogleGenerativeAiInstrumentor().instrument(tracer_provider=provider)
    client = genai.Client(
        api_key=os.environ["GOOGLE_API_KEY"],
        http_options=HttpOptions(base_url=mock_url()),
    )
    client.models.generate_content(
        model=MODEL,
        contents=[
            Part.from_text(text="Describe this square."),
            Part.from_bytes(data=_png(), mime_type="image/png"),
        ],
    )
    finish(provider)


def _png() -> bytes:
    """Return a 64x64 solid blue PNG."""
    width = 64
    height = 64
    row = b"\x00" + bytes((80, 160, 240)) * width
    raw = row * height
    return b"".join(
        (
            b"\x89PNG\r\n\x1a\n",
            _chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)),
            _chunk(b"IDAT", zlib.compress(raw)),
            _chunk(b"IEND", b""),
        )
    )


def _chunk(tag: bytes, data: bytes) -> bytes:
    return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data))


if __name__ == "__main__":
    main()
