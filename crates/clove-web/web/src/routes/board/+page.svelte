<script lang="ts">
  import type { Item, Status } from '$lib/types';
  import { store } from '$lib/store.svelte';
  import { api } from '$lib/api';
  import { toasts } from '$lib/toast.svelte';
  import Card from '$lib/components/Card.svelte';
  import StatusGlyph from '$lib/components/StatusGlyph.svelte';

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

  // keyboard move-mode: focus a card, press m to cycle status
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

<div class="board">
  {#each columns as col (col.key)}
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
      <div class="col-body">
        {#each col.items as item (item.id)}
          <div role="listitem" class="card-wrap" tabindex="-1">
            <Card {item} ondragstart={(e) => onDragStart(e, item.id)} />
            <div class="kbd-move">
              <button class="btn sm" aria-label="move {item.id} left" onclick={() => move(item, -1)}>‹</button>
              <button class="btn sm" aria-label="move {item.id} right" onclick={() => move(item, 1)}>›</button>
            </div>
          </div>
        {/each}
        {#if col.items.length === 0}
          <div class="empty dim">No items</div>
        {/if}
      </div>
    </section>
  {/each}
</div>

<style>
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
    display: flex;
    flex-direction: column;
    gap: 10px;
    min-height: 120px;
  }
  .card-wrap {
    position: relative;
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
