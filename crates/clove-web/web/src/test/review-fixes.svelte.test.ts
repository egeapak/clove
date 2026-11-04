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
import { related, ensureRelated, resetRelated, rememberRelated } from '$lib/related.svelte';
import { stubApi, StubError, item, META } from './fake-api';
import ListPage from '../routes/list/+page.svelte';
import RelatedItem from '$lib/components/RelatedItem.svelte';
import BlockedBadge from '$lib/components/BlockedBadge.svelte';

const ID = 'proj-7af3q2k9';
const OTHER = 'proj-0000000B';

let otherTitle = 'First title';
let otherFails = false;
/** What `/meta` answers. The store's refetch replaces `store.meta` with it. */
let metaAnswer: unknown = META;

function settle() {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

beforeEach(() => {
  goto.mockClear();
  resetRelated();
  store.resetForTests();
  store.meta = null;
  otherTitle = 'First title';
  otherFails = false;
  metaAnswer = META;
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
    if (path === '/meta') return metaAnswer;
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

  it('does not fetch a dep that a stored item lists as dangling', async () => {
    const MISSING = 'proj-0000000D';
    const calls = stubApi(() => {
      throw new StubError(404, 'NOT_FOUND');
    });
    store.replaceAll([item({ id: ID, deps: [MISSING], blocked_by: [MISSING], dangling_deps: [MISSING] })]);
    ensureRelated(MISSING);
    await settle();
    store.liveRev += 1;
    ensureRelated(MISSING);
    await settle();
    expect(calls).not.toContain(`/items/${MISSING}`);
    expect(related(MISSING)).toBeNull();
  });

  it('shows a dangling dep chip as missing without a request', async () => {
    const MISSING = 'proj-0000000D';
    const calls = stubApi(() => {
      throw new StubError(404, 'NOT_FOUND');
    });
    store.replaceAll([item({ id: ID, deps: [MISSING], blocked_by: [MISSING], dangling_deps: [MISSING] })]);
    render(RelatedItem, { id: MISSING });
    await settle();
    expect(calls).toEqual([]);
    expect(screen.getByText('missing item')).toBeTruthy();
  });

  it('fetches a dep once no stored item lists it as dangling', async () => {
    const MISSING = 'proj-0000000D';
    const calls = stubApi((path) => {
      if (path === `/items/${MISSING}`) return item({ id: MISSING, title: 'Created later' });
      throw new StubError(404, 'NOT_FOUND');
    });
    store.replaceAll([item({ id: ID, deps: [MISSING], blocked_by: [MISSING], dangling_deps: [MISSING] })]);
    ensureRelated(MISSING);
    await settle();
    expect(calls).toEqual([]);

    store.replaceAll([item({ id: ID, deps: [MISSING] })]);
    store.liveRev += 1;
    ensureRelated(MISSING);
    await settle();
    expect(calls).toEqual([`/items/${MISSING}`]);
    expect(related(MISSING)?.title).toBe('Created later');
  });

  it('sends no request for a dangling id in a blocked badge', async () => {
    const MISSING = 'proj-0000000D';
    const calls = stubApi(() => {
      throw new StubError(404, 'NOT_FOUND');
    });
    store.replaceAll([item({ id: ID, deps: [MISSING], blocked_by: [MISSING], dangling_deps: [MISSING] })]);
    render(BlockedBadge, { blockedBy: [MISSING] });
    await settle();
    expect(calls).toEqual([]);
  });

  it('does not fetch a missing ref of a closed item, of any relation kind', async () => {
    const calls = stubApi(() => {
      throw new StubError(404, 'NOT_FOUND');
    });
    const refs = ['proj-0000000D', 'proj-0000000P', 'proj-0000000R'];
    store.replaceAll([
      item({ id: ID, status: 'closed', deps: [refs[0]], parent: refs[1], relates: [refs[2]], missing_refs: refs })
    ]);
    for (const r of refs) ensureRelated(r);
    await settle();
    expect(calls).toEqual([]);
    for (const r of refs) expect(related(r)).toBeNull();
  });

  it('does not fetch a missing ref of a remembered item until a live update', async () => {
    const MISSING = 'proj-0000000D';
    const calls = stubApi(() => {
      throw new StubError(404, 'NOT_FOUND');
    });
    rememberRelated([item({ id: 'proj-0000000K', parent: ID, deps: [MISSING], missing_refs: [MISSING] })]);
    ensureRelated(MISSING);
    await settle();
    expect(calls).toEqual([]);
    expect(related(MISSING)).toBeNull();

    // A live update may have created the id, so the remembered answer is stale.
    store.liveRev += 1;
    ensureRelated(MISSING);
    await settle();
    expect(calls).toEqual([`/items/${MISSING}`]);
  });

  it('caches a 404 as missing', async () => {
    ensureRelated('proj-0000000Z');
    await settle();
    expect(related('proj-0000000Z')).toBeNull();
  });
});

describe('related chip', () => {
  it('keeps following live updates after one lands while its fetch is in flight', async () => {
    let fetches = 0;
    let release: () => void = () => {};
    const gate = new Promise<void>((resolve) => (release = resolve));
    stubApi((path) => {
      if (path === `/items/${OTHER}`) {
        fetches += 1;
        return item({ id: OTHER, title: `Title ${fetches}` });
      }
      throw new StubError(404, 'NOT_FOUND');
    });
    // Hold the first response so a live update lands mid-fetch.
    const realFetch = globalThis.fetch;
    let first = true;
    vi.stubGlobal('fetch', async (input: string | URL) => {
      if (first) {
        first = false;
        await gate;
      }
      return realFetch(input);
    });

    render(RelatedItem, { id: OTHER });
    await settle();
    store.liveRev += 1;
    await settle();
    release();
    await settle();
    await settle();
    const afterFirst = fetches;

    store.liveRev += 1;
    await settle();
    await settle();
    expect(fetches).toBeGreaterThan(afterFirst);
    expect(related(OTHER)?.title).toBe(`Title ${fetches}`);
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
    metaAnswer = {
      ...META,
      sync: [{ provider: 'github', repo: 'o/r', url: 'https://github.com/o/r', issue_url: 'https://github.com/o/r/issues/' }]
    };
    store.meta = metaAnswer as never;
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
