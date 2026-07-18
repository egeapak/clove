import { describe, it, expect, vi, afterEach } from 'vitest';
import { api } from './api';

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
