import { useEffect, useMemo, useState } from 'react';
import { useSearchParams, Link } from 'react-router-dom';
import axios from 'axios';
import {
  LineChart, Line, XAxis, YAxis, CartesianGrid, Tooltip,
  ResponsiveContainer, Legend,
} from 'recharts';
import { ArrowLeft, Loader2, GitCompare, TrendingDown, TrendingUp, Minus } from 'lucide-react';
import { useTheme } from '../../contexts/ThemeContext';
import { API_BASE } from '../../config/api';
import type { Experiment, ExperimentResults, VariantResults } from '../../components/benchmark/types';
import { unwrap } from './useBenchmarkApi';

const VARIANT_COLORS = [
  '#6366f1', '#10b981', '#f59e0b', '#ef4444', '#8b5cf6', '#06b6d4',
];

function fmtMs(ms: number | null | undefined): string {
  if (ms == null) return '—';
  if (ms < 1000) return `${Math.round(ms)} ms`;
  return `${(ms / 1000).toFixed(1)} s`;
}

interface LoadedExperiment {
  experiment: Experiment;
  results: ExperimentResults | null;
}

/** Everything fetched for one `?ids=` value; `loading` is derived from it. */
interface LoadedSet {
  /** The raw `ids` query-string value these items were fetched for. */
  key: string;
  items: LoadedExperiment[];
}

/** One row of the trend chart: experiment label/date plus one P50 per variant. */
type TrendPoint = Record<string, string | number | null | undefined>;

export default function BenchmarkCompare() {
  const { isDark } = useTheme();
  const [searchParams] = useSearchParams();
  const idsParam = searchParams.get('ids') ?? '';
  const ids = useMemo(() => idsParam.split(',').filter(Boolean), [idsParam]);
  const [loadedSet, setLoadedSet] = useState<LoadedSet | null>(null);

  useEffect(() => {
    if (ids.length === 0) return;
    let cancelled = false;

    Promise.all(
      ids.map(async (id) => {
        const [expRes, resultsRes] = await Promise.allSettled([
          axios.get(`${API_BASE}/api/v1/benchmark/experiments/${encodeURIComponent(id)}`),
          axios.get(`${API_BASE}/api/v1/benchmark/experiments/${encodeURIComponent(id)}/results`),
        ]);
        return {
          experiment: expRes.status === 'fulfilled' ? unwrap<Experiment | null>(expRes.value.data) : null,
          results: resultsRes.status === 'fulfilled' ? unwrap<ExperimentResults | null>(resultsRes.value.data) : null,
        };
      })
    ).then(items => {
      if (cancelled) return;
      setLoadedSet({
        key: idsParam,
        items: items.filter((i): i is LoadedExperiment => i.experiment != null),
      });
    });

    return () => { cancelled = true; };
  }, [ids, idsParam]);

  // Loading whenever ids were requested and the settled set is for a different
  // ids value (or nothing has settled yet).
  const loading = ids.length > 0 && loadedSet?.key !== idsParam;
  const loaded: LoadedExperiment[] = loadedSet?.key === idsParam ? loadedSet.items : [];

  if (loading) {
    return (
      <div className="flex items-center justify-center h-full">
        <Loader2 size={24} className="animate-spin text-indigo-400" />
      </div>
    );
  }

  if (loaded.length < 2) {
    return (
      <div className="p-8">
        <Link to="/benchmarks" className={`flex items-center gap-2 text-sm mb-4 ${isDark ? 'text-slate-400 hover:text-white' : 'text-gray-500 hover:text-gray-900'}`}>
          <ArrowLeft size={16} /> Back to Benchmarks
        </Link>
        <p className={isDark ? 'text-slate-400' : 'text-gray-500'}>Select at least 2 experiments to compare.</p>
      </div>
    );
  }

  // Collect all unique variant names across experiments
  const allVariantNames = [...new Set(
    loaded.flatMap(l => l.results?.variant_results?.map(vr => vr.variant_name) ?? [])
  )];

  // Build trend data: one point per experiment, one line per variant
  const trendData = loaded.map(l => {
    const point: TrendPoint = {
      label: l.experiment.name,
      date: l.experiment.completed_at ?? l.experiment.created_at,
    };
    for (const vr of l.results?.variant_results ?? []) {
      point[vr.variant_name] = vr.summary.p50_e2e_latency_ms;
    }
    return point;
  });

  // Build delta table: compare first and last experiments
  const first = loaded[0];
  const last = loaded[loaded.length - 1];

  type MetricRow = {
    label: string;
    getter: (vr: VariantResults) => number | null | undefined;
  };

  const metricRows: MetricRow[] = [
    { label: 'P50 E2E', getter: vr => vr.summary.p50_e2e_latency_ms },
    { label: 'P90 E2E', getter: vr => vr.summary.p90_e2e_latency_ms },
    { label: 'P99 E2E', getter: vr => vr.summary.p99_e2e_latency_ms },
    { label: 'Avg TTFT', getter: vr => vr.summary.avg_ttft_ms },
    { label: 'Avg TTFB', getter: vr => vr.summary.avg_ttfb_ms },
    // ── TTS-only fields ──
    // Skipped automatically by the table renderer when both old and new
    // sides are nullish, so non-TTS experiments are unaffected.
    { label: 'P50 TTS TTFB', getter: vr => vr.summary.p50_tts_ttfb_ms },
    { label: 'P90 TTS TTFB', getter: vr => vr.summary.p90_tts_ttfb_ms },
    { label: 'Avg TTS synth', getter: vr => vr.summary.avg_tts_synthesis_ms },
    { label: 'Cold TTS synth', getter: vr => vr.summary.cold_tts_synthesis_ms },
    { label: 'Warm TTS synth', getter: vr => vr.summary.warm_tts_synthesis_ms },
    // ── Conversation / barge-in aggregates ──
    // Same hide-when-all-null behavior as TTS rows. Only ms-valued
    // metrics live here; counts / rates are surfaced elsewhere because
    // this table renders every value through `fmtMs`.
    { label: 'Avg turn latency', getter: vr => vr.summary.avg_turn_latency_ms },
    { label: 'P50 barge-in RTT', getter: vr => vr.summary.p50_barge_in_rtt_ms },
    { label: 'P95 barge-in RTT', getter: vr => vr.summary.p95_barge_in_rtt_ms },
    { label: 'P50 recovery', getter: vr => vr.summary.p50_recovery_ms },
    { label: 'P95 recovery', getter: vr => vr.summary.p95_recovery_ms },
  ];

  const headerCell = `text-xs font-medium uppercase tracking-wide px-4 py-3 text-left ${
    isDark ? 'text-slate-400' : 'text-gray-500'
  }`;
  const cell = `px-4 py-3 text-sm font-mono ${isDark ? 'text-slate-200' : 'text-gray-800'}`;
  const rowBorder = isDark ? 'border-slate-700' : 'border-gray-200';

  function DeltaCell({ oldVal, newVal }: { oldVal: number | null | undefined; newVal: number | null | undefined }) {
    if (oldVal == null || newVal == null) return <span className={isDark ? 'text-slate-500' : 'text-gray-400'}>—</span>;
    const pct = ((newVal - oldVal) / oldVal) * 100;
    const isImprovement = pct < 0; // lower is better for latency
    const Icon = pct < -1 ? TrendingDown : pct > 1 ? TrendingUp : Minus;
    return (
      <span className={`flex items-center gap-1 ${
        isImprovement ? 'text-emerald-400' : pct > 1 ? 'text-red-400' : (isDark ? 'text-slate-400' : 'text-gray-500')
      }`}>
        <Icon size={12} />
        {pct > 0 ? '+' : ''}{pct.toFixed(1)}%
      </span>
    );
  }

  return (
    <div className="p-8 overflow-y-auto h-full">
      <Link to="/benchmarks" className={`flex items-center gap-2 text-sm mb-4 ${isDark ? 'text-slate-400 hover:text-white' : 'text-gray-500 hover:text-gray-900'}`}>
        <ArrowLeft size={16} /> Back to Benchmarks
      </Link>

      <div className="flex items-center gap-3 mb-8">
        <GitCompare size={24} className="text-indigo-400" />
        <h1 className={`text-2xl font-bold ${isDark ? 'text-white' : 'text-gray-900'}`}>
          Compare Experiments
        </h1>
        <span className={`text-sm ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
          {loaded.length} experiments
        </span>
      </div>

      {/* P50 E2E Trend Chart */}
      <div className={`rounded-xl border p-5 mb-8 ${isDark ? 'bg-slate-800 border-slate-700' : 'bg-white border-gray-200'}`}>
        <h3 className={`text-sm font-semibold mb-4 ${isDark ? 'text-white' : 'text-gray-900'}`}>
          P50 E2E Latency Trend
        </h3>
        <ResponsiveContainer width="100%" height={300}>
          <LineChart data={trendData}>
            <CartesianGrid strokeDasharray="3 3" stroke={isDark ? '#334155' : '#e5e7eb'} />
            <XAxis
              dataKey="label"
              tick={{ fill: isDark ? '#94a3b8' : '#6b7280', fontSize: 12 }}
            />
            <YAxis
              tickFormatter={(v: number) => fmtMs(v)}
              tick={{ fill: isDark ? '#94a3b8' : '#6b7280', fontSize: 11 }}
              width={72}
            />
            <Tooltip
              contentStyle={{
                backgroundColor: isDark ? '#1e293b' : '#fff',
                border: `1px solid ${isDark ? '#334155' : '#e5e7eb'}`,
                borderRadius: 8,
                fontSize: 12,
              }}
              formatter={(value) => fmtMs(Number(value))}
            />
            <Legend wrapperStyle={{ fontSize: 12 }} />
            {allVariantNames.map((name, i) => (
              <Line
                key={name}
                type="monotone"
                dataKey={name}
                stroke={VARIANT_COLORS[i % VARIANT_COLORS.length]}
                strokeWidth={2}
                dot={{ r: 4 }}
                connectNulls
              />
            ))}
          </LineChart>
        </ResponsiveContainer>
      </div>

      {/* Delta tables — one per variant */}
      {allVariantNames.map(variantName => {
        const firstVr = first.results?.variant_results?.find(vr => vr.variant_name === variantName);
        const lastVr = last.results?.variant_results?.find(vr => vr.variant_name === variantName);
        if (!firstVr && !lastVr) return null;

        return (
          <div key={variantName} className={`rounded-xl border mb-6 ${isDark ? 'bg-slate-800 border-slate-700' : 'bg-white border-gray-200'}`}>
            <h3 className={`text-sm font-semibold px-5 py-4 border-b ${isDark ? 'text-white border-slate-700' : 'text-gray-900 border-gray-200'}`}>
              {variantName}
            </h3>
            <div className="overflow-x-auto">
              <table className="w-full">
                <thead>
                  <tr className={`border-b ${rowBorder}`}>
                    <th className={headerCell}>Metric</th>
                    <th className={headerCell}>{first.experiment.name}</th>
                    {loaded.length > 2 && loaded.slice(1, -1).map(l => (
                      <th key={l.experiment.id} className={headerCell}>{l.experiment.name}</th>
                    ))}
                    <th className={headerCell}>{last.experiment.name}</th>
                    <th className={headerCell}>Change</th>
                  </tr>
                </thead>
                <tbody>
                  {metricRows.map((row, i) => {
                    const firstVal = firstVr ? row.getter(firstVr) : null;
                    const lastVal = lastVr ? row.getter(lastVr) : null;
                    // Hide rows where every column is empty (e.g. TTS
                    // metrics on a non-TTS experiment).
                    const middleVals = loaded.length > 2
                      ? loaded.slice(1, -1).map(l => {
                          const vr = l.results?.variant_results?.find(v => v.variant_name === variantName);
                          return vr ? row.getter(vr) : null;
                        })
                      : [];
                    if (firstVal == null && lastVal == null && middleVals.every(v => v == null)) {
                      return null;
                    }
                    return (
                      <tr key={row.label} className={`border-b ${rowBorder} ${i % 2 === 0 ? (isDark ? 'bg-slate-800/50' : 'bg-gray-50/50') : ''}`}>
                        <td className={`${cell} font-sans font-medium ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>{row.label}</td>
                        <td className={cell}>{fmtMs(firstVal)}</td>
                        {loaded.length > 2 && loaded.slice(1, -1).map(l => {
                          const vr = l.results?.variant_results?.find(v => v.variant_name === variantName);
                          return <td key={l.experiment.id} className={cell}>{fmtMs(vr ? row.getter(vr) : null)}</td>;
                        })}
                        <td className={cell}>{fmtMs(lastVal)}</td>
                        <td className={cell}><DeltaCell oldVal={firstVal} newVal={lastVal} /></td>
                      </tr>
                    );
                  })}
                  {/* Pass rate row */}
                  <tr className={`border-b ${rowBorder}`}>
                    <td className={`${cell} font-sans font-medium ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>Pass Rate</td>
                    <td className={cell}>
                      {firstVr ? `${firstVr.summary.scenarios_passed}/${firstVr.summary.scenarios_run}` : '—'}
                    </td>
                    {loaded.length > 2 && loaded.slice(1, -1).map(l => {
                      const vr = l.results?.variant_results?.find(v => v.variant_name === variantName);
                      return <td key={l.experiment.id} className={cell}>{vr ? `${vr.summary.scenarios_passed}/${vr.summary.scenarios_run}` : '—'}</td>;
                    })}
                    <td className={cell}>
                      {lastVr ? `${lastVr.summary.scenarios_passed}/${lastVr.summary.scenarios_run}` : '—'}
                    </td>
                    <td className={cell}>
                      {firstVr && lastVr ? (
                        firstVr.summary.scenarios_run > 0 && lastVr.summary.scenarios_run > 0 ? (
                          <span className={
                            (lastVr.summary.scenarios_passed / lastVr.summary.scenarios_run) >=
                            (firstVr.summary.scenarios_passed / firstVr.summary.scenarios_run)
                              ? 'text-emerald-400' : 'text-red-400'
                          }>
                            {((lastVr.summary.scenarios_passed / lastVr.summary.scenarios_run) * 100).toFixed(0)}%
                          </span>
                        ) : '—'
                      ) : '—'}
                    </td>
                  </tr>
                </tbody>
              </table>
            </div>
          </div>
        );
      })}
    </div>
  );
}
