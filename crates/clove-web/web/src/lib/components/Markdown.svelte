<script lang="ts">
  import { renderMarkdown } from '$lib/markdown';
  let { source }: { source: string } = $props();
  let html = $state('');

  $effect(() => {
    const src = source ?? '';
    // Cancellation token: a stale async render must not clobber a newer one.
    let cancelled = false;
    renderMarkdown(src)
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
  .md :global(h1),
  .md :global(h2),
  .md :global(h3) {
    font-size: 14px;
    margin: 18px 0 8px;
    font-weight: 600;
    color: var(--text);
  }
  .md :global(h1) {
    font-size: 17px;
  }
  .md :global(p) {
    color: var(--text-muted);
    margin: 8px 0;
  }
  .md :global(ul),
  .md :global(ol) {
    margin: 8px 0;
    padding-left: 20px;
    color: var(--text-muted);
  }
  .md :global(li) {
    margin: 3px 0;
  }
  /* Native GFM task lists render as `<li><input type=checkbox disabled> text`.
     GFM marks the parent list with `class="contains-task-list"`; style the
     items that hold a checkbox so they sit flush and lose the bullet. */
  .md :global(li:has(> input[type='checkbox'])) {
    list-style: none;
    margin-left: -20px;
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  .md :global(li > input[type='checkbox']) {
    margin: 0;
    flex: none;
    accent-color: var(--accent);
  }
  /* GFM strikethrough renders as `<del>`. */
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
    font-size: 12px;
    background: var(--surface-inset);
    padding: 1px 4px;
    border-radius: 3px;
  }
  .md :global(pre code) {
    background: none;
    padding: 0;
  }
  .md :global(a) {
    color: var(--accent);
  }
  .md :global(blockquote) {
    border-left: 3px solid var(--border-strong);
    margin: 8px 0;
    padding-left: 12px;
    color: var(--text-dim);
  }
</style>
