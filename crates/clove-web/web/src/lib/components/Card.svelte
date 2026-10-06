<script lang="ts">
  import type { Item } from '$lib/types';
  import StatusGlyph from './StatusGlyph.svelte';
  import PriorityGlyph from './PriorityGlyph.svelte';
  import TypeIcon from './TypeIcon.svelte';
  import ShortId from './ShortId.svelte';
  import LabelChip from './LabelChip.svelte';
  import Avatar from './Avatar.svelte';
  import BlockedBadge from './BlockedBadge.svelte';
  import ExternalRef from './ExternalRef.svelte';
  import RelatedItem from './RelatedItem.svelte';
  import { tooltip } from '$lib/tooltip';

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
    <ShortId id={item.id} title={item.title} />
    <TypeIcon type={item.type} />
    <PriorityGlyph priority={item.priority} />
  </div>
  <div class="card-title" use:tooltip={{ content: item.title, whenTruncated: true }}>{item.title}</div>
  <div class="card-foot">
    {#each item.labels.slice(0, 2) as l (l)}
      <LabelChip label={l} />
    {/each}
    <BlockedBadge blockedBy={item.blocked_by} />
    {#if item.deps.length && !item.blocked_by.length}
      <span class="deps">
        <span class="dim" aria-hidden="true">→</span><span class="sr-only">depends on</span>
        {#each item.deps as d (d)}<RelatedItem id={d} link={false} compact />{/each}
      </span>
    {/if}
    <span class="right"><ExternalRef {item} compact link={false} /><Avatar name={item.assignee} /></span>
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
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .card-title {
    display: -webkit-box;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 3;
    line-clamp: 3;
    overflow: hidden;
    overflow-wrap: anywhere;
  }
  .deps {
    display: inline-flex;
    align-items: baseline;
    gap: 8px;
    min-width: 0;
    max-width: 100%;
    font-size: 11px;
  }
  .deps :global(.rel) {
    max-width: 150px;
  }
</style>
