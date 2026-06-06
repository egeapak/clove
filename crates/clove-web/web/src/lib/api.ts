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
import { MOCK_ITEMS, MOCK_COMMENTS, mockHistory } from './mock';

const BASE = '/api/v1';

/** Set true once any real backend call fails in dev, so we stop retrying. */
let mockMode = false;
export function isMockMode() {
  return mockMode;
}

class ApiError extends Error {
  code: string;
  exit: number;
  constructor(code: string, message: string, exit: number) {
    super(message);
    this.code = code;
    this.exit = exit;
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
  const env = (await res.json()) as Envelope<T>;
  if (!env.ok || env.error) {
    throw new ApiError(
      env.error?.code ?? 'ERROR',
      env.error?.message ?? `HTTP ${res.status}`,
      env.error?.exit ?? 1
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

function qs(query: ListQuery): string {
  const p = new URLSearchParams();
  if (query.status) p.set('status', query.status);
  if (query.assignee) p.set('assignee', query.assignee);
  if (query.q) p.set('q', query.q);
  if (query.sort) p.set('sort', query.sort);
  if (query.dir) p.set('dir', query.dir);
  if (query.mode && query.mode !== 'list') p.set('mode', query.mode);
  for (const t of query.type ?? []) p.append('type', t);
  for (const pr of query.priority ?? []) p.append('priority', String(pr));
  for (const l of query.label ?? []) p.append('label', l);
  const s = p.toString();
  return s ? '?' + s : '';
}

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
  async patch(id: string, fields: Partial<Pick<Item, 'status' | 'priority' | 'assignee' | 'type'>>) {
    return withMock(
      () => req<Item>('/items/' + encodeURIComponent(id), { method: 'PATCH', body: JSON.stringify(fields) }),
      () => {
        const it = { ...MOCK_ITEMS.find((i) => i.id === id)!, ...fields, updated: new Date().toISOString() };
        const idx = MOCK_ITEMS.findIndex((i) => i.id === id);
        MOCK_ITEMS[idx] = it;
        return it;
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
        (MOCK_COMMENTS[id] ??= []).push({ author: 'you', timestamp: new Date().toISOString(), body });
        const idx = MOCK_ITEMS.findIndex((i) => i.id === id);
        MOCK_ITEMS[idx] = { ...MOCK_ITEMS[idx], comment_count: MOCK_ITEMS[idx].comment_count + 1 };
        return MOCK_ITEMS[idx];
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
function filterMock(q: ListQuery): Item[] {
  let items = [...MOCK_ITEMS];
  if (q.mode === 'ready') items = items.filter((i) => i.ready && i.status !== 'closed');
  if (q.mode === 'blocked') items = items.filter((i) => i.blocked_by.length > 0);
  if (q.status) items = items.filter((i) => i.status === q.status);
  if (q.assignee) items = items.filter((i) => i.assignee === q.assignee);
  if (q.type?.length) items = items.filter((i) => q.type!.includes(i.type));
  if (q.priority?.length) items = items.filter((i) => q.priority!.includes(i.priority));
  if (q.label?.length) items = items.filter((i) => q.label!.every((l) => i.labels.includes(l)));
  if (q.q) {
    const needle = q.q.toLowerCase();
    items = items.filter(
      (i) => i.title.toLowerCase().includes(needle) || i.id.includes(needle) || i.body.toLowerCase().includes(needle)
    );
  }
  return items;
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
