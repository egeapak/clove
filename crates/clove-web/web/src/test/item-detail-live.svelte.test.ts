// @vitest-environment jsdom
import { describe, it, expect, vi, beforeAll, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup } from '@testing-library/svelte';
// The stub itself: the routes' `$app/stores` resolves to it under vitest.
import { page } from './app-stubs/stores';
import { store, startLive } from '$lib/store.svelte';
import type { Comment, Item } from '$lib/types';
import { stubApi, item, META } from './fake-api';
import DetailPage from '../routes/items/[id]/+page.svelte';

const ID = 'proj-7af3q2k9';

// The server's current answer for the item; tests change it to play "someone
// edited this from the CLI".
let current: Item;
let comments: Comment[];

class FakeSocket {
  static OPEN = 1;
  static CONNECTING = 0;
  static last: FakeSocket | null = null;
  readyState = FakeSocket.OPEN;
  onopen: (() => void) | null = null;
  onmessage: ((ev: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  onerror: (() => void) | null = null;
  constructor(public url: string) {
    FakeSocket.last = this;
  }
  close() {}
}

/** Deliver a live `batch` frame, as the server does after any store change. */
function liveBatch(seq: number) {
  FakeSocket.last!.onmessage!({ data: JSON.stringify({ event: 'batch', data: { seq } }) });
}

beforeAll(async () => {
  vi.stubGlobal('WebSocket', FakeSocket);
  stubApi((path) => {
    if (path === '/meta') return META;
    if (path === `/items/${ID}`) return current;
    if (path === `/items/${ID}/deptree`) {
      return { id: ID, title: current.title, status: current.status, ready: true, cycle_ref: false, children: [] };
    }
    if (path === `/items/${ID}/comments`) return comments;
    // List/board rows are lean: no body.
    if (path === '/items') return [{ ...current, body: '' }];
    throw new Error('unexpected request ' + path);
  });
  current = item({ id: ID, title: 'Before', body: 'old body' });
  comments = [];
  await startLive();
  FakeSocket.last!.onopen!();
});

beforeEach(() => {
  current = item({ id: ID, title: 'Before', body: 'old body', updated: '2026-01-01T00:00:00Z' });
  comments = [];
  page.set({ ...pageValue(`http://localhost/p/demo/items/${ID}`) });
});

afterEach(() => cleanup());

function pageValue(href: string) {
  return {
    url: new URL(href),
    params: { id: ID },
    route: { id: '/items/[id]' },
    status: 200,
    error: null,
    data: {},
    state: {},
    form: null
  };
}

function edit(over: Partial<Item>, at: string) {
  current = { ...current, ...over, updated: at };
}

describe('item detail page live updates', () => {
  it('refreshes an item opened by direct URL when a live batch arrives', async () => {
    render(DetailPage, { props: { data: { id: ID } } });
    expect(await screen.findByRole('heading', { name: 'Before' })).toBeInTheDocument();

    edit({ title: 'After', status: 'in_progress' }, '2026-01-02T00:00:00Z');
    liveBatch(1);

    expect(await screen.findByRole('heading', { name: 'After' })).toBeInTheDocument();
  });

  it('picks up a title change through the board-wide store refetch', async () => {
    store.setQuery({ limit: 0 });
    render(DetailPage, { props: { data: { id: ID } } });
    expect(await screen.findByRole('heading', { name: 'Before' })).toBeInTheDocument();

    edit({ title: 'Renamed' }, '2026-01-02T12:00:00Z');
    liveBatch(2);

    expect(await screen.findByRole('heading', { name: 'Renamed' })).toBeInTheDocument();
  });

  it('refreshes the body when the item was reached from the board', async () => {
    // The board holds the whole store with lean rows (no body), so the store
    // copy updates on a batch but the body came from the detail fetch.
    store.setQuery({ limit: 0 });
    render(DetailPage, { props: { data: { id: ID } } });
    expect(await screen.findByText('old body')).toBeInTheDocument();

    edit({ body: 'new body' }, '2026-01-03T00:00:00Z');
    liveBatch(3);

    expect(await screen.findByText('new body')).toBeInTheDocument();
  });

  it('refreshes open comments when a live batch arrives', async () => {
    page.set(pageValue(`http://localhost/p/demo/items/${ID}?view=comments`));
    render(DetailPage, { props: { data: { id: ID } } });
    expect(await screen.findByText('No comments yet.')).toBeInTheDocument();

    comments = [{ author: 'cli', timestamp: '2026-01-04T00:00:00Z', body: 'from the terminal' }];
    edit({ comment_count: 1 }, '2026-01-04T00:00:00Z');
    liveBatch(4);

    expect(await screen.findByText('from the terminal')).toBeInTheDocument();
  });
});
