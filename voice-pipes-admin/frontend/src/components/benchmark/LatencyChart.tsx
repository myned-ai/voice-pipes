import {
  LineChart, Line, XAxis, YAxis, CartesianGrid, Tooltip,
  ResponsiveContainer, Legend, ReferenceLine,
} from 'recharts';
import type { TooltipValueType } from 'recharts';
import { useTheme } from '../../contexts/ThemeContext';
import type { VariantResults } from './types';

const VARIANT_COLORS = [
  '#6366f1', // indigo
  '#10b981', // emerald
  '#f59e0b', // amber
  '#ef4444', // red
  '#8b5cf6', // violet
  '#06b6d4', // cyan
];

function fmtMs(ms: number | null | undefined): string {
  if (ms == null) return '—';
  if (ms < 1000) return `${Math.round(ms)} ms`;
  return `${(ms / 1000).toFixed(1)} s`;
}

const tooltipStyle = (isDark: boolean) => ({
  contentStyle: {
    backgroundColor: isDark ? '#1e293b' : '#fff',
    border: `1px solid ${isDark ? '#334155' : '#e5e7eb'}`,
    borderRadius: 8,
    fontSize: 12,
  },
  // Returning `[value, undefined]` keeps the formatted value but blanks the
  // series name in the tooltip row (recharts only renders a name when the
  // second tuple element is truthy).
  formatter: (value: TooltipValueType | undefined) => [fmtMs(Number(value)), undefined],
});

/* ── Latency percentiles line chart ─────────────────────── */

export function LatencyPercentilesChart({ variantResults }: { variantResults: VariantResults[] }) {
  const { isDark } = useTheme();

  // One row per percentile, one series per variant
  const data = [
    { pct: 'P50', ...Object.fromEntries(variantResults.map(vr => [vr.variant_name, vr.summary.p50_e2e_latency_ms])) },
    { pct: 'P90', ...Object.fromEntries(variantResults.map(vr => [vr.variant_name, vr.summary.p90_e2e_latency_ms])) },
    { pct: 'P99', ...Object.fromEntries(variantResults.map(vr => [vr.variant_name, vr.summary.p99_e2e_latency_ms])) },
  ];

  const axisColor = isDark ? '#94a3b8' : '#6b7280';
  const gridColor = isDark ? '#334155' : '#e5e7eb';

  return (
    <div className={`rounded-xl border p-5 ${isDark ? 'bg-slate-800 border-slate-700' : 'bg-white border-gray-200'}`}>
      <h3 className={`text-sm font-semibold mb-4 ${isDark ? 'text-white' : 'text-gray-900'}`}>
        E2E Latency Percentiles
      </h3>
      <ResponsiveContainer width="100%" height={280}>
        <LineChart data={data} margin={{ top: 8, right: 24, left: 0, bottom: 8 }}>
          <CartesianGrid strokeDasharray="3 3" stroke={gridColor} />
          <XAxis dataKey="pct" tick={{ fill: axisColor, fontSize: 13, fontWeight: 600 }} />
          <YAxis
            tickFormatter={(v: number) => fmtMs(v)}
            tick={{ fill: axisColor, fontSize: 11 }}
            width={72}
          />
          <Tooltip {...tooltipStyle(isDark)} />
          <Legend wrapperStyle={{ fontSize: 12 }} />
          {variantResults.map((vr, i) => (
            <Line
              key={vr.variant_id}
              type="monotone"
              dataKey={vr.variant_name}
              stroke={VARIANT_COLORS[i % VARIANT_COLORS.length]}
              strokeWidth={2.5}
              dot={{ r: 5, strokeWidth: 2, fill: VARIANT_COLORS[i % VARIANT_COLORS.length] }}
              activeDot={{ r: 7 }}
              connectNulls={false}
              isAnimationActive={false}
            />
          ))}
        </LineChart>
      </ResponsiveContainer>
    </div>
  );
}

/* ── TTFT / TTFB stat cards ──────────────────────────────── */

export function TtftTtfbChart({ variantResults }: { variantResults: VariantResults[] }) {
  const { isDark } = useTheme();

  const rows = variantResults.map((vr, i) => ({
    name: vr.variant_name,
    color: VARIANT_COLORS[i % VARIANT_COLORS.length],
    ttft: vr.summary.avg_ttft_ms,
    ttfb: vr.summary.avg_ttfb_ms,
  }));

  if (rows.every(r => r.ttft == null && r.ttfb == null)) return null;

  return (
    <div className={`rounded-xl border p-5 ${isDark ? 'bg-slate-800 border-slate-700' : 'bg-white border-gray-200'}`}>
      <h3 className={`text-sm font-semibold mb-4 ${isDark ? 'text-white' : 'text-gray-900'}`}>
        TTFT / TTFB Breakdown
      </h3>
      <div className="space-y-4">
        {rows.map(row => (
          <div key={row.name}>
            <div className="flex items-center gap-2 mb-2">
              <span className="inline-block w-3 h-3 rounded-full flex-shrink-0" style={{ backgroundColor: row.color }} />
              <span className={`text-sm font-medium ${isDark ? 'text-slate-200' : 'text-gray-800'}`}>{row.name}</span>
            </div>
            <div className="grid grid-cols-2 gap-3 pl-5">
              {[{ label: 'Avg TTFT', value: row.ttft }, { label: 'Avg TTFB', value: row.ttfb }].map(({ label, value }) => (
                <div
                  key={label}
                  className={`rounded-lg p-3 ${isDark ? 'bg-slate-700/60' : 'bg-gray-50'}`}
                >
                  <div className={`text-xs mb-1 ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>{label}</div>
                  <div className={`text-lg font-semibold tabular-nums ${isDark ? 'text-white' : 'text-gray-900'}`}>
                    {fmtMs(value)}
                  </div>
                </div>
              ))}
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}

/* ── Latency samples line chart (per-scenario) ───────────── */

export function LatencySamplesChart({
  variantResults,
  scenarioId,
}: {
  variantResults: VariantResults[];
  scenarioId: string;
}) {
  const { isDark } = useTheme();

  let maxReps = 0;
  for (const vr of variantResults) {
    const scenario = vr.scenarios.find(s => s.scenario_id === scenarioId);
    if (scenario?.latency_samples) maxReps = Math.max(maxReps, scenario.latency_samples.length);
  }

  const data = Array.from({ length: maxReps }, (_, i) => {
    // `rep` is the x value; every other key is a variant name -> latency sample.
    const entry: Record<string, number | null> = { rep: i + 1 };
    for (const vr of variantResults) {
      const scenario = vr.scenarios.find(s => s.scenario_id === scenarioId);
      entry[vr.variant_name] = scenario?.latency_samples?.[i] ?? null;
    }
    return entry;
  });

  if (data.length === 0) return null;

  const axisColor = isDark ? '#94a3b8' : '#6b7280';
  const gridColor = isDark ? '#334155' : '#e5e7eb';

  // compute per-variant mean for reference lines
  const means = variantResults.map(vr => {
    const scenario = vr.scenarios.find(s => s.scenario_id === scenarioId);
    const samples = scenario?.latency_samples;
    if (!samples?.length) return null;
    return samples.reduce((a, b) => a + b, 0) / samples.length;
  });

  return (
    <div className={`rounded-xl border p-5 ${isDark ? 'bg-slate-800 border-slate-700' : 'bg-white border-gray-200'}`}>
      <h3 className={`text-sm font-semibold mb-4 ${isDark ? 'text-white' : 'text-gray-900'}`}>
        Latency per Repetition
      </h3>
      <ResponsiveContainer width="100%" height={220}>
        <LineChart data={data} margin={{ top: 8, right: 24, left: 0, bottom: 8 }}>
          <CartesianGrid strokeDasharray="3 3" stroke={gridColor} />
          <XAxis
            dataKey="rep"
            tick={{ fill: axisColor, fontSize: 11 }}
            label={{ value: 'Rep', position: 'insideBottom', offset: -2, fill: axisColor, fontSize: 11 }}
          />
          <YAxis
            tickFormatter={(v: number) => fmtMs(v)}
            tick={{ fill: axisColor, fontSize: 11 }}
            width={72}
          />
          <Tooltip {...tooltipStyle(isDark)} />
          <Legend wrapperStyle={{ fontSize: 12 }} />
          {variantResults.map((vr, i) => (
            <Line
              key={vr.variant_id}
              type="linear"
              dataKey={vr.variant_name}
              stroke={VARIANT_COLORS[i % VARIANT_COLORS.length]}
              strokeWidth={2}
              dot={{ r: 4, strokeWidth: 2, fill: VARIANT_COLORS[i % VARIANT_COLORS.length] }}
              activeDot={{ r: 6 }}
              connectNulls={false}
              isAnimationActive={false}
            />
          ))}
          {means.map((mean, i) =>
            mean != null ? (
              <ReferenceLine
                key={variantResults[i].variant_id}
                y={mean}
                stroke={VARIANT_COLORS[i % VARIANT_COLORS.length]}
                strokeDasharray="5 3"
                strokeOpacity={0.5}
              />
            ) : null
          )}
        </LineChart>
      </ResponsiveContainer>
    </div>
  );
}
