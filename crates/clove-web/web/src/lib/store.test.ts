import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { store } from './store.svelte';
import type { Item } from './types';

function item(over: Partial<Item> & { id: string }): Item {
  return {
    title: 't',
    status: 'open',
    type: 'bug',
    priority: 2,
    assignee: null,
    parent: null,
    labels: [],
    deps: [],
    relates: [],
    created: '',
    updated: '2020-01-01T00:00:00.000Z',
    closed: null,
    body: '',
    comment_count: 0,
    ready: true,
    blocked_by: [],
    dangling_deps: [],
    ...over
  };
}

describe('store optimistic concurrency', () => {
  beforeEach(() => {
    // Isolate: the store is a singleton, so drop any pending edits leaked by a
    // prior test before reseeding the canonical server state.
    (store as unknown as { pending: Map<string, unknown> }).pending.clear();
    store.replaceAll([item({ id: 'proj-1', status: 'open', priority: 2 })]);
  });

  it('composes two overlapping edits without clobbering each other on settle', () => {
    // Edit A (status) then edit B (priority) before A resolves.
    const a = store.optimistic('proj-1', { status: 'in_progress' });
    const b = store.optimistic('proj-1', { priority: 0 });
    expect(store.items.get('proj-1')!.status).toBe('in_progress');
    expect(store.items.get('proj-1')!.priority).toBe(0);

    // A settles: the server payload has A but NOT B. B must survive.
    a.settle(item({ id: 'proj-1', status: 'in_progress', priority: 2 }));
    expect(store.items.get('proj-1')!.status).toBe('in_progress');
    expect(store.items.get('proj-1')!.priority).toBe(0); // B still applied

    // B settles with the fully-updated server payload.
    b.settle(item({ id: 'proj-1', status: 'in_progress', priority: 0 }));
    expect(store.items.get('proj-1')!.status).toBe('in_progress');
    expect(store.items.get('proj-1')!.priority).toBe(0);
  });

  it('settles edits that complete out of order without leaking a patch', () => {
    // Edit A (status) then edit B (priority); B's HTTP response arrives FIRST,
    // then A FAILS. The positional (shift-oldest) settle consumed A's patch on
    // B's settle, so A's rollback then removed B's patch and… nothing was left
    // tracking reality: B's value leaked into `pending` forever.
    const a = store.optimistic('proj-1', { status: 'in_progress' });
    const b = store.optimistic('proj-1', { priority: 0 });

    b.settle(item({ id: 'proj-1', status: 'open', priority: 0 }));
    a.rollback();

    // The visible item reflects exactly B's settled server state.
    expect(store.items.get('proj-1')!.status).toBe('open');
    expect(store.items.get('proj-1')!.priority).toBe(0);
    // No pending ledger entry survives to overwrite future refetches.
    const pending = (store as unknown as { pending: Map<string, unknown> }).pending;
    expect(pending.size).toBe(0);
    // A later server refetch is respected, not overwritten by a leaked patch.
    store.replaceAll([item({ id: 'proj-1', status: 'closed', priority: 4 })]);
    expect(store.items.get('proj-1')!.status).toBe('closed');
    expect(store.items.get('proj-1')!.priority).toBe(4);
  });

  it('rolling back one edit leaves the other edit intact', () => {
    store.optimistic('proj-1', { status: 'in_progress' });
    const b = store.optimistic('proj-1', { priority: 0 });

    // B fails → its rollback must restore priority but keep A's status.
    b.rollback();
    expect(store.items.get('proj-1')!.priority).toBe(2); // B undone
    expect(store.items.get('proj-1')!.status).toBe('in_progress'); // A survives
  });

  it('rollback of the only edit restores the pre-edit snapshot', () => {
    const edit = store.optimistic('proj-1', { status: 'closed' });
    expect(store.items.get('proj-1')!.status).toBe('closed');
    edit.rollback();
    expect(store.items.get('proj-1')!.status).toBe('open');
  });
});

describe('store holds one server window', () => {
  beforeEach(() => {
    (store as unknown as { pending: Map<string, unknown> }).pending.clear();
    (store as unknown as { query: unknown }).query = null;
  });

  it('reports the pre-window total and renders the rows in server order', () => {
    store.replaceAll([item({ id: 'proj-B' }), item({ id: 'proj-A' })], {
      total: 412,
      offset: 100,
      limit: 2
    });
    // Server order, not id order and not Map iteration luck.
    expect(store.all.map((i) => i.id)).toEqual(['proj-B', 'proj-A']);
    expect(store.total).toBe(412);
    expect(store.offset).toBe(100);
    expect(store.limit).toBe(2);
  });

  it('describes exactly what it holds when no window is given', () => {
    store.replaceAll([item({ id: 'proj-1' })]);
    expect(store.total).toBe(1);
    expect(store.offset).toBe(0);
  });

  it('keeps an item fetched out of band out of the current page', () => {
    store.replaceAll([item({ id: 'proj-1' })], { total: 9, offset: 0, limit: 1 });
    // The detail and edit routes upsert the full item they load. Before the
    // list paged, an extra id in the cache was harmless because the cache *was*
    // the whole store; now it would render as a row in a page it is not part of.
    store.upsert(item({ id: 'proj-off-page' }));
    expect(store.items.has('proj-off-page')).toBe(true);
    expect(store.all.map((i) => i.id)).toEqual(['proj-1']);
  });
});

describe('store query loading', () => {
  const ITEMS = /\/items\?/;

  function envelope(data: unknown, meta: Record<string, unknown> = {}) {
    return new Response(JSON.stringify({ v: 1, ok: true, data, _meta: meta }), {
      status: 200,
      headers: { 'content-type': 'application/json' }
    });
  }
  const META = {
    id_prefix: 'proj',
    types: [],
    statuses: [],
    priorities: [],
    labels: [],
    assignees: [],
    daemon: { running: false, web_addr: null },
    source: 'files'
  };

  beforeEach(() => {
    (store as unknown as { pending: Map<string, unknown> }).pending.clear();
    (store as unknown as { query: unknown }).query = null;
    store.replaceAll([]);
  });
  afterEach(() => vi.unstubAllGlobals());

  it('does not refetch when the query serializes to the same request', async () => {
    let itemCalls = 0;
    vi.stubGlobal('fetch', async (url: string | URL) => {
      const u = String(url);
      if (ITEMS.test(u) || u.endsWith('/items')) {
        itemCalls++;
        return envelope([], { total: 0 });
      }
      return envelope(META);
    });

    store.setQuery({ limit: 100, offset: 0, sort: 'rank', dir: 'asc' });
    // A distinct object with identical contents — what a route's `$effect`
    // hands over on every unrelated re-render.
    store.setQuery({ limit: 100, offset: 0, sort: 'rank', dir: 'asc' });
    await new Promise((r) => setTimeout(r, 50));
    expect(itemCalls).toBe(1);

    store.setQuery({ limit: 100, offset: 100, sort: 'rank', dir: 'asc' });
    await new Promise((r) => setTimeout(r, 50));
    expect(itemCalls).toBe(2);
  });

  it('ignores a superseded response that lands after a newer one', async () => {
    vi.stubGlobal('fetch', async (url: string | URL) => {
      const u = String(url);
      if (!u.includes('/items')) return envelope(META);
      // The FIRST query (offset=0) is the slow one, so it resolves last.
      const slow = !u.includes('offset=');
      await new Promise((r) => setTimeout(r, slow ? 60 : 0));
      return envelope([item({ id: slow ? 'proj-STALE' : 'proj-FRESH' })], {
        total: 2,
        offset: slow ? 0 : 100,
        limit: 1
      });
    });

    store.setQuery({ limit: 1, offset: 0 });
    store.setQuery({ limit: 1, offset: 100 });
    await new Promise((r) => setTimeout(r, 150));

    // Both responses have arrived; the older one must not have overwritten the
    // newer. The query changes per keystroke in the filter box, and `fetch`
    // makes no ordering promise.
    expect(store.all.map((i) => i.id)).toEqual(['proj-FRESH']);
    expect(store.offset).toBe(100);
  });

  it('loads metadata without fetching any items before a view declares a query', async () => {
    let itemCalls = 0;
    vi.stubGlobal('fetch', async (url: string | URL) => {
      if (String(url).includes('/items')) {
        itemCalls++;
        return envelope([], { total: 0 });
      }
      return envelope(META);
    });

    await store.refetch();
    // A deep link to the list page used to pull the entire store here, before
    // the route had a chance to say which page it wanted.
    expect(itemCalls).toBe(0);
    expect(store.meta?.id_prefix).toBe('proj');
  });
});
