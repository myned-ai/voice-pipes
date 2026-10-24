/* ── time-bucketed aggregation ──────────────────────────── */

export type AggFn = 'avg' | 'p50' | 'p95' | 'p99' | 'max';

export function aggregate(values: number[], how: AggFn): number {
  if (values.length === 0) return 0;
  if (how === 'avg') return values.reduce((a, b) => a + b, 0) / values.length;
  if (how === 'max') return Math.max(...values);
  const sorted = [...values].sort((a, b) => a - b);
  const p = how === 'p50' ? 0.5 : how === 'p95' ? 0.95 : 0.99;
  return sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * p))]!;
}

/** Compute a percentile (0..1) from an UNSORTED numeric array. */
export function percentileOf(values: number[], p: number): number | null {
  if (values.length === 0) return null;
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * p))]!;
}

/** Choose a Y-axis upper bound for a latency chart that keeps the bulk of the
 *  data legible despite outliers.  Clamps to ~p95 of the plotted values but
 *  never below the SLO line (so the SLO stays on-chart) and never above the
 *  true max.  Returns `{ yMax, outliers }` — outliers are points above yMax. */
export function latencyYDomain(values: number[], sloMs: number): { yMax: number; outliers: number } {
  if (values.length === 0) return { yMax: sloMs * 2, outliers: 0 };
  const max = Math.max(...values);
  const p95 = percentileOf(values, 0.95) ?? max;
  // Headroom above p95 so the busiest band isn't flush with the top.
  const clamped = Math.min(max, Math.max(sloMs * 1.2, p95 * 1.25));
  const yMax = Math.ceil(clamped / 500) * 500; // round up to nearest 0.5s
  const outliers = values.filter((v) => v > yMax).length;
  return { yMax, outliers };
}

/** Pick a sensible bucket size (ms) given the visible window in days. */
export function autoBucketMs(days: number): number {
  if (days <= 1) return 60 * 1000;          // 1 min
  if (days <= 7) return 5 * 60 * 1000;      // 5 min
  if (days <= 30) return 60 * 60 * 1000;    // 1 h
  return 24 * 60 * 60 * 1000;               // 1 d
}

export function bucketSizeMs(b: 'auto' | '1m' | '5m' | '1h' | '1d', days: number): number {
  switch (b) {
    case '1m': return 60 * 1000;
    case '5m': return 5 * 60 * 1000;
    case '1h': return 60 * 60 * 1000;
    case '1d': return 24 * 60 * 60 * 1000;
    case 'auto': return autoBucketMs(days);
  }
}
