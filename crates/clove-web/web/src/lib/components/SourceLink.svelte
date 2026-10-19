<script lang="ts">
  import { store } from '$lib/store.svelte';
  import { tooltip } from '$lib/tooltip';

  // The AGPL-3.0 section 13 source offer: a link to the source of the exact
  // clove build serving this page (`/meta` → `source_code`). Hidden until the
  // metadata loads, and against an older server that does not send it.
  const source = $derived(store.meta?.source_code);
</script>

{#if source}
  <a
    class="source-link"
    href={source.url}
    target="_blank"
    rel="noopener noreferrer"
    aria-label="Source code"
    use:tooltip={`Source code: clove ${source.version} (${source.license})`}
  >
    <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" aria-hidden="true"
      ><path d="m16 18 6-6-6-6M8 6l-6 6 6 6" /></svg
    >
  </a>
{/if}

<style>
  .source-link {
    display: flex;
    align-items: center;
    color: var(--text-muted);
  }
  .source-link:hover {
    color: var(--text);
  }
</style>
