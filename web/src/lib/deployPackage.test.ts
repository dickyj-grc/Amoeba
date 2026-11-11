import { describe, expect, it } from 'vitest';
import {
  buildDeployForm,
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
  });

  it('writes a multi-entry zip with correct offsets', () => {
    const zip = buildZip([
      { name: 'a.txt', data: new TextEncoder().encode('alpha') },
      { name: 'b.txt', data: new TextEncoder().encode('beta') },
    ]);
    expect(readZip(zip)).toEqual({ 'a.txt': 'alpha', 'b.txt': 'beta' });
  });
});
