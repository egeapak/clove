<script lang="ts">
  import { priorityGlyph, priorityLabel, priorityColorVar } from '$lib/glyphs';
  let { priority, label = false }: { priority: number; label?: boolean } = $props();
</script>

<span
  class="pr mono p{priority}"
  style="color:{priorityColorVar(priority)}"
  role="img"
  aria-label={priorityLabel(priority)}
  title={priorityLabel(priority)}
>
  {priorityGlyph(priority)}{#if label}&nbsp;p{priority}{/if}
</span>

<style>
  .pr {
    font-weight: 700;
  }
  /* p2 and p3 share the `•` glyph and differ by hue; add a subtle shape/size
     differentiator so they're distinguishable beyond colour alone. */
  .pr.p3 {
    font-weight: 400;
    font-size: 0.85em;
    opacity: 0.85;
  }
</style>
