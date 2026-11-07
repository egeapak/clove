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

/**
 * The missing references of items known only through `rememberRelated` (the
 * detail route's children), with the `store.liveRev` they were fetched at. An
 * entry from an older rev is ignored: a live update may have created the id.
 */
const rememberedMissing = new Map<string, { refs: string[]; rev: number }>();
/** Bumped by `rememberRelated`, so a reader of `knownDangling` re-runs. */
let rememberedRev = $state(0);

/** What `danglingIds` was built from; any change rebuilds it. */
let danglingKey: { items: Map<string, Item>; liveRev: number; remembered: number } | null = null;
let danglingIds = new Set<string>();

function missingOf(it: Item): string[] {
  return [...(it.dangling_deps ?? []), ...(it.missing_refs ?? [])];
}

/**
 * True when the server has already said that `id` does not exist: a stored item
 * lists it in `dangling_deps` or `missing_refs`, or a remembered item fetched at
 * the current live rev does. A fetch would only answer 404. `store.items` is
 * replaced on every change, so the set is rebuilt only when it, the live rev or
 * the remembered items change.
 */
function knownDangling(id: string): boolean {
  const items = store.items;
  const liveRev = store.liveRev;
  const remembered = rememberedRev;
  const key = danglingKey;
  if (!key || key.items !== items || key.liveRev !== liveRev || key.remembered !== remembered) {
    danglingKey = { items, liveRev, remembered };
    danglingIds = new Set();
    for (const it of items.values()) for (const d of missingOf(it)) danglingIds.add(d);
    for (const [owner, entry] of rememberedMissing) {
      if (entry.rev !== liveRev || items.has(owner)) continue;
      for (const d of entry.refs) danglingIds.add(d);
    }
  }
  return danglingIds.has(id);
}

/**
 * What is known about `id` now: the live store first, then the ids the server
 * reports as missing, then the fetched cache.
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
 * the server already reports as missing (see `knownDangling`) is missing
 * without a request.
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
    rememberedMissing.set(it.id, { refs: missingOf(it), rev: store.liveRev });
  }
  rememberedRev += 1;
}

/** Test hook: forget everything fetched. */
export function resetRelated(): void {
  fetched.clear();
  fetchedAt.clear();
  inFlight.clear();
  rememberedMissing.clear();
  danglingKey = null;
  danglingIds = new Set();
}
