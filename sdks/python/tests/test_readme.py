"""Execute the README's tested code blocks (plan F § P12).

Every ```python block preceded by a `<!-- test -->` line runs through the
`memory` pipeline: `llmobs.init` installs the in-memory runtime with the test
API key and the score queue pointed at the `http_recorder` fixture, so the
documented examples (score included) never reach the network. Everything
except `init` is the real SDK, so the examples keep working as the code moves.
"""

import re
import sys
from collections.abc import Callable
from pathlib import Path
from types import ModuleType
from typing import Protocol

import pytest
from opentelemetry.sdk.trace.export.in_memory_span_exporter import InMemorySpanExporter

import llmobs
from llmobs._runtime import Runtime

README = Path(__file__).resolve().parents[1] / "README.md"
MARKER = "<!-- test -->"

# One (slug, code) pair per marked block; the slug is the nearest heading.
_FENCE = re.compile(r"^```python\n(.*?)^```", re.DOTALL | re.MULTILINE)


def _tested_blocks() -> list[tuple[str, str]]:
    text = README.read_text(encoding="utf-8")
    found: list[tuple[str, str]] = []
    seen: dict[str, int] = {}
    for match in _FENCE.finditer(text):
        before = text[: match.start()].splitlines()
        if not before or before[-1].strip() != MARKER:
            continue
        heading = next((line for line in reversed(before[:-1]) if line.startswith("#")), "")
        slug = re.sub(r"[^a-z0-9]+", "-", heading.lower()).strip("-")
        seen[slug] = seen.get(slug, 0) + 1
        if seen[slug] > 1:
            slug = f"{slug}-{seen[slug]}"
        found.append((slug, match.group(1)))
    return found


BLOCKS = _tested_blocks()


class Recorder(Protocol):
    """The surface of the ``http_recorder`` fixture used by the tests."""

    url: str
    requests: list[tuple[str, str, dict[str, str], bytes]]


class _ReadmeSdk(ModuleType):
    """The real ``llmobs`` module with ``init`` bound to the test pipeline."""

    def __init__(self, real: ModuleType, start: Callable[..., Runtime], url: str) -> None:
        super().__init__("llmobs")
        # ``init`` is skipped: the override below must not be shadowed by a
        # setattr on the instance.
        for name in real.__all__:
            if name != "init":
                setattr(self, name, getattr(real, name))
        self.__all__ = list(real.__all__)
        self._start = start
        self._url = url

    def init(self, **options: object) -> None:
        """Install the in-memory runtime, replacing the example's endpoints."""
        options.pop("api_key", None)
        options.pop("base_url", None)
        self._start(api_key="sk-llmobs-test", base_url=self._url, **options)


@pytest.mark.parametrize(("slug", "code"), BLOCKS, ids=[slug for slug, _ in BLOCKS])
def test_readme_block(
    slug: str,
    code: str,
    memory: tuple[InMemorySpanExporter, Callable[..., Runtime]],
    http_recorder: Recorder,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    exporter, start = memory
    sdk = _ReadmeSdk(llmobs, start, http_recorder.url)
    monkeypatch.setitem(sys.modules, "llmobs", sdk)
    exec(  # noqa: S102  # executing the documented example is the test
        compile(code, f"README.md [{slug}]", "exec"),
        {"llmobs": sdk, "__name__": f"readme_{slug.replace('-', '_')}"},
    )
    assert exporter.get_finished_spans(), "the example exported no spans"
    if slug == "scores":
        # The score example must deliver through the recorder, never the network.
        assert any(
            method == "POST" and path == "/api/v1/scores"
            for method, path, _, _ in http_recorder.requests
        )


def test_required_examples_are_marked() -> None:
    """The sections the plan pins must keep their tested blocks (§ P12)."""
    slugs = {slug for slug, _ in BLOCKS}
    assert {
        "quick-start",
        "manual-spans-and-llm-span",
        "context-and-baggage",
        "scores",
    } <= slugs
