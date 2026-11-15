import { describe, expect, it } from 'vitest';
import { changedSettings, freeName, renameTokenRole, toDraft } from './settings';

const base = {
  permissions: { read: ['admin', 'mcp:lightpanda'], add: ['admin', 'mcp:lightpanda'] },
  public: false,
  memory: '1Gi',
  cooldown_seconds: 300,
};

describe('settings form', () => {
  it('sends nothing when nothing changed', () => {
    expect(changedSettings(base, toDraft(base))).toEqual({});
  });

  it('sends only the fields that changed', () => {
    const draft = toDraft(base);
    draft.permissions.add = 'admin, research';
    draft.memory = '2Gi';
    draft.cooldown = '0';
    expect(changedSettings(base, draft)).toEqual({
      permissions: { read: ['admin', 'mcp:lightpanda'], add: ['admin', 'research'] },
      memory: '2Gi',
      cooldown_seconds: 0,
    });
  });

  it('rejects numbers the gateway would refuse', () => {
    const draft = toDraft(base);
    draft.cooldown = '1.5';
    expect(() => changedSettings(base, draft)).toThrow(/whole seconds/);
  });

  it('keeps token grants pointing at a renamed app', () => {
    expect(renameTokenRole(base, 'lightpanda', 'lp2').permissions?.read).toEqual(['admin', 'mcp:lp2']);
  });

  it('suggests the first free name', () => {
    expect(freeName('lightpanda', ['lightpanda', 'lightpanda-2'])).toBe('lightpanda-3');
    expect(freeName('new', ['lightpanda'])).toBe('new');
  });
});
