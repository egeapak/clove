import { browser, dev } from '$app/environment';
import type { Item, ConnState, Meta } from './types';
import { api, isMockMode } from './api';

/** Normalized client cache: Map<id, Item>. All views derive from this. */
class Store {
  items = $state<Map<string, Item>>(new Map());
  meta = $state<Meta | null>(null);
  conn = $state<ConnState>('connecting');
  loaded = $state(false);

  // pending optimistic ops: id -> snapshot before write
  private pending = new Map<string, Item>();

  get all(): Item[] {
    return [...this.items.values()];
  }

  upsert(item: Item) {
    // out-of-order guard: drop a stale server event for a non-pending item
    const cur = this.items.get(item.id);
    if (cur && !this.pending.has(item.id) && cur.updated > item.updated) return;
    const next = new Map(this.items);
    next.set(item.id, item);
    this.items = next;
  }

  remove(id: string) {
    if (!this.items.has(id)) return;
    const next = new Map(this.items);
    next.delete(id);
    this.items = next;
  }

  replaceAll(list: Item[]) {
    this.items = new Map(list.map((i) => [i.id, i]));
    this.loaded = true;
  }

  /** Apply an optimistic local patch; returns a rollback fn. */
  optimistic(id: string, patch: Partial<Item>): () => void {
    const before = this.items.get(id);
    if (!before) return () => {};
    if (!this.pending.has(id)) this.pending.set(id, before);
    this.upsert({ ...before, ...patch, updated: new Date().toISOString() });
    return () => {
      const snap = this.pending.get(id);
      if (snap) this.upsert(snap);
      this.pending.delete(id);
    };
  }

  settle(id: string, server?: Item) {
    this.pending.delete(id);
    if (server) this.upsert(server);
  }

  async refetch() {
    const [list, meta] = await Promise.all([api.items(), api.meta()]);
    this.replaceAll(list);
    this.meta = meta;
    if (isMockMode()) this.conn = 'mock';
  }
}

export const store = new Store();

// ---- WebSocket live channel with backoff + resync ----
let ws: WebSocket | null = null;
let backoff = 500;
let lastSeq = -1;
let started = false;

function connect() {
  if (!browser) return;
  const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
  const url = `${proto}//${location.host}/api/v1/events`;
  try {
    ws = new WebSocket(url);
  } catch {
    scheduleReconnect();
    return;
  }
  store.conn = 'connecting';

  ws.onopen = () => {
    backoff = 500;
    store.conn = 'live';
    void store.refetch(); // resync on (re)connect
  };

  ws.onmessage = (ev) => {
    let frame: { event: string; data?: any };
    try {
      frame = JSON.parse(ev.data);
    } catch {
      return;
    }
    switch (frame.event) {
      case 'hello':
        lastSeq = frame.data?.seq ?? lastSeq;
        break;
      case 'item.upserted':
        if (frame.data?.item) store.upsert(frame.data.item);
        break;
      case 'item.deleted':
        if (frame.data?.id) store.remove(frame.data.id);
        break;
      case 'batch': {
        // A full rescan is sub-second even at 10k items, so any change just
        // triggers a refetch of the (lean) list. `seq` is tracked for logging /
        // future gap detection.
        const seq = frame.data?.seq;
        lastSeq = seq ?? lastSeq;
        void store.refetch();
        break;
      }
      case 'stats.updated':
      case 'ping':
        break;
    }
  };

  ws.onclose = () => {
    store.conn = 'offline';
    scheduleReconnect();
  };
  ws.onerror = () => {
    try {
      ws?.close();
    } catch {}
  };
}

function scheduleReconnect() {
  if (isMockMode()) {
    store.conn = 'mock';
    return;
  }
  const jitter = Math.random() * 300;
  setTimeout(connect, Math.min(backoff, 10000) + jitter);
  backoff = Math.min(backoff * 2, 10000);
}

/** Kick off data load + live channel. Idempotent. */
export async function startLive() {
  if (started) return;
  started = true;
  await store.refetch().catch(() => {});
  if (isMockMode() || (dev && store.meta?.source === 'mock')) {
    store.conn = 'mock';
    return;
  }
  connect();
}
