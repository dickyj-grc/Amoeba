"""Tests for the admin apps API."""

from __future__ import annotations

import os
from pathlib import Path

import requests

from conftest import auth_header


def test_install_and_list_app(base_url: str, admin_token: str, package_dir: Path) -> None:
    """Install the hello-world app package and verify it appears in the catalog."""
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
    print(f"Installed hello-world app: {resp.json()}")

    resp = requests.get(
        f"{base_url}/admin/apps", headers=auth_header(admin_token), timeout=10
    )
    resp.raise_for_status()
    apps = resp.json()["apps"]
    print(f"Installed apps: {apps}")
    assert "hello-world" in apps
