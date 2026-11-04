// @vitest-environment jsdom
import { it, expect, vi, afterEach } from 'vitest';
import { render, cleanup } from '@testing-library/svelte';
import { page } from './app-stubs/stores';
import { store } from '$lib/store.svelte';
import { resetRelated } from '$lib/related.svelte';
import type { Item } from '$lib/types';
import { stubApi, StubError, item, META } from './fake-api';
import DetailPage from '../routes/items/[id]/+page.svelte';

const ID = 'proj-7af3q2k9';
const DEP = 'proj-0000000D';

function settle() {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

it('an older item load that lands last does not overwrite the store', async () => {
  resetRelated();
  store.resetForTests();
  store.meta = META as never;
  page.set({
    url: new URL(`http://localhost/items/${ID}`),
    params: { id: ID },
    route: { id: '/items/[id]' },
    status: 200,
    error: null,
    data: {},
    state: {},
    form: null
  });
  // Same `updated` on both answers: creating the dep does not touch this item.
  const stale = item({ id: ID, deps: [DEP], blocked_by: [DEP], dangling_deps: [DEP], updated: '2026-01-01T00:00:00Z' });
  const fresh = item({ id: ID, deps: [DEP], blocked_by: [DEP], updated: '2026-01-01T00:00:00Z' });
  const answers: Item[] = [stale, fresh];
  stubApi((path) => {
    if (path === `/items/${ID}`) return answers.shift();
    if (path === `/items/${ID}/deptree`) {
      return { id: ID, title: 'x', status: 'open', ready: false, cycle_ref: false, children: [] };
    }
    if (path === '/items') return [];
    if (path === `/items/${DEP}`) return item({ id: DEP, title: 'Dep' });
    throw new StubError(404, 'NOT_FOUND');
  });
  // Hold the first item response until the second one has landed.
  const realFetch = globalThis.fetch;
  let release: () => void = () => {};
  const gate = new Promise<void>((resolve) => (release = resolve));
  let firstItem = true;
  vi.stubGlobal('fetch', async (input: string | URL) => {
    const res = realFetch(input);
    if (firstItem && String(input).endsWith(`/items/${ID}`)) {
      firstItem = false;
      await gate;
    }
    return res;
  });

  render(DetailPage, { data: { id: ID } } as never);
  await settle();
  store.liveRev += 1;
  await settle();
  await settle();
  expect(store.items.get(ID)?.dangling_deps).toEqual([]);

  release();
  await settle();
  await settle();
  expect(store.items.get(ID)?.dangling_deps).toEqual([]);
});
