// The mock backend's emulation of `GET /api/v1/items`.
//
// This used to be the *live* path too: the SPA fetched the whole store with no
// query and filtered/sorted it in the browser, so the server-side `Filters` and
// `Order` were never exercised by the UI and the two answered the same URL
// differently more than once. The list route now sends the query and renders
// exactly the page it gets back (read-path roadmap §5), which leaves these
// functions with one job — making `api.ts`'s `filterMock` answer a query the
// way the server would, so the dev fixture and a live backend agree.
//
// Everything here therefore mirrors `clove_core::view::{Filters, Order}`, not
// what the UI happens to need.
import type { Item, ListQuery, Status, ItemType } from './types';

/**
 * Tab/mode filter: 'all'|'list' (no-op), 'ready', or 'blocked' — `?mode=` on
 * the API, which routes to `Engine::ready` / `Engine::blocked`.
 *
 * Both partitions are over **active** items, so a closed item with an open
 * dependency is neither ready nor blocked. (`blocked` here read
 * `blocked_by.length > 0` alone, which counted closed items too — invisible
 * while this was only a client-side tab, wrong the moment it has to agree with
 * `clove blocked`.) The one thing the mock cannot reproduce is the server's
 * *excluded* set — items in a dependency cycle, which belong to neither
 * partition — since the fixture has no cycles.
 */
export function matchesTab(item: Item, tab: string | undefined): boolean {
  if (item.status === 'closed' && (tab === 'ready' || tab === 'blocked')) return false;
  if (tab === 'ready') return item.ready;
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
 * Byte-order string comparison, as `Ord for str` does it on the server.
 *
 * Not `localeCompare`: that is *collation*, which folds case and punctuation
 * according to the viewer's locale, so `proj-A` vs `proj-a` and an id
 * containing `-` can order differently in the browser than in Rust. While the
 * SPA held the whole store this only reshuffled ties; under paging a
 * disagreement about the tiebreak silently repeats and skips rows across a page
 * boundary.
 */
export function cmp(a: string, b: string): number {
  return a < b ? -1 : a > b ? 1 : 0;
}

// Enum sorts follow lifecycle/declaration order, not alphabetical — the same
// arrays `clove_core::view::SortField::{STATUS_ORDER, TYPE_ORDER}` rank by.
const STATUS_ORDER: Status[] = ['open', 'in_progress', 'closed'];
const TYPE_ORDER: ItemType[] = ['bug', 'feature', 'chore', 'docs', 'epic'];

/**
 * Sort items the way `clove_core::view::Order` does: one field, then **always**
 * an id tiebreak, with the direction reversing the whole key.
 *
 * The `rank` sort — `(priority, topological rank, id)` on the server — has no
 * client-side equivalent, so callers pass a `rankOf` lookup; the mock backend
 * uses the fixture's authoring order, which is defined to be rank order.
 *
 * The id tiebreak is not decoration. Every other key here has ties (two items
 * of the same priority, two timestamps in the same second), and a sort whose
 * ties resolve to input order is not a total order: paging over it repeats and
 * skips rows. This function previously broke ties on `rankOf` — the *fetched
 * array's* insertion order — which was indistinguishable from the id tiebreak
 * only while the fetch was the entire store.
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
    else if (sort === 'id') c = cmp(a.id, b.id);
    else if (sort === 'created') c = cmp(a.created, b.created);
    else if (sort === 'updated') c = cmp(a.updated, b.updated);
    else if (sort === 'status') c = STATUS_ORDER.indexOf(a.status) - STATUS_ORDER.indexOf(b.status);
    else if (sort === 'type') c = TYPE_ORDER.indexOf(a.type) - TYPE_ORDER.indexOf(b.type);
    else c = ranked(a.id) - ranked(b.id); // 'rank' — server canonical order
    if (c === 0) c = cmp(a.id, b.id);
    return c * mul;
  });
}
