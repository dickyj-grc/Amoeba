"""Fail-closed error-case tests."""

from __future__ import annotations

import pytest
import requests

from conftest import auth_header, login


class TestAuthenticationErrors:
    def test_no_token_returns_401(self, base_url: str) -> None:
        resp = requests.get(f"{base_url}/v1/hello-world/", timeout=10)
        assert resp.status_code == 401

    def test_invalid_token_returns_401(self, base_url: str) -> None:
        resp = requests.get(
            f"{base_url}/v1/hello-world/",
            headers=auth_header("not-a-real-token"),
            timeout=10,
        )
        assert resp.status_code == 401

    @pytest.mark.parametrize(
        "username,password",
        [
            ("admin", "wrong-password"),
            ("nobody", "whatever"),
        ],
    )
    def test_bad_credentials_return_401(self, base_url: str, username: str, password: str) -> None:
        resp = requests.post(
            f"{base_url}/auth/login",
            json={"username": username, "password": password},
            timeout=10,
        )
        assert resp.status_code == 401


class TestAdminErrors:
    def test_unknown_service_returns_404(self, base_url: str, admin_token: str) -> None:
        resp = requests.get(
            f"{base_url}/v1/does-not-exist/",
            headers=auth_header(admin_token),
            timeout=10,
        )
        assert resp.status_code == 404

    def test_duplicate_user_returns_409(self, base_url: str, admin_token: str, analyst_token: str) -> None:
        # `analyst_token` (unused directly) guarantees the 'analyst' user already
        # exists before this runs, regardless of test collection/execution order.
        resp = requests.post(
            f"{base_url}/admin/users",
            headers=auth_header(admin_token),
            json={"username": "analyst", "password": "x", "roles": ["viewer"], "org_id": "org_e2e"},
            timeout=10,
        )
        assert resp.status_code == 409

    def test_missing_package_returns_400(self, base_url: str, admin_token: str) -> None:
        resp = requests.post(
            f"{base_url}/admin/apps",
            headers=auth_header(admin_token),
            # `files=` (not `data=`) forces genuine multipart/form-data encoding so
            # this actually exercises the handler's "missing 'package' field" path
            # instead of silently sending application/x-www-form-urlencoded.
            files={"values": (None, "{}")},
            timeout=10,
        )
        assert resp.status_code == 400

    def test_invalid_zip_returns_400(self, base_url: str, admin_token: str) -> None:
        resp = requests.post(
            f"{base_url}/admin/apps",
            headers=auth_header(admin_token),
            files={"package": ("bad.zip", b"not-a-zip", "application/zip")},
            data={"values": "{}"},
            timeout=10,
        )
        assert resp.status_code == 400

    def test_non_admin_cannot_create_user(self, base_url: str, analyst_token: str) -> None:
        resp = requests.post(
            f"{base_url}/admin/users",
            headers=auth_header(analyst_token),
            json={"username": "hacker", "password": "x", "roles": ["admin"], "org_id": "org_e2e"},
            timeout=10,
        )
        assert resp.status_code == 403


class TestTokenRevocation:
    def test_revoked_token_is_rejected(self, base_url: str) -> None:
        token = login(base_url, "admin", "admin123")
        resp = requests.post(
            f"{base_url}/auth/revoke",
            headers=auth_header(token),
            timeout=10,
        )
        resp.raise_for_status()
        resp = requests.get(
            f"{base_url}/admin/apps",
            headers=auth_header(token),
            timeout=10,
        )
        assert resp.status_code == 401
