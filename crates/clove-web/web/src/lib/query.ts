// Single URL <-> ListQuery mapping, used by both the list page (read URL) and
// api.ts qs() (write query string). The list "tab" param and the ListQuery
// "mode" field are the same concept (all|list / ready / blocked); we read either
// param name and always write `mode`.
import type { ListQuery, Status, ItemType } from './types';

const MODE_VALUES = new Set(['ready', 'blocked']);

/** Parse URLSearchParams into a ListQuery. Accepts `tab` or `mode`. */
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
    type: p.getAll('type') as ItemType[],
    priority: p.getAll('priority').map(Number),
    label: p.getAll('label')
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
  for (const t of query.type ?? []) p.append('type', t);
  for (const pr of query.priority ?? []) p.append('priority', String(pr));
  for (const l of query.label ?? []) p.append('label', l);
  return p;
}

/** Query-string suffix ("" or "?..."). Used by api.ts. */
export function queryString(query: ListQuery): string {
  const s = buildParams(query).toString();
  return s ? '?' + s : '';
}
