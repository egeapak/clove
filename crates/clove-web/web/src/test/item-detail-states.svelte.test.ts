// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, waitFor } from '@testing-library/svelte';
import { page } from './app-stubs/stores';
import { store } from '$lib/store.svelte';
import { resetRelated } from '$lib/related.svelte';
import type { Comment, Item } from '$lib/types';
import { item, META } from './fake-api';
import DetailPage from '../routes/items/[id]/+page.svelte';
import EditPage from '../routes/items/[id]/edit/+page.svelte';

const A = 'proj-0000000A';
const B = 'proj-0000000B';

let items: Map<string, Item>;
let comments: Comment[];
/** What a write answers: an HTTP status to fail with, or nothing to succeed. */
let failWrites: number | null;
let writes: Array<{ method: string; path: string; body: unknown }>;

function at(id: string, search = '') {
  page.set({
    url: new URL(`http://localhost/items/${id}${search}`),
    params: { id },
    route: { id: '/items/[id]' },
    status: 200,
    error: null,
    data: {},
    state: {},
    form: null
  });
}

const ok = (data: unknown) =>
  new Response(JSON.stringify({ v: 1, ok: true, data, _meta: {} }), {
    status: 200,
    headers: { 'content-type': 'application/json' }
  });
const fail = (status: number, code: string) =>
  new Response(JSON.stringify({ v: 1, ok: false, error: { code, message: code, exit: 1 }, _meta: {} }), {
    status,
    headers: { 'content-type': 'application/json' }
  });

beforeEach(() => {
  resetRelated();
  store.resetForTests();
  store.meta = null;
  items = new Map([
    [A, item({ id: A, title: 'Alpha', body: 'alpha body', relates: [B] })],
    [B, item({ id: B, title: 'Beta', body: 'beta body' })]
  ]);
  comments = [];
  failWrites = null;
  writes = [];
  vi.stubGlobal('fetch', async (input: string | URL, init?: RequestInit) => {
    const url = new URL(String(input), 'http://localhost');
    const path = url.pathname.slice(url.pathname.indexOf('/api/v1') + '/api/v1'.length);
    const method = init?.method ?? 'GET';
    if (method !== 'GET') {
      writes.push({ method, path, body: init?.body ? JSON.parse(String(init.body)) : null });
      if (failWrites) return fail(failWrites, 'IO_ERROR');
      const one = /^\/items\/([^/]+)/.exec(path)!;
      const cur = items.get(one[1])!;
      const next = method === 'PATCH' ? { ...cur, ...(writes.at(-1)!.body as object), updated: '2026-02-01T00:00:00Z' } : cur;
      items.set(next.id, next);
      return ok(next);
    }
    if (path === '/meta') return ok(META);
    if (path === '/items') return ok([]);
    if (path.endsWith('/comments')) return ok(comments);
    if (path.endsWith('/deptree')) return ok(null);
    const one = /^\/items\/([^/]+)$/.exec(path);
    if (one) {
      const found = items.get(one[1]);
      return found ? ok(found) : fail(404, 'ITEM_NOT_FOUND');
    }
    throw new Error('unexpected request ' + path);
  });
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe('an item that does not exist', () => {
  it('shows a not-found state instead of loading forever', async () => {
    at('proj-ZZZZZZZZ');
    render(DetailPage, { props: { data: { id: 'proj-ZZZZZZZZ' } } });
    const alert = await screen.findByRole('alert');
    expect(alert).toHaveTextContent('Item #ZZZZZZZZ not found');
    expect(screen.getByRole('link', { name: 'Back to the list' })).toHaveAttribute('href', '/list');
    expect(screen.queryByText(/Loading/)).toBeNull();
  });

  it('shows the same state on the edit route', async () => {
    at('proj-ZZZZZZZZ');
    render(EditPage, { props: { data: { id: 'proj-ZZZZZZZZ' } } });
    expect(await screen.findByRole('alert')).toHaveTextContent('Item #ZZZZZZZZ not found');
    expect(screen.queryByText(/Loading/)).toBeNull();
  });

  it('replaces an open item that is deleted elsewhere', async () => {
    at(A);
    render(DetailPage, { props: { data: { id: A } } });
    expect(await screen.findByRole('heading', { name: 'Alpha' })).toBeInTheDocument();
    items.delete(A);
    store.liveRev += 1; // a live batch
    expect(await screen.findByRole('alert')).toHaveTextContent('Item #0000000A not found');
    expect(screen.queryByRole('heading', { name: 'Alpha' })).toBeNull();
  });

  it('offers a retry when the load fails for another reason', async () => {
    const real = globalThis.fetch;
    let down = true;
    vi.stubGlobal('fetch', async (input: string | URL, init?: RequestInit) => {
      if (down && /\/items\/[^/]+$/.test(String(input))) return fail(500, 'IO_ERROR');
      return real(input, init);
    });
    at(A);
    render(DetailPage, { props: { data: { id: A } } });
    expect(await screen.findByRole('alert')).toHaveTextContent('Couldn’t load #0000000A');
    down = false;
    await fireEvent.click(screen.getByRole('button', { name: 'Retry' }));
    expect(await screen.findByRole('heading', { name: 'Alpha' })).toBeInTheDocument();
  });
});

describe('comments', () => {
  it('takes a failed comment back out of the thread and restores the draft', async () => {
    at(A, '?view=comments');
    const { container } = render(DetailPage, { props: { data: { id: A } } });
    const box = (await screen.findByLabelText('New comment')) as HTMLTextAreaElement;
    failWrites = 500;
    await fireEvent.input(box, { target: { value: 'hello fail' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Comment' }));
    await waitFor(() => expect(writes).toHaveLength(1));
    await waitFor(() => expect(box.value).toBe('hello fail'));
    expect(container.querySelectorAll('.comments .comment')).toHaveLength(0);
    expect(screen.getByText('No comments yet.')).toBeInTheDocument();
  });
});

describe('moving from one item to the next in the same page', () => {
  it('does not carry the delete confirmation, drafts or comments over', async () => {
    comments = [{ author: 'ann', timestamp: '2026-01-01T00:00:00Z', body: 'about alpha' }];
    at(A, '?view=comments');
    const { rerender, container } = render(DetailPage, { props: { data: { id: A } } });
    expect(await screen.findByText('about alpha')).toBeInTheDocument();
    await fireEvent.input(screen.getByLabelText('New comment'), { target: { value: 'draft for alpha' } });
    await fireEvent.input(screen.getByLabelText('Label to add'), { target: { value: 'lbl' } });
    await fireEvent.click(screen.getByRole('button', { name: 'Delete item' }));
    expect(screen.getByText('Permanently delete #0000000A?')).toBeInTheDocument();

    // A relationship link: same component, new id.
    comments = [];
    at(B, '?view=comments');
    await rerender({ data: { id: B } });
    expect(await screen.findByRole('heading', { name: 'Beta' })).toBeInTheDocument();
    expect(screen.queryByText(/Permanently delete/)).toBeNull();
    expect(screen.getByRole('button', { name: 'Delete item' })).toBeInTheDocument();
    expect((screen.getByLabelText('New comment') as HTMLTextAreaElement).value).toBe('');
    expect((screen.getByLabelText('Label to add') as HTMLInputElement).value).toBe('');
    await waitFor(() => expect(container.querySelectorAll('.comments .comment')).toHaveLength(0));
    expect(screen.queryByText('about alpha')).toBeNull();
  });
});

describe('inline edits on an item outside the store window', () => {
  /** Open A, then let a live refetch replace the window with one that lacks it. */
  async function openOutsideWindow() {
    at(A);
    render(DetailPage, { props: { data: { id: A } } });
    await screen.findByRole('heading', { name: 'Alpha' });
    store.replaceAll([]);
    expect(store.items.has(A)).toBe(false);
    return screen.getByLabelText('Status') as HTMLSelectElement;
  }

  it('applies the server answer', async () => {
    const status = await openOutsideWindow();
    await fireEvent.change(status, { target: { value: 'closed' } });
    await waitFor(() => expect(writes).toHaveLength(1));
    // The server's own `updated`, not the optimistic stamp: the answer landed.
    await waitFor(() => expect(store.items.get(A)?.updated).toBe('2026-02-01T00:00:00Z'));
    expect(store.items.get(A)?.status).toBe('closed');
    expect(status.value).toBe('closed');
  });

  it('rolls a rejected edit back', async () => {
    const status = await openOutsideWindow();
    failWrites = 500;
    await fireEvent.change(status, { target: { value: 'closed' } });
    await waitFor(() => expect(writes).toHaveLength(1));
    await waitFor(() => expect(status.value).toBe('open'));
  });
});

describe('detail sidebar controls', () => {
  it('have accessible names', async () => {
    at(A, '?view=deptree');
    render(DetailPage, { props: { data: { id: A } } });
    await screen.findByRole('heading', { name: 'Alpha' });
    expect(screen.getByRole('combobox', { name: 'Status' })).toBeInTheDocument();
    expect(screen.getByRole('combobox', { name: 'Priority' })).toBeInTheDocument();
    expect(screen.getByRole('textbox', { name: 'Assignee' })).toBeInTheDocument();
    expect(screen.getByRole('textbox', { name: 'Label to add' })).toBeInTheDocument();
    expect(screen.getByRole('textbox', { name: 'Dependency id to add' })).toBeInTheDocument();
  });
});
