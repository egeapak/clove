import { defineConfig } from 'vitest/config';

// Pure-string markdown tests — no DOM environment needed.
export default defineConfig({
  test: {
    include: ['src/**/*.test.ts'],
    environment: 'node'
  }
});
