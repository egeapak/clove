// Title and status of an item referenced by id (a parent, dep, relation), for
// views that show relationships. The store holds only the current query's
// window, so an id outside it is fetched once and remembered here.
import { SvelteMap } from 'svelte/reactivity';
import type { Status } from './types';
import { api } from './api';
import { store } from './store.svelte';

export interface RelatedSummary {
  title: string;
  status: Status;
}

/** `null`: fetched and not found (a dangling reference). */
const fetched = new SvelteMap<string, RelatedSummary | null>();
const inFlight = new Set<string>();

/** What is known about `id` now: the live store first, then the fetched cache. */
export function related(id: string): RelatedSummary | null | undefined {
  const live = store.items.get(id);
  if (live) return { title: live.title, status: live.status };
  return fetched.get(id);
}

/** Fetch `id` unless it is already known or on its way. Call from an effect. */
export function ensureRelated(id: string): void {
  if (store.items.has(id) || fetched.has(id) || inFlight.has(id)) return;
  inFlight.add(id);
  api
    .item(id)
    .then((it) => fetched.set(id, { title: it.title, status: it.status }))
    .catch(() => fetched.set(id, null))
    .finally(() => inFlight.delete(id));
}

/** Test hook: forget everything fetched. */
export function resetRelated(): void {
  fetched.clear();
  inFlight.clear();
}
