"""Tests for deploying Obscura through Amoeba and using it to fetch a URL."""

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
            data={"values": "{}"},
            timeout=30,
        )
    resp.raise_for_status()
    print(f"Installed {app_name} app: {resp.json()}")


def test_obscura_install_and_cdp_endpoint(base_url: str, admin_token: str) -> None:
    """Install Obscura and verify its CDP HTTP endpoint is reachable through Amoeba."""
    package_dir = Path(__file__).parent.parent.parent / "examples" / "app-packages" / "obscura"
    _install_package(base_url, admin_token, package_dir, "obscura")
    wait_for_app_ready(base_url, admin_token, "obscura", timeout_seconds=300)

    # Give the CDP server a moment to finish starting inside the container.
    time.sleep(5)

    resp = requests.get(
        f"{base_url}/v1/obscura/json/version",
        headers=auth_header(admin_token),
        timeout=30,
    )
    print(f"Obscura /json/version status: {resp.status_code}, body: {resp.text[:200]}")
    resp.raise_for_status()
    data = resp.json()
    assert "Browser" in data or "browser" in data, f"unexpected CDP version response: {data}"


def test_obscura_can_fetch_url(base_url: str, admin_token: str, ssh) -> None:
    """Use Obscura's CLI inside the container to fetch a URL and verify the output."""
    # Warm the container first so the fetch isn't also cold-booting it.
    resp = requests.get(
        f"{base_url}/v1/obscura/json/version",
        headers=auth_header(admin_token),
        timeout=30,
    )
    resp.raise_for_status()

    out = ssh.run(
        "docker compose -p obscura exec -T obscura obscura fetch https://example.com --dump text",
        timeout=60,
    )
    print(f"obscura fetch output: {out[:500]}")
    assert "Example Domain" in out, "obscura fetch did not return expected page content"
