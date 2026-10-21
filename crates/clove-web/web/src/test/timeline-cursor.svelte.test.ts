// @vitest-environment jsdom
import { describe, it, expect, afterEach, beforeEach, vi } from 'vitest';
import { render, cleanup, fireEvent } from '@testing-library/svelte';
import { stubApi, META, item } from './fake-api';
import { formatAxisDate } from '$lib/timeline';
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

  it('keeps the chip inside the plot via an inline clamp and ignores pointer events', async () => {
    const { container, axis } = await renderTimeline();
    await fireEvent.pointerMove(axis, { clientX: PLOT.left, pointerType: 'mouse' });
    const el = chip(container) as HTMLElement;
    expect(el.getAttribute('style')).toContain('clamp(var(--chip-half)');
    expect(el.getAttribute('style')).toContain('calc(100% - var(--chip-half))');
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
