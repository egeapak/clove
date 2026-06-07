# clove web UI (frontend)

The SvelteKit single-page app served by `clove serve` and the `cloved` daemon.
It is **embedded into the Rust binary** at build time — you don't run this
separately in production.

## Stack
- SvelteKit (Svelte 5 runes) + Vite + TypeScript, `@sveltejs/adapter-static` (SPA).
- esbuild minifier; markdown via `micromark` + `micromark-extension-gfm` + a custom
  id-autolink extension (`src/lib/micromark-clove-id.ts`); list/board virtualized
  with `@tanstack/virtual-core`. No other heavy runtime deps.

## How it's built & embedded
`crates/clove-web/build.rs` runs `npm run build` (when `npm` is available and a
source is newer than the last build), writes the static output to `../dist/`,
gzips it into `../dist-gz/`, and the crate embeds **only** `dist-gz/` via
`rust-embed`. At server start the assets are decompressed once into memory and
served from there (gzip or identity by `Accept-Encoding`).

- A **Node-free `cargo build` still works** — `build.rs` embeds a placeholder page
  when `npm` is missing. Set `CLOVE_SKIP_WEB_BUILD=1` to skip the npm build.
- `dist/` and `dist-gz/` are **git-ignored and generated** — never commit them.

## Develop
```sh
npm install
npm run dev      # Vite dev server; proxies /api to http://127.0.0.1:7373.
                 # With no backend running it falls back to mock fixtures.
npm run check    # svelte-check (type-check) — must be 0/0
npm run test     # vitest (markdown + id-autolink extension tests)
npm run build    # production build → ../dist
```

To see it against the real backend: run `clove serve` (or `cloved`) in a repo,
then `npm run dev` and open the dev URL (the `/api` proxy points at port 7373).

## Layout
- `src/routes/` — `board/`, `list/`, `items/[id]/`, `timeline/`, plus the app shell (`+layout.svelte`).
- `src/lib/` — `api.ts` (typed client + mock fallback), `store.svelte.ts` (normalized
  cache + WebSocket reconciliation), `filter.ts` / `query.ts` (shared filter + URL
  state), `markdown.ts` + `micromark-clove-id.ts`, `theme.svelte.ts` (4 runtime
  themes), `virtual.svelte.ts` (virtualizer wrapper), and `components/`.
