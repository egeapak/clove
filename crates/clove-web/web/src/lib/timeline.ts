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
