<script lang="ts">
  import type { DepTreeNode } from '$lib/types';
  import Self from './DepTree.svelte';
  import { statusGlyph, statusColorVar, shortId } from '$lib/glyphs';

  let {
    node,
    prefix = '',
    isLast = true,
    root = true,
    currentId
  }: { node: DepTreeNode; prefix?: string; isLast?: boolean; root?: boolean; currentId?: string } = $props();

  const branch = $derived(root ? '' : isLast ? '└─ ' : '├─ ');
  const childPrefix = $derived(root ? '' : prefix + (isLast ? '   ' : '│  '));
</script>

<div class="line">
  <span class="lvl">{prefix}{branch}</span>
  <span class="glyph" style="color:{statusColorVar(node.status)}" aria-label={node.status}>{statusGlyph(node.status)}</span>
  <a class="id" href="../items/{node.id}">{shortId(node.id)}</a>
  <span class="ttl" class:cur={node.id === currentId}>{node.title}{#if node.id === currentId} <b>(this)</b>{/if}</span>
  {#if node.ready && node.status !== 'closed'}<span class="ready">ready</span>{/if}
  {#if node.cycle_ref}<span class="cycle">(cycle)</span>{/if}
</div>
{#if !node.cycle_ref}
  {#each node.children as child, i (child.id)}
    <Self node={child} prefix={childPrefix} isLast={i === node.children.length - 1} root={false} {currentId} />
  {/each}
{/if}

<style>
  .line {
    display: flex;
    align-items: baseline;
    gap: 6px;
    white-space: pre;
  }
  .lvl {
    color: var(--text-dim);
  }
  .id {
    color: var(--accent);
    font-weight: 600;
  }
  .ttl {
    color: var(--text-muted);
    white-space: normal;
  }
  .ttl.cur {
    color: var(--text);
  }
  .ready {
    color: var(--green);
    font-size: 10px;
  }
  .cycle {
    color: var(--red);
    font-size: 10px;
  }
</style>
