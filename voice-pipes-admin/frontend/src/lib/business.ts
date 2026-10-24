/* Business / outcome metrics derived from the window KPIs.
 *
 * Cost-per-call is the most universally-wanted voice-ops business metric and is
 * unambiguous from data we already have (window spend ÷ sessions / turns), plus
 * a projected monthly run-rate.  Pure + unit-tested.  When a pipeline filter is
 * active these are per-pipeline (the spend is server-scoped to it). */

export interface CostEfficiencyInput {
  /** Total window spend in USD (may be 0 when providers are unpriced/local). */
  spendTotal: number | null | undefined;
  sessions: number;
  turns: number;
  /** Length of the analysis window in days (for the monthly run-rate). */
  windowDays: number;
}

export interface CostEfficiency {
  spendTotal: number;
  costPerSession: number | null;
  costPerTurn: number | null;
  /** Spend extrapolated to 30 days at the current window's rate. */
  projectedMonthlySpend: number | null;
}

export function deriveCostEfficiency(i: CostEfficiencyInput): CostEfficiency {
  const spend = i.spendTotal ?? 0;
  return {
    spendTotal: spend,
    costPerSession: i.sessions > 0 ? spend / i.sessions : null,
    costPerTurn: i.turns > 0 ? spend / i.turns : null,
    projectedMonthlySpend: i.windowDays > 0 ? (spend / i.windowDays) * 30 : null,
  };
}
