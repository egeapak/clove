// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/svelte';
import { page } from './app-stubs/stores';
import { store } from '$lib/store.svelte';
import { resetRelated } from '$lib/related.svelte';
import type { Item } from '$lib/types';
import { item, META } from './fake-api';
import BoardPage from '../routes/board/+page.svelte';
import NewItemModal from '$lib/components/NewItemModal.svelte';

let items: Item[] = [];

beforeEach(() => {
  resetRelated();
  store.resetForTests();
  store.meta = null;
  page.set({
    url: new URL('http://localhost/board'),
    params: {},
    route: { id: null },
    status: 200,
    error: null,
    data: {},
    state: {},
    form: null
  });
  vi.stubGlobal('fetch', async (input: string | URL, init?: RequestInit) => {
    const url = new URL(String(input), 'http://localhost');
    const path = url.pathname.slice(url.pathname.indexOf('/api/v1') + '/api/v1'.length);
    const ok = (data: unknown) =>
      new Response(JSON.stringify({ v: 1, ok: true, data, _meta: {} }), {
        status: 200,
        headers: { 'content-type': 'application/json' }
      });
    if (path === '/meta') return ok(META);
    if (path === '/items') return ok(items);
    const one = /^\/items\/([^/]+)$/.exec(path);
    if (one && init?.method === 'PATCH') {
      const cur = items.find((i) => i.id === one[1])!;
      return ok({ ...cur, ...JSON.parse(String(init.body)), updated: '2026-02-01T00:00:00Z' });
    }
    throw new Error('unexpected request ' + path);
  });
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe('board keyboard move', () => {
  it('keeps focus on the moved card', async () => {
    items = [
      item({ id: 'proj-0000000A', title: 'First' }),
      item({ id: 'proj-0000000B', title: 'Second' })
    ];
    vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockReturnValue(600);
    vi.spyOn(HTMLElement.prototype, 'offsetWidth', 'get').mockReturnValue(1200);
    render(BoardPage);
    const move = await screen.findByRole('button', { name: 'Move #0000000A to In Progress' });
    move.focus();
    await fireEvent.click(move);
    // The card now lives in the next column; its "move right" button targets Closed.
    const again = await screen.findByRole('button', { name: 'Move #0000000A to Closed' });
    await waitFor(() => expect(document.activeElement).toBe(again));
  });
});

describe('new item modal', () => {
  it('closes at once on Escape while the form is untouched', async () => {
    const onclose = vi.fn();
    render(NewItemModal, { props: { onclose } });
    await fireEvent.keyDown(screen.getByRole('dialog'), { key: 'Escape' });
    expect(onclose).toHaveBeenCalledTimes(1);
  });

  it('asks before Escape or the backdrop discards a draft', async () => {
    const onclose = vi.fn();
    render(NewItemModal, { props: { onclose } });
    const dialog = screen.getByRole('dialog');
    await fireEvent.input(screen.getByPlaceholderText('Short summary…'), { target: { value: 'A draft title' } });

    await fireEvent.keyDown(dialog, { key: 'Escape' });
    expect(onclose).not.toHaveBeenCalled();
    expect(screen.getByRole('alert')).toHaveTextContent('Discard this draft?');

    // Escape again keeps editing; the draft is still there.
    await fireEvent.keyDown(dialog, { key: 'Escape' });
    expect(screen.queryByRole('alert')).toBeNull();
    expect((screen.getByPlaceholderText('Short summary…') as HTMLInputElement).value).toBe('A draft title');
    expect(onclose).not.toHaveBeenCalled();

    await fireEvent.click(screen.getByRole('button', { name: 'close' }));
    expect(onclose).not.toHaveBeenCalled();
    await fireEvent.click(screen.getByRole('button', { name: 'Discard' }));
    expect(onclose).toHaveBeenCalledTimes(1);
  });
});
