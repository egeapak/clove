import { browser, dev } from '$app/environment';
import type {
  Item,
  Comment,
  DepTreeNode,
  Board,
  Meta,
  StatsHistoryPoint,
  Envelope,
  ListQuery
} from './types';
import type { PatchPayload } from './itemForm';
import { MOCK_ITEMS, MOCK_COMMENTS, mockHistory } from './mock';
import { applyFilters } from './filter';
import { queryString } from './query';

const BASE = '/api/v1';

/** Stand-in author for mock-mode comment authorship (real server assigns it). */
const MOCK_META_USER = 'you';

/** Set true once any real backend call fails in dev, so we stop retrying. */
let mockMode = false;
export function isMockMode() {
  return mockMode;
}

export class ApiError extends Error {
  code: string;
  exit: number;
  status: number;
  constructor(code: string, message: string, exit: number, status = 0) {
    super(message);
    this.code = code;
    this.exit = exit;
    this.status = status;
  }
}

async function req<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(BASE + path, {
    ...init,
    headers: {
      ...(init?.body ? { 'Content-Type': 'application/json' } : {}),
      ...init?.headers
    }
  });
  let env: Envelope<T> | null = null;
  try {
    env = (await res.json()) as Envelope<T>;
  } catch {
    // Non-JSON body (e.g. a bare 502 from a proxy). Surface it as an ApiError
    // carrying the HTTP status instead of leaving an unhandled rejection.
    throw new ApiError('NON_JSON', `HTTP ${res.status}`, 1, res.status);
  }
  if (!env.ok || env.error) {
    throw new ApiError(
      env.error?.code ?? 'ERROR',
      env.error?.message ?? `HTTP ${res.status}`,
      env.error?.exit ?? 1,
      res.status
    );
  }
  return env.data as T;
}

/** In dev, when no backend responds, fall back to mock data. */
async function withMock<T>(real: () => Promise<T>, mock: () => T): Promise<T> {
  if (mockMode) return mock();
  if (!dev) return real();
  try {
    return await real();
  } catch (e) {
    if (e instanceof ApiError) throw e; // real backend, real error
    mockMode = true;
    if (browser) console.info('[clove] no backend — using mock fixture data');
    return mock();
  }
}

// URL <-> ListQuery mapping lives in query.ts (shared with the list page).
const qs = queryString;

export const api = {
  async items(query: ListQuery = {}): Promise<Item[]> {
    return withMock(
      () => req<Item[]>('/items' + qs(query)),
      () => filterMock(query)
    );
  },

  async item(id: string): Promise<Item> {
    return withMock(
      () => req<Item>('/items/' + encodeURIComponent(id)),
      () => {
        const it = MOCK_ITEMS.find((i) => i.id === id);
        if (!it) throw new ApiError('NOT_FOUND', 'item not found', 1);
        return it;
      }
    );
  },

  async comments(id: string): Promise<Comment[]> {
    return withMock(
      () => req<Comment[]>(`/items/${encodeURIComponent(id)}/comments`),
      () => MOCK_COMMENTS[id] ?? []
    );
  },

  async deptree(id: string, depth = 4): Promise<DepTreeNode> {
    return withMock(
      () => req<DepTreeNode>(`/items/${encodeURIComponent(id)}/deptree?depth=${depth}`),
      () => mockDeptree(id)
    );
  },

  async board(): Promise<Board> {
    return withMock(
      () => req<Board>('/board?group_by=status'),
      () => mockBoard()
    );
  },

  async meta(): Promise<Meta> {
    return withMock(
      () => req<Meta>('/meta'),
      () => mockMeta()
    );
  },

  async history(): Promise<StatsHistoryPoint[]> {
    return withMock(
      () => req<StatsHistoryPoint[]>('/stats/history'),
      () => mockHistory()
    );
  },

  // ---- writes ----
  async patch(id: string, fields: PatchPayload) {
    return withMock(
      () => req<Item>('/items/' + encodeURIComponent(id), { method: 'PATCH', body: JSON.stringify(fields) }),
      () => {
        const idx = MOCK_ITEMS.findIndex((i) => i.id === id);
        const prev = MOCK_ITEMS[idx];
        const it: Item = { ...prev, updated: new Date().toISOString() };
        if (fields.title !== undefined) it.title = fields.title;
        if (fields.status !== undefined) it.status = fields.status;
        if (fields.priority !== undefined) it.priority = fields.priority;
        if (fields.type !== undefined) it.type = fields.type as Item['type'];
        if (fields.assignee !== undefined) it.assignee = fields.assignee;
        if (fields.body !== undefined) it.body = fields.body;
        if (fields.labels !== undefined) it.labels = [...fields.labels];
        MOCK_ITEMS[idx] = it;
        return it;
      }
    );
  },

  /** Set or clear (`null`) an item's parent. */
  async setParent(id: string, parent: string | null) {
    return withMock(
      () =>
        req<Item>(`/items/${encodeURIComponent(id)}/parent`, {
          method: 'PUT',
          body: JSON.stringify({ parent })
        }),
      () => {
        const idx = MOCK_ITEMS.findIndex((i) => i.id === id);
        MOCK_ITEMS[idx] = { ...MOCK_ITEMS[idx], parent, updated: new Date().toISOString() };
        return MOCK_ITEMS[idx];
      }
    );
  },

  async setLabels(id: string, add: string[], remove: string[]) {
    return withMock(
      () =>
        req<Item>(`/items/${encodeURIComponent(id)}/labels`, {
          method: 'PUT',
          body: JSON.stringify({ add, remove })
        }),
      () => {
        const idx = MOCK_ITEMS.findIndex((i) => i.id === id);
        const set = new Set(MOCK_ITEMS[idx].labels);
        add.forEach((l) => set.add(l));
        remove.forEach((l) => set.delete(l));
        MOCK_ITEMS[idx] = { ...MOCK_ITEMS[idx], labels: [...set], updated: new Date().toISOString() };
        return MOCK_ITEMS[idx];
      }
    );
  },

  async addComment(id: string, body: string) {
    return withMock(
      () =>
        req<Item>(`/items/${encodeURIComponent(id)}/comments`, {
          method: 'POST',
          body: JSON.stringify({ body })
        }),
      () => {
        // The server assigns the author; the mock stamps a stand-in so a later
        // GET /comments reconcile has a canonical author to return.
        (MOCK_COMMENTS[id] ??= []).push({
          author: MOCK_META_USER,
          timestamp: new Date().toISOString(),
          body
        });
        const idx = MOCK_ITEMS.findIndex((i) => i.id === id);
        MOCK_ITEMS[idx] = { ...MOCK_ITEMS[idx], comment_count: MOCK_ITEMS[idx].comment_count + 1 };
        return MOCK_ITEMS[idx];
      }
    );
  },

  /** Delete an item. Pass force=true to delete despite dependents (409). */
  async delete(id: string, opts: { force?: boolean } = {}): Promise<void> {
    return withMock(
      async () => {
        await req<unknown>(
          `/items/${encodeURIComponent(id)}${opts.force ? '?force=' : ''}`,
          { method: 'DELETE' }
        );
      },
      () => {
        const dependents = MOCK_ITEMS.filter((i) => i.deps.includes(id));
        if (dependents.length && !opts.force) {
          throw new ApiError('HAS_DEPENDENTS', `${dependents.length} item(s) depend on this`, 1, 409);
        }
        const idx = MOCK_ITEMS.findIndex((i) => i.id === id);
        if (idx >= 0) MOCK_ITEMS.splice(idx, 1);
        // Drop dangling refs in mock data so subsequent reads stay consistent.
        for (const it of MOCK_ITEMS) {
          if (it.deps.includes(id)) it.deps = it.deps.filter((d) => d !== id);
        }
      }
    );
  },

  async addDep(id: string, dep: string) {
    return withMock(
      () => req<Item>(`/items/${encodeURIComponent(id)}/deps`, { method: 'POST', body: JSON.stringify({ dep }) }),
      () => {
        const idx = MOCK_ITEMS.findIndex((i) => i.id === id);
        const deps = [...new Set([...MOCK_ITEMS[idx].deps, dep])];
        MOCK_ITEMS[idx] = { ...MOCK_ITEMS[idx], deps, updated: new Date().toISOString() };
        return MOCK_ITEMS[idx];
      }
    );
  },

  async removeDep(id: string, dep: string) {
    return withMock(
      () => req<Item>(`/items/${encodeURIComponent(id)}/deps/${encodeURIComponent(dep)}`, { method: 'DELETE' }),
      () => {
        const idx = MOCK_ITEMS.findIndex((i) => i.id === id);
        MOCK_ITEMS[idx] = {
          ...MOCK_ITEMS[idx],
          deps: MOCK_ITEMS[idx].deps.filter((d) => d !== dep),
          updated: new Date().toISOString()
        };
        return MOCK_ITEMS[idx];
      }
    );
  },

  async create(fields: {
    title: string;
    type?: string;
    priority?: number;
    labels?: string[];
    assignee?: string;
    body?: string;
  }) {
    return withMock(
      () => req<Item>('/items', { method: 'POST', body: JSON.stringify(fields) }),
      () => {
        const id = String(Math.max(...MOCK_ITEMS.map((i) => Number(i.id) || 0)) + 1);
        const it: Item = {
          id,
          title: fields.title,
          status: 'open',
          type: (fields.type as Item['type']) ?? 'feature',
          priority: fields.priority ?? 2,
          assignee: fields.assignee ?? null,
          parent: null,
          labels: fields.labels ?? [],
          deps: [],
          relates: [],
          created: new Date().toISOString(),
          updated: new Date().toISOString(),
          closed: null,
          body: fields.body ?? '',
          comment_count: 0,
          ready: true,
          blocked_by: [],
          dangling_deps: []
        };
        MOCK_ITEMS.unshift(it);
        return it;
      }
    );
  }
};

// ---- mock helpers ----
// Filtering uses the same shared logic as the live list page so the two can't
// diverge. The server returns canonical rank order; the mock list is already in
// that order (MOCK_ITEMS authoring order), so no extra sort is applied here.
function filterMock(q: ListQuery): Item[] {
  return applyFilters(MOCK_ITEMS, q);
}

function mockBoard(): Board {
  const cols: Array<[string, string]> = [
    ['open', 'Open'],
    ['in_progress', 'In Progress'],
    ['closed', 'Closed']
  ];
  return {
    columns: cols.map(([key, label]) => {
      const items = MOCK_ITEMS.filter((i) => i.status === key);
      return { key, label, count: items.length, items };
    })
  };
}

function mockMeta(): Meta {
  return {
    id_prefix: '',
    types: ['bug', 'feature', 'chore', 'docs', 'epic'],
    statuses: ['open', 'in_progress', 'closed'],
    priorities: [0, 1, 2, 3, 4],
    labels: [...new Set(MOCK_ITEMS.flatMap((i) => i.labels))].sort(),
    assignees: [...new Set(MOCK_ITEMS.map((i) => i.assignee).filter(Boolean) as string[])].sort(),
    daemon: { running: false, web_addr: null },
    source: 'mock'
  };
}

function mockDeptree(id: string): DepTreeNode {
  const seen = new Set<string>();
  function build(curId: string): DepTreeNode {
    const it = MOCK_ITEMS.find((i) => i.id === curId);
    const cycle = seen.has(curId);
    seen.add(curId);
    const children = it && !cycle ? it.deps.map(build) : [];
    return {
      id: curId,
      title: it?.title ?? '(missing)',
      status: it?.status ?? 'open',
      ready: it?.ready ?? false,
      cycle_ref: cycle,
      children
    };
  }
  return build(id);
}
