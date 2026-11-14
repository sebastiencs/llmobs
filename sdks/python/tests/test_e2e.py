"""End-to-end: the SDK against a running ``llmobs serve`` (``just e2e``, D-G-20).

``tools/e2e/run.sh`` starts the server and sets ``LLMOBS_E2E_BASE_URL`` and
``LLMOBS_E2E_API_KEY``; without them the test is skipped.
"""

import os
import time
from collections.abc import Callable
from typing import Any

import pytest
import requests

import llmobs

pytestmark = [
    pytest.mark.e2e,
    pytest.mark.skipif(not os.environ.get("LLMOBS_E2E_BASE_URL"), reason="needs just e2e"),
]

POLL_LIMIT_S = 10.0
POLL_INTERVAL_S = 0.25
REQUEST_TIMEOUT_S = 10


def _poll(url: str, key: str, ready: Callable[[dict[str, Any]], bool]) -> dict[str, Any]:
    """GET ``url`` every 250 ms until it answers 200 with a body ``ready`` accepts."""
    deadline = time.monotonic() + POLL_LIMIT_S
    status = 0
    while time.monotonic() < deadline:
        response = requests.get(
            url,
            headers={"Authorization": f"Bearer {key}"},
            timeout=REQUEST_TIMEOUT_S,
        )
        status = response.status_code
        if status == 200:
            body: dict[str, Any] = response.json()
            if ready(body):
                return body
        time.sleep(POLL_INTERVAL_S)
    pytest.fail(f"{url}: not ready after {POLL_LIMIT_S:g} s (last status {status})")


def test_traced_function_and_score_read_back_through_the_api() -> None:
    base_url = os.environ["LLMOBS_E2E_BASE_URL"]
    key = os.environ["LLMOBS_E2E_API_KEY"]
    llmobs.init(api_key=key, base_url=base_url)
    trace_ids: list[str | None] = []

    @llmobs.observe(name="e2e-py")
    def traced() -> str:
        trace_ids.append(llmobs.current_trace_id())
        return "ok"

    assert traced() == "ok"
    trace_id = trace_ids[0]
    assert trace_id is not None
    assert llmobs.score(name="e2e", value=1.0, trace_id=trace_id) is not None
    llmobs.shutdown()

    trace = _poll(
        f"{base_url}/api/v1/projects/current/traces/{trace_id}",
        key,
        ready=lambda _body: True,
    )
    assert trace["trace"]["name"] == "e2e-py"

    _poll(
        f"{base_url}/api/v1/projects/current/scores?trace_id={trace_id}",
        key,
        ready=lambda body: any(
            item.get("name") == "e2e"
            and item.get("value") == {"data_type": "numeric", "value": 1.0}
            for item in body.get("data", [])
        ),
    )
