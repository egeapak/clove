// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, within } from '@testing-library/svelte';
import { page } from './app-stubs/stores';
import { store } from '$lib/store.svelte';
import { resetRelated } from '$lib/related.svelte';
import type { Item, Meta } from '$lib/types';
import { stubApi, item, META, StubError } from './fake-api';
import ExternalRef from '$lib/components/ExternalRef.svelte';
import Card from '$lib/components/Card.svelte';
import BoardPage from '../routes/board/+page.svelte';
import DetailPage from '../routes/items/[id]/+page.svelte';
import ListPage from '../routes/list/+page.svelte';
import TimelinePage from '../routes/timeline/+page.svelte';

const SYNCED_META = {
  ...META,
  sync: [
    {
      provider: 'github',
      repo: 'egeapak/clove',
      url: 'https://github.com/egeapak/clove',
      issue_url: 'https://github.com/egeapak/clove/issues/'
    }
  ]
} as unknown as Meta;

function tooltipText(): string | null {
  const tip = document.querySelector('.clove-tooltip') as HTMLElement | null;
  return tip && !tip.hidden ? tip.textContent : null;
}

async function hoverText(el: Element): Promise<string | null> {
  await fireEvent.mouseEnter(el);
  const text = tooltipText();
  await fireEvent.mouseLeave(el);
  return text;
}

function at(href: string, params: Record<string, string> = {}) {
  page.set({
    url: new URL(href),
    params,
    route: { id: null },
    status: 200,
    error: null,
    data: {},
    state: {},
    form: null
  });
}

let items: Item[] = [];
let meta: unknown = META;

/** Set what the fake server answers and drop whatever an earlier test left in
 *  the shared store (its query is idempotent, so it would not refetch). */
async function serve(next: Item[], nextMeta: unknown = META) {
  items = next;
  meta = nextMeta;
  await store.refetch();
}

beforeEach(() => {
  resetRelated();
  store.meta = null;
  items = [];
  meta = META;
  stubApi((path) => {
    if (path === '/meta') return meta;
    if (path === '/items') return items;
    if (path === '/stats/history') return [];
    const one = /^\/items\/([^/]+)$/.exec(path);
    if (one) {
      const found = items.find((i) => i.id === one[1]);
      if (found) return found;
      throw new StubError(404, 'NOT_FOUND');
    }
    if (path.endsWith('/deptree')) return null;
    throw new Error('unexpected request ' + path);
  });
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe('external ids', () => {
  it('link a GitHub-synced item to its issue', async () => {
    store.meta = SYNCED_META;
    render(ExternalRef, { item: { external_ref: 'gh-67', source_system: 'github' } });
    const link = screen.getByRole('link', { name: /GitHub #67/ });
    expect(link).toHaveAttribute('href', 'https://github.com/egeapak/clove/issues/67');
    expect(link).toHaveAttribute('target', '_blank');
    expect(link.hasAttribute('title')).toBe(false);
    expect(await hoverText(link)).toBe('Open GitHub #67');
  });

  it('use the short spelling in compact rows', () => {
    store.meta = SYNCED_META;
    render(ExternalRef, { item: { external_ref: 'gh-67' }, compact: true });
    expect(screen.getByRole('link').textContent?.trim()).toBe('GH#67');
  });

  it('mark an item with no ref as not synced when the project syncs', async () => {
    store.meta = SYNCED_META;
    const { container } = render(ExternalRef, { item: { external_ref: null } });
    const marker = container.querySelector('.unsynced')!;
    expect(marker.textContent?.trim()).toBe('not synced');
    expect(await hoverText(marker)).toBe('Not synced to GitHub egeapak/clove');
  });

  it('stay quiet in a project with no sync target', () => {
    store.meta = META as unknown as Meta;
    const { container } = render(ExternalRef, { item: { external_ref: null } });
    expect(container.textContent?.trim()).toBe('');
  });

  it('never nest a link inside a board card', () => {
    store.meta = SYNCED_META;
    const { container } = render(Card, { item: item({ id: 'proj-0000000A', external_ref: 'gh-5' }) });
    expect(container.querySelectorAll('a a')).toHaveLength(0);
    expect(container.textContent).toContain('GH#5');
  });
});

describe('board move buttons', () => {
  it('say which column a press moves the card to', async () => {
    at('http://localhost/board');
    await serve([item({ id: 'proj-0000000A', title: 'Movable', status: 'in_progress' })]);
    vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockReturnValue(600);
    vi.spyOn(HTMLElement.prototype, 'offsetWidth', 'get').mockReturnValue(400);
    render(BoardPage);
    const left = await screen.findByRole('button', { name: 'Move #0000000A to Open' });
    const right = screen.getByRole('button', { name: 'Move #0000000A to Closed' });
    expect(await hoverText(left)).toBe('Move to ○ Open');
    expect(await hoverText(right)).toBe('Move to ● Closed');
    expect(left.hasAttribute('title')).toBe(false);
  });
});

describe('timeline', () => {
  it('links both the row label and the bar to the item, with a themed tooltip', async () => {
    at('http://localhost/timeline');
    await serve([item({ id: 'proj-0000000A', title: 'Plotted', type: 'feature' })]);
    const { container } = render(TimelinePage);
    await screen.findByText('Plotted');
    const label = container.querySelector('a.rowlabel')!;
    const bar = container.querySelector('a.bar')!;
    expect(label.getAttribute('href')).toBe('/items/proj-0000000A');
    expect(bar.getAttribute('href')).toBe('/items/proj-0000000A');
    expect(bar.hasAttribute('title')).toBe(false);
    expect(await hoverText(bar)).toBe('feature · Plotted');
  });
});

describe('detail relationships', () => {
  it('show each related item by title, linked, with the full id on hover', async () => {
    const ID = 'proj-0000000A';
    items = [
      item({
        id: ID,
        title: 'Child',
        parent: 'proj-0000000B',
        deps: ['proj-0000000C'],
        relates: ['proj-0000000D'],
        duplicates: ['proj-0000000E']
      }),
      item({ id: 'proj-0000000B', title: 'The parent epic', type: 'epic' }),
      item({ id: 'proj-0000000C', title: 'A closed dependency', status: 'closed' }),
      item({ id: 'proj-0000000D', title: 'Something related' })
    ];
    at(`http://localhost/items/${ID}`, { id: ID });
    const { container } = render(DetailPage, { props: { data: { id: ID } } });
    await screen.findByRole('heading', { name: 'Child' });
    const side = container.querySelector('.detail-side') as HTMLElement;

    const parent = await within(side).findByRole('link', { name: /The parent epic/ });
    expect(parent).toHaveAttribute('href', '/items/proj-0000000B');
    expect(parent.textContent).toContain('#0000000B');
    expect(await hoverText(parent)).toBe('proj-0000000B · The parent epic');

    expect(await within(side).findByRole('link', { name: /A closed dependency/ })).toHaveAttribute(
      'href',
      '/items/proj-0000000C'
    );
    expect(await within(side).findByRole('link', { name: /Something related/ })).toBeInTheDocument();
    // A reference to an item that no longer exists still renders, as missing.
    expect(await within(side).findByRole('link', { name: /#0000000E missing item/ })).toBeInTheDocument();

    const kinds = [...side.querySelectorAll('.rel-kind')].map((k) => k.textContent);
    expect(kinds).toEqual(['part of', 'depends on', 'relates to', 'duplicates']);
  });

  it('shows the external id in the header', async () => {
    const ID = 'proj-0000000A';
    await serve([item({ id: ID, title: 'Synced one', external_ref: 'gh-67', source_system: 'github' })], SYNCED_META);
    at(`http://localhost/items/${ID}`, { id: ID });
    render(DetailPage, { props: { data: { id: ID } } });
    await screen.findByRole('heading', { name: 'Synced one' });
    await vi.waitFor(() => expect(screen.getByRole('link', { name: /GitHub #67/ })).toBeInTheDocument());
  });
});

describe('list sync column and filter', () => {
  function layout() {
    vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockReturnValue(600);
    vi.spyOn(HTMLElement.prototype, 'offsetWidth', 'get').mockReturnValue(1200);
  }

  it('shows a sync column and filter when the project syncs', async () => {
    meta = SYNCED_META;
    items = [
      item({ id: 'proj-0000000A', title: 'On GitHub', external_ref: 'gh-3' }),
      item({ id: 'proj-0000000B', title: 'Local only' })
    ];
    at('http://localhost/list');
    layout();
    const { container } = render(ListPage);
    await screen.findByText('Local only');
    await vi.waitFor(() => expect(screen.getByRole('combobox', { name: 'Sync filter' })).toBeInTheDocument());
    const heads = [...container.querySelectorAll('thead th')].map((t) => t.textContent?.trim());
    expect(heads).toContain('Sync');
    const row = screen.getByText('Local only').closest('tr')!;
    expect(row.querySelector('.unsynced')).not.toBeNull();
    expect(screen.getByText('On GitHub').closest('tr')!.textContent).toContain('GH#3');
  });

  it('sends the filter to the server', async () => {
    meta = SYNCED_META;
    const calls = stubApi((path, url) => {
      if (path === '/meta') return meta;
      if (path === '/items') {
        expect(url.searchParams.get('synced')).toBe('false');
        return [item({ id: 'proj-0000000B', title: 'Local only' })];
      }
      throw new Error('unexpected request ' + path);
    });
    at('http://localhost/list?synced=false');
    layout();
    render(ListPage);
    await screen.findByText('Local only');
    expect(calls).toContain('/items');
  });

  it('has neither in a project with no sync target', async () => {
    items = [item({ id: 'proj-0000000B', title: 'Local only' })];
    at('http://localhost/list');
    layout();
    const { container } = render(ListPage);
    await screen.findByText('Local only');
    expect(screen.queryByRole('combobox', { name: 'Sync filter' })).toBeNull();
    expect([...container.querySelectorAll('thead th')].map((t) => t.textContent?.trim())).not.toContain('Sync');
    expect(container.querySelector('.unsynced')).toBeNull();
  });
});
