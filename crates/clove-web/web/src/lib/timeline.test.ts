import { describe, it, expect } from 'vitest';
import { chipCenter, dateAtFraction, fractionAtX, formatAxisDate } from './timeline';

describe('fractionAtX', () => {
  it('maps the plot edges and middle', () => {
    expect(fractionAtX(100, 100, 400)).toBe(0);
    expect(fractionAtX(300, 100, 400)).toBe(0.5);
    expect(fractionAtX(500, 100, 400)).toBe(1);
  });
  it('clamps outside the plot', () => {
    expect(fractionAtX(50, 100, 400)).toBe(0);
    expect(fractionAtX(900, 100, 400)).toBe(1);
  });
  it('accounts for a scrolled (negative) plot offset', () => {
    expect(fractionAtX(100, -300, 800)).toBe(0.5);
  });
  it('is 0 for a degenerate plot', () => {
    expect(fractionAtX(10, 0, 0)).toBe(0);
  });
});

describe('dateAtFraction', () => {
  const min = Date.UTC(2026, 0, 1);
  const max = Date.UTC(2026, 0, 11);
  it('hits the domain ends and middle', () => {
    expect(dateAtFraction(min, max, 0)).toBe(min);
    expect(dateAtFraction(min, max, 1)).toBe(max);
    expect(dateAtFraction(min, max, 0.5)).toBe(Date.UTC(2026, 0, 6));
  });
  it('clamps out-of-range fractions', () => {
    expect(dateAtFraction(min, max, -2)).toBe(min);
    expect(dateAtFraction(min, max, 3)).toBe(max);
  });
  it('composes with fractionAtX over a wide, scrolled track', () => {
    const fraction = fractionAtX(250, -750, 2000);
    expect(dateAtFraction(min, max, fraction)).toBe(min + (max - min) * 0.5);
  });
});

describe('formatAxisDate', () => {
  it('formats as the axis does (local month + day)', () => {
    const t = new Date(2026, 2, 5, 12).getTime();
    expect(formatAxisDate(t)).toBe(
      new Intl.DateTimeFormat(undefined, { month: 'short', day: 'numeric' }).format(new Date(t))
    );
  });
});

describe('chipCenter', () => {
  it('leaves a chip that fits where the pointer is', () => {
    expect(chipCenter(500, 36, 0, 1000)).toBe(500);
  });
  it('keeps the chip inside the plot edges when nothing is scrolled', () => {
    expect(chipCenter(3, 36, 0, 1000)).toBe(36);
    expect(chipCenter(998, 36, 0, 1000)).toBe(964);
  });
  it('keeps the chip right of the label column when the plot is scrolled', () => {
    // Scrolled 723px: the visible plot starts at 723, so a pointer at its left
    // edge must not centre the chip at 726 (half of it under the labels).
    expect(chipCenter(726, 36, 723, 1716)).toBe(759);
    expect(chipCenter(1714, 36, 723, 1716)).toBe(1680);
  });
  it('pins the chip to the start of a span narrower than the chip', () => {
    expect(chipCenter(10, 36, 0, 50)).toBe(36);
  });
});
