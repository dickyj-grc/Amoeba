"""Tests for the admin apps API."""

from __future__ import annotations

import os
import time
from pathlib import Path

import requests

from conftest import auth_header


def _wait_for_ready(base_url: str, admin_token: str, app_name: str, timeout_seconds: int = 120) -> dict:
    """Poll the app status endpoint until it reaches ready or error."""
    deadline = time.time() + timeout_seconds
    while time.time() < deadline:
        resp = requests.get(
            f"{base_url}/admin/apps/{app_name}",
            headers=auth_header(admin_token),
            timeout=10,
        )
        resp.raise_for_status()
        body = resp.json()
        state = body["state"]["state"]
        print(f"App '{app_name}' state: {state}")
        if state == "ready":
            return body
        if state == "error":
            raise AssertionError(f"app '{app_name}' failed to become ready: {body['state']['message']}")
        time.sleep(2)
    raise AssertionError(f"app '{app_name}' did not become ready within {timeout_seconds}s")


def test_install_and_list_app(base_url: str, admin_token: str, package_dir: Path) -> None:
    """Install the hello-world app package, wait for it to be ready, and verify the catalog."""
    zip_path = Path("/tmp/hello-world.amoeba.zip")
    os.system(f"cd {package_dir} && zip -r {zip_path} amoeba.yaml compose.yaml > /dev/null")

    with open(zip_path, "rb") as f:
        resp = requests.post(
            f"{base_url}/admin/apps",
            headers=auth_header(admin_token),
            files={"package": ("hello-world.amoeba.zip", f, "application/zip")},
            data={"values": '{"env":{"GREETING":"e2e test"}}'},
            timeout=30,
        )
    resp.raise_for_status()
    body = resp.json()
    print(f"Installed hello-world app: {body}")
    assert body["state"]["state"] in ("installed", "pulling")

    # Wait for the background readiness probe to finish.
    _wait_for_ready(base_url, admin_token, "hello-world")

    resp = requests.get(
        f"{base_url}/admin/apps", headers=auth_header(admin_token), timeout=10
    )
    resp.raise_for_status()
    apps = resp.json()["apps"]
    print(f"Installed apps: {apps}")
    assert "hello-world" in apps
