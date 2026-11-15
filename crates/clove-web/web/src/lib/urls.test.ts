import { describe, it, expect } from 'vitest';
import { apiBase, eventsUrl, isActiveTab, PROJECTS_URL, projectSlug } from './urls';

describe('apiBase', () => {
  it('is the root API when the app is served at the root', () => {
    expect(apiBase('')).toBe('/api/v1');
  });

  it('nests the API under the project prefix when served by the hub', () => {
    expect(apiBase('/p/clove')).toBe('/p/clove/api/v1');
  });
});

describe('eventsUrl', () => {
  it('uses ws: for an http page and keeps the base', () => {
    expect(eventsUrl({ protocol: 'http:', host: '127.0.0.1:7373' }, '/p/clove')).toBe(
      'ws://127.0.0.1:7373/p/clove/api/v1/events'
    );
  });

  it('uses wss: for an https page', () => {
    expect(eventsUrl({ protocol: 'https:', host: 'example.test' }, '')).toBe(
      'wss://example.test/api/v1/events'
    );
  });
});

describe('projects', () => {
  it('lists projects from the hub root, not the project prefix', () => {
    expect(PROJECTS_URL).toBe('/api/v1/projects');
  });

  it('reads the slug out of a hub base', () => {
    expect(projectSlug('/p/clove')).toBe('clove');
    expect(projectSlug('/p/my-repo-a1b2c3')).toBe('my-repo-a1b2c3');
  });

  it('has no slug when served standalone', () => {
    expect(projectSlug('')).toBeNull();
    expect(projectSlug('/something/else')).toBeNull();
  });
});

describe('isActiveTab', () => {
  const active = (path: string, base = '') =>
    ['board', 'list', 'timeline'].filter((t) => isActiveTab(path, base, t));

  it('matches the view as a path segment', () => {
    expect(active('/list')).toEqual(['list']);
    expect(active('/list/')).toEqual(['list']);
    expect(active('/timeline')).toEqual(['timeline']);
    expect(active('/board')).toEqual(['board']);
  });

  it('treats the app root as the board', () => {
    expect(active('/')).toEqual(['board']);
    expect(active('/p/demo/', '/p/demo')).toEqual(['board']);
    expect(active('/p/demo', '/p/demo')).toEqual(['board']);
  });

  it('lights no tab on an item page', () => {
    expect(active('/items/proj-7AF3Q2K9')).toEqual([]);
    expect(active('/items/proj-7AF3Q2K9/edit')).toEqual([]);
  });

  it('ignores a project name that contains a view name', () => {
    expect(active('/p/list-app/board', '/p/list-app')).toEqual(['board']);
    expect(active('/p/list-app/items/proj-7AF3Q2K9', '/p/list-app')).toEqual([]);
    expect(active('/p/timeline/list', '/p/timeline')).toEqual(['list']);
  });
});
