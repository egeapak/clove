/** Fraction (0..1) of the plot a pointer is at, clamped so it never leaves it. */
export function fractionAtX(clientX: number, plotLeft: number, plotWidth: number): number {
  if (!(plotWidth > 0)) return 0;
  return Math.min(1, Math.max(0, (clientX - plotLeft) / plotWidth));
}

/** Epoch milliseconds at `fraction` of the plot, which spans the `[min, max]` domain. */
export function dateAtFraction(min: number, max: number, fraction: number): number {
  const clamped = Math.min(1, Math.max(0, fraction));
  return min + (max - min) * clamped;
}

/** The local-time date label shared by the axis ticks and the hover chip. */
export function formatAxisDate(epochMs: number): string {
  return new Intl.DateTimeFormat(undefined, { month: 'short', day: 'numeric' }).format(new Date(epochMs));
}

/**
 * Centre (px from the plot's left edge) for the hover chip at plot position
 * `x`, kept fully inside the visible part of the plot: `[visibleStart,
 * visibleEnd]` in the same plot coordinates. When the timeline scrolls
 * sideways the sticky label column hides the plot's left part, so the plot's
 * own edges are not enough. A visible span narrower than the chip pins it to
 * the span's start.
 */
export function chipCenter(x: number, half: number, visibleStart: number, visibleEnd: number): number {
  const lo = visibleStart + half;
  const hi = visibleEnd - half;
  if (hi < lo) return lo;
  return Math.min(hi, Math.max(lo, x));
}

/**
 * The rows to render for a column of `count` fixed-height rows of which the
 * pixels `[top, bottom]` (measured from the top of the first row) are on
 * screen: `first` inclusive, `last` exclusive, widened by `overscan` rows on
 * each side so a scroll or a Tab step lands on a row that already exists.
 */
export function rowWindow(
  top: number,
  bottom: number,
  rowH: number,
  count: number,
  overscan: number
): { first: number; last: number } {
  const lo = Math.max(0, Math.min(top, count * rowH));
  const hi = Math.max(lo, Math.min(bottom, count * rowH));
  const first = Math.max(0, Math.floor(lo / rowH) - overscan);
  const last = Math.min(count, Math.ceil(hi / rowH) + overscan);
  return { first: Math.min(first, last), last };
}
