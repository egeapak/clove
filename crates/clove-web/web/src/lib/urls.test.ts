import { describe, it, expect } from 'vitest';
import { apiBase, eventsUrl, PROJECTS_URL, projectSlug } from './urls';

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
