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

## Encrypted secrets (age)

Amoeba can decrypt age-encrypted secret files bundled inside the package. This lets authors ship secrets that only the target server can read.

1. Generate an age key pair on the Amoeba server and keep the secret key safe:

```bash
age-keygen -o amoeba.age.key
export AMOEBA_AGE_SECRET_KEY=$(grep AGE-SECRET-KEY amoeba.age.key)
```

2. In `amoeba.yaml`, declare the secret with a `file`:

```yaml
schema:
  secrets:
    API_KEY:
      description: API key
      required: true
      file: secrets/api_key.age
```

3. Encrypt the secret with the server's age public key:

```bash
age -r age1... -o secrets/api_key.age < api_key.txt
```

4. Include `secrets/api_key.age` in the zip. At install time Amoeba decrypts it using `AMOEBA_AGE_SECRET_KEY` and writes the plaintext to `/etc/amoeba/secrets/hello-world/API_KEY`.

User-provided secret values still take precedence over encrypted files.

## Uninstall

```bash
curl -X DELETE http://localhost:8080/admin/apps/hello-world \
  -H "Authorization: Bearer $ADMIN_JWT"
```
