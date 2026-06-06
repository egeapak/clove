// Lazy CommonMark renderer with task-list support and a minimal sanitizer.
// markdown-it is imported dynamically so it only loads on the detail route.

let mdPromise: Promise<(src: string) => string> | null = null;

// Minimal allowlist sanitizer: strip <script>/<style>, event handlers, and
// javascript: URLs from rendered HTML. markdown-it already escapes raw HTML
// when html:false, so this is defense-in-depth.
function sanitize(html: string): string {
  return html
    .replace(/<\s*(script|style|iframe|object|embed)[^>]*>[\s\S]*?<\s*\/\s*\1\s*>/gi, '')
    .replace(/\son\w+\s*=\s*"[^"]*"/gi, '')
    .replace(/\son\w+\s*=\s*'[^']*'/gi, '')
    .replace(/(href|src)\s*=\s*"(\s*javascript:[^"]*)"/gi, '$1="#"')
    .replace(/(href|src)\s*=\s*'(\s*javascript:[^']*)'/gi, "$1='#'");
}

// Render GitHub-style `[ ]`/`[x]` task list items as disabled checkboxes.
function renderTaskLists(html: string): string {
  return html
    .replace(/<li>\s*\[ \]\s*/g, '<li class="task"><input type="checkbox" disabled> ')
    .replace(/<li>\s*\[[xX]\]\s*/g, '<li class="task done"><input type="checkbox" checked disabled> ');
}

// Autolink clove ids: `#prefix-XXXXXXXX` (8 Crockford base32) and the short
// `#XXXXXXXX` form. Done on the sanitized HTML, but only OUTSIDE <code>/<pre>
// and outside existing tags/attributes, so we don't mangle code or hrefs.
const CLOVE_ID = /#((?:[a-z][a-z0-9]*-)?[0-9A-HJKMNP-TV-Z]{8})\b/g;
function autolinkIds(html: string): string {
  // Split on code/pre spans and tags; only rewrite plain text segments.
  const parts = html.split(/(<pre[\s\S]*?<\/pre>|<code[\s\S]*?<\/code>|<[^>]+>)/g);
  return parts
    .map((seg, i) => {
      // Odd indices are the captured delimiters (tags / code blocks) — leave them.
      if (i % 2 === 1) return seg;
      return seg.replace(CLOVE_ID, (_m, id: string) => `<a href="/items/${id}">#${id}</a>`);
    })
    .join('');
}

async function getMd() {
  const { default: MarkdownIt } = await import('markdown-it');
  const md = new MarkdownIt({
    html: false,
    linkify: true,
    breaks: false,
    typographer: true
  });
  return (src: string) => autolinkIds(renderTaskLists(sanitize(md.render(src))));
}

export async function renderMarkdown(src: string): Promise<string> {
  if (!mdPromise) mdPromise = getMd();
  const render = await mdPromise;
  return render(src);
}
