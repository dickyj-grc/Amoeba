import { describe, expect, it } from 'vitest';
import { libraryMatch, normalizeQuery, searchLibrary } from './library';
import { LibraryEntry } from '../types/amoeba';

const entry = (id: string, title: string, images: string[], repos: string[], tags: string[] = []): LibraryEntry => ({
  id,
  title,
  summary: `${title} summary`,
  tags,
  images,
  repos,
  default_name: id,
  description: '',
  port: 8080,
  public: false,
  local_upstream: false,
  env: {},
  secrets: {},
});

const lightpanda = entry('lightpanda-mcp', 'Lightpanda (MCP)', ['lightpanda/browser'], ['github.com/lightpanda-io/browser'], ['browser', 'mcp']);
const obscura = entry('obscura', 'Obscura (MCP)', ['h4ckf0r0day/obscura'], [], ['browser', 'mcp']);
const hello = entry('hello-world', 'Hello world', ['hashicorp/http-echo'], [], ['example']);
const all = [hello, obscura, lightpanda];

describe('app library search', () => {
  it('normalizes pasted images and URLs', () => {
    expect(normalizeQuery('lightpanda/browser:latest')).toBe('lightpanda/browser');
    expect(normalizeQuery('docker.io/lightpanda/browser@sha256:abc123')).toBe('lightpanda/browser');
    expect(normalizeQuery('https://github.com/lightpanda-io/browser.git')).toBe('github.com/lightpanda-io/browser');
    expect(normalizeQuery('ghcr.io/org/app:1.2')).toBe('ghcr.io/org/app');
    expect(normalizeQuery('localhost:5000/app')).toBe('localhost:5000/app');
  });

  it('finds apps by image, repo, name or tag, exact matches first', () => {
    expect(searchLibrary(all, 'lightpanda/browser:latest')[0].id).toBe('lightpanda-mcp');
    expect(searchLibrary(all, 'https://github.com/lightpanda-io/browser')[0].id).toBe('lightpanda-mcp');
    expect(searchLibrary(all, 'light')[0].id).toBe('lightpanda-mcp');
    expect(searchLibrary(all, 'browser').map((e) => e.id).sort()).toEqual(['lightpanda-mcp', 'obscura']);
    expect(searchLibrary(all, '')).toHaveLength(3);
    expect(searchLibrary(all, 'postgres')).toEqual([]);
  });

  it('matches only exact images or repos for the Docker Image hint', () => {
    expect(libraryMatch(all, 'lightpanda/browser:latest')?.id).toBe('lightpanda-mcp');
    expect(libraryMatch(all, 'lightpanda')).toBeUndefined();
  });
});
