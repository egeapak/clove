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
  // Labels are canonicalized to lowercase on write, and the server canonicalizes
  // the *query* too (`normalize_label`), so `?label=AREA:Core` matches. Without
  // the same fold here the UI silently returned nothing for a query the API
  // answers.
  if (q.label?.length) {
    const want = q.label!.map((l) => l.trim().toLowerCase());
    out = out.filter((i) => {
      const have = i.labels.map((l) => l.toLowerCase());
      return want.every((l) => have.includes(l));
    });
  }
  if (q.q) {
    const n = q.q.toLowerCase();
    // `clove_core::view::q_matches`: a substring over id, title, and each label
    // **separately** — never the body, which the lean list endpoint omits anyway.
    //
    // Per-field, not a concatenated haystack. Joining them let a needle
    // containing a space match across a field boundary (`q=widget area:core`
    // spanning the end of the title and the start of a label), which the server
    // does not do. Divergence here is invisible to the Rust test suite, because
    // the shipped UI filters client-side and never asks the server.
    out = out.filter(
      (i) =>
        i.id.toLowerCase().includes(n) ||
        i.title.toLowerCase().includes(n) ||
        i.labels.some((l) => l.toLowerCase().includes(n))
    );
  }
  return out;
}

/**
 * The natural first direction for a sort column (mirrors the TUI's
 * `cycle_sort_field`): rank/id/priority read best ascending (p0 first, the
 * server's canonical order preserved), timestamps read best newest-first.
 */
export function defaultDir(sort: string): 'asc' | 'desc' {
  return sort === 'created' || sort === 'updated' ? 'desc' : 'asc';
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
