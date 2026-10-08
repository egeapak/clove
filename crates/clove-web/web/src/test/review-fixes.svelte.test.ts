// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent } from '@testing-library/svelte';

const { goto } = vi.hoisted(() => ({ goto: vi.fn(async () => {}) }));
vi.mock('$app/navigation', () => ({
  goto,
  invalidate: async () => {},
  invalidateAll: async () => {}
}));

import { page } from './app-stubs/stores';
import { store } from '$lib/store.svelte';
import { related, ensureRelated, resetRelated } from '$lib/related.svelte';
import { stubApi, StubError, item, META } from './fake-api';
import ListPage from '../routes/list/+page.svelte';

const ID = 'proj-7af3q2k9';
const OTHER = 'proj-0000000B';

let otherTitle = 'First title';
let otherFails = false;

function settle() {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

beforeEach(() => {
  goto.mockClear();
  resetRelated();
  store.meta = null;
  otherTitle = 'First title';
  otherFails = false;
  page.set({
    url: new URL('http://localhost/list'),
    params: {},
    route: { id: null },
    status: 200,
    error: null,
    data: {},
    state: {},
    form: null
  });
  stubApi((path) => {
    if (path === '/meta') return META;
    if (path === '/items') return [item({ id: ID, title: 'Row', external_ref: 'gh-5', source_system: 'github' })];
    if (path === '/stats/history') return [];
    if (path === `/items/${OTHER}`) {
      if (otherFails) throw new StubError(500, 'INTERNAL');
      return item({ id: OTHER, title: otherTitle });
    }
    throw new StubError(404, 'NOT_FOUND');
  });
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe('related-item cache', () => {
  it('refetches after a live update instead of keeping the first answer', async () => {
    ensureRelated(OTHER);
    await settle();
    expect(related(OTHER)?.title).toBe('First title');

    otherTitle = 'Renamed';
    store.liveRev += 1;
    ensureRelated(OTHER);
    await settle();
    expect(related(OTHER)?.title).toBe('Renamed');
  });

  it('does not cache a transient error as a missing item', async () => {
    otherFails = true;
    ensureRelated(OTHER);
    await settle();
    expect(related(OTHER)).toBeUndefined();

    otherFails = false;
    ensureRelated(OTHER);
    await settle();
    expect(related(OTHER)?.title).toBe('First title');
  });

  it('caches a 404 as missing', async () => {
    ensureRelated('proj-0000000Z');
    await settle();
    expect(related('proj-0000000Z')).toBeNull();
  });
});

describe('list page', () => {
  it('keeps the title column a usable width by scrolling the table sideways', async () => {
    const { container } = render(ListPage);
    await screen.findByText('1–1 of 1', { exact: false });
    const table = container.querySelector('table') as HTMLElement;
    // Fixed columns: 732px, or 836px when the Sync column shows; title gets at least 220px.
    const min = parseInt(table.style.minWidth, 10);
    expect([952, 1056]).toContain(min);
  });

  it('does not open the cursor row when Enter is pressed on a link', async () => {
    store.meta = {
      ...META,
      sync: [{ provider: 'github', repo: 'o/r', url: 'https://github.com/o/r', issue_url: 'https://github.com/o/r/issues/' }]
    } as never;
    vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockReturnValue(600);
    vi.spyOn(HTMLElement.prototype, 'offsetWidth', 'get').mockReturnValue(800);
    render(ListPage);
    const link = await screen.findByRole('link', { name: /GitHub #5/ });
    await fireEvent.keyDown(link, { key: 'Enter' });
    expect(goto).not.toHaveBeenCalled();
    await fireEvent.keyDown(window, { key: 'Enter' });
    expect(goto).toHaveBeenCalled();
  });
});
