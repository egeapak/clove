// Manual screenshot tool for the web UI (artifacts, not committed — like the
// TUI's generate_screenshots). Requires playwright + a chromium build:
//   npm i -D playwright && npx playwright install chromium
// Drive it against a Vite dev server proxying to a running `clove serve`:
//   (term 1) cd /tmp/demo && clove serve --port 7373
//   (term 2) npm run dev -- --port 5179
//   (term 3) node scripts/screenshot.mjs http://localhost:5179 demo-SSA54SHG
// Output: docs/screenshots/web-*.png
import { chromium } from 'playwright';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import fs from 'node:fs';

const base = process.argv[2] ?? 'http://localhost:5179';
const id = process.argv[3] ?? '42';
const here = path.dirname(fileURLToPath(import.meta.url));
const outDir = path.resolve(here, '../../../../docs/screenshots');
fs.mkdirSync(outDir, { recursive: true });

const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1280, height: 880 }, deviceScaleFactor: 2 });

async function shot(name) {
  await page.waitForTimeout(400);
  await page.screenshot({ path: path.join(outDir, `${name}.png`) });
  console.log('wrote', name);
}

// 1. Detail page — shows the new "Edit" affordance.
await page.goto(`${base}/items/${id}`, { waitUntil: 'networkidle' });
await page.getByRole('link', { name: 'Edit' }).waitFor({ timeout: 15000 });
await shot('web-detail-edit-link');

// 2. Edit page — the shared ItemForm in edit mode (prefilled) + relationships.
await page.goto(`${base}/items/${id}/edit`, { waitUntil: 'networkidle' });
await page.getByRole('button', { name: 'Save changes' }).waitFor({ timeout: 15000 });
await shot('web-edit-page');

// 3. Edit page, body Preview tab (renders the prefilled Markdown body live).
await page.getByRole('tab', { name: 'Preview' }).click();
await page.waitForTimeout(500);
await shot('web-edit-preview');

// 4. New-item modal (shared ItemForm in create mode), opened with the `c` shortcut.
await page.goto(`${base}/list`, { waitUntil: 'networkidle' });
await page.waitForTimeout(600);
await page.keyboard.press('c');
await page.getByRole('dialog', { name: 'Create item' }).waitFor({ timeout: 15000 });
await shot('web-new-modal');

await browser.close();
