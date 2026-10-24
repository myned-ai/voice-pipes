// systemStatusStrip.logic — pure derivation behind the L0 "is the system
// healthy?" glance strip.  Kept out of the component module so Fast Refresh
// sees a components-only file there, and so the verdict is unit-testable
// without rendering (see SystemStatusStrip.test.ts).

import type { Slo } from '../lib/slo';

export type HealthLevel = 'ok' | 'warn' | 'critical';

/** Minimal shape we need from the analytics KPI (avoids importing Dashboard). */
export interface KpiLike {
  total_sessions?: number;
  total_utterances?: number;
  error_rate_pct?: number | null;
  p95_latency_ms?: number | null;
  spend_total_usd?: number;
  spend_by_provider?: Record<string, { cap_pct: number | null }>;
}

export interface SystemSignal {
  key: string;
  label: string;
  level: HealthLevel;
  detail: string;
}

export interface SystemHealth {
  level: HealthLevel;
  headline: string;
  signals: SystemSignal[];
}

const RANK: Record<HealthLevel, number> = { ok: 0, warn: 1, critical: 2 };

/** A p95 latency SLO inherently allows ~5% of turns to exceed the target, so
 *  that 5% is the latency error budget against which the breach is measured. */
const LATENCY_BUDGET_PCT = 5;

function fmtMs(n: number): string {
  return n >= 1000 ? `${(n / 1000).toFixed(1)}s` : `${Math.round(n)}ms`;
}

/** Pure: turn the window KPIs into a worst-of verdict + the signals behind it.
 *  When `latencyBreachPct` (% of turns over the p95 SLO) is supplied, an
 * SLO error-budget-burn signal is added: how many times the allowed
 *  budget the window has consumed (1× = exactly at budget, >1× = over). */
export function deriveSystemHealth(kpi: KpiLike, slo: Slo, latencyBreachPct?: number | null): SystemHealth {
  const signals: SystemSignal[] = [];

  const er = kpi.error_rate_pct ?? null;
  if (er != null) {
    const level: HealthLevel = er >= 5 ? 'critical' : er > slo.error_rate_max_pct ? 'warn' : 'ok';
    signals.push({ key: 'errors', label: 'Error rate', level, detail: `${er.toFixed(1)}%` });
  }

  const p95 = kpi.p95_latency_ms ?? null;
  if (p95 != null) {
    const level: HealthLevel =
      p95 > slo.total_p95_ms ? 'critical' : p95 > slo.total_p95_ms * 0.8 ? 'warn' : 'ok';
    signals.push({ key: 'latency', label: 'p95 latency', level, detail: `${fmtMs(p95)} / ${fmtMs(slo.total_p95_ms)} SLO` });
  }

  if (latencyBreachPct != null) {
    const errBurn = slo.error_rate_max_pct > 0 ? (kpi.error_rate_pct ?? 0) / slo.error_rate_max_pct : 0;
    const latBurn = latencyBreachPct / LATENCY_BUDGET_PCT;
    const worst = Math.max(errBurn, latBurn);
    const level: HealthLevel = worst >= 2 ? 'critical' : worst >= 1 ? 'warn' : 'ok';
    signals.push({ key: 'slo_burn', label: 'SLO burn', level, detail: `${worst.toFixed(1)}× budget` });
  }

  const caps = Object.values(kpi.spend_by_provider ?? {}).map((p) => p.cap_pct ?? 0);
  if (caps.length > 0) {
    const maxCap = Math.max(...caps);
    const level: HealthLevel = maxCap >= 100 ? 'critical' : maxCap >= 80 ? 'warn' : 'ok';
    signals.push({ key: 'budget', label: 'Spend cap', level, detail: `${maxCap.toFixed(0)}% of cap` });
  }

  const level = signals.reduce<HealthLevel>((acc, s) => (RANK[s.level] > RANK[acc] ? s.level : acc), 'ok');
  const headline =
    level === 'critical' ? 'Critical — action required'
      : level === 'warn' ? 'Degraded — needs attention'
        : 'All systems operational';
  return { level, headline, signals };
}
