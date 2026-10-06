<script lang="ts">
  import type { ItemType } from '$lib/types';
  import { typeIcon, typeColorVar } from '$lib/glyphs';
  let { type, label = false }: { type: ItemType; label?: boolean } = $props();
</script>

<span
  class="ty mono"
  class:labelled={label}
  style="color:{typeColorVar(type)};border-color:{typeColorVar(type)}"
  role="img"
  aria-label="{type}"
  title="{type}"
>
  {typeIcon(type)}{#if label}<span class="name">{type}</span>{/if}
</span>

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
    gap: 6px;
    padding: 2px 8px;
  }
  .name {
    font-weight: 500;
  }
</style>
