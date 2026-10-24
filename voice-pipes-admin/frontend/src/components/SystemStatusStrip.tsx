import { CheckCircle, AlertTriangle, XCircle } from 'lucide-react';
import { useTheme } from '../contexts/ThemeContext';
import type { Slo } from '../lib/slo';
import { deriveSystemHealth, type HealthLevel, type KpiLike } from './systemStatusStrip.logic';

/* ── L0 "is the system healthy?" glance strip ──────────────────
   A one-row verdict synthesised from the window KPIs (error rate, tail
   latency vs SLO, budget), shown above the detailed cards so an operator
   gets a green/amber/red answer without parsing the grid.  The derivation
   is a pure function (`deriveSystemHealth` in ./systemStatusStrip.logic) so
   it is unit-tested. */

/* ── presentation ─────────────────────────────────────────── */

function levelClasses(level: HealthLevel, isDark: boolean) {
  switch (level) {
    case 'ok':
      return {
        band: isDark ? 'bg-emerald-500/10 border-emerald-500/30' : 'bg-emerald-50 border-emerald-200',
        text: isDark ? 'text-emerald-300' : 'text-emerald-700',
        Icon: CheckCircle,
      };
    case 'warn':
      return {
        band: isDark ? 'bg-amber-500/10 border-amber-500/30' : 'bg-amber-50 border-amber-200',
        text: isDark ? 'text-amber-300' : 'text-amber-700',
        Icon: AlertTriangle,
      };
    case 'critical':
      return {
        band: isDark ? 'bg-rose-500/10 border-rose-500/40' : 'bg-rose-50 border-rose-200',
        text: isDark ? 'text-rose-300' : 'text-rose-700',
        Icon: XCircle,
      };
  }
}

function signalColor(level: HealthLevel, isDark: boolean): string {
  if (level === 'critical') return isDark ? 'text-rose-300' : 'text-rose-600';
  if (level === 'warn') return isDark ? 'text-amber-300' : 'text-amber-600';
  return isDark ? 'text-emerald-300' : 'text-emerald-600';
}

function Chip({ label, value, valueClass, isDark }: { label: string; value: string; valueClass?: string; isDark: boolean }) {
  return (
    <span className="inline-flex items-baseline gap-1.5 whitespace-nowrap">
      <span className={`text-[10px] uppercase tracking-wide ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>{label}</span>
      <span className={`text-sm font-semibold tabular-nums ${valueClass ?? (isDark ? 'text-slate-200' : 'text-gray-800')}`}>{value}</span>
    </span>
  );
}

export default function SystemStatusStrip({ kpi, slo, latencyBreachPct, windowLabel }: { kpi: KpiLike; slo: Slo; latencyBreachPct?: number | null; windowLabel?: string }) {
  const { isDark } = useTheme();
  const health = deriveSystemHealth(kpi, slo, latencyBreachPct);
  const { band, text, Icon } = levelClasses(health.level, isDark);

  return (
    <div className={`rounded-xl border px-4 py-3 flex flex-wrap items-center gap-x-6 gap-y-2 ${band}`}>
      <div className={`inline-flex items-center gap-2 font-semibold ${text}`}>
        <Icon size={18} />
        <span>{health.headline}</span>
      </div>
      <div className="flex flex-wrap items-center gap-x-5 gap-y-1.5 ml-auto">
        {kpi.total_sessions != null && (
          <Chip label="Sessions" value={String(kpi.total_sessions)} isDark={isDark} />
        )}
        {health.signals.map((s) => (
          <Chip key={s.key} label={s.label} value={s.detail} valueClass={signalColor(s.level, isDark)} isDark={isDark} />
        ))}
        {kpi.spend_total_usd != null && (
          <Chip label="Spend" value={`$${kpi.spend_total_usd.toFixed(2)}`} isDark={isDark} />
        )}
        {windowLabel && (
          <span className={`text-[10px] ${isDark ? 'text-slate-600' : 'text-gray-400'}`}>{windowLabel}</span>
        )}
      </div>
    </div>
  );
}
