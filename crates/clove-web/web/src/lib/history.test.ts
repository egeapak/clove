import { describe, it, expect } from 'vitest';
import { dailyHistory, lastDays } from './history';
import type { StatsHistoryPoint } from './types';

function snap(captured_at: string, created: number, closed: number, open: number): StatsHistoryPoint {
  return { date: captured_at.slice(0, 10), captured_at, created, closed, open, total: open, ready: open, blocked: 0 };
}

describe('dailyHistory', () => {
  it('folds hourly snapshots into one point per day', () => {
    const daily = dailyHistory([
      snap('2026-10-05T22:00:00Z', 0, 0, 4),
      snap('2026-10-05T23:00:00Z', 2, 1, 5),
      snap('2026-10-06T00:00:00Z', 1, 0, 6),
      snap('2026-10-06T01:00:00Z', 0, 3, 3),
      snap('2026-10-06T02:00:00Z', 4, 0, 7)
    ]);
    expect(daily.map((p) => p.date)).toEqual(['2026-10-05', '2026-10-06']);
    // Per-snapshot throughput deltas add up over the day...
    expect(daily.map((p) => [p.created, p.closed])).toEqual([
      [2, 1],
      [5, 3]
    ]);
    // ...while levels are the day's last capture, not a sum.
    expect(daily.map((p) => p.open)).toEqual([5, 7]);
    expect(daily[1].captured_at).toBe('2026-10-06T02:00:00Z');
  });

  it('leaves an already-daily series alone', () => {
    const series: StatsHistoryPoint[] = [
      { date: '2026-10-04', created: 1, closed: 0, open: 1 },
      { date: '2026-10-05', created: 0, closed: 1, open: 0 }
    ];
    expect(dailyHistory(series)).toEqual(series);
  });
});

describe('lastDays', () => {
  const daily = dailyHistory([
    snap('2026-08-01T10:00:00Z', 1, 0, 1),
    snap('2026-09-07T10:00:00Z', 1, 0, 2),
    snap('2026-09-08T10:00:00Z', 1, 0, 3),
    snap('2026-10-06T10:00:00Z', 1, 0, 4)
  ]);

  it('windows by calendar days, not by point count', () => {
    // 30 days ending 2026-10-06 start on 2026-09-07.
    expect(lastDays(daily, 30, '2026-10-06').map((p) => p.date)).toEqual([
      '2026-09-07',
      '2026-09-08',
      '2026-10-06'
    ]);
    expect(lastDays(daily, 1, '2026-10-06').map((p) => p.date)).toEqual(['2026-10-06']);
    expect(lastDays(daily, 90, '2026-10-06')).toHaveLength(4);
  });
});
