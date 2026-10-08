// @vitest-environment jsdom
import { describe, it, expect, afterEach } from 'vitest';
import { render, cleanup, fireEvent } from '@testing-library/svelte';
import TypeIcon from '$lib/components/TypeIcon.svelte';
import Card from '$lib/components/Card.svelte';
import { item } from './fake-api';

afterEach(cleanup);

/** How many times `word` appears as visible text (aria labels don't count). */
function visibleCount(root: HTMLElement, word: string): number {
  return (root.textContent ?? '').split(/\s+/).filter((w) => w === word).length;
}

function tooltipEl(): HTMLElement | null {
  return document.querySelector('.clove-tooltip');
}

describe('an item type is shown once', () => {
  it('where there is room it is the full name alone', () => {
    const { container } = render(TypeIcon, { type: 'feature', label: true });
    expect(container.textContent?.trim()).toBe('feature');
  });

  it('the compact badge is the letter, named by a themed tooltip on hover', async () => {
    const { container } = render(TypeIcon, { type: 'chore' });
    const badge = container.querySelector('[role="img"]') as HTMLElement;
    expect(badge.textContent?.trim()).toBe('C');
    expect(badge.getAttribute('aria-label')).toBe('chore');
    // No native title: it would show a second, unthemed tooltip.
    expect(badge.hasAttribute('title')).toBe(false);

    await fireEvent.mouseEnter(badge);
    expect(tooltipEl()?.hidden).toBe(false);
    expect(tooltipEl()?.textContent).toBe('chore');
    await fireEvent.mouseLeave(badge);
    expect(tooltipEl()?.hidden).toBe(true);
  });

  it('an epic card shows its type only as the badge', () => {
    const { container } = render(Card, { item: item({ id: 'proj-0epic000', type: 'epic' }) });
    expect(visibleCount(container, 'epic')).toBe(0);
    expect(container.querySelectorAll('[role="img"][aria-label="epic"]')).toHaveLength(1);
  });
});
