import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig, type Plugin } from 'vite';
import fs from 'node:fs';
import path from 'node:path';

// The notices the bundled npm packages' licenses require to travel with the
// SPA. The file lands in dist/ next to index.html, so it is embedded in the
// clove binary with the rest of the SPA and served at /THIRD_PARTY_LICENSES.txt.
// Collected from the modules rollup actually bundled (not package.json), so
// devDependencies whose runtime ends up in the bundle (svelte, @sveltejs/kit)
// are listed and build-only tools are not.
function thirdPartyLicenses(): Plugin {
  let ssr = false;
  // Notices for code that is vendored into our own sources rather than imported.
  const vendored = [
    'GitHub Octicons `mark-github` (src/lib/components/GitHubMark.svelte)\n' +
      'License: MIT\nCopyright (c) GitHub Inc.'
  ];
  return {
    name: 'clove-third-party-licenses',
    apply: 'build',
    configResolved(config) {
      ssr = !!config.build.ssr;
    },
    generateBundle() {
      if (ssr) return;
      const roots = new Map<string, string>();
      for (const id of this.getModuleIds()) {
        const m = /^(.*[\\/]node_modules[\\/])((?:@[^\\/]+[\\/])?[^\\/]+)/.exec(id);
        if (m) roots.set(m[2].replace(/\\/g, '/'), m[1] + m[2]);
      }
      const sections = [...roots.keys()].sort().map((name) => {
        const dir = roots.get(name)!;
        const pkg = JSON.parse(fs.readFileSync(path.join(dir, 'package.json'), 'utf8'));
        const file = fs
          .readdirSync(dir)
          .find((f) => /^(licen[cs]e|copying)(\.(md|txt))?$/i.test(f));
        if (!file) {
          throw new Error(`third-party-licenses: ${name} ships no license file`);
        }
        const text = fs.readFileSync(path.join(dir, file), 'utf8').trim();
        return `${pkg.name} ${pkg.version}\nLicense: ${pkg.license}\n\n${text}`;
      });
      const rule = '\n\n' + '-'.repeat(78) + '\n\n';
      this.emitFile({
        type: 'asset',
        fileName: 'THIRD_PARTY_LICENSES.txt',
        source:
          'Third-party software bundled in the clove web UI.\n' +
          'clove itself is licensed under AGPL-3.0-only; see LICENSE in the source.' +
          rule +
          [...sections, ...vendored].join(rule) +
          '\n'
      });
    }
  };
}

export default defineConfig({
  plugins: [sveltekit(), thirdPartyLicenses()],
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
