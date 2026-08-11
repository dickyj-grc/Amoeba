"""Shared pytest fixtures and configuration for the Amoeba e2e tests."""

from __future__ import annotations

import os
import time
from pathlib import Path

import pytest
import requests

from do_client import DigitalOceanClient, env
from ssh_client import SshClient, wait_for_ssh


# --------------------------------------------------------------------------- #
# CLI options
# --------------------------------------------------------------------------- #

def pytest_addoption(parser: pytest.Parser) -> None:
    parser.addoption(
        "--keep-droplet",
        action="store_true",
        default=False,
        help="Do not destroy the DigitalOcean droplet after tests finish",
    )
    parser.addoption(
        "--droplet-name",
        action="store",
        default="amoeba-nightly-e2e",
        help="Name of the DigitalOcean droplet to create",
    )


# --------------------------------------------------------------------------- #
# Fixtures
# --------------------------------------------------------------------------- #

@pytest.fixture(scope="session")
def droplet_name(pytestconfig: pytest.Config) -> str:
    return pytestconfig.getoption("--droplet-name")


@pytest.fixture(scope="session")
def ssh_private_key() -> str:
    return env("DO_SSH_PRIVATE_KEY")


@pytest.fixture(scope="session")
def ssh_public_key() -> str:
    return env("DO_SSH_PUBLIC_KEY")


@pytest.fixture(scope="session")
def jwt_secret() -> str:
    return env("AMOEBA_LOCAL_JWT_SECRET")


@pytest.fixture(scope="session")
def age_secret_key() -> str | None:
    return os.environ.get("AMOEBA_AGE_SECRET_KEY")


@pytest.fixture(scope="session")
def package_dir() -> Path:
    return Path(__file__).parent.parent.parent / "examples" / "app-packages" / "hello-world"


@pytest.fixture(scope="session")
def cloud_init_user_data(age_secret_key: str | None) -> str:
    """Load cloud-init script and substitute env vars."""
    path = Path(__file__).parent.parent / "e2e-cloud-init.sh"
    data = path.read_text()
    data = data.replace(
        'JWT_SECRET="${AMOEBA_LOCAL_JWT_SECRET:-change-me-in-production}"',
        f'JWT_SECRET="{env("AMOEBA_LOCAL_JWT_SECRET")}"',
    )
    data = data.replace(
        'AGE_SECRET="${AMOEBA_AGE_SECRET_KEY:-}"',
        f'AGE_SECRET="{age_secret_key or ""}"',
    )
    return data


@pytest.fixture(scope="session")
def do_client() -> DigitalOceanClient:
    return DigitalOceanClient()


@pytest.fixture(scope="session")
def droplet(
    do_client: DigitalOceanClient,
    droplet_name: str,
    ssh_public_key: str,
    ssh_private_key: str,
    cloud_init_user_data: str,
    pytestconfig: pytest.Config,
) -> str:
    """Create a DO droplet, wait for SSH + Amoeba, yield its IP, then destroy it."""
    ssh_key_id = do_client.create_or_reuse_ssh_key(droplet_name, ssh_public_key)
    droplet_id = do_client.create_droplet(
        droplet_name,
        ssh_key_id,
        cloud_init_user_data,
    )
    try:
        ip = do_client.wait_for_droplet(droplet_id)
        wait_for_ssh(ip, ssh_private_key)
        try:
            wait_for_amoeba(ip, timeout=600)
        except RuntimeError:
            _dump_cloud_init_log(ip, ssh_private_key)
            raise
        yield ip
    finally:
        if not pytestconfig.getoption("--keep-droplet"):
            do_client.destroy_droplet(droplet_id)


@pytest.fixture(scope="session")
def base_url(droplet: str) -> str:
    return f"http://{droplet}"


@pytest.fixture(scope="session")
def ssh(droplet: str, ssh_private_key: str) -> SshClient:
    return SshClient(droplet, ssh_private_key)


@pytest.fixture(scope="session")
def admin_token(base_url: str, ssh: SshClient) -> str:
    """Seed the admin user and return its JWT."""
    ssh.run(
        "cd /opt/amoeba && docker compose exec -T orchestrator amoeba-admin add-user "
        "admin admin123 --roles admin --org org_e2e --users-file /etc/amoeba/users.json"
    )
    print("Created admin user")
    return login(base_url, "admin", "admin123")


@pytest.fixture(scope="session")
def analyst_token(base_url: str, admin_token: str) -> str:
    create_user(base_url, admin_token, "analyst", "analyst123", ["analyst"])
    return login(base_url, "analyst", "analyst123")


@pytest.fixture(scope="session")
def viewer_token(base_url: str, admin_token: str) -> str:
    create_user(base_url, admin_token, "viewer", "viewer123", ["viewer"])
    return login(base_url, "viewer", "viewer123")


# --------------------------------------------------------------------------- #
# Helpers
# --------------------------------------------------------------------------- #

def _dump_cloud_init_log(ip: str, private_key: str) -> None:
    """Best-effort: pull cloud-init's output log before the droplet is destroyed.

    The droplet is torn down as soon as the `droplet` fixture's setup fails, so this
    is the only chance to see why provisioning (docker install, image pull, etc.)
    never reached a healthy Amoeba.
    """
    out_path = Path("e2e-cloud-init-failure.log")
    try:
        log = SshClient(ip, private_key).run("cat /var/log/cloud-init-output.log", timeout=30)
    except Exception as exc:
        out_path.write_text(f"Could not retrieve cloud-init log: {exc}\n")
        print(f"Could not retrieve cloud-init log: {exc}")
        return
    out_path.write_text(log)
    print(f"Saved cloud-init log to {out_path} ({len(log)} bytes)")


def wait_for_amoeba(ip: str, timeout: int = 300) -> None:
    """Wait until Amoeba responds on port 80 (any non-5xx status means it is up)."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        try:
            resp = requests.get(f"http://{ip}/v1/health-check/", timeout=5)
            if resp.status_code < 500:
                print("Amoeba is reachable")
                return
        except requests.RequestException:
            pass
        time.sleep(5)
    raise RuntimeError("Timed out waiting for Amoeba")


def wait_for_app_ready(base_url: str, admin_token: str, app_name: str, timeout_seconds: int = 180) -> dict:
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


def login(base_url: str, username: str, password: str) -> str:
    """Obtain a JWT for the given user."""
    resp = requests.post(
        f"{base_url}/auth/login",
        json={"username": username, "password": password},
        timeout=10,
    )
    resp.raise_for_status()
    return resp.json()["token"]


def create_user(
    base_url: str,
    admin_token: str,
    username: str,
    password: str,
    roles: list[str],
) -> None:
    """Create a user via the admin API."""
    resp = requests.post(
        f"{base_url}/admin/users",
        headers={"Authorization": f"Bearer {admin_token}"},
        json={"username": username, "password": password, "roles": roles, "org_id": "org_e2e"},
        timeout=10,
    )
    resp.raise_for_status()
    print(f"Created user '{username}' with roles {roles}")


def auth_header(token: str) -> dict[str, str]:
    return {"Authorization": f"Bearer {token}"}
