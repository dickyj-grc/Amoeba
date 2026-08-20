"""Tests for proxy routing and scale-to-zero behavior."""

from __future__ import annotations

import requests

from conftest import auth_header, wait_for_container_stopped


def test_proxy_responds_and_container_stops(base_url: str, admin_token: str, ssh) -> None:
    """Hit the app endpoint, verify the container is running, wait for cooldown,
    then verify it stopped."""
    resp = requests.get(
        f"{base_url}/v1/hello-world/",
        headers=auth_header(admin_token),
        timeout=30,
    )
    resp.raise_for_status()
    print(f"Proxy response: {resp.text.strip()}")
    assert "e2e test" in resp.text

    # Verify container is running
    out = ssh.run("docker ps --filter name=amoeba-hello-world --format '{{.Names}}'")
    assert "amoeba-hello-world" in out, "Container should be running after request"

    # Wait for cooldown (30s) + reaper sweep (10s) + docker stop
    print("Waiting for hello-world to scale to zero after cooldown...")
    wait_for_container_stopped(ssh, "amoeba-hello-world")
    print("Container scaled to zero as expected")
