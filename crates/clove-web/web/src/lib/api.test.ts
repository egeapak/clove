import { describe, it, expect, vi, afterEach } from 'vitest';
import { api, filterMock } from './api';

// `$app/environment` is stubbed with dev=false (see src/test/app-stubs), so
// withMock() takes the real() branch and actually issues fetch — which we stub.
function okJson() {
  return new Response(JSON.stringify({ v: 1, ok: true, data: {} }), {
    status: 200,
    headers: { 'content-type': 'application/json' }
  });
}

describe('api.delete force query param', () => {
  afterEach(() => vi.unstubAllGlobals());

  it('sends the literal force=true the server requires', async () => {
    let calledUrl = '';
    vi.stubGlobal('fetch', async (url: string | URL) => {
      calledUrl = String(url);
      return okJson();
    });
    await api.delete('proj-1', { force: true });
    expect(calledUrl).toContain('?force=true');
  });

  it('omits the force param entirely when not forcing', async () => {
    let calledUrl = '';
    vi.stubGlobal('fetch', async (url: string | URL) => {
      calledUrl = String(url);
      return okJson();
    });
    await api.delete('proj-1');
    expect(calledUrl).not.toContain('force');
  });
});

describe('api.history snapshot shape', () => {
  afterEach(() => vi.unstubAllGlobals());

  it('parses recorded-snapshot points including the richer level fields', async () => {
    vi.stubGlobal('fetch', async () =>
      new Response(
        JSON.stringify({
          v: 1,
          ok: true,
          data: [
            { date: '2026-07-16', created: 0, closed: 0, open: 2, total: 2, ready: 1, blocked: 1 },
            { date: '2026-07-17', created: 1, closed: 0, open: 3, total: 3, ready: 2, blocked: 1 }
          ],
          _meta: { synthesized: false, snapshots: 2 }
        }),
        { status: 200, headers: { 'content-type': 'application/json' } }
      )
    );
    const points = await api.history();
    expect(points).toHaveLength(2);
    // The synthesized-fallback fields are always present...
    expect(points[1].open).toBe(3);
    // ...and the snapshot-only levels come through when the server sends them.
    expect(points[1].total).toBe(3);
    expect(points[1].ready).toBe(2);
    expect(points[1].blocked).toBe(1);
  });
});

describe('api.items sends a window and reads the server counts', () => {
  afterEach(() => vi.unstubAllGlobals());

  function itemsResponse(ids: string[], meta: Record<string, unknown>) {
    return new Response(
      JSON.stringify({
        v: 1,
        ok: true,
        data: ids.map((id) => ({ id, title: id })),
        _meta: meta
      }),
      { status: 200, headers: { 'content-type': 'application/json' } }
    );
  }

  it('puts limit and offset on the URL, including an explicit limit=0', async () => {
    const urls: string[] = [];
    vi.stubGlobal('fetch', async (url: string | URL) => {
      urls.push(String(url));
      return itemsResponse([], {});
    });

    await api.items({ limit: 100, offset: 200, sort: 'updated', dir: 'desc' });
    expect(urls[0]).toContain('limit=100');
    expect(urls[0]).toContain('offset=200');
    expect(urls[0]).toContain('sort=updated');

    // `limit=0` means unlimited and must survive: it is how a view that really
    // does render everything says so, rather than inheriting the API default.
    await api.items({ limit: 0 });
    expect(urls[1]).toContain('limit=0');
    // offset 0 is every surface's default, so it is left off.
    expect(urls[1]).not.toContain('offset');
  });

  it('reports _meta.total, not the number of rows that came back', async () => {
    vi.stubGlobal('fetch', async () =>
      itemsResponse(['proj-1', 'proj-2'], { total: 412, returned: 2, offset: 100, limit: 2 })
    );
    const page = await api.items({ limit: 2, offset: 100 });
    expect(page.items.map((i) => i.id)).toEqual(['proj-1', 'proj-2']);
    // The whole point of the pager: the total describes the match set, the rows
    // describe the window. Falling back to `items.length` here would render
    // "101–102 of 2".
    expect(page.total).toBe(412);
    expect(page.returned).toBe(2);
    expect(page.offset).toBe(100);
    expect(page.limit).toBe(2);
  });

  it('falls back to the rows when a server sends no _meta', async () => {
    vi.stubGlobal('fetch', async () => itemsResponse(['proj-1', 'proj-2'], {}));
    const page = await api.items();
    expect(page.total).toBe(2);
    expect(page.returned).toBe(2);
  });
});

describe('filterMock answers a query the way the server would', () => {
  it('windows the match set and reports the pre-window total', () => {
    const all = filterMock({ limit: 0 });
    expect(all.total).toBeGreaterThan(3);
    expect(all.returned).toBe(all.items.length);

    const page = filterMock({ limit: 3, offset: 2 });
    expect(page.items).toHaveLength(3);
    expect(page.returned).toBe(3);
    // Pre-window count, exactly as `_meta.total` is on the server — a mock
    // reporting the page size would show "3–5 of 3" in the pager.
    expect(page.total).toBe(all.total);
    expect(page.items.map((i) => i.id)).toEqual(all.items.slice(2, 5).map((i) => i.id));
  });

  it('treats an absent limit as unlimited, like the web API default', () => {
    expect(filterMock({}).items).toHaveLength(filterMock({ limit: 0 }).items.length);
  });

  it('applies the requested sort before windowing', () => {
    // The whole match set, ordered by priority; the first page must be that
    // order's head, not the fixture's authoring order sliced.
    const sorted = filterMock({ sort: 'priority', dir: 'asc', limit: 0 });
    const prios = sorted.items.map((i) => i.priority);
    expect([...prios].sort((a, b) => a - b)).toEqual(prios);

    const first = filterMock({ sort: 'priority', dir: 'asc', limit: 4 });
    expect(first.items.map((i) => i.id)).toEqual(sorted.items.slice(0, 4).map((i) => i.id));
    expect(first.total).toBe(sorted.total);
  });

  it('windows the filtered set, not the whole fixture', () => {
    const closed = filterMock({ status: 'closed', limit: 0 });
    const unfiltered = filterMock({ limit: 0 });
    expect(closed.total).toBeLessThan(unfiltered.total);
    expect(closed.items.every((i) => i.status === 'closed')).toBe(true);
    expect(filterMock({ status: 'closed', limit: 1 }).total).toBe(closed.total);
  });
});
