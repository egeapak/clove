import { describe, it, expect } from 'vitest';
import { buildParams, parseQuery, queryString } from './query';

describe('query serialization matches the server CSV contract', () => {
  it('serializes multi-select filters as a single comma-joined key', () => {
    const p = buildParams({
      mode: 'list',
      type: ['bug', 'feature'],
      priority: [0, 1],
      label: ['a', 'b']
    });
    // One key per field (serde_urlencoded collapses repeated keys to the last),
    // comma-joined — the server splits on commas via read.rs csv().
    expect(p.getAll('type')).toEqual(['bug,feature']);
    expect(p.get('type')).toBe('bug,feature');
    expect(p.get('priority')).toBe('0,1');
    expect(p.get('label')).toBe('a,b');
  });

  it('round-trips CSV through parseQuery (list-page URL contract)', () => {
    const qs = queryString({
      mode: 'list',
      type: ['bug', 'feature'],
      priority: [0, 1],
      label: ['a', 'b']
    });
    const parsed = parseQuery(new URLSearchParams(qs.slice(1)));
    expect(parsed.type).toEqual(['bug', 'feature']);
    expect(parsed.priority).toEqual([0, 1]);
    expect(parsed.label).toEqual(['a', 'b']);
  });

  it('omits empty multi-selects', () => {
    const p = buildParams({ mode: 'list', type: [], priority: [], label: [] });
    expect(p.has('type')).toBe(false);
    expect(p.has('priority')).toBe(false);
    expect(p.has('label')).toBe(false);
  });
});
