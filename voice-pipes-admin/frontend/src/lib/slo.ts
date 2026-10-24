/**
 * Per-pipeline SLO targets used as reference lines on Dashboard charts and
 * for breach-percentage badges on KPI cards.
 *
 * The defaults live here for now (frontend defaults, with an orchestrator
 * override to come later). They can
 * be overridden per pipeline by setting `VITE_SLO_OVERRIDES_JSON` to a JSON
 * object of the form:
 *
 *   { "pipeline-a": { "total_p95_ms": 2500, "ttft_p95_ms": 800,
 *                     "endpointing_p95_ms": 700, "error_rate_max_pct": 0.5 } }
 */

export interface Slo {
  total_p95_ms: number;
  ttft_p95_ms: number;
  endpointing_p95_ms: number;
  error_rate_max_pct: number;
}

const DEFAULT_SLO: Slo = {
  total_p95_ms: 3000,
  ttft_p95_ms: 1200,
  endpointing_p95_ms: 800,
  error_rate_max_pct: 1.0,
};

let OVERRIDES: Record<string, Partial<Slo>> = {};
try {
  // Vite injects import.meta.env.* at build time.
  const raw = (import.meta as { env?: Record<string, string | undefined> })
    .env?.VITE_SLO_OVERRIDES_JSON;
  if (raw && raw.trim().length > 0) {
    OVERRIDES = JSON.parse(raw);
  }
} catch {
  // Bad JSON should not break the dashboard — fall back to defaults.
  OVERRIDES = {};
}

export function getSlo(pipelineName: string | null | undefined): Slo {
  const key = pipelineName ?? '';
  const ovr = OVERRIDES[key] ?? OVERRIDES['*'] ?? {};
  return { ...DEFAULT_SLO, ...ovr };
}

/** Default SLO used for global (non-pipeline-specific) chart reference lines. */
export const GLOBAL_SLO: Slo = getSlo('*');

/**
 * Compute what fraction of `samples` are at or below the SLO target.
 * Returns a percentage (0–100). For "max" SLOs (i.e. lower-is-better),
 * pass the actual sample values and the target as `slo`.
 */
export function breachPct(samples: number[], slo: number): number | null {
  if (samples.length === 0) return null;
  const breaches = samples.filter((v) => v > slo).length;
  return Math.round((breaches / samples.length) * 1000) / 10;
}
