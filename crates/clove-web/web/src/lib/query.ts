// Single URL <-> ListQuery mapping, used by both the list page (read URL) and
// api.ts qs() (write query string). The list "tab" param and the ListQuery
// "mode" field are the same concept (all|list / ready / blocked); we read either
// param name and always write `mode`.
import type { ListQuery, Status, ItemType } from './types';

const MODE_VALUES = new Set(['ready', 'blocked']);

/**
 * Read a multi-select param as CSV — the server's contract (read.rs csv():
 * a single value split on commas). serde_urlencoded collapses *repeated* keys to
 * the last value, so multi-select filters must travel as one comma-joined value.
 */
function csv(p: URLSearchParams, key: string): string[] {
  const v = p.get(key);
  if (!v) return [];
  return v
    .split(',')
    .map((s) => s.trim())
    .filter(Boolean);
}

/**
 * Parse URLSearchParams into a ListQuery. Accepts `tab` or `mode`.
 *
 * The **window** (`limit`/`offset`) is deliberately not read here. This parses
 * the *browser* URL, whose paging state is a 1-based `?page=` the list route
 * turns into `limit`/`offset`; `buildParams` writes those two for the *API*
 * URL. The two query strings have never been the same thing (the browser also
 * accepts `tab`, which the API does not).
 */
export function parseQuery(p: URLSearchParams): ListQuery {
  const rawMode = p.get('mode') ?? p.get('tab') ?? '';
  const mode = MODE_VALUES.has(rawMode) ? (rawMode as 'ready' | 'blocked') : 'list';
  const q: ListQuery = {
    mode,
    status: (p.get('status') as Status) || undefined,
    assignee: p.get('assignee') || undefined,
    q: p.get('q') || undefined,
    sort: p.get('sort') || undefined,
    dir: (p.get('dir') as 'asc' | 'desc') || undefined,
    type: csv(p, 'type') as ItemType[],
    priority: csv(p, 'priority').map(Number),
    label: csv(p, 'label')
  };
  return q;
}

/** Serialize a ListQuery into URLSearchParams. Writes `mode` (not `tab`). */
export function buildParams(query: ListQuery): URLSearchParams {
  const p = new URLSearchParams();
  if (query.status) p.set('status', query.status);
  if (query.assignee) p.set('assignee', query.assignee);
  if (query.q) p.set('q', query.q);
  if (query.sort) p.set('sort', query.sort);
  if (query.dir) p.set('dir', query.dir);
  if (query.mode && query.mode !== 'list') p.set('mode', query.mode);
  // Multi-select filters go as a single comma-joined value (server CSV contract);
  // labels never contain commas (parseLabels splits on them), so this is lossless.
  if (query.type?.length) p.set('type', query.type.join(','));
  if (query.priority?.length) p.set('priority', query.priority.map(String).join(','));
  if (query.label?.length) p.set('label', query.label.join(','));
  // The window is sent explicitly, including `limit: 0`: the API default is
  // unlimited and stays that way (other clients depend on it), so a paging
  // client has to ask rather than inherit. `offset: 0` is the default on every
  // surface, so it is omitted — but `limit: 0` is a real request for
  // "everything" and must survive.
  if (query.limit !== undefined) p.set('limit', String(query.limit));
  if (query.offset) p.set('offset', String(query.offset));
  return p;
}

/** Query-string suffix ("" or "?..."). Used by api.ts. */
export function queryString(query: ListQuery): string {
  const s = buildParams(query).toString();
  return s ? '?' + s : '';
}
