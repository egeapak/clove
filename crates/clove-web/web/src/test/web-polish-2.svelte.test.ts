// @vitest-environment jsdom
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, cleanup, fireEvent, within } from '@testing-library/svelte';
import { readFileSync, readdirSync, statSync } from 'node:fs';
import path from 'node:path';
import { page } from './app-stubs/stores';
import { store } from '$lib/store.svelte';
import { resetRelated } from '$lib/related.svelte';
import type { Item, Meta } from '$lib/types';
import { stubApi, item, META } from './fake-api';
import ExternalRef from '$lib/components/ExternalRef.svelte';
import Card from '$lib/components/Card.svelte';
import Avatar from '$lib/components/Avatar.svelte';
import PriorityGlyph from '$lib/components/PriorityGlyph.svelte';
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

function shownTip(): HTMLElement | null {
  const tip = document.querySelector('.clove-tooltip') as HTMLElement | null;
  return tip && !tip.hidden ? tip : null;
}

async function hoverText(el: Element): Promise<string | null> {
  await fireEvent.mouseEnter(el);
  const text = shownTip()?.textContent ?? null;
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

function layout() {
  vi.spyOn(HTMLElement.prototype, 'offsetHeight', 'get').mockReturnValue(600);
  vi.spyOn(HTMLElement.prototype, 'offsetWidth', 'get').mockReturnValue(1200);
}

let items: Item[] = [];
let comments: unknown[] = [];
let requests: URL[] = [];

async function serve(next: Item[]) {
  items = next;
  await store.refetch();
}

beforeEach(() => {
  resetRelated();
  store.meta = null;
  items = [];
  comments = [];
  requests = [];
  stubApi((p, url) => {
    requests.push(url);
    if (p === '/meta') return META;
    if (p === '/items') {
      const parent = url.searchParams.get('parent');
      return parent ? items.filter((i) => i.parent === parent) : items;
    }
    if (p === '/stats/history') return [];
    if (p.endsWith('/comments')) return comments;
    if (p.endsWith('/deptree')) return null;
    const one = /^\/items\/([^/]+)$/.exec(p);
    if (one) {
      const found = items.find((i) => i.id === one[1]);
      if (found) return found;
    }
    throw new Error('unexpected request ' + p);
  });
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe('board move tooltip', () => {
  it("names the target column in its header's glyph and colour", async () => {
    at('http://localhost/board');
    await serve([item({ id: 'proj-0000000A', title: 'Movable', status: 'open' })]);
    layout();
    const { container } = render(BoardPage);
    const right = await screen.findByRole('button', { name: 'Move #0000000A to In Progress' });
    await fireEvent.mouseEnter(right);
    const tip = shownTip()!;
    expect(tip.textContent).toBe('Move to ◐ In Progress');
    const [glyph, label] = tip.querySelectorAll('span');
    const header = container.querySelector('section[aria-label="In Progress column"] .col-head .st') as HTMLElement;
    expect(glyph.textContent).toBe(header.textContent?.trim());
    expect(glyph.style.color).toBe(header.style.color);
    expect(label.textContent).toBe('In Progress');
    expect(label.style.color).toBe(header.style.color);
  });
});

describe('detail children', () => {
  const EPIC = 'proj-0000000E';

  it('lists the children from ?parent=, with a count', async () => {
    await serve([
      item({ id: EPIC, title: 'The epic', type: 'epic' }),
      item({ id: 'proj-0000000A', title: 'First child', parent: EPIC }),
      item({ id: 'proj-0000000B', title: 'Second child', parent: EPIC, status: 'closed' }),
      item({ id: 'proj-0000000C', title: 'Unrelated' })
    ]);
    at(`http://localhost/items/${EPIC}`, { id: EPIC });
    const { container } = render(DetailPage, { props: { data: { id: EPIC } } });
    await screen.findByRole('heading', { name: 'The epic' });
    const side = container.querySelector('.detail-side') as HTMLElement;
    const first = await within(side).findByRole('link', { name: /First child/ });
    expect(first).toHaveAttribute('href', '/items/proj-0000000A');
    expect(within(side).getByRole('link', { name: /Second child/ })).toBeInTheDocument();
    expect(within(side).queryByRole('link', { name: /Unrelated/ })).toBeNull();

    const group = first.closest('.rel-group')!;
    expect(group.querySelector('.rel-kind')!.textContent?.replace(/\s+/g, ' ').trim()).toBe('children2');
    expect(group.querySelector('.rel-count')!.textContent).toBe('2');

    const asked = requests.find((u) => u.searchParams.has('parent'))!;
    expect(asked.searchParams.get('parent')).toBe(EPIC);
    expect(asked.searchParams.get('limit')).toBe('0');
    // The children came with their titles: no per-child fetch.
    expect(requests.some((u) => u.pathname.endsWith('/items/proj-0000000A'))).toBe(false);
  });

  it('shows no children from a server that ignores the filter', async () => {
    stubApi((p) => {
      if (p === '/meta') return META;
      if (p === '/items') return items;
      if (p.endsWith('/deptree')) return null;
      const one = /^\/items\/([^/]+)$/.exec(p);
      if (one) return items.find((i) => i.id === one[1]);
      throw new Error('unexpected request ' + p);
    });
    await serve([item({ id: EPIC, title: 'Lonely epic', type: 'epic' }), item({ id: 'proj-0000000C', title: 'Unrelated' })]);
    at(`http://localhost/items/${EPIC}`, { id: EPIC });
    const { container } = render(DetailPage, { props: { data: { id: EPIC } } });
    await screen.findByRole('heading', { name: 'Lonely epic' });
    await new Promise((r) => setTimeout(r, 20));
    const kinds = [...container.querySelectorAll('.rel-kind')].map((k) => k.textContent);
    expect(kinds).toEqual([]);
  });

  it('caps a long list behind "Show all"', async () => {
    await serve([
      item({ id: EPIC, title: 'Big epic', type: 'epic' }),
      ...Array.from({ length: 15 }, (_, n) =>
        item({ id: `proj-000000${String(n).padStart(2, '0')}`, title: `Child ${n}`, parent: EPIC })
      )
    ]);
    at(`http://localhost/items/${EPIC}`, { id: EPIC });
    const { container } = render(DetailPage, { props: { data: { id: EPIC } } });
    const more = await screen.findByRole('button', { name: 'Show all 15' });
    const side = container.querySelector('.detail-side') as HTMLElement;
    expect(within(side).getAllByRole('link', { name: /Child \d+/ })).toHaveLength(12);
    await fireEvent.click(more);
    expect(within(side).getAllByRole('link', { name: /Child \d+/ })).toHaveLength(15);
  });
});

describe('detail comments', () => {
  it('render as Markdown, with raw HTML escaped', async () => {
    const ID = 'proj-0000000A';
    await serve([item({ id: ID, title: 'Talked about', comment_count: 1 })]);
    comments = [
      { author: 'ege', timestamp: '2026-01-02T00:00:00Z', body: 'Looks **good**, see `x()`\n\n- one\n- two\n\n<img src=x onerror=alert(1)>' }
    ];
    at(`http://localhost/items/${ID}?view=comments`, { id: ID });
    const { container } = render(DetailPage, { props: { data: { id: ID } } });
    await vi.waitFor(() => expect(container.querySelector('.comment .md strong')).not.toBeNull());
    const md = container.querySelector('.comment .md')!;
    expect(md.querySelector('strong')!.textContent).toBe('good');
    expect(md.querySelector('code')!.textContent).toBe('x()');
    expect(md.querySelectorAll('li')).toHaveLength(2);
    expect(md.querySelector('img')).toBeNull();
    expect(md.textContent).toContain('<img src=x onerror=alert(1)>');
  });
});

describe('board card dependencies', () => {
  it('show each dependency by title, with the full id and title on hover, never as a nested link', async () => {
    await serve([
      item({ id: 'proj-0000000A', title: 'Needs things', deps: ['proj-0000000B', 'proj-0000000C'] }),
      item({ id: 'proj-0000000B', title: 'A finished prerequisite', status: 'closed' }),
      item({ id: 'proj-0000000C', title: 'Another closed one', status: 'closed' })
    ]);
    const { container } = render(Card, { item: store.items.get('proj-0000000A')! });
    const deps = container.querySelector('.deps')!;
    const refs = [...deps.querySelectorAll('.rel')];
    expect(refs.map((r) => r.querySelector('.rtitle')?.textContent)).toEqual([
      'A finished prerequisite',
      'Another closed one'
    ]);
    expect(await hoverText(refs[0])).toBe('proj-0000000B · A finished prerequisite');
    expect(container.querySelectorAll('a a')).toHaveLength(0);
    expect(deps.textContent).not.toContain('#0000000B');
  });

  it('give the clamped title its full text on hover, only when clipped', async () => {
    const { container } = render(Card, { item: item({ id: 'proj-0000000A', title: 'A very long card title' }) });
    const title = container.querySelector('.card-title') as HTMLElement;
    expect(await hoverText(title)).toBeNull();
    vi.spyOn(title, 'scrollHeight', 'get').mockReturnValue(80);
    vi.spyOn(title, 'clientHeight', 'get').mockReturnValue(54);
    expect(await hoverText(title)).toBe('A very long card title');
  });
});

describe('list layout', () => {
  it('fixes column widths in a colgroup that matches the header', async () => {
    store.meta = SYNCED_META;
    await serve([item({ id: 'proj-0000000A', title: 'Row one', external_ref: 'gh-1' })]);
    at('http://localhost/list');
    layout();
    const { container } = render(ListPage);
    await screen.findByText('Row one');
    const cols = [...container.querySelectorAll('table > colgroup > col')] as HTMLElement[];
    const heads = container.querySelectorAll('thead th');
    expect(cols).toHaveLength(heads.length);
    // Every column but the title has a fixed width; the title takes the rest.
    const unsized = cols.filter((c) => !c.style.width);
    expect(unsized).toHaveLength(1);
    expect(cols.indexOf(unsized[0])).toBe([...heads].findIndex((h) => h.textContent?.trim() === 'Title'));
  });

  it('shows the full title and id on hover', async () => {
    await serve([item({ id: 'proj-0000000A', title: 'A title too long for its cell' })]);
    at('http://localhost/list');
    layout();
    render(ListPage);
    const text = await screen.findByText('A title too long for its cell');
    vi.spyOn(text, 'scrollWidth', 'get').mockReturnValue(500);
    vi.spyOn(text, 'clientWidth', 'get').mockReturnValue(200);
    expect(await hoverText(text)).toBe('A title too long for its cell');
    const id = text.closest('tr')!.querySelector('.id')!;
    expect(await hoverText(id)).toBe('proj-0000000A · A title too long for its cell');
  });

  it('labels the filter chips with themed tooltips', async () => {
    at('http://localhost/list');
    layout();
    render(ListPage);
    const chip = await screen.findByRole('button', { name: 'Filter by type epic' });
    expect(await hoverText(chip)).toBe('type: epic');
  });
});

describe('timeline', () => {
  it('gives a row label the full id and title, and a time axis wider than a short window', async () => {
    at('http://localhost/timeline');
    await serve([
      item({ id: 'proj-0000000A', title: 'An old item', created: '2025-01-01T00:00:00Z' }),
      item({ id: 'proj-0000000B', title: 'A recent item', created: '2026-01-01T00:00:00Z' })
    ]);
    const { container } = render(TimelinePage);
    await screen.findByText('An old item');
    const label = container.querySelector('a.rowlabel')!;
    expect(await hoverText(label)).toBe('proj-0000000A · An old item');
    const grid = container.querySelector('.tl-scroll > .tl-grid') as HTMLElement;
    // A year at 14px a day.
    expect(parseInt(grid.style.getPropertyValue('--track-w'))).toBeGreaterThan(5000);
  });
});

describe('timeline status', () => {
  it('sets closed work apart: a muted bar and the closed glyph, in the status colours', async () => {
    at('http://localhost/timeline');
    await serve([
      item({ id: 'proj-0000000A', title: 'Still open', status: 'open' }),
      item({ id: 'proj-0000000B', title: 'Underway', status: 'in_progress' }),
      item({ id: 'proj-0000000C', title: 'Done', status: 'closed', closed: '2026-01-05T00:00:00Z' })
    ]);
    const { container } = render(TimelinePage);
    await screen.findByText('Done');
    const tracks = [...container.querySelectorAll('.track')];
    const byTitle = (t: string) => tracks.find((tr) => tr.querySelector('a.bar')!.getAttribute('aria-label')!.includes(t))!;

    const done = byTitle('Done');
    expect(done.querySelector('a.bar')!.classList.contains('closed')).toBe(true);
    const doneEnd = done.querySelector('.bar-end') as HTMLElement;
    expect(doneEnd.textContent).toBe('●');
    expect(doneEnd.style.color).toBe('var(--status-closed)');

    for (const [title, glyph, color] of [
      ['Still open', '○', 'var(--status-open)'],
      ['Underway', '◐', 'var(--status-in-progress)']
    ]) {
      const tr = byTitle(title);
      expect(tr.querySelector('a.bar')!.classList.contains('closed')).toBe(false);
      const end = tr.querySelector('.bar-end') as HTMLElement;
      expect(end.textContent).toBe(glyph);
      expect(end.style.color).toBe(color);
    }

    const doneLabel = [...container.querySelectorAll('a.rowlabel')].find((a) => a.textContent?.includes('Done'))!;
    expect(doneLabel.classList.contains('closed')).toBe(true);
    expect(doneLabel.querySelector('.st')!.getAttribute('aria-label')).toBe('closed');
  });
});

describe('GitHub mark', () => {
  it('marks a GitHub ref, and only a GitHub ref', () => {
    store.meta = SYNCED_META;
    const gh = render(ExternalRef, { item: { external_ref: 'gh-67', source_system: 'github' } });
    expect(gh.container.querySelector('svg.gh')).not.toBeNull();
    cleanup();
    const tk = render(ExternalRef, { item: { external_ref: 'tk:x-1', source_system: 'tk' } });
    expect(tk.container.querySelector('svg.gh')).toBeNull();
  });
});

describe('themed tooltips replace native titles', () => {
  it('on the avatar and the priority glyph', async () => {
    const av = render(Avatar, { name: 'ege' });
    const el = av.container.querySelector('.av')!;
    expect(el.hasAttribute('title')).toBe(false);
    expect(await hoverText(el)).toBe('ege');
    const pr = render(PriorityGlyph, { priority: 0 });
    const glyph = pr.container.querySelector('.pr')!;
    expect(glyph.hasAttribute('title')).toBe(false);
    expect(await hoverText(glyph)).toMatch(/p0/i);
  });

  it('nowhere in the rendered views', async () => {
    store.meta = SYNCED_META;
    await serve([
      item({ id: 'proj-0000000A', title: 'One', assignee: 'ege', deps: ['proj-0000000B'], external_ref: 'gh-2' }),
      item({ id: 'proj-0000000B', title: 'Two', blocked_by: ['proj-0000000A'], parent: 'proj-0000000A' })
    ]);
    layout();
    const views = [
      () => (at('http://localhost/list'), render(ListPage)),
      () => (at('http://localhost/board'), render(BoardPage)),
      () => (at('http://localhost/timeline'), render(TimelinePage)),
      () => (at('http://localhost/items/proj-0000000A', { id: 'proj-0000000A' }),
        render(DetailPage, { props: { data: { id: 'proj-0000000A' } } }))
    ];
    for (const view of views) {
      const { container } = view();
      await screen.findAllByText(/One/);
      expect([...container.querySelectorAll('[title]')].map((e) => e.outerHTML)).toEqual([]);
      cleanup();
    }
  });

  it('nowhere in the component sources', () => {
    const root = path.resolve('src');
    const files: string[] = [];
    const walk = (dir: string) => {
      for (const name of readdirSync(dir)) {
        const full = path.join(dir, name);
        if (statSync(full).isDirectory()) walk(full);
        else if (name.endsWith('.svelte')) files.push(full);
      }
    };
    walk(root);
    expect(files.length).toBeGreaterThan(10);
    // A `title=` on a lowercase (DOM) element; component props like
    // `<ShortId title=…>` are fine.
    const nativeTitle = /<[a-z][\w-]*\b[^<>]*?\stitle=/;
    const offenders = files.filter((f) => nativeTitle.test(readFileSync(f, 'utf8')));
    expect(offenders.map((f) => path.relative(root, f))).toEqual([]);
  });
});
