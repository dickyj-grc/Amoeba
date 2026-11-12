import { describe, expect, it } from 'vitest';
import { sameServices, toServiceItem } from './services';
import { ServiceItem } from '../types/amoeba';

const item = (name: string, active_requests = 0): ServiceItem => ({
  name,
  state: { phase: 'Cold', active_requests },
});

describe('sameServices', () => {
  it('treats an unchanged refresh as the same list', () => {
    expect(sameServices([item('a'), item('b')], [item('a'), item('b')])).toBe(true);
  });

  it('notices a changed state or a different list', () => {
    expect(sameServices([item('a')], [item('a', 2)])).toBe(false);
    expect(sameServices([item('a')], [item('a'), item('b')])).toBe(false);
  });
});

describe('toServiceItem', () => {
  const status = (state: string, running: boolean | null, message: string | null = null) =>
    ({
      name: 'lightpanda',
      state: { state, message, updated_at: 0 },
      runtime: { running, active_requests: 2, last_request_at: null, port: 9222, public: true },
    }) as any;

  it('shows the real lifecycle and run state', () => {
    expect(toServiceItem(status('ready', true)).state.phase).toBe('Warm');
    expect(toServiceItem(status('ready', false)).state.phase).toBe('Cold');
    expect(toServiceItem(status('ready', null)).state.phase).toBe('Cold');
    expect(toServiceItem(status('pulling', null)).state.phase).toBe('Warming');
    const failed = toServiceItem(status('error', null, 'nothing answered on port 8080'));
    expect(failed.state).toEqual({ phase: 'Error', active_requests: 2, error: 'nothing answered on port 8080' });
    expect(failed.port).toBe(9222);
  });
});
