// @vitest-environment jsdom
import { describe, it, expect, afterEach } from 'vitest';
import { render, cleanup } from '@testing-library/svelte';
import TypeIcon from '$lib/components/TypeIcon.svelte';
import Card from '$lib/components/Card.svelte';
import { item } from './fake-api';

afterEach(cleanup);

/** How many times `word` appears as visible text (aria labels and titles don't count). */
function visibleCount(root: HTMLElement, word: string): number {
  return (root.textContent ?? '').split(/\s+/).filter((w) => w === word).length;
}

describe('an item type is shown once', () => {
  it('the labelled badge carries the glyph and the name in one chip', () => {
    const { container } = render(TypeIcon, { type: 'feature', label: true });
    const chips = container.querySelectorAll('[role="img"]');
    expect(chips).toHaveLength(1);
    expect(chips[0].getAttribute('title')).toBe('feature');
    expect(chips[0].textContent?.replace(/\s+/g, '')).toBe('Ffeature');
  });

  it('the compact badge names its type on hover', () => {
    const { container } = render(TypeIcon, { type: 'chore' });
    expect(container.querySelector('[role="img"]')?.getAttribute('title')).toBe('chore');
    expect(visibleCount(container, 'chore')).toBe(0);
  });

  it('an epic card shows its type only as the badge', () => {
    const { container } = render(Card, { item: item({ id: 'proj-0epic000', type: 'epic' }) });
    expect(visibleCount(container, 'epic')).toBe(0);
    expect(container.querySelectorAll('[role="img"][aria-label="epic"]')).toHaveLength(1);
  });
});
