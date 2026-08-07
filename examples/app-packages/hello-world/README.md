# Hello World Amoeba App Package

This is a minimal example of an Amoeba App Package.

## Files

- `amoeba.yaml` — app manifest, routing, permissions, and form schema
- `compose.yaml` — standard Docker Compose file

## Build the package

From this directory:

```bash
zip -r hello-world.amoeba.zip amoeba.yaml compose.yaml
```

## Install via curl

With an admin JWT in `$ADMIN_JWT`:

```bash
curl -X POST http://localhost:8080/admin/apps \
  -H "Authorization: Bearer $ADMIN_JWT" \
  -F "package=@hello-world.amoeba.zip" \
  -F 'values={"env":{"GREETING":"Hello from curl!"}};type=application/json'
```

Then visit:

```bash
curl -H "Authorization: Bearer $ADMIN_JWT" \
  http://localhost:8080/v1/hello-world/
```

## Uninstall

```bash
curl -X DELETE http://localhost:8080/admin/apps/hello-world \
  -H "Authorization: Bearer $ADMIN_JWT"
```
