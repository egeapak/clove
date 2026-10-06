<script lang="ts">
  import { base } from '$app/paths';
  import { shortId } from '$lib/glyphs';
  import { related, ensureRelated } from '$lib/related.svelte';
  import { tooltip } from '$lib/tooltip';
  import StatusGlyph from './StatusGlyph.svelte';

  let { id }: { id: string } = $props();

  $effect(() => ensureRelated(id));

  const info = $derived(related(id));
  const title = $derived(info ? info.title : info === null ? 'missing item' : '');
</script>

<a
  class="rel"
  class:missing={info === null}
  href="{base}/items/{id}"
  aria-label="{shortId(id)}{title ? ' ' + title : ''}"
  use:tooltip={title ? `${id} · ${title}` : id}
>
  {#if info}<StatusGlyph status={info.status} />{/if}
  <span class="rid mono">{shortId(id)}</span>
  {#if title}<span class="rtitle">{title}</span>{/if}
</a>

<style>
  .rel {
    display: inline-flex;
    align-items: baseline;
    gap: 6px;
    min-width: 0;
    max-width: 100%;
    color: var(--text-muted);
    text-decoration: none;
  }
  .rel:hover {
    color: var(--text);
    text-decoration: none;
  }
  .rel:hover .rtitle {
    text-decoration: underline;
  }
  .rid {
    flex: none;
    font-size: 11px;
    color: var(--accent);
    opacity: 0.8;
  }
  .rtitle {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .missing .rtitle {
    font-style: italic;
    color: var(--text-dim);
  }
</style>
