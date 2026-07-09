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
