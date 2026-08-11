"""Tests for deploying Obscura through Amoeba and driving it via MCP."""

from __future__ import annotations

import os
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


def _mcp_call(base_url: str, admin_token: str, method: str, params: dict, request_id: int = 1) -> dict:
    """POST a JSON-RPC request to Obscura's MCP endpoint through Amoeba's proxy.

    MCP's Streamable HTTP transport is plain request/response HTTP (unlike raw
    CDP, which requires a WebSocket upgrade Amoeba's proxy doesn't support),
    so this goes straight through `/v1/obscura/mcp` like any other route.
    """
    resp = requests.post(
        f"{base_url}/v1/obscura/mcp",
        headers={
            **auth_header(admin_token),
            "Content-Type": "application/json",
            "Accept": "application/json, text/event-stream",
        },
        json={"jsonrpc": "2.0", "id": request_id, "method": method, "params": params},
        timeout=30,
    )
    resp.raise_for_status()
    body = resp.json()
    assert "error" not in body, f"MCP call {method} returned an error: {body['error']}"
    return body["result"]


def _tool_text(result: dict) -> str:
    """Extract the concatenated text content from an MCP tools/call result."""
    return "".join(part["text"] for part in result["content"] if part["type"] == "text")


def test_obscura_install_and_mcp_endpoint(base_url: str, admin_token: str) -> None:
    """Install Obscura and verify its MCP endpoint is reachable through Amoeba."""
    package_dir = Path(__file__).parent.parent.parent / "examples" / "app-packages" / "obscura"
    _install_package(base_url, admin_token, package_dir, "obscura")
    wait_for_app_ready(base_url, admin_token, "obscura", timeout_seconds=300)

    result = _mcp_call(
        base_url,
        admin_token,
        "initialize",
        {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "amoeba-e2e", "version": "1"},
        },
    )
    print(f"MCP initialize result: {result}")
    assert result["serverInfo"]["name"] == "obscura-mcp"


def test_obscura_can_navigate_and_extract(base_url: str, admin_token: str) -> None:
    """Drive Obscura's browser via MCP tools/call through Amoeba and verify page content."""
    nav_result = _mcp_call(
        base_url,
        admin_token,
        "tools/call",
        {"name": "browser_navigate", "arguments": {"url": "https://example.com"}},
    )
    print(f"navigate result: {_tool_text(nav_result)}")

    snapshot_result = _mcp_call(
        base_url,
        admin_token,
        "tools/call",
        {"name": "browser_snapshot", "arguments": {}},
    )
    text = _tool_text(snapshot_result)
    print(f"snapshot: {text[:200]}")
    assert "Example Domain" in text, "obscura snapshot did not contain expected page content"
