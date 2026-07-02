import { describe, it, expect, beforeEach } from 'vitest';
import { store } from './store.svelte';
import type { Item } from './types';

function item(over: Partial<Item> & { id: string }): Item {
  return {
    title: 't',
    status: 'open',
    type: 'bug',
    priority: 2,
    assignee: null,
    parent: null,
    labels: [],
    deps: [],
    relates: [],
    created: '',
    updated: '2020-01-01T00:00:00.000Z',
    closed: null,
    body: '',
    comment_count: 0,
    ready: true,
    blocked_by: [],
    dangling_deps: [],
    ...over
  };
}

describe('store optimistic concurrency', () => {
  beforeEach(() => {
    // Isolate: the store is a singleton, so drop any pending edits leaked by a
    // prior test before reseeding the canonical server state.
    (store as unknown as { pending: Map<string, unknown> }).pending.clear();
    store.replaceAll([item({ id: 'proj-1', status: 'open', priority: 2 })]);
  });

  it('composes two overlapping edits without clobbering each other on settle', () => {
    // Edit A (status) then edit B (priority) before A resolves.
    store.optimistic('proj-1', { status: 'in_progress' });
    store.optimistic('proj-1', { priority: 0 });
    expect(store.items.get('proj-1')!.status).toBe('in_progress');
    expect(store.items.get('proj-1')!.priority).toBe(0);

    // A settles: the server payload has A but NOT B. B must survive.
    store.settle('proj-1', item({ id: 'proj-1', status: 'in_progress', priority: 2 }));
    expect(store.items.get('proj-1')!.status).toBe('in_progress');
    expect(store.items.get('proj-1')!.priority).toBe(0); // B still applied

    // B settles with the fully-updated server payload.
    store.settle('proj-1', item({ id: 'proj-1', status: 'in_progress', priority: 0 }));
    expect(store.items.get('proj-1')!.status).toBe('in_progress');
    expect(store.items.get('proj-1')!.priority).toBe(0);
  });

  it('rolling back one edit leaves the other edit intact', () => {
    store.optimistic('proj-1', { status: 'in_progress' });
    const rollbackB = store.optimistic('proj-1', { priority: 0 });

    // B fails → its rollback must restore priority but keep A's status.
    rollbackB();
    expect(store.items.get('proj-1')!.priority).toBe(2); // B undone
    expect(store.items.get('proj-1')!.status).toBe('in_progress'); // A survives
  });

  it('rollback of the only edit restores the pre-edit snapshot', () => {
    const rollback = store.optimistic('proj-1', { status: 'closed' });
    expect(store.items.get('proj-1')!.status).toBe('closed');
    rollback();
    expect(store.items.get('proj-1')!.status).toBe('open');
  });
});
