<script lang="ts">
  import { base } from '$app/paths';
  import { shortId } from '$lib/glyphs';
  import { related, ensureRelated } from '$lib/related.svelte';
  import { tooltip } from '$lib/tooltip';
  import StatusGlyph from './StatusGlyph.svelte';

  let {
    id,
    link = true,
    compact = false
  }: {
    id: string;
    /** False inside another link (a board card), where a nested <a> is invalid. */
    link?: boolean;
    /** Title only, for tight spots; the id moves to the tooltip. */
    compact?: boolean;
  } = $props();

  $effect(() => ensureRelated(id));

  const info = $derived(related(id));
  const title = $derived(info ? info.title : info === null ? 'missing item' : '');
  const label = $derived(`${shortId(id)}${title ? ' ' + title : ''}`);
  const tip = $derived(title ? `${id} · ${title}` : id);
</script>

{#snippet body()}
  {#if info}<StatusGlyph status={info.status} />{/if}
  {#if !compact || !title}<span class="rid mono">{shortId(id)}</span>{/if}
  {#if title}<span class="rtitle">{title}</span>{/if}
{/snippet}

{#if link}
  <a class="rel" class:missing={info === null} class:compact href="{base}/items/{id}" aria-label={label} use:tooltip={tip}>
    {@render body()}
  </a>
{:else}
  <span class="rel" class:missing={info === null} class:compact role="img" aria-label={label} use:tooltip={tip}>
    {@render body()}
  </span>
{/if}

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
  a.rel:hover {
    color: var(--text);
    text-decoration: none;
  }
  a.rel:hover .rtitle {
    text-decoration: underline;
  }
  .rel.compact {
    gap: 4px;
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
