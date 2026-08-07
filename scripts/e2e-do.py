#!/usr/bin/env python3
"""
Nightly end-to-end test for Amoeba on DigitalOcean.

Creates a DO droplet, provisions it with Caddy + Amoeba + test apps,
exercises the admin and proxy APIs, verifies scale-to-zero, then destroys
the droplet.

Required environment variables:
    DIGITALOCEAN_TOKEN      DO API personal access token
    DO_SSH_PRIVATE_KEY      PEM-encoded private key for SSH
    DO_SSH_PUBLIC_KEY       Public key registered with DO
    AMOEBA_LOCAL_JWT_SECRET Secret used by Amoeba to sign JWTs

Optional:
    AMOEBA_AGE_SECRET_KEY   Age identity for decrypting app secrets
    DO_REGION               Default: nyc3
    DO_SIZE                 Default: s-4vcpu-8gb (≈ $48/mo)
    DO_IMAGE                Default: ubuntu-24-04-x64
"""

import argparse
import os
import socket
import sys
import time
import traceback
from pathlib import Path

import paramiko
import requests

DO_API = "https://api.digitalocean.com/v2"


def env(name: str, default: str | None = None) -> str:
    value = os.environ.get(name, default)
    if value is None:
        raise RuntimeError(f"Environment variable {name} is required")
    return value


def do_headers() -> dict:
    return {
        "Authorization": f"Bearer {env('DIGITALOCEAN_TOKEN')}",
        "Content-Type": "application/json",
    }


def create_ssh_key(name: str, public_key: str) -> int:
    """Register or reuse a public SSH key in DO. Returns the key ID."""
    resp = requests.get(f"{DO_API}/account/keys", headers=do_headers())
    resp.raise_for_status()
    for key in resp.json().get("ssh_keys", []):
        if key["name"] == name:
            print(f"Reusing existing SSH key '{name}' id={key['id']}")
            return key["id"]

    resp = requests.post(
        f"{DO_API}/account/keys",
        headers=do_headers(),
        json={"name": name, "public_key": public_key},
    )
    resp.raise_for_status()
    key_id = resp.json()["ssh_key"]["id"]
    print(f"Created SSH key '{name}' id={key_id}")
    return key_id


def create_droplet(name: str, ssh_key_id: int, user_data: str) -> int:
    """Create the droplet and return its ID."""
    payload = {
        "name": name,
        "region": env("DO_REGION", "nyc3"),
        "size": env("DO_SIZE", "s-4vcpu-8gb"),
        "image": env("DO_IMAGE", "ubuntu-24-04-x64"),
        "ssh_keys": [ssh_key_id],
        "user_data": user_data,
        "backups": False,
        "ipv6": False,
        "monitoring": False,
        "tags": ["amoeba-e2e"],
    }
    resp = requests.post(f"{DO_API}/droplets", headers=do_headers(), json=payload)
    resp.raise_for_status()
    droplet_id = resp.json()["droplet"]["id"]
    print(f"Created droplet '{name}' id={droplet_id}")
    return droplet_id


def wait_for_droplet(droplet_id: int, timeout: int = 300) -> str:
    """Poll until the droplet is active and return its public IPv4."""
    deadline = time.time() + timeout
    while time.time() < deadline:
        resp = requests.get(f"{DO_API}/droplets/{droplet_id}", headers=do_headers())
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


def load_private_key(path: str):
    """Load an RSA or Ed25519 private key."""
    for loader in (paramiko.RSAKey, paramiko.Ed25519Key):
        try:
            return loader.from_private_key_file(path)
        except paramiko.ssh_exception.SSHException:
            continue
    raise RuntimeError(f"Could not load SSH private key from {path}")


def wait_for_ssh(ip: str, private_key: str, timeout: int = 300) -> None:
    """Wait until port 22 accepts connections and auth succeeds."""
    deadline = time.time() + timeout
    key = load_private_key(private_key)
    while time.time() < deadline:
        try:
            client = paramiko.SSHClient()
            client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
            client.connect(ip, username="root", pkey=key, timeout=10, banner_timeout=30)
            client.close()
            print("SSH is ready")
            return
        except (socket.timeout, paramiko.ssh_exception.SSHException, OSError):
            time.sleep(5)
    raise RuntimeError("Timed out waiting for SSH")


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


def login(ip: str, username: str, password: str) -> str:
    """Obtain a JWT for the given user."""
    resp = requests.post(
        f"http://{ip}/auth/login",
        json={"username": username, "password": password},
        timeout=10,
    )
    resp.raise_for_status()
    return resp.json()["token"]


def create_user(ip: str, admin_token: str, username: str, password: str, roles: list[str]) -> None:
    """Create a user via the admin API."""
    resp = requests.post(
        f"http://{ip}/admin/users",
        headers={"Authorization": f"Bearer {admin_token}"},
        json={"username": username, "password": password, "roles": roles, "org_id": "org_e2e"},
        timeout=10,
    )
    resp.raise_for_status()
    print(f"Created user '{username}' with roles {roles}")


def test_admin_apps(ip: str, admin_token: str) -> None:
    """Install and list an app via the admin API."""
    package_path = Path(__file__).parent / "../examples/app-packages/hello-world"
    zip_path = "/tmp/hello-world.amoeba.zip"
    os.system(f"cd {package_path} && zip -r {zip_path} amoeba.yaml compose.yaml > /dev/null")

    with open(zip_path, "rb") as f:
        resp = requests.post(
            f"http://{ip}/admin/apps",
            headers={"Authorization": f"Bearer {admin_token}"},
            files={"package": ("hello-world.amoeba.zip", f, "application/zip")},
            data={"values": '{"env":{"GREETING":"e2e test"}}'},
            timeout=30,
        )
    resp.raise_for_status()
    print(f"Installed hello-world app: {resp.json()}")

    resp = requests.get(
        f"http://{ip}/admin/apps", headers={"Authorization": f"Bearer {admin_token}"}, timeout=10
    )
    resp.raise_for_status()
    print(f"Installed apps: {resp.json()['apps']}")
    assert "hello-world" in resp.json()["apps"]


def test_user_groups_and_rbac(ip: str, admin_token: str) -> tuple[str, str]:
    """Create users with different roles and verify their API access.
    Returns the analyst and viewer tokens."""
    create_user(ip, admin_token, "analyst", "analyst123", ["analyst"])
    create_user(ip, admin_token, "viewer", "viewer123", ["viewer"])

    analyst_token = login(ip, "analyst", "analyst123")
    viewer_token = login(ip, "viewer", "viewer123")

    # Analyst can read (GET) and add (POST) the hello-world service.
    for user, token, method, path, expected in [
        ("analyst", analyst_token, "GET", "/v1/hello-world/", 200),
        ("analyst", analyst_token, "POST", "/v1/hello-world/", 200),
        ("viewer", viewer_token, "GET", "/v1/hello-world/", 200),
        ("viewer", viewer_token, "POST", "/v1/hello-world/", 403),
    ]:
        resp = requests.request(
            method,
            f"http://{ip}{path}",
            headers={"Authorization": f"Bearer {token}"},
            timeout=10,
        )
        assert resp.status_code == expected, (
            f"{user} {method} {path} expected {expected}, got {resp.status_code}"
        )
        print(f"{user} {method} {path} -> {expected}")

    # Non-admin users cannot access the admin apps API.
    for user, token in [("analyst", analyst_token), ("viewer", viewer_token)]:
        resp = requests.get(
            f"http://{ip}/admin/apps",
            headers={"Authorization": f"Bearer {token}"},
            timeout=10,
        )
        assert resp.status_code == 403, f"{user} GET /admin/apps expected 403, got {resp.status_code}"
        print(f"{user} GET /admin/apps -> 403")


def test_error_cases_and_protection(ip: str, admin_token: str, analyst_token: str) -> None:
    """Verify Amoeba fails closed on common error and abuse cases."""
    # 1. No token on a private service -> 401
    resp = requests.get(f"http://{ip}/v1/hello-world/", timeout=10)
    assert resp.status_code == 401, f"expected 401 without token, got {resp.status_code}"
    print("no token -> 401")

    # 2. Invalid token format -> 401
    resp = requests.get(
        f"http://{ip}/v1/hello-world/",
        headers={"Authorization": "Bearer not-a-real-token"},
        timeout=10,
    )
    assert resp.status_code == 401, f"expected 401 for invalid token, got {resp.status_code}"
    print("invalid token -> 401")

    # 3. Wrong password -> 401
    resp = requests.post(
        f"http://{ip}/auth/login",
        json={"username": "admin", "password": "wrong-password"},
        timeout=10,
    )
    assert resp.status_code == 401, f"expected 401 for wrong password, got {resp.status_code}"
    print("wrong password -> 401")

    # 4. Unknown user -> 401
    resp = requests.post(
        f"http://{ip}/auth/login",
        json={"username": "nobody", "password": "whatever"},
        timeout=10,
    )
    assert resp.status_code == 401, f"expected 401 for unknown user, got {resp.status_code}"
    print("unknown user -> 401")

    # 5. Unknown service -> 404
    resp = requests.get(
        f"http://{ip}/v1/does-not-exist/",
        headers={"Authorization": f"Bearer {admin_token}"},
        timeout=10,
    )
    assert resp.status_code == 404, f"expected 404 for unknown service, got {resp.status_code}"
    print("unknown service -> 404")

    # 6. Duplicate user -> 409
    resp = requests.post(
        f"http://{ip}/admin/users",
        headers={"Authorization": f"Bearer {admin_token}"},
        json={"username": "analyst", "password": "x", "roles": ["viewer"], "org_id": "org_e2e"},
        timeout=10,
    )
    assert resp.status_code == 409, f"expected 409 for duplicate user, got {resp.status_code}"
    print("duplicate user -> 409")

    # 7. Install request missing package -> 400
    resp = requests.post(
        f"http://{ip}/admin/apps",
        headers={"Authorization": f"Bearer {admin_token}"},
        data={"values": "{}"},
        timeout=10,
    )
    assert resp.status_code == 400, f"expected 400 for missing package, got {resp.status_code}"
    print("missing package -> 400")

    # 8. Invalid zip package -> 400
    resp = requests.post(
        f"http://{ip}/admin/apps",
        headers={"Authorization": f"Bearer {admin_token}"},
        files={"package": ("bad.zip", b"not-a-zip", "application/zip")},
        data={"values": "{}"},
        timeout=10,
    )
    assert resp.status_code == 400, f"expected 400 for invalid zip, got {resp.status_code}"
    print("invalid zip -> 400")

    # 9. Non-admin creating a user -> 403
    resp = requests.post(
        f"http://{ip}/admin/users",
        headers={"Authorization": f"Bearer {analyst_token}"},
        json={"username": "hacker", "password": "x", "roles": ["admin"], "org_id": "org_e2e"},
        timeout=10,
    )
    assert resp.status_code == 403, f"expected 403 for non-admin user create, got {resp.status_code}"
    print("non-admin create user -> 403")

    # 10. Revoked token cannot be used -> 401
    revocable = login(ip, "admin", "admin123")
    resp = requests.post(
        f"http://{ip}/auth/revoke",
        headers={"Authorization": f"Bearer {revocable}"},
        timeout=10,
    )
    resp.raise_for_status()
    resp = requests.get(
        f"http://{ip}/admin/apps",
        headers={"Authorization": f"Bearer {revocable}"},
        timeout=10,
    )
    assert resp.status_code == 401, f"expected 401 for revoked token, got {resp.status_code}"
    print("revoked token -> 401")


def test_proxy_and_scale_to_zero(ip: str, token: str) -> None:
    """Hit the app endpoint, verify the container is running, wait for cooldown,
    then verify it stopped."""
    # The hello-world app in the example uses the compose stack named "hello-world".
    # We proxy through Caddy on port 80.
    resp = requests.get(
        f"http://{ip}/v1/hello-world/",
        headers={"Authorization": f"Bearer {token}"},
        timeout=30,
    )
    resp.raise_for_status()
    print(f"Proxy response: {resp.text.strip()}")
    assert "e2e test" in resp.text

    # Verify container is running
    ssh_run(ip, "docker ps --filter name=amoeba-hello-world --format '{{.Names}}'")

    # Wait for cooldown (30s in the cloud-init services.json)
    print("Waiting 45s for cooldown...")
    time.sleep(45)

    # Verify container stopped
    out = ssh_run(ip, "docker ps --filter name=amoeba-hello-world --format '{{.Names}}'")
    assert "amoeba-hello-world" not in out, "Container should have stopped after cooldown"
    print("Container scaled to zero as expected")


def ssh_run(ip: str, command: str) -> str:
    """Run a command on the droplet via SSH and return stdout."""
    key = load_private_key(env("DO_SSH_PRIVATE_KEY"))
    client = paramiko.SSHClient()
    client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
    client.connect(ip, username="root", pkey=key, timeout=30)
    stdin, stdout, stderr = client.exec_command(command)
    exit_code = stdout.channel.recv_exit_status()
    out = stdout.read().decode()
    err = stderr.read().decode()
    client.close()
    if exit_code != 0:
        raise RuntimeError(f"SSH command failed ({exit_code}): {err or out}")
    return out


def create_admin_user(ip: str) -> None:
    """Seed the admin user via amoeba-admin inside the running container."""
    ssh_run(
        ip,
        "cd /opt/amoeba && docker compose exec -T orchestrator amoeba-admin add-user "
        "admin admin123 --roles admin --org org_e2e --users-file /etc/amoeba/users.json",
    )
    print("Created admin user")


def destroy_droplet(droplet_id: int) -> None:
    """Delete the droplet."""
    resp = requests.delete(f"{DO_API}/droplets/{droplet_id}", headers=do_headers())
    if resp.status_code == 204:
        print(f"Destroyed droplet {droplet_id}")
    else:
        print(f"Failed to destroy droplet {droplet_id}: {resp.status_code} {resp.text}")


def load_user_data() -> str:
    """Load cloud-init script and substitute env vars."""
    path = Path(__file__).parent / "e2e-cloud-init.sh"
    data = path.read_text()
    data = data.replace(
        'JWT_SECRET="${AMOEBA_LOCAL_JWT_SECRET:-change-me-in-production}"',
        f'JWT_SECRET="{env("AMOEBA_LOCAL_JWT_SECRET")}"',
    )
    data = data.replace(
        'AGE_SECRET="${AMOEBA_AGE_SECRET_KEY:-}"',
        f'AGE_SECRET="{env("AMOEBA_AGE_SECRET_KEY", "")}"',
    )
    return data


def main() -> int:
    parser = argparse.ArgumentParser(description="Amoeba nightly e2e on DigitalOcean")
    parser.add_argument("--name", default="amoeba-nightly-e2e", help="Droplet name")
    parser.add_argument("--keep", action="store_true", help="Skip droplet destruction")
    args = parser.parse_args()

    droplet_id: int | None = None
    try:
        print("Loading cloud-init user data...")
        user_data = load_user_data()

        print("Registering SSH key...")
        ssh_key_id = create_ssh_key(args.name, env("DO_SSH_PUBLIC_KEY"))

        print("Creating droplet...")
        droplet_id = create_droplet(args.name, ssh_key_id, user_data)

        print("Waiting for droplet...")
        ip = wait_for_droplet(droplet_id)

        print("Waiting for SSH...")
        wait_for_ssh(ip, env("DO_SSH_PRIVATE_KEY"))

        print("Waiting for Amoeba...")
        wait_for_amoeba(ip)

        print("Creating admin user...")
        create_admin_user(ip)

        print("Logging in as admin...")
        admin_token = login(ip, "admin", "admin123")

        print("Testing admin apps API...")
        test_admin_apps(ip, admin_token)

        print("Testing user groups and RBAC...")
        analyst_token, _viewer_token = test_user_groups_and_rbac(ip, admin_token)

        print("Testing error cases and protection...")
        test_error_cases_and_protection(ip, admin_token, analyst_token)

        print("Testing proxy and scale-to-zero...")
        test_proxy_and_scale_to_zero(ip, admin_token)

        print("All e2e checks passed.")
        return 0
    except Exception as e:
        print(f"E2E test failed: {e}")
        traceback.print_exc()
        return 1
    finally:
        if droplet_id is not None and not args.keep:
            destroy_droplet(droplet_id)


if __name__ == "__main__":
    sys.exit(main())
