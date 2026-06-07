// Single source of truth for client-side list filtering + sorting, shared by
// the live list page (routes/list) and the mock backend (api.ts filterMock) so
// the two can never diverge.
import type { Item, ListQuery } from './types';

/** Tab/mode filter: 'all'|'list' (no-op), 'ready', or 'blocked'. */
export function matchesTab(item: Item, tab: string | undefined): boolean {
  if (tab === 'ready') return item.ready && item.status !== 'closed';
  if (tab === 'blocked') return item.blocked_by.length > 0;
  return true;
}

/** Apply every filter in a ListQuery to a list of items (does not sort). */
export function applyFilters(items: Item[], q: ListQuery): Item[] {
  let out = items.filter((i) => matchesTab(i, q.mode));
  if (q.status) out = out.filter((i) => i.status === q.status);
  if (q.assignee) out = out.filter((i) => (i.assignee ?? '') === q.assignee);
  if (q.type?.length) out = out.filter((i) => q.type!.includes(i.type));
  if (q.priority?.length) out = out.filter((i) => q.priority!.includes(i.priority));
  if (q.label?.length) out = out.filter((i) => q.label!.every((l) => i.labels.includes(l)));
  if (q.q) {
    const n = q.q.toLowerCase();
    out = out.filter(
      (i) =>
        i.title.toLowerCase().includes(n) ||
        i.id.toLowerCase().includes(n) ||
        i.body.toLowerCase().includes(n)
    );
  }
  return out;
}

/**
 * Sort items by column + direction. The `rank` sort (default) preserves the
 * server's canonical order — callers pass a `rankOf` lookup (insertion index
 * from the store's replaceAll) so it isn't approximated with priority.
 */
export function sortItems(
  items: Item[],
  sort: string,
  dir: 'asc' | 'desc',
  rankOf?: (id: string) => number
): Item[] {
  const mul = dir === 'asc' ? 1 : -1;
  const ranked = (id: string) => (rankOf ? rankOf(id) : 0);
  return [...items].sort((a, b) => {
    let c = 0;
    if (sort === 'priority') c = a.priority - b.priority;
    else if (sort === 'id') c = a.id.localeCompare(b.id);
    else if (sort === 'created') c = a.created.localeCompare(b.created);
    else if (sort === 'updated') c = a.updated.localeCompare(b.updated);
    else c = ranked(a.id) - ranked(b.id); // 'rank' — server canonical order
    if (c === 0 && sort !== 'rank') c = ranked(a.id) - ranked(b.id);
    return c * mul;
  });
}
