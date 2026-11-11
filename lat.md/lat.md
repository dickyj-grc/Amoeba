This directory defines the high-level concepts, business logic, and architecture of this project using markdown. It is managed by [lat.md](https://www.npmjs.com/package/lat.md) — a tool that anchors source code to these definitions. Install the `lat` command with `npm i -g lat.md` and run `lat --help`.

- [[gateway]] — How the proxy names operations, forwards caller identity, and injects upstream credentials.
- [[secrets]] — How app secrets are declared, stored, validated, and sent from the web console.
- [[deploy]] — Console deploy flow and building images from a Git repo or Dockerfile on the Amoeba host.
