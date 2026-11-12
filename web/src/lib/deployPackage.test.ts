import { describe, expect, it } from 'vitest';
import {
  BuildError,
  buildDeployForm,
  buildRequestFor,
  requestBuild,
  validateServiceName,
  fetchBuildStatus,
  formatElapsed,
  buildInstallValues,
  buildManifest,
  buildZip,
  crc32,
  type DeployInput,
} from './deployPackage';

const input: DeployInput = {
  name: 'pdf-parser',
  image: 'myorg/pdf-parser:v1',
  description: 'test',
  port: 8080,
  cooldownSeconds: 300,
  isPublic: false,
  envPairs: [
    { key: 'PORT', value: '8080', isSecret: false },
    { key: 'API_KEY', value: 's3cret', isSecret: true },
    { key: '  ', value: 'ignored', isSecret: false },
  ],
};

// Reads the stored entries back out, checking each CRC like the server does.
function readZip(zip: Uint8Array): Record<string, string> {
  const view = new DataView(zip.buffer, zip.byteOffset, zip.byteLength);
  const files: Record<string, string> = {};
  let pos = 0;
  while (view.getUint32(pos, true) === 0x04034b50) {
    const crc = view.getUint32(pos + 14, true);
    const size = view.getUint32(pos + 18, true);
    const nameLen = view.getUint16(pos + 26, true);
    const name = new TextDecoder().decode(zip.subarray(pos + 30, pos + 30 + nameLen));
    const data = zip.subarray(pos + 30 + nameLen, pos + 30 + nameLen + size);
    expect(crc32(data)).toBe(crc);
    files[name] = new TextDecoder().decode(data);
    pos += 30 + nameLen + size;
  }
  expect(view.getUint32(zip.length - 22, true)).toBe(0x06054b50);
  return files;
}

describe('deploy package', () => {
  it('computes the standard CRC-32', () => {
    expect(crc32(new TextEncoder().encode('123456789'))).toBe(0xcbf43926);
  });

  // @lat: [[secrets#Console deploy]]
  it('sends secrets as values and declares them in the manifest', async () => {
    const form = buildDeployForm(input);

    const values = JSON.parse(form.get('values') as string);
    expect(values).toEqual({ env: { PORT: '8080' }, secrets: { API_KEY: 's3cret' } });

    const pkg = form.get('package') as File;
    const files = readZip(new Uint8Array(await pkg.arrayBuffer()));
    const manifest = JSON.parse(files['amoeba.yaml']);
    expect(manifest.api_version).toBe('v1');
    expect(manifest.app).toEqual({ type: 'image', image: 'myorg/pdf-parser:v1' });
    expect(manifest.placement).toEqual({ port: 8080, cooldown_seconds: 300 });
    expect(manifest.schema.env).toEqual({ PORT: { type: 'string' } });
    expect(manifest.schema.secrets).toEqual({ API_KEY: { required: true, inject: 'env' } });
    // The secret value never goes into the package itself.
    expect(files['amoeba.yaml']).not.toContain('s3cret');
  });

  it('rejects unsafe or repeated variable names', () => {
    expect(() =>
      buildInstallValues([{ key: '../../services.json', value: 'x', isSecret: true }]),
    ).toThrow(/Invalid variable name/);
    expect(() =>
      buildInstallValues([
        { key: 'TOKEN', value: 'a', isSecret: false },
        { key: 'TOKEN', value: 'b', isSecret: true },
      ]),
    ).toThrow(/more than once/);
  });

  it('builds a manifest with empty schema when there are no variables', () => {
    const manifest = buildManifest(input, { env: {}, secrets: {} }) as any;
    expect(manifest.schema).toEqual({ env: {}, secrets: {} });
    expect(manifest.local_upstream).toBeUndefined();
    const local = buildManifest({ ...input, localUpstream: true }, { env: {}, secrets: {} }) as any;
    expect(local.local_upstream).toBe(true);
  });

  it('writes a multi-entry zip with correct offsets', () => {
    const zip = buildZip([
      { name: 'a.txt', data: new TextEncoder().encode('alpha') },
      { name: 'b.txt', data: new TextEncoder().encode('beta') },
    ]);
    expect(readZip(zip)).toEqual({ 'a.txt': 'alpha', 'b.txt': 'beta' });
  });
});

describe('source builds', () => {
  it('maps each tab to a build request', () => {
    const fields = { gitUrl: ' https://github.com/a/b.git ', dockerfile: 'FROM x', contextUrl: '' };
    expect(buildRequestFor('git', fields)).toEqual({ source: 'git', url: 'https://github.com/a/b.git' });
    expect(buildRequestFor('dockerfile', fields)).toEqual({ source: 'dockerfile', dockerfile: 'FROM x' });
    expect(
      buildRequestFor('dockerfile', { ...fields, contextUrl: 'https://github.com/a/b.git' }),
    ).toEqual({ source: 'dockerfile', dockerfile: 'FROM x', context_url: 'https://github.com/a/b.git' });
    expect(buildRequestFor('image', fields)).toBeNull();
  });

  it('requires a lowercase service name', () => {
    expect(validateServiceName('pdf-parser')).toBeNull();
    expect(validateServiceName('PdfParser')).not.toBeNull();
    expect(validateServiceName('a/b')).not.toBeNull();
  });

  // @lat: [[deploy#Source builds#Console flow]]
  it('posts the build and returns the image tag', async () => {
    const calls: [string, RequestInit][] = [];
    const ok: typeof fetch = async (url, init) => {
      calls.push([url as string, init!]);
      return new Response(JSON.stringify({ image: 'amoeba/api:latest', log: [] }));
    };
    const image = await requestBuild('api', { source: 'git', url: 'https://x/y.git' }, 'tok', ok);
    expect(image).toBe('amoeba/api:latest');
    expect(calls[0][0]).toBe('/admin/apps/api/build');
    expect(calls[0][1].method).toBe('POST');
    expect((calls[0][1].headers as Record<string, string>).Authorization).toBe('Bearer tok');
    expect(JSON.parse(calls[0][1].body as string)).toEqual({ source: 'git', url: 'https://x/y.git' });
  });

  it('surfaces the build log when the build fails', async () => {
    const failing: typeof fetch = async () =>
      new Response(JSON.stringify({ error: 'build failed: docker exited with code 1', log: ['step 1', 'boom'] }), {
        status: 422,
      });
    const err = await requestBuild('api', { source: 'dockerfile', dockerfile: 'FROM x' }, 'tok', failing).catch(
      (e) => e,
    );
    expect(err).toBeInstanceOf(BuildError);
    expect(err.message).toContain('docker exited');
    expect(err.log).toEqual(['step 1', 'boom']);

    const forbidden: typeof fetch = async () => new Response('Forbidden', { status: 403 });
    const err2 = await requestBuild('api', { source: 'git', url: 'https://x/y.git' }, 'tok', forbidden).catch(
      (e) => e,
    );
    expect(err2.message).toBe('Forbidden');
    expect(err2.log).toEqual([]);
  });
});

describe('build progress', () => {
  it('formats elapsed time', () => {
    expect(formatElapsed(42)).toBe('42s');
    expect(formatElapsed(185)).toBe('3m 05s');
    expect(formatElapsed(3720)).toBe('1h 02m');
  });

  it('reads the status, and null when there is no build', async () => {
    const status = { state: 'running', step: 'git clone', elapsed_seconds: 3, log: ['$ git clone'] };
    const ok = (async () => new Response(JSON.stringify(status), { status: 200 })) as typeof fetch;
    expect(await fetchBuildStatus('svc', 't', ok)).toEqual(status);
    const missing = (async () => new Response('no build', { status: 404 })) as typeof fetch;
    expect(await fetchBuildStatus('svc', 't', missing)).toBeNull();
  });
});
