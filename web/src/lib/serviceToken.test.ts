import { describe, expect, it } from 'vitest';
import { expiresIn, mcpClientConfig, mcpUrl, serviceUrl } from './serviceToken';

describe('service token config', () => {
  it('builds the gateway route from the console origin', () => {
    expect(serviceUrl('http://localhost:3064/', 'lightpanda')).toBe('http://localhost:3064/v1/lightpanda/');
    expect(mcpUrl('http://localhost:3064', 'lightpanda', '/mcp')).toBe('http://localhost:3064/v1/lightpanda/mcp');
    expect(mcpUrl('http://localhost:3064', 'lightpanda', '')).toBe('http://localhost:3064/v1/lightpanda/');
  });

  it('puts the token in an MCP client config', () => {
    expect(mcpClientConfig('https://amoeba.example', 'finance-db', 'eyJ.t', 'mcp')).toEqual({
      mcpServers: {
        'finance-db': {
          type: 'http',
          url: 'https://amoeba.example/v1/finance-db/mcp',
          headers: { Authorization: 'Bearer eyJ.t' },
        },
      },
    });
  });

  it('describes how long a token has left', () => {
    expect(expiresIn(100, 200)).toBe('expired');
    expect(expiresIn(1000 + 30 * 86400, 1000)).toBe('in 30 days');
    expect(expiresIn(1000 + 3600, 1000)).toBe('in 1 hour');
    expect(expiresIn(1000 + 90, 1000)).toBe('in 1 minute');
    expect(expiresIn(1030, 1000)).toBe('in under a minute');
  });
});
