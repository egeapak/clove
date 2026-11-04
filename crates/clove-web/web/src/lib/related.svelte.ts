// Title and status of an item referenced by id (a parent, dep, relation), for
// views that show relationships. The store holds only the current query's
// window, so an id outside it is fetched once and remembered here.
import { SvelteMap } from 'svelte/reactivity';
import type { Item, Status } from './types';
import { api, ApiError } from './api';
import { store } from './store.svelte';

export interface RelatedSummary {
  title: string;
  status: Status;
}

/** `null`: fetched and not found (a dangling reference). */
const fetched = new SvelteMap<string, RelatedSummary | null>();
/** The `store.liveRev` each entry was fetched at; an older one is refetched. */
const fetchedAt = new Map<string, number>();
const inFlight = new Set<string>();

/** The `store.items` that `danglingIds` was built from. */
let danglingFrom: Map<string, Item> | null = null;
let danglingIds = new Set<string>();

/**
 * True when an item in the store lists `id` in its `dangling_deps`: the server
 * has already said that `id` does not exist, so a fetch would only 404.
 * `store.items` is replaced on every change, so the set is rebuilt only then.
 */
function knownDangling(id: string): boolean {
  const items = store.items;
  if (items !== danglingFrom) {
    danglingFrom = items;
    danglingIds = new Set();
    for (const it of items.values()) for (const d of it.dangling_deps ?? []) danglingIds.add(d);
  }
  return danglingIds.has(id);
}

/**
 * What is known about `id` now: the live store first, then the dangling deps
 * of the stored items, then the fetched cache.
 */
export function related(id: string): RelatedSummary | null | undefined {
  const live = store.items.get(id);
  if (live) return { title: live.title, status: live.status };
  if (knownDangling(id)) return null;
  return fetched.get(id);
}

/**
 * Fetch `id` unless the cache is current for the latest live update or a fetch
 * is on its way. Call from an effect: reading `store.liveRev` re-runs it on
 * every live update. The previous value stays visible while refetching, and
 * only a 404 is cached as missing, so a transient error is retried. An id that
 * a stored item lists in `dangling_deps` is missing without a request.
 */
export function ensureRelated(id: string): void {
  // Read first: an early return must not drop the caller's effect subscription.
  const rev = store.liveRev;
  if (store.items.has(id) || inFlight.has(id)) return;
  if (knownDangling(id)) return;
  if (fetchedAt.get(id) === rev) return;
  inFlight.add(id);
  api
    .item(id)
    .then((it) => {
      fetched.set(id, { title: it.title, status: it.status });
      fetchedAt.set(id, rev);
    })
    .catch((err) => {
      if (err instanceof ApiError && err.status === 404) {
        fetched.set(id, null);
        fetchedAt.set(id, rev);
      }
    })
    .finally(() => {
      inFlight.delete(id);
      if (store.liveRev !== rev) ensureRelated(id);
    });
}

/** Record items fetched by another query, so their references need no fetch. */
export function rememberRelated(items: Item[]): void {
  for (const it of items) {
    fetched.set(it.id, { title: it.title, status: it.status });
    fetchedAt.set(it.id, store.liveRev);
  }
}

/** Test hook: forget everything fetched. */
export function resetRelated(): void {
  fetched.clear();
  fetchedAt.clear();
  inFlight.clear();
}
