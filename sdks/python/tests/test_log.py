import logging
import threading

import pytest

from llmobs._log import reset_warnings, warn_once


@pytest.fixture(autouse=True)
def _fresh_warnings() -> None:
    reset_warnings()


def test_warn_once_logs_the_first_time_per_key(caplog: pytest.LogCaptureFixture) -> None:
    with caplog.at_level(logging.WARNING, logger="llmobs"):
        warn_once("same", "message one")
        warn_once("same", "message one")
        warn_once("same", "message two")
    records = [record for record in caplog.records if record.name == "llmobs"]
    assert len(records) == 1
    assert records[0].levelno == logging.WARNING
    assert records[0].getMessage() == "message one"


def test_warn_once_logs_different_keys_separately(caplog: pytest.LogCaptureFixture) -> None:
    with caplog.at_level(logging.WARNING, logger="llmobs"):
        warn_once("a", "message a")
        warn_once("b", "message b")
    records = [record for record in caplog.records if record.name == "llmobs"]
    assert [record.getMessage() for record in records] == ["message a", "message b"]


def test_warn_once_is_thread_safe(caplog: pytest.LogCaptureFixture) -> None:
    with caplog.at_level(logging.WARNING, logger="llmobs"):
        threads = [threading.Thread(target=warn_once, args=("same", "message")) for _ in range(10)]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()
    records = [record for record in caplog.records if record.name == "llmobs"]
    assert len(records) == 1
    assert records[0].getMessage() == "message"
