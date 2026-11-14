// @vitest-environment jsdom
import { describe, it, expect, afterEach, vi } from 'vitest';
import { render, cleanup } from '@testing-library/svelte';
import { stubApi, META, item } from './fake-api';
import { rowWindow } from '$lib/timeline';
import TimelinePage from '../routes/timeline/+page.svelte';

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const N = 500;
const ROW_H = 38;
const id = (n: number) => 'clov-' + String(n).padStart(8, '0');

function manyItems() {
  const day = 86400000;
  const t0 = Date.parse('2025-01-01T00:00:00Z');
  return Array.from({ length: N }, (_, n) =>
    item({
      id: id(n),
      title: 'Item ' + n,
      created: new Date(t0 + n * day).toISOString(),
      updated: new Date(t0 + n * day).toISOString(),
      // Every row depends on the row before it: N - 1 arrows in total.
      deps: n > 0 ? [id(n - 1)] : []
    })
  );
}

describe('rowWindow', () => {
  it('covers the visible rows plus the overscan', () => {
    expect(rowWindow(0, 380, ROW_H, 500, 5)).toEqual({ first: 0, last: 15 });
    expect(rowWindow(3800, 4180, ROW_H, 500, 5)).toEqual({ first: 95, last: 115 });
  });
  it('stays inside the row count', () => {
    expect(rowWindow(18000, 19000, ROW_H, 500, 5)).toEqual({ first: 468, last: 500 });
    expect(rowWindow(0, 380, ROW_H, 3, 5)).toEqual({ first: 0, last: 3 });
    expect(rowWindow(0, 380, ROW_H, 0, 5)).toEqual({ first: 0, last: 0 });
  });
  it('keeps a window when the rows are off screen', () => {
    // Above the viewport (bottom < top after clamping) and below it.
    expect(rowWindow(19000, 19000, ROW_H, 500, 5).last).toBe(500);
    expect(rowWindow(0, 0, ROW_H, 500, 5)).toEqual({ first: 0, last: 5 });
  });
});

describe('timeline row window', () => {
  it('renders only the rows near the viewport, and follows the scroll', async () => {
    stubApi((path) => {
      if (path === '/meta') return META;
      if (path === '/items') return manyItems();
      if (path === '/stats/history') return [];
      throw new Error('unexpected request ' + path);
    });
    const { container } = render(TimelinePage);
    await vi.waitFor(() => expect(container.querySelector('.bar')).not.toBeNull());

    const count = (sel: string) => container.querySelectorAll(sel).length;
    // jsdom's viewport is 768px high: about 20 rows, plus the overscan.
    expect(count('a.rowlabel')).toBeGreaterThan(0);
    expect(count('a.rowlabel')).toBeLessThan(60);
    expect(count('.track')).toBe(count('a.rowlabel'));
    expect(count('svg.arrows path:not(marker path)')).toBeLessThan(60);
    // The full height is still reserved, so the page scroll bar is right.
    const tracks = container.querySelector('.tl-tracks') as HTMLElement;
    expect(tracks.style.height).toBe(N * ROW_H + 'px');
    expect(container.querySelector(`a.rowlabel[href$="${id(300)}"]`)).toBeNull();

    // Scroll so that row 300 is at the top of the viewport.
    tracks.getBoundingClientRect = () =>
      ({ top: -300 * ROW_H, bottom: (N - 300) * ROW_H, left: 0, right: 0, width: 0, height: N * ROW_H, x: 0, y: 0, toJSON: () => ({}) }) as DOMRect;
    document.dispatchEvent(new Event('scroll'));
    await vi.waitFor(() => expect(container.querySelector(`a.rowlabel[href$="${id(300)}"]`)).not.toBeNull());
    expect(container.querySelector(`a.rowlabel[href$="${id(0)}"]`)).toBeNull();
    expect(count('a.rowlabel')).toBeLessThan(60);
    const row = container.querySelector(`a.rowlabel[href$="${id(300)}"]`) as HTMLElement;
    expect(row.style.top).toBe(300 * ROW_H + 'px');
  });
});
