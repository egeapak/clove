// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/svelte';

const { goto } = vi.hoisted(() => ({ goto: vi.fn(async () => {}) }));
vi.mock('$app/navigation', () => ({
  goto,
  invalidate: async () => {},
  invalidateAll: async () => {}
}));

import { page } from './app-stubs/stores';
import { store } from '$lib/store.svelte';
import type { Item } from '$lib/types';
import { stubApi, item, META } from './fake-api';
import ListPage from '../routes/list/+page.svelte';

function at(href: string) {
  page.set({
    url: new URL(href),
    params: {},
    route: { id: null },
    status: 200,
    error: null,
    data: {},
    state: {},
    form: null
  });
}

function layout() {
  vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockReturnValue(600);
  vi.spyOn(HTMLElement.prototype, 'offsetWidth', 'get').mockReturnValue(1200);
}

const many = (n: number): Item[] =>
  Array.from({ length: n }, (_, i) => item({ id: `proj-${String(i).padStart(8, '0')}`, title: `Item ${i}` }));

/** Answers `/items` like the server: `q` filters titles, then the window. */
function serve(all: Item[], hold?: Promise<void>) {
  const requests: URL[] = [];
  vi.stubGlobal('fetch', async (input: string | URL) => {
    const url = new URL(String(input), 'http://localhost');
    const path = url.pathname.slice(url.pathname.indexOf('/api/v1') + '/api/v1'.length);
    requests.push(url);
    const json = (data: unknown, meta: Record<string, unknown> = {}) =>
      new Response(JSON.stringify({ v: 1, ok: true, data, _meta: meta }), {
        status: 200,
        headers: { 'content-type': 'application/json' }
      });
    if (path === '/meta') return json(META);
    if (path === '/items') {
      const q = url.searchParams.get('q');
      const matched = q ? all.filter((i) => i.title.includes(q)) : all;
      const offset = Number(url.searchParams.get('offset') ?? 0);
      const limit = Number(url.searchParams.get('limit') ?? 0);
      const rows = limit ? matched.slice(offset, offset + limit) : matched.slice(offset);
      if (hold && !q) await hold;
      return json(rows, { total: matched.length, offset, limit });
    }
    throw new Error('unexpected request ' + path);
  });
  return requests;
}

beforeEach(() => {
  goto.mockClear();
  store.resetForTests();
  store.meta = null;
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe('list filter box', () => {
  it('keeps a trailing space when the URL catches up with the typed text', async () => {
    serve(many(5));
    at('http://localhost/list');
    render(ListPage);
    const box = screen.getByLabelText('Filter') as HTMLInputElement;
    await fireEvent.input(box, { target: { value: 'Item 2 ' } });
    // The debounced write lands: the URL carries the trimmed text.
    at('http://localhost/list?q=Item+2');
    await waitFor(() => expect(store.loaded).toBe(true));
    expect(box.value).toBe('Item 2 ');
  });

  it('still follows the URL when it says something else', async () => {
    serve(many(5));
    at('http://localhost/list?q=Item+2');
    render(ListPage);
    const box = screen.getByLabelText('Filter') as HTMLInputElement;
    await waitFor(() => expect(box.value).toBe('Item 2'));
    at('http://localhost/list?q=Item+4');
    await waitFor(() => expect(box.value).toBe('Item 4'));
    at('http://localhost/list');
    await waitFor(() => expect(box.value).toBe(''));
  });

  it('drops a pending debounced URL write when the page is destroyed', async () => {
    vi.useFakeTimers();
    serve(many(5));
    at('http://localhost/list');
    const { unmount } = render(ListPage);
    const box = screen.getByLabelText('Filter') as HTMLInputElement;
    await fireEvent.input(box, { target: { value: 'abc' } });
    unmount();
    goto.mockClear();
    vi.advanceTimersByTime(1000);
    expect(goto).not.toHaveBeenCalled();
  });
});

describe('list paging', () => {
  it('does not clamp the page against the total of the previous query', async () => {
    // The store still holds the answer to an earlier search: 3 matches.
    const all = [...many(500).map((i, n) => (n < 3 ? { ...i, title: `uniq ${i.title}` } : i))];
    let release!: () => void;
    const hold = new Promise<void>((r) => (release = r));
    serve(all, hold);
    store.setQuery({ mode: 'list', q: 'uniq', sort: 'rank', dir: 'asc', limit: 100 });
    await waitFor(() => expect(store.total).toBe(3));

    // Back to a deep page of the unfiltered list; its response is still in flight.
    at('http://localhost/list?page=5');
    render(ListPage);
    await new Promise((r) => setTimeout(r, 20));
    expect(goto).not.toHaveBeenCalled();

    release();
    await screen.findByText('401–500 of 500');
    expect(goto).not.toHaveBeenCalled();
  });

  it('still steps back when the page is past the end of its own result', async () => {
    serve(many(150));
    at('http://localhost/list?page=5');
    render(ListPage);
    await waitFor(() => expect(goto).toHaveBeenCalled());
    expect(String((goto.mock.calls.at(-1) as unknown[])[0])).toBe('?page=2');
  });
});

describe('list rows', () => {
  it('links each row to its item, so it can be reached and opened without a mouse', async () => {
    serve(many(3));
    at('http://localhost/list');
    layout();
    render(ListPage);
    const link = (await screen.findByRole('link', { name: 'Item 1' })) as HTMLAnchorElement;
    expect(link.getAttribute('href')).toBe('/items/proj-00000001');
    // The link navigates by itself; the row handler must not navigate again.
    link.addEventListener('click', (e) => e.preventDefault()); // jsdom cannot navigate
    await fireEvent.click(link);
    expect(goto).not.toHaveBeenCalled();
    // A click elsewhere on the row still opens the item.
    await fireEvent.click(link.closest('tr')!.querySelector('td')!);
    expect(String((goto.mock.calls.at(-1) as unknown[])[0])).toBe('/items/proj-00000001');
  });

  it('scrolls the keyboard cursor into view', async () => {
    serve(many(100));
    at('http://localhost/list');
    layout();
    const { container } = render(ListPage);
    await screen.findByText('1–100 of 100');
    const wrap = container.querySelector('.table-wrap') as HTMLElement;
    // jsdom lays nothing out: give the scroller a scrollable extent.
    Object.defineProperty(wrap, 'scrollHeight', { value: 100 * 38 });
    Object.defineProperty(wrap, 'clientHeight', { value: 600 });
    const scrolled = vi.fn();
    wrap.scrollTo = scrolled as unknown as typeof wrap.scrollTo;
    for (let i = 0; i < 60; i++) await fireEvent.keyDown(window, { key: 'j' });
    expect(scrolled).toHaveBeenCalled();
    const tops = scrolled.mock.calls.map((c) => (c as [{ top: number }])[0].top);
    expect(Math.max(...tops)).toBeGreaterThan(0);
  });
});
