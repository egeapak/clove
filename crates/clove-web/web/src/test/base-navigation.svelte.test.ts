// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent } from '@testing-library/svelte';

// Served by the daemon hub: the app lives under a project prefix, and every
// navigation must stay inside it.
const { goto } = vi.hoisted(() => ({ goto: vi.fn(async () => {}) }));
vi.mock('$app/paths', () => ({ base: '/p/demo', assets: '/p/demo' }));
vi.mock('$app/navigation', () => ({
  goto,
  invalidate: async () => {},
  invalidateAll: async () => {}
}));

// The stub itself: the routes' `$app/stores` resolves to it under vitest.
import { page } from './app-stubs/stores';
import { stubApi, item, META } from './fake-api';
import ListPage from '../routes/list/+page.svelte';
import TimelinePage from '../routes/timeline/+page.svelte';
import DetailPage from '../routes/items/[id]/+page.svelte';

const ID = 'proj-7af3q2k9';

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

/** Where the browser lands: `goto` resolves its target against the current URL. */
function landedOn(from: string): string {
  expect(goto).toHaveBeenCalled();
  const target = String((goto.mock.calls.at(-1) as unknown[])[0]);
  return new URL(target, from).pathname;
}

beforeEach(() => {
  goto.mockClear();
  stubApi((path) => {
    if (path === '/meta') return META;
    if (path === '/items') return [item({ id: ID, title: 'Row' })];
    if (path === '/stats/history') return [];
    if (path === `/items/${ID}`) return item({ id: ID, title: 'Row' });
    if (path === `/items/${ID}/deptree`) return null;
    throw new Error('unexpected request ' + path);
  });
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe('navigation under the hub prefix', () => {
  it('opens a list row with Enter inside the project', async () => {
    const from = 'http://localhost/p/demo/list';
    at(from);
    render(ListPage);
    await screen.findByText('1–1 of 1', { exact: false });
    await fireEvent.keyDown(window, { key: 'Enter' });
    expect(landedOn(from)).toBe(`/p/demo/items/${ID}`);
  });

  it('opens a clicked list row inside the project', async () => {
    const from = 'http://localhost/p/demo/list';
    at(from);
    // jsdom lays nothing out; give the virtualized table a viewport to fill.
    vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockReturnValue(600);
    vi.spyOn(HTMLElement.prototype, 'offsetWidth', 'get').mockReturnValue(800);
    render(ListPage);
    const cell = await screen.findByText('Row');
    await fireEvent.click(cell.closest('tr')!);
    expect(landedOn(from)).toBe(`/p/demo/items/${ID}`);
  });

  it('links a timeline bar and its row label inside the project', async () => {
    at('http://localhost/p/demo/timeline');
    const { container } = render(TimelinePage);
    await screen.findByRole('link', { name: /bug Row/ });
    const bar = container.querySelector('a.bar')!;
    const label = container.querySelector('a.rowlabel')!;
    expect(bar.getAttribute('href')).toBe(`/p/demo/items/${ID}`);
    expect(label.getAttribute('href')).toBe(`/p/demo/items/${ID}`);
  });

  it('returns to the project list after deleting an item', async () => {
    const from = `http://localhost/p/demo/items/${ID}`;
    at(from);
    vi.stubGlobal('fetch', async (input: string | URL, init?: RequestInit) => {
      const path = new URL(String(input), 'http://localhost').pathname;
      const data =
        init?.method === 'DELETE' ? { deleted: ID } : path.endsWith('/meta') ? META : item({ id: ID, title: 'Row' });
      return new Response(JSON.stringify({ v: 1, ok: true, data }), {
        status: 200,
        headers: { 'content-type': 'application/json' }
      });
    });
    render(DetailPage, { props: { data: { id: ID } } });
    await screen.findByRole('heading', { name: 'Row' });
    await fireEvent.click(screen.getByRole('button', { name: 'Delete item' }));
    await fireEvent.click(await screen.findByRole('button', { name: 'Delete' }));
    await vi.waitFor(() => expect(goto).toHaveBeenCalled());
    expect(landedOn(from)).toBe('/p/demo/list');
  });
});
