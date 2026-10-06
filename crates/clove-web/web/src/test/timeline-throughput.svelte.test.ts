// @vitest-environment jsdom
import { describe, it, expect, afterEach, vi } from 'vitest';
import { render, cleanup } from '@testing-library/svelte';
import type { StatsHistoryPoint } from '$lib/types';
import { stubApi, META } from './fake-api';
import TimelinePage from '../routes/timeline/+page.svelte';

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

/** A recorded-snapshot series: `perDay` hourly captures on each of the last `days` days. */
function hourlySnapshots(days: number, perDay: number): StatsHistoryPoint[] {
  const out: StatsHistoryPoint[] = [];
  const today = Date.parse(new Date().toISOString().slice(0, 10) + 'T00:00:00Z');
  for (let d = days - 1; d >= 0; d--) {
    for (let h = 0; h < perDay; h++) {
      const captured_at = new Date(today - d * 86_400_000 + h * 3_600_000).toISOString();
      out.push({ date: captured_at.slice(0, 10), captured_at, created: 1, closed: 1, open: 3 });
    }
  }
  return out;
}

describe('timeline throughput', () => {
  it('charts one bar per day from hourly snapshots', async () => {
    stubApi((path) => {
      if (path === '/meta') return META;
      if (path === '/items') return [];
      if (path === '/stats/history') return hourlySnapshots(3, 4);
      throw new Error('unexpected request ' + path);
    });
    const { container } = render(TimelinePage);
    const bars = () => container.querySelectorAll('svg[aria-label="throughput chart"] rect');
    await vi.waitFor(() => expect(bars()).toHaveLength(3));
  });
});
