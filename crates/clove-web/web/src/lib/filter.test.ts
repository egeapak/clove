import { describe, it, expect } from 'vitest';
import { applyFilters, sortItems } from './filter';
import type { Item } from './types';

// A "lean" list item, as served by GET /api/v1/items — note: NO `body` key
// (read.rs serializes via frontmatter_value only). We deliberately omit it to
// reproduce live data, casting through `unknown` so TS doesn't add it for us.
function lean(over: Partial<Item> & { id: string; title: string }): Item {
  const base = {
    status: 'open',
    type: 'bug',
    priority: 2,
    assignee: null,
    parent: null,
    labels: [] as string[],
    deps: [] as string[],
    relates: [] as string[],
    created: '',
    updated: '',
    closed: null,
    comment_count: 0,
    ready: true,
    blocked_by: [] as string[],
    dangling_deps: [] as string[],
    ...over
  };
  return base as unknown as Item;
}

describe('applyFilters list search (lean list items carry no body)', () => {
  it('does not crash when body is undefined (live data)', () => {
    const a = lean({ id: 'proj-1', title: 'Alpha' });
    const b = lean({ id: 'proj-2', title: 'Beta', labels: ['x'] });
    // 'alpha' matches a's title; b matches nothing → the old code hit b.body.
    const out = applyFilters([a, b], { q: 'alpha' });
    expect(out.map((i) => i.id)).toEqual(['proj-1']);
  });

  it('matches labels like the server, and never searches body', () => {
    const it = lean({ id: 'proj-9', title: 'Widget', labels: ['urgent'] });
    (it as unknown as { body: string }).body = 'secret sauce';
    // label hit
    expect(applyFilters([it], { q: 'urgent' }).length).toBe(1);
    // id / title hits
    expect(applyFilters([it], { q: 'proj-9' }).length).toBe(1);
    expect(applyFilters([it], { q: 'widget' }).length).toBe(1);
    // body must NOT be searched (server searches id/title/labels only)
    expect(applyFilters([it], { q: 'secret' }).length).toBe(0);
  });

  it('matches each field separately, not a concatenated haystack', () => {
    const it = lean({ id: 'proj-9', title: 'Alpha widget', labels: ['area:core'] });
    // Each field on its own still matches.
    expect(applyFilters([it], { q: 'alpha' }).length).toBe(1);
    expect(applyFilters([it], { q: 'area:core' }).length).toBe(1);
    // ...but a needle spanning two fields does not. Joining id/title/labels
    // into one string let `widget area:core` match across the boundary between
    // the title and the first label; the server never did, so the UI answered
    // a query the API would not.
    expect(applyFilters([it], { q: 'widget area:core' }).length).toBe(0);
    expect(applyFilters([it], { q: 'proj-9 alpha' }).length).toBe(0);
  });

  it('canonicalizes a label query the way the server does', () => {
    const it = lean({ id: 'proj-9', title: 'Alpha', labels: ['area:core'] });
    // Labels are stored lowercase and the server lowercases the query too, so
    // this matched through the API and returned nothing in the UI.
    expect(applyFilters([it], { label: ['AREA:Core'] }).length).toBe(1);
    expect(applyFilters([it], { label: ['area:core'] }).length).toBe(1);
    expect(applyFilters([it], { label: ['area:other'] }).length).toBe(0);
  });
});

describe('sortItems mirrors clove_core::view::Order', () => {
  // Three items that tie on every sort key except the id, delivered in the
  // reverse of id order — so a sorter that falls back to "the order the rows
  // arrived in" answers differently from one that falls back to the id.
  const tied = [
    lean({ id: 'proj-C', title: 'c', priority: 1, updated: '2020-01-01T00:00:00Z' }),
    lean({ id: 'proj-B', title: 'b', priority: 1, updated: '2020-01-01T00:00:00Z' }),
    lean({ id: 'proj-A', title: 'a', priority: 1, updated: '2020-01-01T00:00:00Z' })
  ];
  // A rank lookup that disagrees with id order, so a tiebreak on `rankOf`
  // cannot accidentally produce the right answer.
  const rankOf = (id: string) => ['proj-C', 'proj-B', 'proj-A'].indexOf(id);

  it('breaks a tie on the id, not on the order the rows arrived in', () => {
    // Every key on the server ends in `.then_with(|| a.id.cmp(&b.id))`. This
    // used to tie-break on the *fetched array's* insertion order, which is only
    // the same answer while the fetch is the whole store: once the list pages,
    // two rows that tie either side of a page boundary can repeat or vanish.
    expect(sortItems(tied, 'priority', 'asc', rankOf).map((i) => i.id)).toEqual([
      'proj-A',
      'proj-B',
      'proj-C'
    ]);
    expect(sortItems(tied, 'updated', 'asc', rankOf).map((i) => i.id)).toEqual([
      'proj-A',
      'proj-B',
      'proj-C'
    ]);
  });

  it('reverses the whole key, id tiebreak included', () => {
    expect(sortItems(tied, 'priority', 'desc', rankOf).map((i) => i.id)).toEqual([
      'proj-C',
      'proj-B',
      'proj-A'
    ]);
  });

  it('sorts status and type in lifecycle order, not alphabetically', () => {
    const items = [
      lean({ id: 'proj-1', title: 'x', status: 'closed', type: 'docs' }),
      lean({ id: 'proj-2', title: 'y', status: 'open', type: 'bug' }),
      lean({ id: 'proj-3', title: 'z', status: 'in_progress', type: 'chore' })
    ];
    // Alphabetically: closed < in_progress < open, and bug < chore < docs only
    // by luck — `epic` would sort before `feature`. The server ranks by the
    // declared arrays instead.
    expect(sortItems(items, 'status', 'asc').map((i) => i.id)).toEqual([
      'proj-2',
      'proj-3',
      'proj-1'
    ]);
    expect(sortItems(items, 'type', 'asc').map((i) => i.id)).toEqual([
      'proj-2',
      'proj-3',
      'proj-1'
    ]);
  });
});

describe('matchesTab partitions the way the server does', () => {
  it('excludes closed items from both ready and blocked', () => {
    // `?mode=ready|blocked` routes to Engine::ready / Engine::blocked, and both
    // partition the *active* items. A closed item with an open dependency used
    // to show up under Blocked here and never through the API.
    const closedButBlocked = lean({
      id: 'proj-1',
      title: 'done',
      status: 'closed',
      ready: false,
      blocked_by: ['proj-9']
    });
    const closedAndReady = lean({ id: 'proj-2', title: 'done too', status: 'closed', ready: true });
    const openBlocked = lean({
      id: 'proj-3',
      title: 'waiting',
      ready: false,
      blocked_by: ['proj-9']
    });

    const items = [closedButBlocked, closedAndReady, openBlocked];
    expect(applyFilters(items, { mode: 'blocked' }).map((i) => i.id)).toEqual(['proj-3']);
    expect(applyFilters(items, { mode: 'ready' }).map((i) => i.id)).toEqual([]);
    // …and 'list' still constrains nothing.
    expect(applyFilters(items, { mode: 'list' })).toHaveLength(3);
  });
});
