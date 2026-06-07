import type { Status, ItemType } from './types';

export function statusGlyph(s: Status): string {
  return s === 'open' ? '○' : s === 'in_progress' ? '◐' : '●';
}
export function statusLabel(s: Status): string {
  return s === 'open' ? 'open' : s === 'in_progress' ? 'in progress' : 'closed';
}
export function statusColorVar(s: Status): string {
  return s === 'open'
    ? 'var(--status-open)'
    : s === 'in_progress'
      ? 'var(--status-in-progress)'
      : 'var(--status-closed)';
}

// p2 and p3 both use `•`, differ only by hue (matches ui/style.rs).
export function priorityGlyph(p: number): string {
  switch (p) {
    case 0:
      return '!';
    case 1:
      return '↑';
    case 2:
    case 3:
      return '•';
    case 4:
      return '↓';
    default:
      return 'p' + p;
  }
}
export function priorityLabel(p: number): string {
  return ['p0 — Critical', 'p1 — High', 'p2 — Normal', 'p3 — Low', 'p4 — Lowest'][p] ?? 'p' + p;
}
export function priorityColorVar(p: number): string {
  return `var(--prio-${p >= 0 && p <= 4 ? p : 4})`;
}

const TYPE_ICONS: Record<ItemType, string> = {
  bug: 'B',
  feature: 'F',
  chore: 'C',
  docs: 'D',
  epic: 'E'
};
export function typeIcon(t: ItemType): string {
  return TYPE_ICONS[t] ?? '?';
}
export function typeColorVar(t: ItemType): string {
  return `var(--type-${t})`;
}

export function initials(name: string | null): string {
  if (!name) return '?';
  const parts = name.trim().split(/[\s_-]+/);
  if (parts.length >= 2) return (parts[0][0] + parts[1][0]).toUpperCase();
  return name.slice(0, 2).toUpperCase();
}

export function shortId(id: string): string {
  // clove ids are `<prefix>-<8 Crockford base32>` (e.g. "clov-AS5Y8MM7").
  // Render just the suffix after the last "-"; fall back to the whole id.
  const dash = id.lastIndexOf('-');
  const suffix = dash >= 0 && dash < id.length - 1 ? id.slice(dash + 1) : id;
  return '#' + suffix;
}

export function relativeTime(iso: string): string {
  const then = new Date(iso).getTime();
  if (isNaN(then)) return iso;
  const diff = Date.now() - then;
  const s = Math.floor(diff / 1000);
  if (s < 60) return s <= 1 ? 'just now' : s + 's ago';
  const m = Math.floor(s / 60);
  if (m < 60) return m + 'm ago';
  const h = Math.floor(m / 60);
  if (h < 24) return h + 'h ago';
  const d = Math.floor(h / 24);
  if (d < 30) return d + 'd ago';
  const mo = Math.floor(d / 30);
  if (mo < 12) return mo + 'mo ago';
  return Math.floor(mo / 12) + 'y ago';
}

export function shortDate(iso: string): string {
  const d = new Date(iso);
  if (isNaN(d.getTime())) return iso;
  return new Intl.DateTimeFormat(undefined, { month: 'short', day: 'numeric' }).format(d);
}
