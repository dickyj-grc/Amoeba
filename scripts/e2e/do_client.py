"""DigitalOcean API client for the Amoeba e2e tests."""

from __future__ import annotations

import os
import time
from typing import Any

import requests

DO_API = "https://api.digitalocean.com/v2"


def _token() -> str:
    token = os.environ.get("DIGITALOCEAN_TOKEN")
    if not token:
        raise RuntimeError("Environment variable DIGITALOCEAN_TOKEN is required")
    return token


def _headers() -> dict[str, str]:
    return {
        "Authorization": f"Bearer {_token()}",
        "Content-Type": "application/json",
    }


def env(name: str, default: str | None = None) -> str:
    value = os.environ.get(name, default)
    if value is None:
        raise RuntimeError(f"Environment variable {name} is required")
    return value


class DigitalOceanClient:
    """Thin wrapper around the DigitalOcean v2 API."""

    def __init__(self) -> None:
        self.session = requests.Session()
        self.session.headers.update(_headers())

    def create_or_reuse_ssh_key(self, name: str, public_key: str) -> int:
        """Register or reuse a public SSH key in DO. Returns the key ID."""
        resp = self.session.get(f"{DO_API}/account/keys")
        resp.raise_for_status()
        for key in resp.json().get("ssh_keys", []):
            if key["name"] == name:
                print(f"Reusing existing SSH key '{name}' id={key['id']}")
                return key["id"]

        resp = self.session.post(
            f"{DO_API}/account/keys",
            json={"name": name, "public_key": public_key},
        )
        resp.raise_for_status()
        key_id = resp.json()["ssh_key"]["id"]
        print(f"Created SSH key '{name}' id={key_id}")
        return key_id

    def create_droplet(
        self,
        name: str,
        ssh_key_id: int,
        user_data: str,
        *,
        region: str | None = None,
        size: str | None = None,
        image: str | None = None,
    ) -> int:
        """Create the droplet and return its ID."""
        payload: dict[str, Any] = {
            "name": name,
            "region": region or env("DO_REGION", "nyc3"),
            "size": size or env("DO_SIZE", "s-4vcpu-8gb"),
            "image": image or env("DO_IMAGE", "ubuntu-24-04-x64"),
            "ssh_keys": [ssh_key_id],
            "user_data": user_data,
            "backups": False,
            "ipv6": False,
            "monitoring": False,
            "tags": ["amoeba-e2e"],
        }
        resp = self.session.post(f"{DO_API}/droplets", json=payload)
        resp.raise_for_status()
        droplet_id = resp.json()["droplet"]["id"]
        print(f"Created droplet '{name}' id={droplet_id}")
        return droplet_id

    def wait_for_droplet(self, droplet_id: int, timeout: int = 300) -> str:
        """Poll until the droplet is active and return its public IPv4."""
        deadline = time.time() + timeout
        while time.time() < deadline:
            resp = self.session.get(f"{DO_API}/droplets/{droplet_id}")
            resp.raise_for_status()
            droplet = resp.json()["droplet"]
            if droplet["status"] == "active":
                for net in droplet.get("networks", {}).get("v4", []):
                    if net["type"] == "public":
                        ip = net["ip_address"]
                        print(f"Droplet is active at {ip}")
                        return ip
            time.sleep(5)
        raise RuntimeError("Timed out waiting for droplet to become active")

    def destroy_droplet(self, droplet_id: int) -> None:
        """Delete the droplet."""
        resp = self.session.delete(f"{DO_API}/droplets/{droplet_id}")
        if resp.status_code == 204:
            print(f"Destroyed droplet {droplet_id}")
        else:
            print(f"Failed to destroy droplet {droplet_id}: {resp.status_code} {resp.text}")
