<script lang="ts">
  import { shortId } from '$lib/glyphs';
  import { tooltip } from '$lib/tooltip';
  let { id, title }: { id: string; title?: string } = $props();
</script>

<span class="id mono" use:tooltip={title ? `${id} · ${title}` : id}>{shortId(id)}</span>

<style>
  .id {
    font-size: 12px;
    color: var(--accent);
    font-weight: 600;
  }
</style>
