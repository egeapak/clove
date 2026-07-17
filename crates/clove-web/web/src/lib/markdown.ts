// Lazy GitHub-flavoured Markdown renderer built on micromark.
//
// micromark is safe by default: raw HTML is escaped and dangerous link
// protocols (`javascript:`/`data:`) are neutralized, so NO sanitizer is needed.
// GFM provides strikethrough (`<del>`), tables, literal-URL autolinking, and
// native task-list checkboxes, so there is no post-processing of the output.
// Clove-id autolinking (`#proj-7af3q2k9`, `#7AF3Q2K9`) is a real micromark
// syntax+HTML extension pair (see `micromark-clove-id.ts`) — not a regex over
// the rendered HTML.
//
// Everything is imported dynamically so the markdown machinery only loads on
// the detail route (its own lazy chunk).

import { base } from '$app/paths';

export interface RenderOptions {
  /** Repo id prefix (from `/meta`) for resolving bare `#7AF3Q2K9` autolinks. */
  idPrefix?: string;
}

type Renderer = (src: string, opts?: RenderOptions) => string;

let mdPromise: Promise<Renderer> | null = null;

async function getMd(): Promise<Renderer> {
  const [{ micromark }, { gfm, gfmHtml }, { cloveId, cloveIdHtml }] =
    await Promise.all([
      import('micromark'),
      import('micromark-extension-gfm'),
      import('./micromark-clove-id')
    ]);

  return (src, opts) =>
    micromark(src, {
      extensions: [gfm(), cloveId()],
      htmlExtensions: [gfmHtml(), cloveIdHtml({ base, idPrefix: opts?.idPrefix })]
    });
}

export async function renderMarkdown(src: string, opts?: RenderOptions): Promise<string> {
  if (!mdPromise) mdPromise = getMd();
  const render = await mdPromise;
  return render(src, opts);
}
