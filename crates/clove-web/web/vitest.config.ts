import { defineConfig } from 'vitest/config';
import { svelte } from '@sveltejs/vite-plugin-svelte';
import path from 'node:path';

// Two flavours of test run under one config:
//  - pure-string logic tests (markdown, micromark, itemForm) — `node` env;
//  - Svelte component tests (`*.svelte.test.ts`) — `jsdom` via a per-file
//    `@vitest-environment jsdom` docblock, rendered with @testing-library/svelte.
// The Svelte plugin compiles components; `$lib`/`$app` are aliased here (vitest
// only — the real build uses SvelteKit's own aliases in vite.config.ts).
export default defineConfig({
  plugins: [svelte({ hot: false })],
  resolve: {
    conditions: ['browser'],
    alias: {
      $lib: path.resolve('./src/lib'),
      $app: path.resolve('./src/test/app-stubs')
    }
  },
  test: {
    include: ['src/**/*.test.ts'],
    environment: 'node',
    setupFiles: ['src/test/setup.ts']
  }
});
