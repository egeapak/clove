import { describe, it, expect } from 'vitest';
import type { Item } from './types';
import {
  buildCreate,
  buildPatch,
  emptyForm,
  formFromItem,
  isEmptyPatch,
  isSubmittable,
  normalizeLabel,
  parseLabels
} from './itemForm';

function item(overrides: Partial<Item> = {}): Item {
  return {
    id: 'proj-7AF3K2MN',
    title: 'Original title',
    status: 'open',
    type: 'feature',
    priority: 2,
    assignee: 'alice',
    parent: null,
    labels: ['area:web', 'urgent'],
    deps: [],
    relates: [],
    created: '2026-01-01T00:00:00Z',
    updated: '2026-01-01T00:00:00Z',
    closed: null,
    body: 'Original body\n',
    comment_count: 0,
    ready: true,
    blocked_by: [],
    dangling_deps: [],
    ...overrides
  };
}

describe('label parsing', () => {
  it('normalizes case and whitespace', () => {
    expect(normalizeLabel('  Area:Web  ')).toBe('area:web');
    expect(normalizeLabel('Multi   Word')).toBe('multi word');
  });

  it('parses comma/newline lists, dropping blanks and dupes', () => {
    expect(parseLabels('a, B ,a\n c')).toEqual(['a', 'b', 'c']);
    expect(parseLabels('  ,  ')).toEqual([]);
  });
});

describe('formFromItem / emptyForm', () => {
  it('prefills every field from an item', () => {
    const f = formFromItem(item());
    expect(f).toEqual({
      title: 'Original title',
      type: 'feature',
      priority: 2,
      status: 'open',
      assignee: 'alice',
      labels: ['area:web', 'urgent'],
      body: 'Original body\n'
    });
  });

  it('maps a null assignee to an empty string', () => {
    expect(formFromItem(item({ assignee: null })).assignee).toBe('');
  });

  it('emptyForm has sane defaults and respects overrides', () => {
    expect(emptyForm().priority).toBe(2);
    expect(emptyForm({ type: 'bug' }).type).toBe('bug');
  });
});

describe('isSubmittable', () => {
  it('requires a non-blank title', () => {
    expect(isSubmittable(emptyForm({ title: '  ' }))).toBe(false);
    expect(isSubmittable(emptyForm({ title: 'x' }))).toBe(true);
  });
});

describe('buildCreate', () => {
  it('trims, canonicalizes labels, and drops blank optionals', () => {
    const payload = buildCreate(
      emptyForm({ title: '  New  ', type: 'bug', priority: 1, labels: ['B', 'a', 'a'], assignee: '  ', body: '  ' })
    );
    expect(payload).toEqual({ title: 'New', type: 'bug', priority: 1, labels: ['a', 'b'] });
  });

  it('includes assignee and body when present', () => {
    const payload = buildCreate(emptyForm({ title: 'x', assignee: ' bob ', body: ' hello ' }));
    expect(payload.assignee).toBe('bob');
    expect(payload.body).toBe('hello');
  });
});

describe('buildPatch', () => {
  it('returns an empty patch when nothing changed', () => {
    const patch = buildPatch(formFromItem(item()), item());
    expect(isEmptyPatch(patch)).toBe(true);
  });

  it('includes only changed scalar fields', () => {
    const f = formFromItem(item());
    f.title = 'New title';
    f.priority = 0;
    const patch = buildPatch(f, item());
    expect(patch).toEqual({ title: 'New title', priority: 0 });
  });

  it('does not send an emptied title (server rejects it)', () => {
    const f = formFromItem(item());
    f.title = '   ';
    expect(buildPatch(f, item()).title).toBeUndefined();
  });

  it('clears the assignee with null when emptied', () => {
    const f = formFromItem(item());
    f.assignee = '';
    expect(buildPatch(f, item()).assignee).toBeNull();
  });

  it('sets a new assignee', () => {
    const f = formFromItem(item());
    f.assignee = 'carol';
    expect(buildPatch(f, item()).assignee).toBe('carol');
  });

  it('sends the full label set only when it differs (order-insensitive)', () => {
    const reordered = formFromItem(item());
    reordered.labels = ['urgent', 'area:web']; // same set, different order
    expect(buildPatch(reordered, item()).labels).toBeUndefined();

    const changed = formFromItem(item());
    changed.labels = ['urgent', 'NEW'];
    expect(buildPatch(changed, item()).labels).toEqual(['new', 'urgent']);
  });

  it('sends body changes verbatim', () => {
    const f = formFromItem(item());
    f.body = 'A brand new body';
    expect(buildPatch(f, item()).body).toBe('A brand new body');
  });

  it('changes the status', () => {
    const f = formFromItem(item());
    f.status = 'closed';
    expect(buildPatch(f, item()).status).toBe('closed');
  });
});
