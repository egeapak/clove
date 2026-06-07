<script lang="ts">
  import type { Item, Status } from '$lib/types';
  import { store, retryLoad } from '$lib/store.svelte';
  import { api } from '$lib/api';
  import { toasts } from '$lib/toast.svelte';
  import Card from '$lib/components/Card.svelte';
  import StatusGlyph from '$lib/components/StatusGlyph.svelte';
  import { Virtual } from '$lib/virtual.svelte';

  const empty = $derived(store.loaded && store.all.length === 0);

  const COLS: Array<{ key: Status; label: string }> = [
    { key: 'open', label: 'Open' },
    { key: 'in_progress', label: 'In Progress' },
    { key: 'closed', label: 'Closed' }
  ];

  // Derive columns directly from the normalized store so live events re-render.
  const columns = $derived(
    COLS.map((c) => ({
      ...c,
      items: store.all
        .filter((i) => i.status === c.key)
        .sort((a, b) => a.priority - b.priority || b.updated.localeCompare(a.updated))
    }))
  );

  // ---- per-column virtualization (@tanstack/virtual-core) ----
  // Cards vary in height (labels/title wrap), so we use the virtualizer's
  // dynamic `measureElement`: each rendered card reports its real size, the
  // estimate (CARD_EST) only seeds the first paint. Each column scrolls
  // independently, so we keep one Virtualizer per column keyed by status.
  // Drag-and-drop is unaffected — cards are still real <a draggable> nodes; we
  // only position the visible slice and pad the rest with a sized container.
  const CARD_EST = 96; // px; seed estimate for an unmeasured card
  const scrollEls = $state<Record<Status, HTMLElement | undefined>>({
    open: undefined,
    in_progress: undefined,
    closed: undefined
  });

  function colItems(key: Status): Item[] {
    return columns.find((c) => c.key === key)?.items ?? [];
  }

  // Build a virtualizer per status column. The closures read live state lazily.
  const virtuals = new Map<Status, Virtual>(
    COLS.map((c) => [
      c.key,
      new Virtual({
        count: 0,
        getScrollElement: () => scrollEls[c.key] ?? null,
        estimateSize: () => CARD_EST,
        overscan: 6,
        getItemKey: (i) => colItems(c.key)[i]?.id ?? i
      })
    ])
  );

  // Mount each virtualizer once its scroll container exists.
  $effect(() => {
    const cleanups = COLS.map((c) => (scrollEls[c.key] ? virtuals.get(c.key)!.attach() : undefined));
    return () => cleanups.forEach((fn) => fn?.());
  });
  // Re-sync counts whenever the derived columns change (live events, drag moves).
  $effect(() => {
    for (const col of columns) virtuals.get(col.key)!.update({ count: col.items.length });
  });

  // Svelte action: register a card node for dynamic measurement. virtual-core
  // reads `data-index` off the node, observes its size, and remeasures on
  // resize; returning the unobserve keeps things tidy on unmount.
  function measureRow(node: HTMLElement, v: Virtual) {
    v.measure(node);
    return { update: () => v.measure(node) };
  }

  let dragId = $state<string | null>(null);
  let overCol = $state<Status | null>(null);

  function onDragStart(e: DragEvent, id: string) {
    dragId = id;
    e.dataTransfer?.setData('text/plain', id);
    if (e.dataTransfer) e.dataTransfer.effectAllowed = 'move';
  }

  function onDragOver(e: DragEvent, key: Status) {
    e.preventDefault();
    overCol = key;
    if (e.dataTransfer) e.dataTransfer.dropEffect = 'move';
  }

  async function onDrop(e: DragEvent, key: Status) {
    e.preventDefault();
    overCol = null;
    const id = dragId ?? e.dataTransfer?.getData('text/plain');
    dragId = null;
    if (!id) return;
    const item = store.items.get(id);
    if (!item || item.status === key) return;

    // optimistic
    const rollback = store.optimistic(id, { status: key });
    try {
      const updated = await api.patch(id, { status: key });
      store.settle(id, updated);
    } catch (err) {
      rollback();
      toasts.error(`Move failed: ${err instanceof Error ? err.message : 'error'}`);
    }
  }

  // Move a card's status one column left/right. Reachable via the focusable
  // ‹/› buttons on each card (keyboard + pointer); no hidden move-mode chord.
  async function move(item: Item, dir: 1 | -1) {
    const order: Status[] = ['open', 'in_progress', 'closed'];
    const idx = order.indexOf(item.status);
    const next = order[(idx + dir + order.length) % order.length];
    const rollback = store.optimistic(item.id, { status: next });
    try {
      store.settle(item.id, await api.patch(item.id, { status: next }));
    } catch {
      rollback();
      toasts.error('Move failed');
    }
  }
</script>

<div class="filters">
  <span class="dim mono count">{store.all.length} items · grouped by status</span>
  <span class="hint dim">Drag a card between columns to change status</span>
</div>

{#if store.loadError && !store.loaded}
  <div class="panel loaderr" role="alert">
    <div class="loaderr-title">Couldn’t reach the backend</div>
    <p class="dim">{store.loadError}</p>
    <button class="btn primary" onclick={() => retryLoad()}>Retry</button>
  </div>
{:else if empty}
  <div class="panel board-empty dim">
    <div class="be-title">No items yet</div>
    <p>This repo has no items. Create one to get started.</p>
  </div>
{:else}
<div class="board">
  {#each columns as col (col.key)}
    {@const v = virtuals.get(col.key)!}
    <section
      class="col"
      class:over={overCol === col.key}
      ondragover={(e) => onDragOver(e, col.key)}
      ondragleave={() => (overCol = null)}
      ondrop={(e) => onDrop(e, col.key)}
      role="list"
      aria-label="{col.label} column"
    >
      <header class="col-head">
        <StatusGlyph status={col.key} />
        {col.label}
        <span class="cnt mono">{col.items.length}</span>
      </header>
      <div class="col-body" bind:this={scrollEls[col.key]}>
        {#if col.items.length === 0}
          <div class="empty dim">No items</div>
        {:else}
          <div class="col-sizer" style="height:{v.total}px">
            {#each v.items as row (row.key)}
              {@const item = col.items[row.index]}
              <div
                role="listitem"
                class="card-wrap"
                tabindex="-1"
                data-index={row.index}
                use:measureRow={v}
                style="transform:translateY({row.start}px)"
              >
                <Card {item} ondragstart={(e) => onDragStart(e, item.id)} />
                <div class="kbd-move">
                  <button class="btn sm" aria-label="move {item.id} left" onclick={() => move(item, -1)}>‹</button>
                  <button class="btn sm" aria-label="move {item.id} right" onclick={() => move(item, 1)}>›</button>
                </div>
              </div>
            {/each}
          </div>
        {/if}
      </div>
    </section>
  {/each}
</div>
{/if}

<style>
  .loaderr,
  .board-empty {
    text-align: center;
    padding: 40px 24px;
  }
  .loaderr-title,
  .be-title {
    font-size: 15px;
    font-weight: 600;
    margin-bottom: 6px;
    color: var(--text);
  }
  .loaderr p,
  .board-empty p {
    margin: 0 0 14px;
  }
  .filters {
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 4px 2px 14px;
  }
  .count {
    font-size: 11px;
  }
  .hint {
    font-size: 11px;
    margin-left: auto;
  }
  .board {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 14px;
  }
  .col {
    background: var(--surface);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    display: flex;
    flex-direction: column;
    backdrop-filter: blur(var(--glass-blur));
    transition: border-color 0.12s;
  }
  .col.over {
    border-color: var(--accent);
    box-shadow: 0 0 0 1px var(--accent) inset;
  }
  .col-head {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 10px 12px;
    border-bottom: 1px solid var(--border);
    font-weight: 600;
    font-size: 13px;
  }
  .cnt {
    font-size: 11px;
    color: var(--text-dim);
    background: var(--surface-inset);
    border: 1px solid var(--border);
    border-radius: var(--radius-pill);
    padding: 1px 8px;
    margin-left: auto;
  }
  .col-body {
    padding: 10px;
    min-height: 120px;
    /* scroll viewport for the virtualizer */
    overflow-y: auto;
    max-height: calc(100vh - 200px);
  }
  /* sizer holds the full virtual height; cards are absolutely placed within */
  .col-sizer {
    position: relative;
    width: 100%;
  }
  .card-wrap {
    position: absolute;
    top: 0;
    left: 0;
    width: 100%;
    /* leave room for the 10px gap that the old flex column provided */
    padding-bottom: 10px;
  }
  .kbd-move {
    position: absolute;
    top: 8px;
    right: 8px;
    display: none;
    gap: 2px;
  }
  .card-wrap:hover .kbd-move,
  .card-wrap:focus-within .kbd-move {
    display: flex;
  }
  .kbd-move .btn {
    padding: 1px 6px;
  }
  .empty {
    text-align: center;
    padding: 24px 0;
    font-size: 12px;
  }
  @media (max-width: 900px) {
    .board {
      grid-template-columns: 1fr;
    }
  }
</style>
