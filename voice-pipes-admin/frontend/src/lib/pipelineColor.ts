/**
 * Stable per-pipeline colour palette.
 *
 * The same `pipeline_name` always maps to the same hex colour across the
 * Dashboard (charts, matrix table, filter chips, failed-session badges).
 * The palette is hand-picked to remain readable on both dark and light
 * Tailwind backgrounds.
 */

const PALETTE: readonly string[] = [
  "#6366f1", // indigo
  "#10b981", // emerald
  "#f59e0b", // amber
  "#ec4899", // pink
  "#06b6d4", // cyan
  "#a78bfa", // violet
  "#f43f5e", // rose
  "#84cc16", // lime
  "#3b82f6", // blue
  "#fb923c", // orange
  "#14b8a6", // teal
  "#eab308", // yellow
];

/** djb2 hash → palette index. Stable for a given input string. */
function hash(s: string): number {
  let h = 5381;
  for (let i = 0; i < s.length; i++) h = ((h << 5) + h + s.charCodeAt(i)) | 0;
  return Math.abs(h);
}

/** Returns a stable hex colour for the given pipeline name. */
export function pipelineColor(name: string | null | undefined): string {
  const key = (name && name.length > 0) ? name : "(unknown)";
  return PALETTE[hash(key) % PALETTE.length]!;
}

/** Returns a translucent variant suitable for chart fills (rgba). */
export function pipelineColorAlpha(name: string | null | undefined, alpha = 0.2): string {
  const hex = pipelineColor(name);
  const r = parseInt(hex.slice(1, 3), 16);
  const g = parseInt(hex.slice(3, 5), 16);
  const b = parseInt(hex.slice(5, 7), 16);
  return `rgba(${r}, ${g}, ${b}, ${alpha})`;
}
