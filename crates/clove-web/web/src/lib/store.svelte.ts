import { browser, dev } from '$app/environment';
import type { Item, ConnState, Meta } from './types';
import { api, isMockMode } from './api';

/** Handle for one in-flight optimistic edit (see `Store.optimistic`). */
export interface OptimisticEdit {
  /** Undo this edit's patch (the failure path). */
  rollback(): void;
  /** Consume this edit's patch with the authoritative server payload. */
  settle(server?: Item): void;
}

/** Normalized client cache: Map<id, Item>. All views derive from this. */
class Store {
  items = $state<Map<string, Item>>(new Map());
  meta = $state<Meta | null>(null);
  conn = $state<ConnState>('connecting');
  loaded = $state(false);
  /** Set when the *initial* load fails (backend down), so views can show a
   *  distinct error/retry panel instead of the empty state. Cleared on success. */
  loadError = $state<string | null>(null);

  // Pending optimistic ops per id. Concurrent edits to the same id must compose:
  // we keep the pre-edit `base` (server-authoritative) plus an ordered list of
  // in-flight patches (each with its own token). The visible item is always
  // `base` with every pending patch layered on top, so one edit settling or
  // rolling back never clobbers another edit that is still in flight.
  private pending = new Map<
    string,
    { base: Item; patches: Array<{ token: symbol; patch: Partial<Item> }> }
  >();
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
    // Rebase each pending entry's `base` onto the fresh server value so a later
    // settle/rollback converges on current data.
    for (const [id, entry] of this.pending) {
      const fresh = next.get(id);
      if (fresh) entry.base = fresh;
      next.set(id, this.merge(entry));
    }
    this.items = next;
    this.loaded = true;
  }

  /** Layer every pending patch (in order) over `base`, stamping `updated`. */
  private merge(entry: { base: Item; patches: Array<{ patch: Partial<Item> }> }): Item {
    let merged = entry.base;
    for (const { patch } of entry.patches) merged = { ...merged, ...patch };
    return { ...merged, updated: new Date().toISOString() };
  }

  /** Force `id` to `item`, bypassing the stale-`updated` guard in upsert(). */
  private forceSet(id: string, item: Item) {
    const next = new Map(this.items);
    next.set(id, item);
    this.items = next;
  }

  /** Recompute the visible item for `id` from its pending entry (if any). */
  private recompute(id: string) {
    const entry = this.pending.get(id);
    if (entry) this.forceSet(id, this.merge(entry));
  }

  /**
   * Apply an optimistic local patch; returns a handle whose `settle`/`rollback`
   * consume exactly THIS edit's patch (matched by token, never by position:
   * HTTP responses can arrive out of order, and dropping "the oldest" patch on
   * settle while rolling back by token corrupted the ledger — a patch could
   * leak into `pending` forever and silently overwrite later server values).
   */
  optimistic(id: string, patch: Partial<Item>): OptimisticEdit {
    const before = this.items.get(id);
    if (!before) return { rollback: () => {}, settle: () => {} };
    let entry = this.pending.get(id);
    if (!entry) {
      entry = { base: before, patches: [] };
      this.pending.set(id, entry);
    }
    const token = Symbol();
    entry.patches.push({ token, patch });
    this.recompute(id);

    // Remove this edit's patch; on settle also rebase the remaining pending
    // edits onto the authoritative server payload so they are not clobbered.
    const consume = (server?: Item) => {
      const e = this.pending.get(id);
      if (!e) {
        if (server) this.forceSet(id, server);
        return;
      }
      const idx = e.patches.findIndex((p) => p.token === token);
      if (idx >= 0) e.patches.splice(idx, 1);
      if (server) e.base = server;
      if (e.patches.length === 0) {
        this.pending.delete(id);
        this.forceSet(id, e.base);
      } else {
        this.recompute(id);
      }
    };
    return {
      rollback: () => consume(),
      settle: (server?: Item) => consume(server)
    };
  }

  /**
   * Apply an authoritative server payload for an edit that carried no
   * optimistic patch (e.g. the full edit form). Pending edits, if any, are
   * rebased on it — their patches stay owned by their own handles.
   */
  settle(id: string, server?: Item) {
    const entry = this.pending.get(id);
    if (!entry) {
      if (server) this.forceSet(id, server);
      return;
    }
    if (server) entry.base = server;
    this.recompute(id);
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
      case 'batch': {
        // A full rescan is sub-second even at 10k items, so any change just
        // triggers a refetch of the (lean) list. `seq` is tracked for logging /
        // future gap detection.
        const seq = frame.data?.seq;
        lastSeq = seq ?? lastSeq;
        // Mirror the onopen resync: surface a failed refetch as a loadError
        // instead of an unhandled rejection + stale data under a 'live' badge.
        void store.refetch().catch((e) => {
          store.loadError = e instanceof Error ? e.message : 'load failed';
        });
        break;
      }
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
