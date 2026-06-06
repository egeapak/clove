<script lang="ts">
  import { priorityGlyph, priorityLabel, priorityColorVar } from '$lib/glyphs';
  let { priority, label = false }: { priority: number; label?: boolean } = $props();
</script>

<span class="pr mono" style="color:{priorityColorVar(priority)}" role="img" aria-label={priorityLabel(priority)}>
  {priorityGlyph(priority)}{#if label}&nbsp;p{priority}{/if}
</span>

<style>
  .pr {
    font-weight: 700;
  }
</style>
