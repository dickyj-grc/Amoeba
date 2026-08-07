"""Tests for proxy routing and scale-to-zero behavior."""

from __future__ import annotations

import time

import requests

from conftest import auth_header


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

    # Wait for cooldown (30s in the cloud-init services.json)
    print("Waiting 45s for cooldown...")
    time.sleep(45)

    # Verify container stopped
    out = ssh.run("docker ps --filter name=amoeba-hello-world --format '{{.Names}}'")
    assert "amoeba-hello-world" not in out, "Container should have stopped after cooldown"
    print("Container scaled to zero as expected")
