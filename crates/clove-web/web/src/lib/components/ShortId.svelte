<script lang="ts">
  import { shortId } from '$lib/glyphs';
  let { id }: { id: string } = $props();
</script>

<span class="id mono">{shortId(id)}</span>

<style>
  .id {
    font-size: 12px;
    color: var(--accent);
    font-weight: 600;
  }
</style>
