<script lang="ts">
  import type { ItemType } from '$lib/types';
  import { typeIcon, typeColorVar } from '$lib/glyphs';
  import { tooltip } from '$lib/tooltip';
  let { type, label = false }: { type: ItemType; label?: boolean } = $props();
</script>

{#if label}
  <span class="ty mono labelled" style="color:{typeColorVar(type)};border-color:{typeColorVar(type)}">{type}</span>
{:else}
  <span
    class="ty mono"
    style="color:{typeColorVar(type)};border-color:{typeColorVar(type)}"
    role="img"
    aria-label={type}
    use:tooltip={type}
  >
    {typeIcon(type)}
  </span>
{/if}

<style>
  .ty {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 18px;
    height: 18px;
    border-radius: var(--radius-sm);
    font-weight: 700;
    font-size: 11px;
    border: 1px solid;
    background: color-mix(in srgb, currentColor 12%, transparent);
    flex: 0 0 auto;
  }
  .ty.labelled {
    width: auto;
    height: auto;
    padding: 2px 8px;
    font-weight: 500;
  }
</style>
