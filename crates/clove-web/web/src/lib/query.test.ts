import { describe, it, expect } from 'vitest';
import { buildParams, parsePage, parseQuery, queryString } from './query';

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

describe('the page number the list route derives its window from', () => {
  const pageOf = (raw: string | null) =>
    parsePage(new URLSearchParams(raw === null ? '' : `page=${raw}`));

  it('never yields a window the strict server parser would reject', () => {
    // The server rejects a malformed `offset` with a 422 instead of silently
    // reading it as 0, so every `?page=` a user can type has to survive
    // `(page - 1) * PAGE_SIZE` as a plain decimal integer string.
    const PAGE_SIZE = 100;
    const cases = ['abc', '-3', '0', '', '1e400', '1e21', '99999999999999999999', 'Infinity', '2.7'];
    for (const raw of cases) {
      const n = pageOf(raw);
      expect(Number.isSafeInteger(n), `page=${raw} -> ${n}`).toBe(true);
      expect(n).toBeGreaterThanOrEqual(1);
      const offset = (n - 1) * PAGE_SIZE;
      expect(Number.isSafeInteger(offset), `page=${raw} -> offset ${offset}`).toBe(true);
      // This is the exact string buildParams puts on the wire.
      expect(String(offset), `page=${raw}`).toMatch(/^\d+$/);
    }
  });

  it('keeps a real page number', () => {
    expect(pageOf('4')).toBe(4);
    expect(pageOf('1')).toBe(1);
    expect(pageOf(null)).toBe(1);
    expect(pageOf('2.7')).toBe(2);
  });
});
