import { describe, it, expect } from 'vitest';
import { renderMarkdown } from './markdown';

describe('renderMarkdown (micromark + GFM + clove-id)', () => {
  it('escapes raw <script> (no live tag)', async () => {
    const out = await renderMarkdown('<script>alert(1)</script>');
    expect(out).not.toContain('<script>');
    expect(out).toContain('&lt;script&gt;');
  });

  it('escapes raw <img onerror> (no live tag)', async () => {
    const out = await renderMarkdown('<img src=x onerror=alert(1)>');
    expect(out).not.toMatch(/<img[^>]*onerror/i);
    expect(out).toContain('&lt;img');
  });

  it('neutralizes javascript: link hrefs', async () => {
    const out = await renderMarkdown('[x](javascript:alert(1))');
    expect(out).not.toContain('javascript:');
  });

  it('renders ~~strike~~ as <del>', async () => {
    const out = await renderMarkdown('~~s~~');
    expect(out).toContain('<del>s</del>');
  });

  it('renders a task list with disabled checkboxes', async () => {
    const out = await renderMarkdown('- [ ] todo\n- [x] done');
    expect(out).toContain('<input type="checkbox" disabled');
    expect(out).toContain('checked');
  });

  it('renders a GFM table as <table>', async () => {
    const out = await renderMarkdown('| a | b |\n|---|---|\n| 1 | 2 |');
    expect(out).toContain('<table>');
  });

  // The shapes real item bodies take (this repo's own tracker).
  it('starts a numbered list straight after a paragraph line', async () => {
    const out = await renderMarkdown('User review (2026-10-06):\n1. List: fill the width.\n2. Detail: full height.');
    expect(out).toContain('<p>User review (2026-10-06):</p>');
    expect(out).toContain('<ol>\n<li>List: fill the width.</li>\n<li>Detail: full height.</li>\n</ol>');
  });

  it('keeps inline code, placeholders and $VARs literal', async () => {
    const out = await renderMarkdown('Open `/p/<slug>/items/<id>`; set $CLOVE_AUTHOR or `$EDITOR`.');
    expect(out).toContain('<code>/p/&lt;slug&gt;/items/&lt;id&gt;</code>');
    expect(out).toContain('set $CLOVE_AUTHOR or <code>$EDITOR</code>');
  });

  it('escapes a bare <id> placeholder in prose rather than dropping it', async () => {
    const out = await renderMarkdown("clove comment <id> 'hi'");
    expect(out).toContain('clove comment &lt;id&gt;');
  });

  it('does not strike through paths that hold single tildes', async () => {
    const out = await renderMarkdown('installed in ~/.cargo/bin (on PATH) would shadow ~/.cargo/bin/x');
    expect(out).not.toContain('<del>');
  });

  it('leaves intraword underscores alone', async () => {
    const out = await renderMarkdown('plugin_install::the_suite_sees_no_plugin');
    expect(out).not.toContain('<em>');
  });

  it('keeps paragraphs apart', async () => {
    const out = await renderMarkdown('First paragraph.\n\nSecond paragraph.');
    expect(out).toBe('<p>First paragraph.</p>\n<p>Second paragraph.</p>');
  });

  it('autolinks a bare URL', async () => {
    const out = await renderMarkdown('see https://github.com/egeapak/clove/issues/67');
    expect(out).toContain('<a href="https://github.com/egeapak/clove/issues/67">');
  });

  it('keeps inline code inside a task item', async () => {
    const out = await renderMarkdown('- [ ] `clove-web` packages the real SPA');
    expect(out).toMatch(/<li><input type="checkbox" disabled="" \/> <code>clove-web<\/code> packages the real SPA<\/li>/);
  });

  it('renders table column alignment', async () => {
    const out = await renderMarkdown('| a | b |\n|:--|--:|\n| 1 | 2 |');
    expect(out).toContain('<th align="left">a</th>');
    expect(out).toContain('<td align="right">2</td>');
  });

  it('renders a nested list and a fenced block', async () => {
    const out = await renderMarkdown('1. step\n   - nested\n\n```sh\nclove sync github o/r\n```');
    expect(out).toMatch(/<li>step\n<ul>\n<li>nested<\/li>/);
    expect(out).toContain('<pre><code class="language-sh">clove sync github o/r\n</code></pre>');
  });

  it('links a clove id in prose', async () => {
    const out = await renderMarkdown('tracking #proj-7af3q2k9 today');
    expect(out).toContain('<a href="/items/proj-7AF3Q2K9">#proj-7af3q2k9</a>');
  });
});
