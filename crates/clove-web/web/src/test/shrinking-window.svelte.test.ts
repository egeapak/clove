// @vitest-environment jsdom
//
// The list and the board render a virtual window over the store's rows. When
// the rows shrink (a filter, a tab, a shorter page, a card moved out of a
// column) the window still names the old indexes until the virtualizer is
// re-synced, so a rendered row must never dereference an index past the end.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/svelte';
import { page } from './app-stubs/stores';
import { store } from '$lib/store.svelte';
import { resetRelated } from '$lib/related.svelte';
import { visibleRows } from '$lib/virtual.svelte';
import type { Item } from '$lib/types';
import { stubApi, item, META } from './fake-api';
import BoardPage from '../routes/board/+page.svelte';
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

const many = (n: number, over: Partial<Item> = {}): Item[] =>
  Array.from({ length: n }, (_, i) =>
    item({ id: `proj-${String(i).padStart(8, '0')}`, title: `Item ${i}`, ...over })
  );

let items: Item[] = [];
let errors: unknown[] = [];
const onError = (e: ErrorEvent) => errors.push(e.error ?? e.message);
const onRejection = (e: PromiseRejectionEvent) => errors.push(e.reason);

beforeEach(() => {
  resetRelated();
  store.resetForTests();
  store.meta = null;
  items = [];
  errors = [];
  window.addEventListener('error', onError);
  window.addEventListener('unhandledrejection', onRejection);
  stubApi((p) => {
    if (p === '/meta') return META;
    if (p === '/items') return items;
    const one = /^\/items\/([^/]+)$/.exec(p);
    if (one) {
      const found = items.find((i) => i.id === one[1]);
      if (found) return found;
    }
    throw new Error('unexpected request ' + p);
  });
});

afterEach(() => {
  window.removeEventListener('error', onError);
  window.removeEventListener('unhandledrejection', onRejection);
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe('visibleRows', () => {
  const v = (index: number) => ({ index, key: index, start: index * 10, end: index * 10 + 10, size: 10, lane: 0 });

  it('pairs each virtual item with its row', () => {
    expect(visibleRows([v(0), v(1)], ['a', 'b', 'c']).map((r) => r.item)).toEqual(['a', 'b']);
  });

  it('drops virtual items whose index is past the end of the rows', () => {
    const out = visibleRows([v(0), v(1), v(2), v(3)], ['a', 'b']);
    expect(out.map((r) => r.item)).toEqual(['a', 'b']);
    expect(out.map((r) => r.v.index)).toEqual([0, 1]);
  });
});

describe('list', () => {
  it('renders the shorter result when the rows shrink under the rendered window', async () => {
    at('http://localhost/list');
    items = many(30);
    layout();
    const { container } = render(ListPage);
    await waitFor(() => expect(container.querySelectorAll('tbody tr:not(.spacer)').length).toBeGreaterThan(3));

    items = many(3);
    await store.refetch();
    await waitFor(() => expect(container.querySelectorAll('tbody tr:not(.spacer)')).toHaveLength(3));
    expect(errors).toEqual([]);
    expect(screen.getByText('1–3 of 3')).toBeInTheDocument();
  });
});

describe('board', () => {
  it('moves a card out of a column whose last card is rendered', async () => {
    at('http://localhost/board');
    items = many(6);
    layout();
    const { container } = render(BoardPage);
    const move = await screen.findByRole('button', { name: 'Move #00000005 to In Progress' });
    const patched = vi.fn();
    stubApi((p) => {
      if (p === '/meta') return META;
      if (p === '/items') return items;
      if (p === '/items/proj-00000005') {
        patched();
        return { ...items[5], status: 'in_progress' };
      }
      throw new Error('unexpected request ' + p);
    });
    await fireEvent.click(move);
    await waitFor(() => expect(patched).toHaveBeenCalled());
    await waitFor(() => {
      const counts = [...container.querySelectorAll('.col-head .cnt')].map((n) => n.textContent);
      expect(counts).toEqual(['5', '1', '0']);
    });
    expect(errors).toEqual([]);
    expect(await screen.findByRole('button', { name: 'Move #00000005 to Closed' })).toBeInTheDocument();
  });
});
