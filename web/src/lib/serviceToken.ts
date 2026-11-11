// What the token dialog offers to save: the service URL behind the gateway
// and an MCP client config that sends the token, so a closed dialog no
// longer means a lost token.

/** `<origin>/v1/<name>/`, the gateway route for a service. */
export function serviceUrl(origin: string, name: string): string {
  return `${origin.replace(/\/+$/, '')}/v1/${encodeURIComponent(name)}/`;
}

/** The service URL plus the app's own MCP path (e.g. `/mcp`); empty path means the root. */
export function mcpUrl(origin: string, name: string, mcpPath: string): string {
  return serviceUrl(origin, name) + mcpPath.trim().replace(/^\/+/, '');
}

/** An `mcpServers` entry for MCP clients that speak streamable HTTP. */
export function mcpClientConfig(
  origin: string,
  name: string,
  token: string,
  mcpPath: string,
): object {
  return {
    mcpServers: {
      [name]: {
        type: 'http',
        url: mcpUrl(origin, name, mcpPath),
        headers: { Authorization: `Bearer ${token}` },
      },
    },
  };
}

/** Copies text, falling back to a selected textarea where the Clipboard API is
 *  missing (plain-http consoles reached by IP are not a secure context). */
export async function copyText(text: string): Promise<boolean> {
  try {
    if (navigator.clipboard && window.isSecureContext) {
      await navigator.clipboard.writeText(text);
      return true;
    }
  } catch {
    // Fall through to the textarea copy.
  }
  const el = document.createElement('textarea');
  el.value = text;
  el.setAttribute('readonly', '');
  el.style.position = 'fixed';
  el.style.opacity = '0';
  document.body.appendChild(el);
  el.select();
  try {
    return document.execCommand('copy');
  } finally {
    document.body.removeChild(el);
  }
}

/** Saves `text` as a file through a temporary download link. */
export function downloadText(fileName: string, text: string, type = 'application/json'): void {
  const url = URL.createObjectURL(new Blob([text], { type }));
  const a = document.createElement('a');
  a.href = url;
  a.download = fileName;
  document.body.appendChild(a);
  a.click();
  document.body.removeChild(a);
  URL.revokeObjectURL(url);
}

/** Lifetimes offered when issuing. AI clients keep the token in their config,
 *  so the default is long; the server accepts 1 minute to 366 days. */
export const TOKEN_LIFETIMES: { label: string; seconds: number }[] = [
  { label: '1 hour', seconds: 60 * 60 },
  { label: '30 days', seconds: 30 * 24 * 60 * 60 },
  { label: '90 days', seconds: 90 * 24 * 60 * 60 },
  { label: '1 year', seconds: 365 * 24 * 60 * 60 },
];

/** "in 3 days", "in 45 minutes", "expired", relative to `nowSecs`. */
export function expiresIn(expiresAt: number, nowSecs: number): string {
  const left = expiresAt - nowSecs;
  if (left <= 0) return 'expired';
  const units: [number, string][] = [
    [24 * 60 * 60, 'day'],
    [60 * 60, 'hour'],
    [60, 'minute'],
  ];
  for (const [size, name] of units) {
    if (left >= size) {
      const n = Math.floor(left / size);
      return `in ${n} ${name}${n === 1 ? '' : 's'}`;
    }
  }
  return 'in under a minute';
}
