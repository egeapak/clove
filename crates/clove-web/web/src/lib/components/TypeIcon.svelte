<script lang="ts">
  import type { ItemType } from '$lib/types';
  import { typeIcon, typeColorVar } from '$lib/glyphs';
  let { type }: { type: ItemType } = $props();
</script>

<span class="ty mono" style="color:{typeColorVar(type)};border-color:{typeColorVar(type)}" role="img" aria-label="{type}">
  {typeIcon(type)}
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
</style>
