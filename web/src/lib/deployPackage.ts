// Builds the multipart body for POST /admin/apps: a .zip holding amoeba.yaml
// plus the install values. Every env var and secret is declared in the
// manifest schema, because the server rejects undeclared env values and only
// injects declared secrets.

export interface EnvPair {
  key: string;
  value: string;
  isSecret: boolean;
}

export interface DeployInput {
  name: string;
  image: string;
  description: string;
  port: number;
  cooldownSeconds: number;
  isPublic: boolean;
  /** Call the app as a local client: no Origin, Host localhost (e.g. MCP servers). */
  localUpstream?: boolean;
  envPairs: EnvPair[];
}

export interface InstallValues {
  env: Record<string, string>;
  secrets: Record<string, string>;
}

// Env var names; also safe as secret file names on the server.
const KEY_PATTERN = /^[A-Za-z_][A-Za-z0-9_]*$/;

/** Splits the form rows into env and secret values. Throws on a bad or repeated key. */
export function buildInstallValues(envPairs: EnvPair[]): InstallValues {
  const values: InstallValues = { env: {}, secrets: {} };
  const seen = new Set<string>();
  for (const pair of envPairs) {
    const key = pair.key.trim();
    if (!key) continue;
    if (!KEY_PATTERN.test(key)) {
      throw new Error(`Invalid variable name: ${key}`);
    }
    if (seen.has(key)) {
      throw new Error(`Variable ${key} is listed more than once`);
    }
    seen.add(key);
    if (pair.isSecret) {
      values.secrets[key] = pair.value;
    } else {
      values.env[key] = pair.value;
    }
  }
  return values;
}

/** The amoeba.yaml manifest (JSON is valid YAML). */
export function buildManifest(input: DeployInput, values: InstallValues): object {
  const env: Record<string, object> = {};
  for (const key of Object.keys(values.env)) {
    env[key] = { type: 'string' };
  }
  const secrets: Record<string, object> = {};
  for (const key of Object.keys(values.secrets)) {
    secrets[key] = { required: true, inject: 'env' };
  }
  return {
    api_version: 'v1',
    name: input.name,
    version: '1.0.0',
    description: input.description,
    app: { type: 'image', image: input.image },
    placement: { port: input.port, cooldown_seconds: input.cooldownSeconds },
    public: input.isPublic,
    ...(input.localUpstream ? { local_upstream: true } : {}),
    schema: { env, secrets },
  };
}

export function buildDeployForm(input: DeployInput): FormData {
  const values = buildInstallValues(input.envPairs);
  const manifest = JSON.stringify(buildManifest(input, values), null, 2);
  const zip = buildZip([{ name: 'amoeba.yaml', data: new TextEncoder().encode(manifest) }]);
  const form = new FormData();
  form.append('package', new Blob([zip], { type: 'application/zip' }), `${input.name}.zip`);
  form.append('values', JSON.stringify(values));
  return form;
}

const CRC_TABLE = (() => {
  const table = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) {
      c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    }
    table[n] = c >>> 0;
  }
  return table;
})();

export function crc32(data: Uint8Array): number {
  let crc = 0xffffffff;
  for (const byte of data) {
    crc = CRC_TABLE[(crc ^ byte) & 0xff] ^ (crc >>> 8);
  }
  return (crc ^ 0xffffffff) >>> 0;
}

/** Minimal zip writer: stored (uncompressed) entries, no extra fields. */
export function buildZip(files: { name: string; data: Uint8Array }[]): Uint8Array<ArrayBuffer> {
  const encoder = new TextEncoder();
  const local: Uint8Array[] = [];
  const central: Uint8Array[] = [];
  let offset = 0;

  for (const file of files) {
    const name = encoder.encode(file.name);
    const crc = crc32(file.data);
    const size = file.data.length;

    const header = new DataView(new ArrayBuffer(30));
    header.setUint32(0, 0x04034b50, true);
    header.setUint16(4, 20, true); // version needed
    header.setUint32(14, crc, true);
    header.setUint32(18, size, true);
    header.setUint32(22, size, true);
    header.setUint16(26, name.length, true);
    local.push(new Uint8Array(header.buffer), name, file.data);

    const entry = new DataView(new ArrayBuffer(46));
    entry.setUint32(0, 0x02014b50, true);
    entry.setUint16(4, 20, true); // version made by
    entry.setUint16(6, 20, true); // version needed
    entry.setUint32(16, crc, true);
    entry.setUint32(20, size, true);
    entry.setUint32(24, size, true);
    entry.setUint16(28, name.length, true);
    entry.setUint32(42, offset, true);
    central.push(new Uint8Array(entry.buffer), name);

    offset += 30 + name.length + size;
  }

  const centralSize = central.reduce((n, part) => n + part.length, 0);
  const end = new DataView(new ArrayBuffer(22));
  end.setUint32(0, 0x06054b50, true);
  end.setUint16(8, files.length, true);
  end.setUint16(10, files.length, true);
  end.setUint32(12, centralSize, true);
  end.setUint32(16, offset, true);

  const parts = [...local, ...central, new Uint8Array(end.buffer)];
  const out = new Uint8Array(parts.reduce((n, part) => n + part.length, 0));
  let pos = 0;
  for (const part of parts) {
    out.set(part, pos);
    pos += part.length;
  }
  return out;
}

// Source builds: POST /admin/apps/<name>/build turns a Git repo or a pasted
// Dockerfile into an image on the Amoeba host; the deploy then runs that image.

export type BuildRequest =
  | { source: 'git'; url: string; ref?: string }
  | { source: 'dockerfile'; dockerfile: string; context_url?: string };

export interface BuildFailure {
  error: string;
  log: string[];
}

/** Lowercase because the name becomes the image tag `amoeba/<name>:latest`. */
export function validateServiceName(name: string): string | null {
  return /^[a-z0-9][a-z0-9._-]{0,127}$/.test(name)
    ? null
    : 'Service name must use lowercase letters, digits, "-", "_" or "." and start with a letter or digit';
}

export function buildRequestFor(
  tab: 'git' | 'dockerfile' | 'image',
  fields: { gitUrl: string; dockerfile: string; contextUrl: string },
): BuildRequest | null {
  switch (tab) {
    case 'git':
      return { source: 'git', url: fields.gitUrl.trim() };
    case 'dockerfile': {
      const contextUrl = fields.contextUrl.trim();
      return contextUrl
        ? { source: 'dockerfile', dockerfile: fields.dockerfile, context_url: contextUrl }
        : { source: 'dockerfile', dockerfile: fields.dockerfile };
    }
    case 'image':
      return null;
  }
}

/** Runs the build and returns the image tag; throws a BuildError with the log on failure. */
export async function requestBuild(
  name: string,
  request: BuildRequest,
  token: string | null,
  fetchFn: typeof fetch = fetch,
): Promise<string> {
  const res = await fetchFn(`/admin/apps/${encodeURIComponent(name)}/build`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${token}` },
    body: JSON.stringify(request),
  });
  const text = await res.text();
  let body: any = null;
  try {
    body = JSON.parse(text);
  } catch {
    // Non-JSON error (e.g. 403 from the auth layer); report the raw text.
  }
  if (!res.ok) {
    throw new BuildError(body?.error || text || `Build failed with status ${res.status}`, body?.log ?? []);
  }
  return body.image as string;
}

export class BuildError extends Error {
  constructor(message: string, public log: string[]) {
    super(message);
    this.name = 'BuildError';
  }
}

/** The latest build for a service, as `GET /admin/apps/<name>/build` reports it. */
export interface BuildStatus {
  state: 'running' | 'succeeded' | 'failed' | 'cancelled';
  step: string;
  elapsed_seconds: number;
  log: string[];
}

/** Reads build progress; null when there is none yet or the request fails. */
export async function fetchBuildStatus(
  name: string,
  token: string | null,
  fetchFn: typeof fetch = fetch,
): Promise<BuildStatus | null> {
  try {
    const res = await fetchFn(`/admin/apps/${encodeURIComponent(name)}/build`, {
      headers: { Authorization: `Bearer ${token}` },
    });
    return res.ok ? ((await res.json()) as BuildStatus) : null;
  } catch {
    return null;
  }
}

/** Stops the running build; the pending build request then fails as cancelled. */
export async function cancelBuild(
  name: string,
  token: string | null,
  fetchFn: typeof fetch = fetch,
): Promise<void> {
  await fetchFn(`/admin/apps/${encodeURIComponent(name)}/build`, {
    method: 'DELETE',
    headers: { Authorization: `Bearer ${token}` },
  });
}

/** `42s`, `3m 05s`, `1h 02m`. */
export function formatElapsed(seconds: number): string {
  const s = Math.max(0, Math.floor(seconds));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${String(s % 60).padStart(2, '0')}s`;
  return `${Math.floor(m / 60)}h ${String(m % 60).padStart(2, '0')}m`;
}
