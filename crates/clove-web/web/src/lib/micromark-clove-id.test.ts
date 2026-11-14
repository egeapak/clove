import { describe, it, expect } from 'vitest';
import { micromark } from 'micromark';
import { gfm, gfmHtml } from 'micromark-extension-gfm';
import { cloveId, cloveIdHtml } from './micromark-clove-id';

// Render with the full real stack (GFM + clove-id) so the tests also prove the
// extension cooperates with code spans, links and autolinks.
function render(src: string, opts: Parameters<typeof cloveIdHtml>[0] = { idPrefix: 'proj' }): string {
  return micromark(src, {
    extensions: [gfm(), cloveId()],
    htmlExtensions: [gfmHtml(), cloveIdHtml(opts)]
  });
}

describe('cloveId micromark extension', () => {
  it('links the prefixed form #proj-7af3q2k9, canonicalizing the suffix case', () => {
    // The API id grammar requires an UPPERCASE suffix; the visible text keeps
    // what the author wrote.
    expect(render('#proj-7af3q2k9')).toContain(
      '<a href="/items/proj-7AF3Q2K9">#proj-7af3q2k9</a>'
    );
  });

  it('links the bare 8-char base32 form #7AF3Q2K9 via the repo prefix', () => {
    expect(render('#7AF3Q2K9')).toContain(
      '<a href="/items/proj-7AF3Q2K9">#7AF3Q2K9</a>'
    );
  });

  it('renders a bare id as plain text when no repo prefix is known', () => {
    const out = render('#7AF3Q2K9', {});
    expect(out).not.toContain('<a href="/items/');
    expect(out).toContain('#7AF3Q2K9');
  });

  it('prepends the app base path to hrefs', () => {
    expect(render('#proj-7af3q2k9', { base: '/clove', idPrefix: 'proj' })).toContain(
      '<a href="/clove/items/proj-7AF3Q2K9">'
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
      '(<a href="/items/proj-7AF3Q2K9">#proj-7af3q2k9</a>)'
    );
  });

  it('links an id before a sentence-final period', () => {
    const out = render('see #7AF3Q2K9.');
    expect(out).toContain(
      'see <a href="/items/proj-7AF3Q2K9">#7AF3Q2K9</a>.'
    );
  });

  it('does NOT link when followed immediately by more id chars (9 chars)', () => {
    expect(render('#7AF3Q2K9X')).not.toContain('<a href="/items/');
  });

  it('does NOT nest a link inside the label of a markdown link', () => {
    const out = render('[fix for #proj-AS5Y8MM7](https://example.com) and #proj-AS5Y8MM7');
    expect(out).toContain('<a href="https://example.com">fix for #proj-AS5Y8MM7</a>');
    // The same id outside the link is still linked.
    expect(out).toContain('and <a href="/items/proj-AS5Y8MM7">#proj-AS5Y8MM7</a>');
    expect(out.match(/<a /g)).toHaveLength(2);
  });

  it('does NOT nest a link inside a reference-style link label', () => {
    const out = render('[see #7AF3Q2K9][ref]\n\n[ref]: https://example.com');
    expect(out).toContain('<a href="https://example.com">see #7AF3Q2K9</a>');
    expect(out).not.toContain('/items/');
  });

  it('still links inside brackets that are not a link', () => {
    expect(render('[see #7AF3Q2K9] for details')).toContain(
      '[see <a href="/items/proj-7AF3Q2K9">#7AF3Q2K9</a>] for details'
    );
  });

  it('still links inside emphasis', () => {
    expect(render('**#7AF3Q2K9**')).toContain('<strong><a href="/items/proj-7AF3Q2K9">#7AF3Q2K9</a></strong>');
  });

  it('does NOT link the fragment of a URL that is not autolinked', () => {
    expect(render('see example.com/#7AF3Q2K9 now')).not.toContain('/items/');
  });

  it('does NOT link a lowercase bare word such as #feedback', () => {
    for (const word of ['#feedback', '#database', '#7af3q2k9', '#7AF3q2K9', '#aBCDEFGH']) {
      const out = render(`tagged ${word} here`);
      expect(out, word).not.toContain('/items/');
      expect(out, word).toContain(word);
    }
  });

  it('still links an all-digit or uppercase bare id', () => {
    expect(render('#12345678')).toContain('<a href="/items/proj-12345678">#12345678</a>');
    expect(render('#ZZZZ0000')).toContain('<a href="/items/proj-ZZZZ0000">#ZZZZ0000</a>');
  });

  it('still links a prefixed id written in upper or mixed case body', () => {
    expect(render('#proj-7AF3q2k9')).toContain('<a href="/items/proj-7AF3Q2K9">#proj-7AF3q2k9</a>');
  });
});
