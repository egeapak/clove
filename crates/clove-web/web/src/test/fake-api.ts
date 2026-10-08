// A `fetch` stand-in for route tests: answers `/api/v1/...` requests from a
// handler keyed on the path after the API root, wrapped in the server envelope.
import { vi } from 'vitest';
import type { Item } from '$lib/types';

/** Thrown by a handler to answer with an error envelope and HTTP status. */
export class StubError extends Error {
  constructor(
    readonly status: number,
    readonly code: string
  ) {
    super(code);
  }
}

export type ApiHandler = (path: string, url: URL) => unknown;

export function stubApi(handler: ApiHandler) {
  const calls: string[] = [];
  vi.stubGlobal('fetch', async (input: string | URL) => {
    const url = new URL(String(input), 'http://localhost');
    const apiRoot = url.pathname.indexOf('/api/v1');
    const path = url.pathname.slice(apiRoot + '/api/v1'.length);
    calls.push(path);
    let data: unknown;
    try {
      data = handler(path, url);
    } catch (e) {
      if (!(e instanceof StubError)) throw e;
      return new Response(JSON.stringify({ v: 1, ok: false, error: { code: e.code, message: e.code, exit: 1 }, _meta: {} }), {
        status: e.status,
        headers: { 'content-type': 'application/json' }
      });
    }
    return new Response(JSON.stringify({ v: 1, ok: true, data, _meta: {} }), {
      status: 200,
      headers: { 'content-type': 'application/json' }
    });
  });
  return calls;
}

export const META = {
  statuses: ['open', 'in_progress', 'closed'],
  types: ['bug', 'feature', 'chore', 'docs', 'epic'],
  priorities: [0, 1, 2, 3, 4],
  labels: [],
  assignees: []
};

export function item(over: Partial<Item> & { id: string }): Item {
  return {
    title: 'An item',
    status: 'open',
    type: 'bug',
    priority: 2,
    assignee: null,
    parent: null,
    labels: [],
    deps: [],
    relates: [],
    created: '2026-01-01T00:00:00Z',
    updated: '2026-01-01T00:00:00Z',
    closed: null,
    body: '',
    comment_count: 0,
    ready: true,
    blocked_by: [],
    dangling_deps: [],
    ...over
  };
}
