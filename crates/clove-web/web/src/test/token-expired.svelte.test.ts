// @vitest-environment jsdom
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, screen, cleanup } from '@testing-library/svelte';
import { createRawSnippet } from 'svelte';
import { store, startLive } from '$lib/store.svelte';
import { UNAUTHORIZED_MESSAGE } from '$lib/api';
import { META } from './fake-api';
import Layout from '../routes/+layout.svelte';

// The access token lasts as long as the server process. After a restart an
// open tab gets 401 on every request and on the WebSocket upgrade (which a
// browser reports as a plain close, with no status).

class FakeSocket {
  static OPEN = 1;
  static CONNECTING = 0;
  static made: FakeSocket[] = [];
  readyState = FakeSocket.CONNECTING;
  onopen: (() => void) | null = null;
  onmessage: ((ev: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  constructor(public url: string) {
    FakeSocket.made.push(this);
  }
  close() {}
}

let restarted = false;
function stubServer() {
  vi.stubGlobal('fetch', async (input: string | URL) => {
    const path = new URL(String(input), 'http://localhost').pathname;
    if (restarted) {
      return new Response('401: this clove web UI needs its access token.', {
        status: 401,
        headers: { 'content-type': 'text/plain; charset=utf-8' }
      });
    }
    const data = path.endsWith('/meta') ? META : [];
    return new Response(JSON.stringify({ v: 1, ok: true, data, _meta: {} }), {
      status: 200,
      headers: { 'content-type': 'application/json' }
    });
  });
}

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

describe('a tab whose server restarted', () => {
  it('stops reconnecting and says how to get a new token', async () => {
    vi.useFakeTimers();
    vi.stubGlobal('WebSocket', FakeSocket);
    stubServer();

    await startLive();
    const first = FakeSocket.made.at(-1)!;
    first.readyState = FakeSocket.OPEN;
    first.onopen!();
    await vi.runAllTimersAsync();
    expect(store.conn).toBe('live');

    // The server restarts with a new token: the socket drops, and the
    // reconnect's upgrade is refused.
    restarted = true;
    first.readyState = 3;
    first.onclose!();
    expect(store.conn).toBe('offline');
    await vi.advanceTimersByTimeAsync(1000);
    const retry = FakeSocket.made.at(-1)!;
    expect(retry).not.toBe(first);
    retry.readyState = 3;
    retry.onclose!();
    // The page asks the API why; the 401 there ends the loop.
    await vi.runAllTimersAsync();
    expect(store.conn).toBe('unauthorized');

    const sockets = FakeSocket.made.length;
    await vi.advanceTimersByTimeAsync(120_000);
    expect(FakeSocket.made.length).toBe(sockets);
    // A tab that becomes visible again does not retry either.
    document.dispatchEvent(new Event('visibilitychange'));
    window.dispatchEvent(new Event('online'));
    expect(FakeSocket.made.length).toBe(sockets);

    // The layout shows the reason and the fix, and the badge says "no access".
    const children = createRawSnippet(() => ({ render: () => '<p>page</p>' }));
    render(Layout, { props: { children } });
    expect(screen.getByRole('alert').textContent).toBe(UNAUTHORIZED_MESSAGE);
    expect(screen.getByRole('status').getAttribute('aria-label')).toBe('connection: no access');
  });
});
