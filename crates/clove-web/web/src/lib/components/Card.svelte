<script lang="ts">
  import type { Item } from '$lib/types';
  import StatusGlyph from './StatusGlyph.svelte';
  import PriorityGlyph from './PriorityGlyph.svelte';
  import TypeIcon from './TypeIcon.svelte';
  import ShortId from './ShortId.svelte';
  import LabelChip from './LabelChip.svelte';
  import Avatar from './Avatar.svelte';
  import BlockedBadge from './BlockedBadge.svelte';
  import { shortId } from '$lib/glyphs';

  let { item, ondragstart }: { item: Item; ondragstart?: (e: DragEvent) => void } = $props();
</script>

<a
  class="card"
  class:blk={item.blocked_by.length > 0}
  href="./items/{item.id}"
  draggable="true"
  ondragstart={ondragstart}
>
  <div class="card-top">
    <StatusGlyph status={item.status} />
    <ShortId id={item.id} />
    <TypeIcon type={item.type} />
    <PriorityGlyph priority={item.priority} />
  </div>
  <div class="card-title">{item.title}</div>
  <div class="card-foot">
    {#each item.labels.slice(0, 2) as l (l)}
      <LabelChip label={l} />
    {/each}
    {#if item.type === 'epic'}
      <span class="dim mono note">epic</span>
    {/if}
    <BlockedBadge blockedBy={item.blocked_by} />
    {#if item.deps.length && !item.blocked_by.length}
      <span class="dim mono note">→ deps {item.deps.map(shortId).join(', ')}</span>
    {/if}
    <span class="right"><Avatar name={item.assignee} /></span>
  </div>
</a>

<style>
  .card {
    background: var(--surface-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    padding: 10px 11px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    text-decoration: none;
    color: inherit;
    cursor: grab;
    box-shadow: var(--shadow-card);
  }
  .card:hover {
    border-color: var(--border-strong);
    background: var(--surface-hover);
    text-decoration: none;
  }
  .card:active {
    cursor: grabbing;
  }
  .card.blk {
    border-left: 2px solid var(--red);
  }
  .card-top {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .card-title {
    font-size: 13px;
    font-weight: 500;
    line-height: 1.35;
    color: var(--text);
  }
  .card-foot {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
  }
  .card-foot .right {
    margin-left: auto;
  }
  .note {
    font-size: 10px;
  }
</style>
