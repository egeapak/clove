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
    // esbuild minifier (Vite default): fastest, with output within a few percent
    // of terser/swc — the best speed/size trade for this project.
    minify: 'esbuild'
  }
});
