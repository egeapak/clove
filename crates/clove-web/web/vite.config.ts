import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

export default defineConfig({
  plugins: [sveltekit()],
  server: {
    proxy: {
      '/api': 'http://127.0.0.1:7373'
    }
  },
  build: {
    target: 'es2020',
    // terser (2 passes) over the default esbuild minifier — ~4% smaller output
    // (initial preload ~46K vs ~48K gz). Build-time only; no runtime cost.
    minify: 'terser',
    terserOptions: {
      compress: { passes: 2 },
      format: { comments: false }
    }
  }
});
