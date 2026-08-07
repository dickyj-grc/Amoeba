"""SSH client helpers for the Amoeba e2e tests."""

from __future__ import annotations

import socket
import time
from pathlib import Path

import paramiko


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


class SshClient:
    """Small wrapper around paramiko for running commands on the droplet."""

    def __init__(self, ip: str, private_key: str) -> None:
        self.ip = ip
        self.private_key = Path(private_key).expanduser().as_posix()
        self._key = load_private_key(self.private_key)

    def run(self, command: str, timeout: int = 30) -> str:
        """Run a command on the droplet via SSH and return stdout."""
        client = paramiko.SSHClient()
        client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
        client.connect(self.ip, username="root", pkey=self._key, timeout=timeout)
        try:
            stdin, stdout, stderr = client.exec_command(command)
            exit_code = stdout.channel.recv_exit_status()
            out = stdout.read().decode()
            err = stderr.read().decode()
            if exit_code != 0:
                raise RuntimeError(f"SSH command failed ({exit_code}): {err or out}")
            return out
        finally:
            client.close()
