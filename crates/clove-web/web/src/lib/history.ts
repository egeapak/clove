// `GET /stats/history` returns one point per recorded stats snapshot (hourly by
// default), so several points share a `date`. Each snapshot's `created`/`closed`
// is the throughput since the *previous* snapshot and the other fields are
// levels at capture time, so a day is the sum of its deltas plus the levels of
// its last capture. The file-synthesized fallback is already one point per day
// and passes through unchanged.
import type { StatsHistoryPoint } from './types';

/** One point per `date`, oldest first: deltas summed, levels from the day's last capture. */
export function dailyHistory(points: StatsHistoryPoint[]): StatsHistoryPoint[] {
  const byDate = new Map<string, StatsHistoryPoint>();
  for (const p of points) {
    const day = byDate.get(p.date);
    byDate.set(
      p.date,
      day ? { ...p, created: day.created + p.created, closed: day.closed + p.closed } : { ...p }
    );
  }
  return [...byDate.values()].sort((a, b) => (a.date < b.date ? -1 : a.date > b.date ? 1 : 0));
}

/** The daily points dated within the `days` calendar days ending `today` (`YYYY-MM-DD`, UTC like the server's dates). */
export function lastDays(daily: StatsHistoryPoint[], days: number, today: string): StatsHistoryPoint[] {
  const first = new Date(Date.parse(today + 'T00:00:00Z') - (days - 1) * 86_400_000)
    .toISOString()
    .slice(0, 10);
  return daily.filter((p) => p.date >= first);
}
