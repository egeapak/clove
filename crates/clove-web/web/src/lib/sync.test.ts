import { describe, it, expect } from 'vitest';
import { externalRef, githubIssueNumber, hasSyncTarget, isSynced, syncTargetLabel } from './sync';
import { parseQuery, buildParams } from './query';
import { applyFilters } from './filter';
import type { Item, Meta } from './types';

const TARGET = {
  provider: 'github',
  repo: 'egeapak/clove',
  url: 'https://github.com/egeapak/clove',
  issue_url: 'https://github.com/egeapak/clove/issues/'
};

function meta(sync: Meta['sync']): Meta {
  return {
    id_prefix: 'clov',
    types: [],
    statuses: [],
    priorities: [],
    labels: [],
    assignees: [],
    daemon: { running: false, web_addr: null },
    source: 'standalone',
    sync
  };
}

describe('external refs', () => {
  it('reads a GitHub issue number only from a gh-<n> ref', () => {
    expect(githubIssueNumber('gh-67')).toBe(67);
    expect(githubIssueNumber('gh-')).toBeNull();
    expect(githubIssueNumber('gh-6x')).toBeNull();
    expect(githubIssueNumber('tk:abc')).toBeNull();
    expect(githubIssueNumber(null)).toBeNull();
  });

  it('links a synced item to its issue on the sync target', () => {
    expect(externalRef({ external_ref: 'gh-67', source_system: 'github' }, meta([TARGET]))).toEqual({
      label: 'GitHub #67',
      short: 'GH#67',
      href: 'https://github.com/egeapak/clove/issues/67'
    });
  });

  it('shows the ref without a link when the repo is unknown or ambiguous', () => {
    const it = { external_ref: 'gh-67', source_system: 'github' };
    expect(externalRef(it, meta([]))?.href).toBeNull();
    expect(externalRef(it, meta([TARGET, { ...TARGET, repo: 'other/repo' }]))?.href).toBeNull();
  });

  it('spells a foreign ref with its source system', () => {
    expect(externalRef({ external_ref: 'tk:x-1', source_system: 'tk' }, null)).toEqual({
      label: 'tk tk:x-1',
      short: 'tk:x-1',
      href: null
    });
  });

  it('has nothing to show for an item with no ref', () => {
    expect(externalRef({ external_ref: null }, meta([TARGET]))).toBeNull();
    expect(externalRef({}, meta([TARGET]))).toBeNull();
  });

  it('knows whether the project syncs at all', () => {
    expect(hasSyncTarget(meta([TARGET]))).toBe(true);
    expect(hasSyncTarget(meta([]))).toBe(false);
    expect(hasSyncTarget(meta(undefined))).toBe(false);
    expect(syncTargetLabel(meta([TARGET]))).toBe('GitHub egeapak/clove');
  });

  it('counts only a GitHub issue ref as synced', () => {
    expect(isSynced({ external_ref: 'gh-1' })).toBe(true);
    expect(isSynced({ external_ref: 'tk:1' })).toBe(false);
    expect(isSynced({ external_ref: null })).toBe(false);
  });
});

describe('the synced list filter', () => {
  it('round-trips through the URL', () => {
    expect(parseQuery(new URLSearchParams('synced=false')).synced).toBe(false);
    expect(parseQuery(new URLSearchParams('synced=true')).synced).toBe(true);
    expect(parseQuery(new URLSearchParams('synced=maybe')).synced).toBeUndefined();
    expect(buildParams({ synced: false }).get('synced')).toBe('false');
    expect(buildParams({}).has('synced')).toBe(false);
  });

  it('filters the mock backend like the server', () => {
    const base = { title: 't', status: 'open', type: 'bug', priority: 2, labels: [], blocked_by: [] };
    const items = [
      { ...base, id: 'a', external_ref: 'gh-1' },
      { ...base, id: 'b', external_ref: null },
      { ...base, id: 'c', external_ref: 'tk:9' }
    ] as unknown as Item[];
    expect(applyFilters(items, { synced: false }).map((i) => i.id)).toEqual(['b', 'c']);
    expect(applyFilters(items, { synced: true }).map((i) => i.id)).toEqual(['a']);
  });
});
