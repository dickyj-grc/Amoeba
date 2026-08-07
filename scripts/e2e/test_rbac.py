"""Tests for user groups and role-based access control."""

from __future__ import annotations

import pytest
import requests

from conftest import auth_header


@pytest.mark.parametrize(
    "user,method,path,expected",
    [
        ("analyst", "GET", "/v1/hello-world/", 200),
        ("analyst", "POST", "/v1/hello-world/", 200),
        ("viewer", "GET", "/v1/hello-world/", 200),
        ("viewer", "POST", "/v1/hello-world/", 403),
    ],
)
def test_role_access(
    base_url: str,
    analyst_token: str,
    viewer_token: str,
    user: str,
    method: str,
    path: str,
    expected: int,
) -> None:
    """Analyst can read and write; viewer can only read."""
    token = analyst_token if user == "analyst" else viewer_token
    resp = requests.request(
        method,
        f"{base_url}{path}",
        headers=auth_header(token),
        timeout=10,
    )
    assert resp.status_code == expected, (
        f"{user} {method} {path} expected {expected}, got {resp.status_code}"
    )
    print(f"{user} {method} {path} -> {expected}")


@pytest.mark.parametrize("user", ["analyst", "viewer"])
def test_non_admin_cannot_access_admin_apps(
    base_url: str, analyst_token: str, viewer_token: str, user: str
) -> None:
    """Only admins may use the admin apps API."""
    token = analyst_token if user == "analyst" else viewer_token
    resp = requests.get(
        f"{base_url}/admin/apps",
        headers=auth_header(token),
        timeout=10,
    )
    assert resp.status_code == 403, f"{user} GET /admin/apps expected 403, got {resp.status_code}"
    print(f"{user} GET /admin/apps -> 403")
