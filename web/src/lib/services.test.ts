import { describe, expect, it } from 'vitest';
import { sameServices } from './services';
import { ServiceItem } from '../types/amoeba';

const item = (name: string, active_requests = 0): ServiceItem => ({
  name,
  state: {
    phase: 'Cold',
    generation: 1,
    observed_generation: 1,
    ready: false,
    healthy: false,
    active_requests,
  },
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
