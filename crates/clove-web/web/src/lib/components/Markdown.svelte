<script lang="ts">
  import { renderMarkdown } from '$lib/markdown';
  import { store } from '$lib/store.svelte';
  let { source }: { source: string } = $props();
  let html = $state('');

  $effect(() => {
    const src = source ?? '';
    // The repo prefix (from /meta) lets bare `#7AF3Q2K9` autolinks resolve.
    const idPrefix = store.meta?.id_prefix;
    // Cancellation token: a stale async render must not clobber a newer one.
    let cancelled = false;
    renderMarkdown(src, { idPrefix })
      .then((h) => {
        if (!cancelled) html = h;
      })
      .catch((e) => {
        if (!cancelled) {
          html = '';
          console.warn('[clove] markdown render failed', e);
        }
      });
    return () => {
      cancelled = true;
    };
  });
</script>

<!--
  SECURITY INVARIANT: this {@html} sink is only safe because `renderMarkdown`
  (markdown.ts) runs micromark with its default `allowDangerousHtml: false`, so
  raw HTML in a body is escaped (`<script>` → `&lt;script&gt;`) and dangerous
  link protocols are neutralized before it ever reaches here — no sanitizer is
  applied after. If markdown.ts ever enables raw HTML, this becomes an XSS sink.
  Pinned by markdown.test.ts ("escapes raw <script>" / "<img onerror>" / js: hrefs).
-->
<!-- eslint-disable-next-line svelte/no-at-html-tags -- micromark escapes raw HTML by default (see markdown.ts) -->
<div class="md">{@html html}</div>

<style>
  .md {
    color: var(--text-muted);
    line-height: 1.6;
    overflow-wrap: break-word;
  }
  .md > :global(:first-child) {
    margin-top: 0;
  }
  .md > :global(:last-child) {
    margin-bottom: 0;
  }
  .md :global(h1),
  .md :global(h2),
  .md :global(h3),
  .md :global(h4),
  .md :global(h5),
  .md :global(h6) {
    font-size: 14px;
    line-height: 1.35;
    margin: 18px 0 8px;
    font-weight: 600;
    color: var(--text);
  }
  .md :global(h1) {
    font-size: 17px;
  }
  .md :global(h2) {
    font-size: 15px;
  }
  .md :global(h5),
  .md :global(h6) {
    font-size: 13px;
    color: var(--text-muted);
  }
  .md :global(p) {
    margin: 8px 0;
  }
  .md :global(strong) {
    color: var(--text);
  }
  .md :global(ul),
  .md :global(ol) {
    margin: 8px 0;
    padding-left: 22px;
  }
  .md :global(li) {
    margin: 3px 0;
  }
  .md :global(li > ul),
  .md :global(li > ol) {
    margin: 2px 0;
  }
  .md :global(li > p) {
    margin: 4px 0;
  }
  .md :global(li::marker) {
    color: var(--text-dim);
  }
  /* GFM task items render as `<li><input type=checkbox disabled> text`. The
     checkbox takes the bullet's place and the text stays in inline flow (a
     flex `li` made every text run and code span a separate flex item). */
  .md :global(li:has(> input[type='checkbox'])) {
    list-style: none;
  }
  .md :global(li > input[type='checkbox']) {
    margin: 0 6px 0 -20px;
    vertical-align: -2px;
    accent-color: var(--accent);
  }
  .md :global(del) {
    color: var(--text-dim);
  }
  .md :global(pre) {
    background: var(--surface-inset);
    border: 1px solid var(--border);
    border-radius: var(--radius-md);
    padding: 12px 14px;
    overflow: auto;
    font-family: var(--font-mono);
    font-size: 12px;
    line-height: 1.5;
    margin: 10px 0;
  }
  .md :global(code) {
    font-family: var(--font-mono);
    font-size: 0.92em;
    color: var(--text);
    background: var(--surface-inset);
    border: 1px solid var(--border);
    padding: 0 4px;
    border-radius: 3px;
  }
  .md :global(pre code) {
    color: inherit;
    font-size: inherit;
    background: none;
    border: none;
    padding: 0;
  }
  .md :global(a) {
    color: var(--accent);
  }
  .md :global(a code) {
    color: inherit;
  }
  .md :global(blockquote) {
    border-left: 3px solid var(--border-strong);
    margin: 8px 0;
    padding-left: 12px;
    color: var(--text-dim);
  }
  .md :global(hr) {
    border: none;
    border-top: 1px solid var(--border);
    margin: 16px 0;
  }
  .md :global(img) {
    max-width: 100%;
  }
  /* A wide table scrolls inside the body instead of widening the page. */
  .md :global(table) {
    display: block;
    max-width: 100%;
    overflow-x: auto;
    border-collapse: collapse;
    margin: 10px 0;
    font-size: 12px;
  }
  .md :global(th),
  .md :global(td) {
    border: 1px solid var(--border);
    padding: 5px 10px;
    text-align: left;
  }
  .md :global(th[align='center']),
  .md :global(td[align='center']) {
    text-align: center;
  }
  .md :global(th[align='right']),
  .md :global(td[align='right']) {
    text-align: right;
  }
  .md :global(th) {
    background: var(--surface-inset);
    color: var(--text);
    font-weight: 600;
  }
  .md :global(.footnotes) {
    font-size: 12px;
    border-top: 1px solid var(--border);
    margin-top: 16px;
  }
</style>

