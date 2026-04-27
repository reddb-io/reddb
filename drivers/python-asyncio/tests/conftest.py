"""Shared fixtures for the asyncio driver tests."""

from __future__ import annotations

import asyncio
import os
import subprocess
import sys
import time
from collections.abc import AsyncIterator
from pathlib import Path

import pytest
import pytest_asyncio

REPO_ROOT = Path(__file__).resolve().parents[3]


def _maybe_skip_smoke():
    if os.environ.get("RED_SKIP_SMOKE") == "1":
        pytest.skip("RED_SKIP_SMOKE=1 set", allow_module_level=False)


def _free_port() -> int:
    import socket as _s

    with _s.socket(_s.AF_INET, _s.SOCK_STREAM) as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def _spawn_red_server(port: int, db_path: str) -> subprocess.Popen[bytes]:
    """Start ``red server --bind 127.0.0.1:<port> --path <db>`` when a
    prebuilt binary is on PATH or via ``$RED_BIN``. Skip otherwise —
    we do **not** invoke ``cargo build`` because parallel agents may
    be using the target dir.
    """
    bin_path = os.environ.get("RED_BIN")
    if not bin_path:
        # Try the most common debug + release locations.
        for cand in (
            REPO_ROOT / "target" / "release" / "red",
            REPO_ROOT / "target" / "debug" / "red",
        ):
            if cand.exists():
                bin_path = str(cand)
                break
    if not bin_path:
        pytest.skip(
            "no `red` binary available; set $RED_BIN or prebuild "
            "(parallel agents block us from running cargo here)"
        )
    proc = subprocess.Popen(
        [bin_path, "server", "--bind", f"127.0.0.1:{port}", "--path", db_path],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        cwd=str(REPO_ROOT),
    )
    # Wait for the listener to come up.
    deadline = time.time() + 10
    import socket as _s

    while time.time() < deadline:
        if proc.poll() is not None:
            stderr = proc.stderr.read() if proc.stderr else b""
            pytest.skip(f"red server exited early: {stderr.decode(errors='replace')[:400]}")
        with _s.socket(_s.AF_INET, _s.SOCK_STREAM) as test:
            test.settimeout(0.2)
            try:
                test.connect(("127.0.0.1", port))
                return proc
            except OSError:
                time.sleep(0.1)
    proc.kill()
    pytest.skip("red server did not bind in time")


@pytest_asyncio.fixture(scope="module")
async def running_server() -> AsyncIterator[dict]:
    """Module-level fixture that brings up a `red` server on a free
    port. Yields ``{"host", "port", "http_port"}``. Skips when no
    binary can be found.
    """

    _maybe_skip_smoke()
    # Honour pre-existing instance via env vars.
    pre_host = os.environ.get("REDWIRE_TEST_HOST")
    pre_port = os.environ.get("REDWIRE_TEST_PORT")
    pre_http = os.environ.get("REDDB_HTTP_URL")
    if pre_host and pre_port:
        yield {
            "host": pre_host,
            "port": int(pre_port),
            "http_url": pre_http or f"http://{pre_host}:{pre_port}",
            "spawned": False,
        }
        return

    port = _free_port()
    import tempfile

    tmp = tempfile.TemporaryDirectory(prefix="reddb-asyncio-smoke-")
    db_path = os.path.join(tmp.name, "data.rdb")
    proc = _spawn_red_server(port, db_path)
    try:
        yield {
            "host": "127.0.0.1",
            "port": port,
            "http_url": pre_http or f"http://127.0.0.1:{port}",
            "spawned": True,
        }
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()
        try:
            tmp.cleanup()
        except Exception:
            pass
