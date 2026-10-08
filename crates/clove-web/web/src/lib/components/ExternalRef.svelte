<script lang="ts">
  import type { Item } from '$lib/types';
  import { store } from '$lib/store.svelte';
  import { externalRef, hasSyncTarget, syncTargetLabel } from '$lib/sync';
  import { tooltip } from '$lib/tooltip';
  import GitHubMark from './GitHubMark.svelte';

  let {
    item,
    compact = false,
    link = true
  }: {
    item: Pick<Item, 'external_ref' | 'source_system'>;
    compact?: boolean;
    /** False inside another link (a board card), where a nested <a> is invalid. */
    link?: boolean;
  } = $props();

  const ref = $derived(externalRef(item, store.meta));
  const unsynced = $derived(!ref && hasSyncTarget(store.meta));
</script>

{#if ref}
  {#if ref.href && link}
    <!-- stopPropagation: a list row navigates on click itself. -->
    <a
      class="xref mono"
      href={ref.href}
      target="_blank"
      rel="noopener noreferrer"
      aria-label="{ref.label} (opens in a new tab)"
      use:tooltip={`Open ${ref.label}`}
      onclick={(e) => e.stopPropagation()}
    >
      {#if ref.github}<GitHubMark />{/if}
      {compact ? ref.short : ref.label}
      <svg width="9" height="9" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" aria-hidden="true"
        ><path d="M7 17 17 7M9 7h8v8" /></svg
      >
    </a>
  {:else}
    <span class="xref mono plain" use:tooltip={ref.label}
      >{#if ref.github}<GitHubMark />{/if}{compact ? ref.short : ref.label}</span
    >
  {/if}
{:else if unsynced}
  <span class="unsynced mono" use:tooltip={`Not synced to ${syncTargetLabel(store.meta)}`}>
    {compact ? 'unsynced' : 'not synced'}
  </span>
{/if}

<style>
  .xref,
  .unsynced {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    font-size: 11px;
    line-height: 1.3;
    border-radius: var(--radius-sm);
    padding: 1px 6px;
    white-space: nowrap;
    flex: none;
  }
  .xref {
    color: var(--text-muted);
    border: 1px solid var(--border-strong);
    background: var(--surface-inset);
    text-decoration: none;
  }
  a.xref:hover {
    color: var(--accent);
    border-color: var(--accent);
    text-decoration: none;
  }
  .unsynced {
    color: var(--text-dim);
    border: 1px dashed var(--border-strong);
  }
</style>
