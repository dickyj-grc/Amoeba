import { LibraryEntry } from '../types/amoeba';

/** Reduces what a user types or pastes to something comparable with the
 *  library's names: drops the scheme, `www.`, `.git`, a trailing slash, an
 *  image tag or digest, and Docker Hub's `docker.io/` and `library/` prefixes. */
export function normalizeQuery(raw: string): string {
  let q = raw.trim().toLowerCase();
  q = q.replace(/^[a-z]+:\/\//, '').replace(/^www\./, '');
  q = q.replace(/\.git$/, '').replace(/\/+$/, '');
  q = q.replace(/@sha256:[0-9a-f]+$/, '');
  // An image tag is the part after the last ':' that follows the last '/'.
  const slash = q.lastIndexOf('/');
  const colon = q.lastIndexOf(':');
  if (colon > slash) q = q.slice(0, colon);
  q = q.replace(/^docker\.io\//, '').replace(/^library\//, '');
  return q;
}

/** Library apps matching `query`, best first. An exact image or GitHub repo
 *  match ranks above a name or tag match. Empty query lists everything. */
export function searchLibrary(entries: LibraryEntry[], query: string): LibraryEntry[] {
  const q = normalizeQuery(query);
  if (!q) return entries;
  const scored = entries
    .map((entry) => {
      const images = entry.images.map((i) => i.toLowerCase());
      const repos = entry.repos.map((r) => r.toLowerCase());
      let score = 0;
      if (images.includes(q) || repos.includes(q)) score = 100;
      else if (entry.id === q || entry.title.toLowerCase() === q) score = 90;
      else if (images.some((i) => i.startsWith(q)) || repos.some((r) => r.startsWith(q))) score = 60;
      else if (entry.title.toLowerCase().includes(q) || entry.id.includes(q)) score = 50;
      else if (entry.tags.some((t) => t.toLowerCase().startsWith(q))) score = 30;
      else if (entry.summary.toLowerCase().includes(q)) score = 10;
      return { entry, score };
    })
    .filter((s) => s.score > 0);
  scored.sort((a, b) => b.score - a.score);
  return scored.map((s) => s.entry);
}

/** The library app for an exact image or GitHub repo, if any. */
export function libraryMatch(entries: LibraryEntry[], imageOrRepo: string): LibraryEntry | undefined {
  const q = normalizeQuery(imageOrRepo);
  if (!q) return undefined;
  return entries.find(
    (e) => e.images.some((i) => i.toLowerCase() === q) || e.repos.some((r) => r.toLowerCase() === q),
  );
}

/** How the console names an API protocol. */
export function interfaceLabel(type: string): string {
  return (
    ({ mcp: 'MCP', openapi: 'REST (OpenAPI)', http: 'HTTP', cdp: 'Browser automation (CDP)' } as Record<string, string>)[
      type
    ] ?? type
  );
}
