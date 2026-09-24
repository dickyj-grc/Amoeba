"""Nightly checks for per-org access, scoped tokens, proxy secrets, and quotas.

hello-world stays tenant-unaware (any org with a listed role). These tests
install separate image apps so that behavior is unchanged.
"""

from __future__ import annotations

import io
import json
import zipfile

import requests

from conftest import auth_header, login, wait_for_app_ready

ECHO_IMAGE = "hashicorp/http-echo:latest"
ECHO_PORT = 5678
PROXY_SECRET = "e2e-proxy-secret-value"


def _package(manifest: str) -> bytes:
    buffer = io.BytesIO()
    with zipfile.ZipFile(buffer, "w") as archive:
        archive.writestr("amoeba.yaml", manifest)
    return buffer.getvalue()


def _install(base_url: str, admin_token: str, manifest: str, values: dict | None = None) -> None:
    resp = requests.post(
        f"{base_url}/admin/apps",
        headers=auth_header(admin_token),
        files={"package": ("app.zip", _package(manifest), "application/zip")},
        data={"values": json.dumps(values or {})},
        timeout=60,
    )
    assert resp.status_code == 201, resp.text


def _image_manifest(name: str, body: str) -> str:
    return f"""
api_version: v1
name: {name}
app:
  type: image
  image: {ECHO_IMAGE}
placement:
  port: {ECHO_PORT}
{body}
"""


def _call(base_url: str, name: str, method: str, token: str, **headers: str) -> requests.Response:
    extra = {key: value for key, value in headers.items() if value is not None}
    return requests.request(
        method,
        f"{base_url}/v1/{name}/",
        headers={**auth_header(token), **extra},
        timeout=30,
    )


def _reason(resp: requests.Response) -> str | None:
    return resp.headers.get("x-amoeba-rejection-reason")


def test_multi_tenant_permissions_differ_by_org(
    base_url: str, admin_token: str
) -> None:
    """org_alpha and org_beta share one service and do not share one policy."""
    _ensure_app(
        base_url,
        admin_token,
        "tenant-echo",
        _image_manifest(
            "tenant-echo",
            """
tenant_permissions:
  org_alpha:
    read: [viewer, analyst, mcp:tenant-echo]
    add: [analyst]
  org_beta:
    read: [analyst]
""",
        ),
    )

    _ensure_user(base_url, admin_token, "alpha_viewer", ["viewer"], "org_alpha")
    _ensure_user(base_url, admin_token, "alpha_analyst", ["analyst"], "org_alpha")
    _ensure_user(base_url, admin_token, "beta_viewer", ["viewer"], "org_beta")
    _ensure_user(base_url, admin_token, "beta_analyst", ["analyst"], "org_beta")
    _ensure_user(base_url, admin_token, "beta_admin", ["admin"], "org_beta")

    alpha_viewer = login(base_url, "alpha_viewer", "pw")
    alpha_analyst = login(base_url, "alpha_analyst", "pw")
    beta_viewer = login(base_url, "beta_viewer", "pw")
    beta_analyst = login(base_url, "beta_analyst", "pw")
    beta_admin = login(base_url, "beta_admin", "pw")

    got = _call(base_url, "tenant-echo", "GET", alpha_viewer, **{"X-Amoeba-Org": "org_beta"})
    assert got.status_code == 200, got.text

    got = _call(base_url, "tenant-echo", "POST", alpha_viewer)
    assert got.status_code == 403 and _reason(got) == "role"

    got = _call(base_url, "tenant-echo", "POST", alpha_analyst)
    assert got.status_code == 200, got.text

    got = _call(base_url, "tenant-echo", "GET", beta_viewer)
    assert got.status_code == 403 and _reason(got) == "role"

    got = _call(base_url, "tenant-echo", "GET", beta_analyst)
    assert got.status_code == 200, got.text

    got = _call(base_url, "tenant-echo", "POST", beta_analyst)
    assert got.status_code == 403 and _reason(got) == "role"

    # The admin role is not implied, on this org or on the operator's org.
    got = _call(base_url, "tenant-echo", "GET", beta_admin)
    assert got.status_code == 403 and _reason(got) == "role"
    got = _call(base_url, "tenant-echo", "GET", admin_token)
    assert got.status_code == 403 and _reason(got) == "tenant"


def test_single_tenant_service_rejects_other_orgs(base_url: str, admin_token: str) -> None:
    _ensure_app(
        base_url,
        admin_token,
        "solo-echo",
        _image_manifest(
            "solo-echo",
            """
tenant: org_alpha
permissions:
  read: [analyst]
""",
        ),
    )

    # Users may already exist when the multi-tenant test ran first.
    _ensure_user(base_url, admin_token, "alpha_analyst", ["analyst"], "org_alpha")
    _ensure_user(base_url, admin_token, "beta_analyst", ["analyst"], "org_beta")
    alpha = login(base_url, "alpha_analyst", "pw")
    beta = login(base_url, "beta_analyst", "pw")

    assert _call(base_url, "solo-echo", "GET", alpha).status_code == 200
    got = _call(base_url, "solo-echo", "GET", beta)
    assert got.status_code == 403 and _reason(got) == "tenant"


def test_scoped_token_is_limited_to_its_org(base_url: str, admin_token: str) -> None:
    _ensure_app(
        base_url,
        admin_token,
        "tenant-echo",
        _image_manifest(
            "tenant-echo",
            """
tenant_permissions:
  org_alpha:
    read: [viewer, analyst, mcp:tenant-echo]
    add: [analyst]
  org_beta:
    read: [analyst]
""",
        ),
    )

    issued = requests.post(
        f"{base_url}/admin/apps/tenant-echo/token",
        headers={**auth_header(admin_token), "Content-Type": "application/json"},
        json={"org_id": "org_alpha"},
        timeout=10,
    )
    assert issued.status_code == 200, issued.text
    body = issued.json()
    assert body["org_id"] == "org_alpha"
    assert body["role"] == "mcp:tenant-echo"
    assert _call(base_url, "tenant-echo", "GET", body["token"]).status_code == 200
    # The scoped role can read but was not granted add.
    got = _call(base_url, "tenant-echo", "POST", body["token"])
    assert got.status_code == 403 and _reason(got) == "role"

    other = requests.post(
        f"{base_url}/admin/apps/tenant-echo/token",
        headers={**auth_header(admin_token), "Content-Type": "application/json"},
        json={"org_id": "org_beta"},
        timeout=10,
    )
    assert other.status_code == 200, other.text
    got = _call(base_url, "tenant-echo", "GET", other.json()["token"])
    assert got.status_code == 403 and _reason(got) == "role"

    missing = requests.post(
        f"{base_url}/admin/apps/tenant-echo/token",
        headers={**auth_header(admin_token), "Content-Type": "application/json"},
        json={"org_id": "org_gamma"},
        timeout=10,
    )
    assert missing.status_code == 403


def test_proxy_secret_is_not_placed_in_the_container(
    base_url: str, admin_token: str, ssh
) -> None:
    _install(
        base_url,
        admin_token,
        _image_manifest(
            "secret-echo",
            """
tenant: org_alpha
permissions:
  read: [analyst]
schema:
  secrets:
    API_KEY:
      required: true
      inject: proxy
""",
        ),
        {"secrets": {"API_KEY": PROXY_SECRET}},
    )
    wait_for_app_ready(base_url, admin_token, "secret-echo")

    catalog = json.loads(ssh.run("cat /opt/amoeba/config/services.json"))
    service = catalog["services"]["secret-echo"]
    assert service["header_from_secret"]["API_KEY"] == "secret-echo/API_KEY"
    assert "API_KEY" not in service.get("env_from_secret", {})
    on_disk = ssh.run("cat /opt/amoeba/config/secrets/secret-echo/API_KEY").strip()
    assert on_disk == PROXY_SECRET

    env = ssh.run("docker inspect -f '{{json .Config.Env}}' secret-echo")
    assert PROXY_SECRET not in env

    _ensure_user(base_url, admin_token, "alpha_analyst", ["analyst"], "org_alpha")
    token = login(base_url, "alpha_analyst", "pw")
    got = _call(base_url, "secret-echo", "GET", token)
    assert got.status_code == 200
    assert PROXY_SECRET not in got.text


def test_policy_patch_replaces_a_single_tenant_map(base_url: str, admin_token: str) -> None:
    _install(
        base_url,
        admin_token,
        _image_manifest(
            "patch-echo",
            """
tenant: org_alpha
permissions:
  read: [analyst]
""",
        ),
    )
    wait_for_app_ready(base_url, admin_token, "patch-echo")

    resp = requests.patch(
        f"{base_url}/admin/apps/patch-echo",
        headers={**auth_header(admin_token), "Content-Type": "application/json"},
        json={
            "tenant_permissions": {
                "org_alpha": {"read": ["viewer"]},
                "org_beta": {"read": ["analyst"]},
            }
        },
        timeout=10,
    )
    assert resp.status_code == 200, resp.text

    _ensure_user(base_url, admin_token, "alpha_analyst", ["analyst"], "org_alpha")
    _ensure_user(base_url, admin_token, "alpha_viewer", ["viewer"], "org_alpha")
    _ensure_user(base_url, admin_token, "beta_analyst", ["analyst"], "org_beta")
    alpha_analyst = login(base_url, "alpha_analyst", "pw")
    alpha_viewer = login(base_url, "alpha_viewer", "pw")
    beta_analyst = login(base_url, "beta_analyst", "pw")

    # The new org map replaces the old one: alpha analysts lose read, viewers gain it.
    got = _call(base_url, "patch-echo", "GET", alpha_analyst)
    assert got.status_code == 403 and _reason(got) == "role"
    assert _call(base_url, "patch-echo", "GET", alpha_viewer).status_code == 200
    assert _call(base_url, "patch-echo", "GET", beta_analyst).status_code == 200


def test_tenant_quota_rejects_another_service_for_the_same_org(
    base_url: str, admin_token: str, ssh
) -> None:
    _ensure_app(
        base_url,
        admin_token,
        "solo-echo",
        _image_manifest(
            "solo-echo",
            """
tenant: org_alpha
permissions:
  read: [analyst]
""",
        ),
    )
    try:
        _set_quota(ssh, _org_service_count(ssh, "org_alpha"))
        resp = requests.post(
            f"{base_url}/admin/apps",
            headers=auth_header(admin_token),
            files={
                "package": (
                    "app.zip",
                    _package(
                        _image_manifest(
                            "quota-echo",
                            """
tenant: org_alpha
permissions:
  read: [analyst]
""",
                        )
                    ),
                    "application/zip",
                )
            },
            data={"values": "{}"},
            timeout=60,
        )
        assert resp.status_code == 409, resp.text
    finally:
        _set_quota(ssh, None)


def _ensure_user(base_url: str, admin_token: str, username: str, roles: list[str], org_id: str) -> None:
    resp = requests.post(
        f"{base_url}/admin/users",
        headers=auth_header(admin_token),
        json={"username": username, "password": "pw", "roles": roles, "org_id": org_id},
        timeout=10,
    )
    if resp.status_code == 409:
        return
    resp.raise_for_status()


def _ensure_app(base_url: str, admin_token: str, name: str, manifest: str) -> None:
    resp = requests.get(
        f"{base_url}/admin/apps/{name}",
        headers=auth_header(admin_token),
        timeout=10,
    )
    if resp.status_code == 200:
        return
    _install(base_url, admin_token, manifest)
    wait_for_app_ready(base_url, admin_token, name)


def _org_service_count(ssh, org: str) -> int:
    catalog = json.loads(ssh.run("cat /opt/amoeba/config/services.json"))
    count = 0
    for service in catalog["services"].values():
        if service.get("tenant") == org or org in (service.get("tenant_permissions") or {}):
            count += 1
    return count


def _set_quota(ssh, limit: int | None) -> None:
    encoded = "None" if limit is None else str(limit)
    ssh.run(
        "python3 -c \"import json; "
        "p='/opt/amoeba/config/services.json'; "
        "c=json.load(open(p)); "
        f"c['max_services_per_tenant']={encoded}; "
        "json.dump(c, open(p,'w'))\""
    )
