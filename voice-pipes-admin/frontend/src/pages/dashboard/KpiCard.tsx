import type React from 'react';

/* ── KPI card component ─────────────────────────────────── */

export function KpiCard({
  icon: Icon,
  label,
  value,
  sub,
  iconColor,
  isDark,
  delta,
}: {
  icon: React.ElementType;
  label: string;
  value: string;
  sub?: string;
  iconColor: string;
  isDark: boolean;
  /** Δ vs prior period. `lowerIsBetter` flips the colour; `null` hides it. */
  delta?: { pct: number | null; lowerIsBetter?: boolean } | null;
}) {
  let deltaEl: React.ReactNode = null;
  if (delta && delta.pct !== null && Number.isFinite(delta.pct)) {
    const lowerBetter = delta.lowerIsBetter ?? false;
    const positive = delta.pct > 0;
    const negative = delta.pct < 0;
    const better = lowerBetter ? negative : positive;
    const worse = lowerBetter ? positive : negative;
    const colour = better
      ? (isDark ? 'text-emerald-400' : 'text-emerald-600')
      : worse
        ? (isDark ? 'text-rose-400' : 'text-rose-600')
        : (isDark ? 'text-slate-500' : 'text-gray-400');
    const arrow = positive ? '↑' : negative ? '↓' : '·';
    const abs = Math.abs(delta.pct).toFixed(delta.pct >= 100 ? 0 : 1);
    deltaEl = (
      <p className={`text-[11px] mt-0.5 font-medium ${colour}`}>
        {arrow} {abs}% vs prior
      </p>
    );
  }
  return (
    <div className={`rounded-xl p-5 border transition-colors ${isDark ? 'bg-slate-800 border-slate-700' : 'bg-white border-gray-200'}`}>
      <div className="flex items-center gap-3">
        <div className={`p-2 rounded-lg shrink-0 ${isDark ? 'bg-slate-700/50' : 'bg-gray-100'}`}>
          <Icon size={20} className={iconColor} />
        </div>
        <div className="min-w-0">
          <p className={`text-xs font-medium uppercase tracking-wide ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>{label}</p>
          <p className={`text-2xl font-bold tabular-nums truncate ${isDark ? 'text-white' : 'text-gray-900'}`} title={value}>{value}</p>
          {sub && <p className={`text-xs mt-0.5 truncate ${isDark ? 'text-slate-500' : 'text-gray-400'}`} title={sub}>{sub}</p>}
          {deltaEl}
        </div>
      </div>
    </div>
  );
}
