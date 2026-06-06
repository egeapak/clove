<script lang="ts">
  import { shortId } from '$lib/glyphs';
  let { blockedBy }: { blockedBy: string[] } = $props();
</script>

{#if blockedBy.length}
  <span class="blocked mono" role="status" aria-label="blocked by {blockedBy.map(shortId).join(', ')}">
    <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true">
      <rect x="5" y="11" width="14" height="9" rx="2" /><path d="M8 11V8a4 4 0 0 1 8 0v3" />
    </svg>
    blocked by {blockedBy.map(shortId).join(', ')}
  </span>
{/if}

<style>
  .blocked {
    display: inline-flex;
    align-items: center;
    gap: 5px;
    font-size: 11px;
    color: var(--red);
    background: color-mix(in srgb, var(--red) 12%, transparent);
    border: 1px solid color-mix(in srgb, var(--red) 45%, transparent);
    border-radius: var(--radius-sm);
    padding: 2px 7px;
  }
</style>
