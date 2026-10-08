// @vitest-environment jsdom
import { describe, it, expect, afterEach, vi } from 'vitest';
import { fireEvent } from '@testing-library/svelte';
import { tooltip } from './tooltip';

function tip(): HTMLElement | null {
  const el = document.querySelector('.clove-tooltip') as HTMLElement | null;
  return el && !el.hidden ? el : null;
}

function anchor(): HTMLElement {
  const el = document.createElement('span');
  document.body.appendChild(el);
  return el;
}

const cleanups: Array<() => void> = [];
afterEach(() => {
  cleanups.splice(0).forEach((fn) => fn());
  document.body.replaceChildren();
  vi.restoreAllMocks();
});

function use(node: HTMLElement, param: Parameters<typeof tooltip>[1]) {
  const action = tooltip(node, param);
  cleanups.push(() => action.destroy());
  return action;
}

describe('tooltip', () => {
  it('shows plain text on hover and focus, and hides on leave', async () => {
    const a = anchor();
    use(a, 'Hello');
    await fireEvent.mouseEnter(a);
    expect(tip()?.textContent).toBe('Hello');
    await fireEvent.mouseLeave(a);
    expect(tip()).toBeNull();
    await fireEvent.focus(a);
    expect(tip()?.textContent).toBe('Hello');
    await fireEvent.blur(a);
    expect(tip()).toBeNull();
  });

  it('renders styled segments as spans, never as markup', async () => {
    const a = anchor();
    use(a, ['Move to ', { text: '◐', color: 'var(--status-in-progress)' }, ' ', { text: '<img src=x onerror=alert(1)>', bold: true }]);
    await fireEvent.mouseEnter(a);
    const el = tip()!;
    expect(el.textContent).toBe('Move to ◐ <img src=x onerror=alert(1)>');
    expect(el.querySelector('img')).toBeNull();
    const spans = el.querySelectorAll('span');
    expect(spans).toHaveLength(2);
    expect(spans[0].style.color).toBe('var(--status-in-progress)');
    expect(spans[1].style.fontWeight).toBe('700');
  });

  it('follows updates', async () => {
    const a = anchor();
    const action = use(a, 'old');
    action.update('new');
    await fireEvent.mouseEnter(a);
    expect(tip()?.textContent).toBe('new');
  });

  it('stays hidden for empty content', async () => {
    const a = anchor();
    use(a, '');
    await fireEvent.mouseEnter(a);
    expect(tip()).toBeNull();
  });

  it('with whenTruncated, shows only while the text is clipped', async () => {
    const a = anchor();
    use(a, { content: 'A long title', whenTruncated: true });
    const scroll = vi.spyOn(a, 'scrollWidth', 'get').mockReturnValue(100);
    vi.spyOn(a, 'clientWidth', 'get').mockReturnValue(100);
    await fireEvent.mouseEnter(a);
    expect(tip()).toBeNull();
    scroll.mockReturnValue(300);
    await fireEvent.mouseEnter(a);
    expect(tip()?.textContent).toBe('A long title');
  });

  it('hands back to the enclosing anchor when the pointer leaves a nested one', async () => {
    const outer = anchor();
    const inner = document.createElement('span');
    outer.appendChild(inner);
    use(outer, 'outer');
    use(inner, 'inner');
    await fireEvent.mouseEnter(inner);
    expect(tip()?.textContent).toBe('inner');
    await fireEvent.mouseLeave(inner, { relatedTarget: outer });
    expect(tip()?.textContent).toBe('outer');
    await fireEvent.mouseLeave(outer, { relatedTarget: document.body });
    expect(tip()).toBeNull();
  });
});
