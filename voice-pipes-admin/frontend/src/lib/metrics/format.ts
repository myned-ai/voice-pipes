// Unit-aware formatter for `MetricFieldDescriptor`-driven values.
//
// Adding a new unit type is one new switch arm. The renderer (the
// generic `<TurnComponentMetricsTable>`) never needs to special-case
// individual field keys — descriptors carry the unit, this formatter
// handles the rest.

import type { MetricUnit } from "../transport/types";

/**
 * Format a metric value for display. Returns `"-"` when the value is
 * `undefined`, `null`, or `NaN`.
 *
 * `format` is an optional numeric formatter token (currently honoured
 * for `ratio`, `tokens_per_sec`, and `raw` units): a string of the form
 * `"0.NN"` selects fixed-decimal output with N digits.
 */
export function formatMetric(
  value: unknown,
  unit: MetricUnit,
  format?: string,
): string {
  if (value === undefined || value === null) return "-";
  if (typeof value === "number" && !Number.isFinite(value)) return "-";

  const fixed = decimalsFromFormat(format);

  switch (unit) {
    case "ms":
      return typeof value === "number"
        ? `${Math.round(value)} ms`
        : String(value);
    case "percent":
      return typeof value === "number"
        ? `${(value * 100).toFixed(fixed ?? 1)}%`
        : String(value);
    case "ratio":
      return typeof value === "number" ? value.toFixed(fixed ?? 2) : String(value);
    case "count":
      return typeof value === "number" ? Math.round(value).toString() : String(value);
    case "tokens_per_sec":
      return typeof value === "number"
        ? `${value.toFixed(fixed ?? 1)} tok/s`
        : String(value);
    case "bytes":
      return typeof value === "number" ? formatBytes(value) : String(value);
    case "raw":
      return typeof value === "number" && fixed != null
        ? value.toFixed(fixed)
        : String(value);
    case "db":
      return typeof value === "number"
        ? `${value.toFixed(fixed ?? 1)} dB`
        : String(value);
  }
}

/** Parse `"0.NN"` → N decimal digits. Returns `undefined` otherwise. */
function decimalsFromFormat(fmt: string | undefined): number | undefined {
  if (!fmt) return undefined;
  const m = /^0(?:\.(0+))?$/.exec(fmt);
  if (!m) return undefined;
  return m[1]?.length ?? 0;
}

function formatBytes(n: number): string {
  if (n < 1024) return `${Math.round(n)} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}
