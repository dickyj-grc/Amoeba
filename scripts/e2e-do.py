#!/usr/bin/env python3
"""
Nightly end-to-end test for Amoeba on DigitalOcean.

This script is a thin wrapper around pytest so existing callers keep working.
For interactive runs, prefer: pytest scripts/e2e/ -v

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
import subprocess
import sys
from pathlib import Path


def main() -> int:
    parser = argparse.ArgumentParser(description="Amoeba nightly e2e on DigitalOcean")
    parser.add_argument("--name", default="amoeba-nightly-e2e", help="Droplet name")
    parser.add_argument("--keep", action="store_true", help="Skip droplet destruction")
    args, pytest_args = parser.parse_known_args()

    e2e_dir = Path(__file__).parent / "e2e"
    if not e2e_dir.is_dir():
        print(f"E2E test directory not found: {e2e_dir}", file=sys.stderr)
        return 1

    cmd = [
        sys.executable,
        "-m",
        "pytest",
        str(e2e_dir),
        "-v",
        f"--droplet-name={args.name}",
    ]
    if args.keep:
        cmd.append("--keep-droplet")
    cmd.extend(pytest_args)

    # Forward any extra pytest args from the environment, e.g. PYTEST_ADDOPTS.
    return subprocess.call(cmd, cwd=Path(__file__).parent.parent)


if __name__ == "__main__":
    sys.exit(main())
