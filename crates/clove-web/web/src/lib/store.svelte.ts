import { browser, dev } from '$app/environment';
import type { Item, ConnState, Meta } from './types';
import { api, isMockMode } from './api';

/** Normalized client cache: Map<id, Item>. All views derive from this. */
class Store {
  items = $state<Map<string, Item>>(new Map());
  meta = $state<Meta | null>(null);
  conn = $state<ConnState>('connecting');
  loaded = $state(false);
  /** Set when the *initial* load fails (backend down), so views can show a
   *  distinct error/retry panel instead of the empty state. Cleared on success. */
  loadError = $state<string | null>(null);

  // pending optimistic ops: id -> snapshot before write
  private pending = new Map<string, Item>();
  // Canonical server order: id -> insertion index from the last replaceAll.
  // The list's default ("rank") sort uses this so it preserves the server's
  // (priority, topo, id) ordering instead of approximating with priority.
  private rankIndex = new Map<string, number>();

  get all(): Item[] {
    return [...this.items.values()];
  }

  /** Server canonical rank for an id (lower = earlier). Unknown ids sort last. */
  rankOf(id: string): number {
    return this.rankIndex.get(id) ?? Number.MAX_SAFE_INTEGER;
  }

  upsert(item: Item) {
    // Out-of-order guard: drop a stale server event for a non-pending item.
    // Live per-id events no longer arrive, so this only guards the rare case of
    // two concurrent reads racing; it must never drop an item for a pending id.
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
    const next = new Map(list.map((i) => [i.id, i]));
    // Record server order for the "rank" sort.
    this.rankIndex = new Map(list.map((i, idx) => [i.id, idx]));
    // Re-apply any in-flight optimistic writes on top of the (possibly stale)
    // server list so a batch-triggered refetch can't clobber a pending write.
    for (const [id, before] of this.pending) {
      const cur = this.items.get(id);
      if (cur) next.set(id, cur); // keep the optimistic value
      else if (before) next.set(id, before);
    }
    this.items = next;
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
      this.pending.delete(id);
      if (snap) {
        // Force the snapshot back even though pending was just cleared.
        const next = new Map(this.items);
        next.set(id, snap);
        this.items = next;
      }
    };
  }

  /** Settle a pending optimistic write with the authoritative server payload. */
  settle(id: string, server?: Item) {
    this.pending.delete(id);
    if (server) {
      // Apply authoritatively, bypassing the stale-updated guard.
      const next = new Map(this.items);
      next.set(server.id, server);
      this.items = next;
    }
  }

  async refetch() {
    const [list, meta] = await Promise.all([api.items(), api.meta()]);
    this.replaceAll(list);
    this.meta = meta;
    this.loadError = null;
    if (isMockMode()) this.conn = 'mock';
  }
}

export const store = new Store();

// ---- WebSocket live channel with backoff + resync ----
let ws: WebSocket | null = null;
let backoff = 500;
let lastSeq = -1;
let started = false;
let reconnectTimer: ReturnType<typeof setTimeout> | null = null;
let lifecycleBound = false;

function connect() {
  if (!browser) return;
  // Guard against a double-connect (e.g. Vite HMR re-running this module, or a
  // visibility/online trigger firing while a socket is already live).
  if (ws && (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING)) return;
  clearReconnect();
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
    void store.refetch().catch((e) => {
      store.loadError = e instanceof Error ? e.message : 'load failed';
    }); // resync on (re)connect
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
      // Live per-id events are no longer emitted by the backend, but the
      // handlers are kept (harmless, future-proof).
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

function clearReconnect() {
  if (reconnectTimer !== null) {
    clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }
}

function scheduleReconnect() {
  if (isMockMode()) {
    store.conn = 'mock';
    return;
  }
  clearReconnect();
  const jitter = Math.random() * 300;
  reconnectTimer = setTimeout(connect, Math.min(backoff, 10000) + jitter);
  backoff = Math.min(backoff * 2, 10000);
}

/** Reset backoff and reconnect immediately (online / tab-visible). */
function reconnectNow() {
  if (isMockMode()) return;
  if (ws && (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING)) return;
  backoff = 500;
  clearReconnect();
  connect();
}

function bindLifecycle() {
  if (lifecycleBound || !browser) return;
  lifecycleBound = true;
  window.addEventListener('online', reconnectNow);
  document.addEventListener('visibilitychange', () => {
    if (document.visibilityState === 'visible') reconnectNow();
  });
  // Avoid double-connect across Vite HMR module reloads.
  if (import.meta.hot) {
    import.meta.hot.dispose(() => stopLive());
  }
}

/** Tear down the live channel + timers (HMR / teardown). */
export function stopLive() {
  clearReconnect();
  if (ws) {
    ws.onopen = ws.onmessage = ws.onclose = ws.onerror = null;
    try {
      ws.close();
    } catch {}
    ws = null;
  }
  started = false;
}

/** Kick off data load + live channel. Idempotent. */
export async function startLive() {
  if (started) return;
  started = true;
  bindLifecycle();
  try {
    await store.refetch();
  } catch (e) {
    // Initial load failed: surface a distinct error so views can offer Retry.
    store.loadError = e instanceof Error ? e.message : 'load failed';
    store.conn = 'offline';
  }
  if (isMockMode() || (dev && store.meta?.source === 'mock')) {
    store.conn = 'mock';
    return;
  }
  connect();
}

/** Retry the initial load after a failure (Retry button). */
export async function retryLoad() {
  store.loadError = null;
  store.conn = 'connecting';
  try {
    await store.refetch();
  } catch (e) {
    store.loadError = e instanceof Error ? e.message : 'load failed';
    store.conn = 'offline';
    return;
  }
  if (!isMockMode()) connect();
}
