// @vitest-environment jsdom
import { describe, it, expect, afterEach, beforeEach, vi } from 'vitest';
import { render, cleanup, fireEvent } from '@testing-library/svelte';
import { stubApi, META, item } from './fake-api';
import { formatAxisDate } from '$lib/timeline';
import { store } from '$lib/store.svelte';
import TimelinePage from '../routes/timeline/+page.svelte';

const NOW = Date.parse('2026-04-11T12:00:00Z');
const CREATED = Date.parse('2026-01-01T00:00:00Z');
// The page pads its created..now domain by 4% on each side.
const PAD = (NOW - CREATED) * 0.04;
const DOMAIN_MIN = CREATED - PAD;
const DOMAIN_MAX = NOW + PAD;

const PLOT = { left: 200, width: 1000 };

beforeEach(() => {
  vi.useFakeTimers({ toFake: ['Date'] });
  vi.setSystemTime(NOW);
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.unstubAllGlobals();
});

async function renderTimeline() {
  stubApi((path) => {
    if (path === '/meta') return META;
    if (path === '/items') {
      return [item({ id: 'clov-AAAAAAAA', created: '2026-01-01T00:00:00Z', updated: '2026-01-01T00:00:00Z' })];
    }
    if (path === '/stats/history') return [];
    throw new Error('unexpected request ' + path);
  });
  const view = render(TimelinePage);
  await vi.waitFor(() => expect(view.container.querySelector('.bar')).not.toBeNull());
  const axis = view.container.querySelector('.tl-axis') as HTMLElement;
  axis.getBoundingClientRect = () =>
    ({
      left: PLOT.left,
      right: PLOT.left + PLOT.width,
      width: PLOT.width,
      top: 0,
      bottom: 34,
      height: 34,
      x: PLOT.left,
      y: 0,
      toJSON: () => ({})
    }) as DOMRect;
  const grid = view.container.querySelector('.tl-grid') as HTMLElement;
  return { ...view, axis, grid };
}

const chip = (root: HTMLElement) => root.querySelector('[data-testid="timeline-cursor-chip"]');
const line = (root: HTMLElement) => root.querySelector('[data-testid="timeline-cursor-line"]');

describe('timeline hover guide', () => {
  it('shows the guide line and the date under the pointer, and hides on leave', async () => {
    const { container, axis, grid } = await renderTimeline();
    expect(chip(container)).toBeNull();

    await fireEvent.pointerMove(axis, { clientX: PLOT.left + PLOT.width / 2, pointerType: 'mouse' });
    const middle = DOMAIN_MIN + (DOMAIN_MAX - DOMAIN_MIN) / 2;
    expect(chip(container)?.textContent?.trim()).toBe(formatAxisDate(middle));
    expect(line(container)).not.toBeNull();
    expect((line(container) as HTMLElement).style.left).toBe('50%');

    await fireEvent.pointerLeave(grid);
    expect(chip(container)).toBeNull();
    expect(line(container)).toBeNull();
  });

  it('works from the track area and for touch/pen pointers', async () => {
    const { container } = await renderTimeline();
    const tracks = container.querySelector('.tl-tracks') as HTMLElement;
    await fireEvent.pointerMove(tracks, { clientX: PLOT.left + PLOT.width / 4, pointerType: 'touch' });
    const quarter = DOMAIN_MIN + (DOMAIN_MAX - DOMAIN_MIN) / 4;
    expect(chip(container)?.textContent?.trim()).toBe(formatAxisDate(quarter));
  });

  it('reads the domain ends at the plot edges and clamps beyond them', async () => {
    const { container, axis } = await renderTimeline();
    await fireEvent.pointerMove(axis, { clientX: PLOT.left, pointerType: 'pen' });
    expect(chip(container)?.textContent?.trim()).toBe(formatAxisDate(DOMAIN_MIN));
    expect((line(container) as HTMLElement).style.left).toBe('0%');

    await fireEvent.pointerMove(axis, { clientX: PLOT.left + PLOT.width, pointerType: 'pen' });
    expect(chip(container)?.textContent?.trim()).toBe(formatAxisDate(DOMAIN_MAX));
    expect((line(container) as HTMLElement).style.left).toBe('100%');
  });

  it('keeps the chip inside the plot and ignores pointer events', async () => {
    const { container, axis } = await renderTimeline();
    await fireEvent.pointerMove(axis, { clientX: PLOT.left, pointerType: 'mouse' });
    // Before layout the widths are 0: the chip is clamped to the plot's start.
    expect((chip(container) as HTMLElement).style.left).toBe('36px');
  });

  it('keeps the chip right of the label column when the plot is scrolled', async () => {
    const resizes: (() => void)[] = [];
    vi.stubGlobal(
      'ResizeObserver',
      class {
        constructor(cb: () => void) {
          resizes.push(cb);
        }
        observe() {}
        disconnect() {}
      }
    );
    const { container, axis } = await renderTimeline();
    const scroller = container.querySelector('.tl-scroll') as HTMLElement;
    const corner = container.querySelector('.tl-corner') as HTMLElement;
    // A 1000px plot behind a 300px label column in an 800px window, scrolled
    // 500px: the visible plot is 500..1000.
    Object.defineProperty(axis, 'clientWidth', { configurable: true, value: 1000 });
    Object.defineProperty(scroller, 'clientWidth', { configurable: true, value: 800 });
    Object.defineProperty(scroller, 'scrollLeft', { configurable: true, value: 500 });
    Object.defineProperty(corner, 'offsetWidth', { configurable: true, value: 300 });
    resizes.forEach((f) => f());
    await fireEvent.scroll(scroller);

    await fireEvent.pointerMove(axis, { clientX: PLOT.left + 510, pointerType: 'mouse' });
    await vi.waitFor(() => expect((chip(container) as HTMLElement).style.left).toBe('536px'));
    await fireEvent.pointerMove(axis, { clientX: PLOT.left + 995, pointerType: 'mouse' });
    expect((chip(container) as HTMLElement).style.left).toBe('964px');
    await fireEvent.pointerMove(axis, { clientX: PLOT.left + 700, pointerType: 'mouse' });
    expect((chip(container) as HTMLElement).style.left).toBe('700px');
  });

  it('hides while the pointer is over the sticky label column', async () => {
    const { container, axis } = await renderTimeline();
    await fireEvent.pointerMove(axis, { clientX: PLOT.left + 100, pointerType: 'mouse' });
    expect(chip(container)).not.toBeNull();
    const labels = container.querySelector('.tl-labels') as HTMLElement;
    await fireEvent.pointerMove(labels, { clientX: PLOT.left + 100, pointerType: 'mouse' });
    expect(chip(container)).toBeNull();
  });
});

describe('timeline axis ticks', () => {
  it('aligns the first and last tick labels inward so they stay inside the plot', async () => {
    const { axis } = await renderTimeline();
    const ticks = [...axis.querySelectorAll<HTMLElement>('.tick')];
    expect(ticks.length).toBeGreaterThanOrEqual(5);
    const first = ticks[0];
    const last = ticks[ticks.length - 1];
    expect(first.style.left).toBe('0%');
    expect(first.classList.contains('tick-start')).toBe(true);
    expect(last.style.left).toBe('100%');
    expect(last.classList.contains('tick-end')).toBe(true);
    expect(last.textContent?.trim()).toBe(formatAxisDate(DOMAIN_MAX));
    for (const t of ticks.slice(1, -1)) {
      expect(t.classList.contains('tick-start') || t.classList.contains('tick-end')).toBe(false);
    }
  });
});

// The shared store keeps the last test's items and does not refetch for the
// same query, so each call reloads it and checks the page draws its own row.
async function renderWithItems(id: string, created: string) {
  stubApi((path) => {
    if (path === '/meta') return META;
    if (path === '/items') return [item({ id, created, updated: created })];
    if (path === '/stats/history') return [];
    throw new Error('unexpected request ' + path);
  });
  const view = render(TimelinePage);
  await store.refetch();
  await vi.waitFor(() => {
    const labels = [...view.container.querySelectorAll('.rowlabel')];
    expect(labels.map((a) => a.getAttribute('href'))).toEqual([expect.stringContaining(id)]);
  });
  return view;
}

describe('timeline plot width', () => {
  it('spaces the axis ticks by the rendered axis width, not the minimum track width', async () => {
    const resizes: (() => void)[] = [];
    vi.stubGlobal(
      'ResizeObserver',
      class {
        constructor(cb: () => void) {
          resizes.push(cb);
        }
        observe() {}
        disconnect() {}
      }
    );
    const { container } = await renderWithItems('clov-WIDEAXIS', '2026-01-01T00:00:00Z');
    const axis = container.querySelector('.tl-axis') as HTMLElement;
    // Before layout clientWidth is 0, so the track width decides: about 1519px -> 11 gaps.
    expect(axis.querySelectorAll('.tick').length).toBe(12);
    Object.defineProperty(axis, 'clientWidth', { configurable: true, value: 2800 });
    resizes.forEach((f) => f());
    await vi.waitFor(() => expect(axis.querySelectorAll('.tick').length).toBe(21));
    const last = [...axis.querySelectorAll<HTMLElement>('.tick')].at(-1)!;
    expect(last.classList.contains('tick-end')).toBe(true);
  });

  it('keeps the "today" label right of its line when there is room', async () => {
    const { container } = await renderWithItems('clov-LONGHIST', '2026-01-01T00:00:00Z');
    expect(container.querySelector('.today')!.classList.contains('flip')).toBe(false);
  });

  it('moves the "today" label left of its line near the right edge of the plot', async () => {
    // A 10-day history gets the minimum 640px track, so today sits about 24px
    // from the right edge: too close for the label.
    const { container } = await renderWithItems('clov-SHORTHIS', '2026-04-01T00:00:00Z');
    expect(container.querySelector('.today')!.classList.contains('flip')).toBe(true);
  });
});
