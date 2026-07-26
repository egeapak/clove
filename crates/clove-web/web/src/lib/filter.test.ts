import { describe, it, expect } from 'vitest';
import { applyFilters } from './filter';
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
