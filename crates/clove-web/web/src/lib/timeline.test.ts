import { describe, it, expect } from 'vitest';
import { dateAtFraction, fractionAtX, formatAxisDate } from './timeline';

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
