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

  it('links a clove id in prose', async () => {
    const out = await renderMarkdown('tracking #proj-7af3q2k9 today');
    expect(out).toContain('<a href="/items/proj-7AF3Q2K9">#proj-7af3q2k9</a>');
  });
});
