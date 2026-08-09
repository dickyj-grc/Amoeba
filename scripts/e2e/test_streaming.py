"""Tests for streaming (SSE) proxy support and scale-to-zero after a stream."""

from __future__ import annotations

import os
import time
from pathlib import Path

import requests

from conftest import auth_header, wait_for_app_ready


def _install_package(base_url: str, admin_token: str, package_dir: Path, app_name: str) -> None:
    zip_path = Path(f"/tmp/{app_name}.amoeba.zip")
    os.system(f"cd {package_dir} && zip -r {zip_path} amoeba.yaml compose.yaml > /dev/null")

    with open(zip_path, "rb") as f:
        resp = requests.post(
            f"{base_url}/admin/apps",
            headers=auth_header(admin_token),
            files={"package": (f"{app_name}.amoeba.zip", f, "application/zip")},
            data={"values": '{"env":{"MESSAGE":"e2e streaming test"}}'},
            timeout=30,
        )
    resp.raise_for_status()
    print(f"Installed {app_name} app: {resp.json()}")


def test_streaming_response_is_not_buffered(base_url: str, admin_token: str) -> None:
    """Install a streaming app and verify SSE chunks arrive as a true stream."""
    package_dir = Path(__file__).parent.parent.parent / "examples" / "app-packages" / "streaming-echo"
    _install_package(base_url, admin_token, package_dir, "streaming-echo")
    wait_for_app_ready(base_url, admin_token, "streaming-echo")

    start = time.time()
    resp = requests.get(
        f"{base_url}/v1/streaming-echo/stream",
        headers={**auth_header(admin_token), "Accept": "text/event-stream"},
        stream=True,
        timeout=30,
    )
    resp.raise_for_status()

    # Collect chunks as they arrive. A buffered response would return all at
    # once after several seconds; a streamed response gives us chunks earlier.
    events = []
    chunk_times = []
    for chunk in resp.iter_content(chunk_size=None):
        if chunk:
            chunk_times.append(time.time() - start)
            events.append(chunk.decode())
    total_duration = time.time() - start

    body = "".join(events)
    print(f"Streamed body ({len(events)} chunks, {total_duration:.2f}s): {body[:200]}")

    assert "text/event-stream" in resp.headers.get("Content-Type", ""), (
        f"expected Content-Type text/event-stream, got {resp.headers.get('Content-Type')}"
    )
    assert len(events) >= 3, f"expected multiple chunks, got {len(events)}"
    # The server emits 5 events 0.5s apart, so the total should be > 1.5s.
    assert total_duration > 1.5, f"stream returned too fast ({total_duration:.2f}s), likely buffered"
    assert '"message": "e2e streaming test"' in body


def test_scale_to_zero_after_stream(base_url: str, admin_token: str, ssh) -> None:
    """After the stream ends and cooldown passes, the container should stop."""
    # Ensure the container is running by making a request.
    resp = requests.get(
        f"{base_url}/v1/streaming-echo/",
        headers=auth_header(admin_token),
        timeout=30,
    )
    resp.raise_for_status()

    out = ssh.run("docker ps --filter name=amoeba-streaming-echo --format '{{.Names}}'")
    assert "amoeba-streaming-echo" in out, "streaming-echo container should be running"

    print("Waiting 45s for cooldown...")
    time.sleep(45)

    out = ssh.run("docker ps --filter name=amoeba-streaming-echo --format '{{.Names}}'")
    assert "amoeba-streaming-echo" not in out, "streaming-echo container should have stopped after cooldown"
    print("streaming-echo scaled to zero as expected")
