This directory defines the high-level concepts, business logic, and architecture of this project using markdown. It is managed by [lat.md](https://www.npmjs.com/package/lat.md) — a tool that anchors source code to these definitions. Install the `lat` command with `npm i -g lat.md` and run `lat --help`.

- [[gateway]] — How the proxy names operations, forwards caller identity, and injects upstream credentials.
- [[secrets]] — How app secrets are declared, stored, validated, and sent from the web console.
- [[deploy]] — Console deploy flow and building images from a Git repo or Dockerfile on the Amoeba host.
- [[e2e]] — How the nightly DigitalOcean e2e picks the commit and orchestrator image it tests.
- [[dev]] — Running Amoeba locally with `scripts/dev.sh`: native mode on macOS with Apple containers, or Docker Compose.
- [[database]] — The database service: approved tables and queries over PostgreSQL with per-role column masking, guardrails and MCP tools.
