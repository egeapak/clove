import { describe, it, expect } from 'vitest';
import { micromark } from 'micromark';
import { gfm, gfmHtml } from 'micromark-extension-gfm';
import { cloveId, cloveIdHtml } from './micromark-clove-id';

// Render with the full real stack (GFM + clove-id) so the tests also prove the
// extension cooperates with code spans, links and autolinks.
function render(src: string): string {
  return micromark(src, {
    extensions: [gfm(), cloveId()],
    htmlExtensions: [gfmHtml(), cloveIdHtml()]
  });
}

describe('cloveId micromark extension', () => {
  it('links the prefixed form #proj-7af3q2k9', () => {
    expect(render('#proj-7af3q2k9')).toContain(
      '<a href="/items/proj-7af3q2k9">#proj-7af3q2k9</a>'
    );
  });

  it('links the bare 8-char base32 form #7AF3Q2K9', () => {
    expect(render('#7AF3Q2K9')).toContain(
      '<a href="/items/7AF3Q2K9">#7AF3Q2K9</a>'
    );
  });

  it('does NOT link inside an inline code span', () => {
    const out = render('`#proj-7af3q2k9`');
    expect(out).toContain('<code>');
    expect(out).not.toContain('<a href="/items/');
  });

  it('does NOT link inside a fenced code block', () => {
    const out = render('```\n#proj-7af3q2k9\n```');
    expect(out).toContain('<pre>');
    expect(out).not.toContain('<a href="/items/');
  });

  it('does NOT link when too short (7 chars)', () => {
    const out = render('#7AF3Q2K');
    expect(out).not.toContain('<a href="/items/');
  });

  it('does NOT link when the body contains excluded letters I/L/O/U', () => {
    for (const bad of ['#7AF3Q2KI', '#7AF3Q2KL', '#7AF3Q2KO', '#7AF3Q2KU']) {
      expect(render(bad)).not.toContain('<a href="/items/');
    }
  });

  it('does NOT link mid-word (x#7AF3Q2K9)', () => {
    expect(render('x#7AF3Q2K9')).not.toContain('<a href="/items/');
  });

  it('does NOT double-link inside an existing markdown link', () => {
    const out = render('[see this](/x/#7AF3Q2K9)');
    expect(out).toContain('<a href="/x/#7AF3Q2K9">see this</a>');
    expect(out).not.toContain('/items/');
  });

  it('does NOT link inside an autolinked URL', () => {
    const out = render('see http://example.com/#7AF3Q2K9 now');
    // the literal URL is one autolink; the id fragment is not separately linked
    expect(out).not.toContain('/items/');
    expect(out).toContain('href="http://example.com/#7AF3Q2K9"');
  });

  it('links an id wrapped in parentheses, leaving the punctuation', () => {
    const out = render('(#proj-7af3q2k9)');
    expect(out).toContain(
      '(<a href="/items/proj-7af3q2k9">#proj-7af3q2k9</a>)'
    );
  });

  it('links an id before a sentence-final period', () => {
    const out = render('see #7AF3Q2K9.');
    expect(out).toContain(
      'see <a href="/items/7AF3Q2K9">#7AF3Q2K9</a>.'
    );
  });

  it('does NOT link when followed immediately by more id chars (9 chars)', () => {
    expect(render('#7AF3Q2K9X')).not.toContain('<a href="/items/');
  });
});
