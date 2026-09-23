import { describe, it, expect, vi, afterEach } from 'vitest';

// Served by the hub under a project prefix: every API call must carry it.
vi.mock('$app/paths', () => ({ base: '/p/demo', assets: '/p/demo' }));

import { api } from './api';

describe('api under a project base', () => {
  afterEach(() => vi.unstubAllGlobals());

  it('requests the project-prefixed API', async () => {
    let calledUrl = '';
    vi.stubGlobal('fetch', async (url: string | URL) => {
      calledUrl = String(url);
      return new Response(JSON.stringify({ v: 1, ok: true, data: [] }), {
        status: 200,
        headers: { 'content-type': 'application/json' }
      });
    });
    await api.items();
    expect(calledUrl.startsWith('/p/demo/api/v1/items')).toBe(true);
  });

  it('lists projects from the hub root', async () => {
    let calledUrl = '';
    vi.stubGlobal('fetch', async (url: string | URL) => {
      calledUrl = String(url);
      return new Response(JSON.stringify({ v: 1, ok: true, data: { projects: [] } }), {
        status: 200,
        headers: { 'content-type': 'application/json' }
      });
    });
    expect(await api.projects()).toEqual([]);
    expect(calledUrl).toBe('/api/v1/projects');
  });
});
