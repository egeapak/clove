<script lang="ts">
  import type { Status } from '$lib/types';
  import { statusGlyph, statusLabel, statusColorVar } from '$lib/glyphs';
  let { status }: { status: Status } = $props();
</script>

<span class="st mono" style="color:{statusColorVar(status)}" role="img" aria-label={statusLabel(status)}>
  {statusGlyph(status)}
</span>

<style>
  .st {
    display: inline-block;
  }
</style>
