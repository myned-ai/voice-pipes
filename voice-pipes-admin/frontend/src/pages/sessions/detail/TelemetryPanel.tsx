import { useLayoutEffect, useMemo, useRef, useState } from 'react';
import { BarChart3 } from 'lucide-react';
import { useTheme } from '../../../contexts/ThemeContext';
import type { Utterance, SessionEvent, SessionData, SessionMetrics } from './types';
import { computeSessionMetrics, type AggregatedSessionMetrics } from './metrics';
import { formatMetadataValue, isRealtimeSession, type MetadataDisplayValue } from './utils';
import {
  providerEndpointingMs,
  realtimeAudioOutputMs,
  realtimeInputAudioMs,
  realtimeTtfbMs,
} from '../../../lib/metrics/lookup';
import { CollapsibleSection } from '../../../components/CollapsibleSection';
import { MetricsGuide } from '../../../components/MetricsGuide';

const MetricRow = ({ label, value, unit, isDark }: { label: string, value: string | number | null, unit?: string, isDark: boolean }) => (
  <div className={`flex justify-between items-center text-sm py-1.5 border-b last:border-0 ${isDark ? 'border-slate-800' : 'border-gray-100'}`}>
    <span className={isDark ? 'text-slate-400' : 'text-gray-500'}>{label}</span>
    <span className={`font-mono font-bold ${isDark ? 'text-emerald-400' : 'text-emerald-600'}`}>
      {value ?? '-'} {unit && value != null && <span className={`text-xs font-normal ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>{unit}</span>}
    </span>
  </div>
);

/** Observed content width of an element, kept in sync on resize. Drives the
 *  density-adaptive labelling below (how many per-turn labels actually fit). */
function useMeasuredWidth<T extends HTMLElement>() {
  const ref = useRef<T>(null);
  const [width, setWidth] = useState(0);
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    setWidth(el.getBoundingClientRect().width);
    const ro = new ResizeObserver(entries => {
      const w = entries[0]?.contentRect.width;
      if (typeof w === 'number') setWidth(w);
    });
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  return { ref, width };
}

/** Approximate glyph width of the 10px mono font used for bar labels. */
const LABEL_CHAR_PX = 6;

/**
 * U-1: tiny inline bar chart for per-turn metric distribution.
 *
 * No charting library — divs only.  Each bar is sized relative to the
 * series max so an outlier turn visually pops, which is the whole point
 * (jitter detection without reading numbers).  `null` values render as
 * a faint dash so the user can tell "no data" apart from "zero".
 *
 * Bars exceeding `target` are drawn in a warning tone so over-budget
 * turns are visible at a glance without reading the value labels
 * underneath.
 */
const MicroBarChart = ({
  values,
  unit,
  color,
  target,
  isDark,
  formatValue,
  labels,
  showValueLabels = true,
}: {
  values: (number | null | undefined)[];
  unit: string;
  color: 'amber' | 'violet' | 'sky';
  target?: number;
  isDark: boolean;
  formatValue?: (v: number) => string;
  /** Per-bar turn labels (utterance ids). Falls back to array index. */
  labels?: (number | string)[];
  /** Density guard: when columns are too narrow for numbers to fit, the
   *  parent hides the per-bar value row (hover tooltips keep exact values). */
  showValueLabels?: boolean;
}) => {
  const numeric = values.map(v => (typeof v === 'number' && Number.isFinite(v) ? v : null));
  const present = numeric.filter((v): v is number => v != null);
  const seriesMax = present.reduce((m, v) => (v > m ? v : m), 0);
  // Cap the y-axis so a single outlier (e.g. a cold-socket TTFT
  // 10x the median) doesn't crush every other bar to a flat line.
  // Use max(target * 2, p90 * 1.5) as the visible ceiling; bars
  // that exceed the cap render at full height with an "↑" marker
  // so the outlier is still obvious.
  const sorted = [...present].sort((a, b) => a - b);
  const p90 = sorted.length > 0 ? sorted[Math.min(sorted.length - 1, Math.floor(sorted.length * 0.9))] : 0;
  const targetCap = target != null ? target * 2 : 0;
  const p90Cap = p90 * 1.5;
  // Always honour the seriesMax when no cap can be derived; otherwise
  // pick the larger of the two caps so a clean (no-outlier) series
  // still uses the full vertical range.
  const cap = Math.max(targetCap, p90Cap);
  const visibleMax = cap > 0 && cap < seriesMax ? cap : seriesMax;
  const palette = {
    amber: isDark ? 'bg-amber-400/80' : 'bg-amber-500/80',
    violet: isDark ? 'bg-violet-400/80' : 'bg-violet-500/80',
    sky: isDark ? 'bg-sky-400/80' : 'bg-sky-500/80',
  } as const;
  const overBudgetColor = isDark ? 'bg-rose-400/90' : 'bg-rose-500/90';
  const barColor = palette[color];
  const dashColor = isDark ? 'bg-slate-700/40' : 'bg-gray-200/80';
  const labelColor = isDark ? 'text-slate-400' : 'text-gray-500';
  const overBudgetLabel = isDark ? 'text-rose-300' : 'text-rose-600';
  const fmt = (v: number) => (formatValue ? formatValue(v) : String(Math.round(v)));
  return (
    <div className="flex flex-col gap-0.5">
      <div className="flex items-end gap-[2px] h-8" title={`max: ${fmt(seriesMax)} ${unit}${visibleMax < seriesMax ? ` (axis capped at ${fmt(visibleMax)})` : ''}`}>
        {numeric.map((v, i) => {
          const overBudget = v != null && target != null && v > target;
          const overCap = v != null && v > visibleMax;
          // Bars exceeding the visible ceiling render at full height
          // (clipped); their numeric label below shows the true value.
          const pct = v != null && visibleMax > 0
            ? (overCap ? 100 : Math.max(4, Math.round((v / visibleMax) * 100)))
            : 0;
          const cls = v == null ? dashColor : overBudget ? overBudgetColor : barColor;
          return (
            <div
              key={i}
              className={`flex-1 min-w-[3px] rounded-sm ${cls}`}
              style={{ height: v == null ? '2px' : `${pct}%`, alignSelf: 'flex-end' }}
              title={
                v == null
                  ? `turn ${labels?.[i] ?? i}: n/a`
                  : `turn ${labels?.[i] ?? i}: ${fmt(v)} ${unit}${overBudget ? ` (over ${fmt(target!)} target)` : ''}${overCap ? ' — clipped' : ''}`
              }
            />
          );
        })}
      </div>
      {/* Numeric value labels under each bar.  Over-budget values are
          tinted to match the warning bar above; clipped values are
          prefixed with ↑ to flag that the bar height is truncated.
          Skipped entirely when columns are too narrow for the numbers
          to fit — overlapping digits are worse than no digits. */}
      {showValueLabels && (
        <div className="flex items-start gap-[2px] text-[10px] font-mono leading-tight">
          {numeric.map((v, i) => {
            const overBudget = v != null && target != null && v > target;
            const overCap = v != null && v > visibleMax;
            return (
              <span
                key={i}
                className={`flex-1 min-w-[3px] text-center overflow-hidden ${v == null ? labelColor : overBudget ? overBudgetLabel : labelColor}`}
              >
                {v == null ? '–' : `${overCap ? '↑' : ''}${fmt(v)}`}
              </span>
            );
          })}
        </div>
      )}
    </div>
  );
};

/** Per-row metric definitions. Targets mirror the backend's diagnostic
 *  budgets in `_compute_targets` so over-budget bars line up with what
 *  the verdict logic flags. */
const PER_TURN_METRICS: ReadonlyArray<{
  key: 'ttft' | 'ttfb' | 'rtf';
  label: string;
  unit: string;
  color: 'amber' | 'violet' | 'sky';
  target: number;
  description: string;
  format?: (v: number) => string;
}> = [
  {
    key: 'ttft',
    label: 'TTFT',
    unit: 'ms',
    color: 'amber',
    target: 800,
    description: 'LLM time-to-first-token — wait from prompt sent to first token streamed back. Lower = snappier reply start.',
  },
  {
    key: 'ttfb',
    label: 'TTFB',
    unit: 'ms',
    color: 'violet',
    target: 400,
    description: 'TTS time-to-first-byte — wait from first LLM token to first audio frame. Lower = audio starts sooner.',
  },
  {
    key: 'rtf',
    label: 'RTF',
    unit: 'x',
    color: 'sky',
    target: 0.6,
    description: 'TTS realtime factor (synth_seconds / audio_seconds). <1.0 means faster than playback; >1.0 means TTS can\'t keep up.',
    format: (v: number) => v.toFixed(2),
  },
];

/** Realtime S2S row set. Replaces the classic TTFT/TTFB/RTF triple for
 *  sessions whose `metric_descriptors` carry a `kind: "realtime"` slice.
 *  See `pipeline/metrics.rs::realtime_default` for the source-of-truth
 *  field keys (`ttfb_ms`, `audio_output_ms`, `input_audio_ms`). */
const PER_TURN_METRICS_REALTIME: ReadonlyArray<{
  key: 'rt_endpointing' | 'rt_ttfb' | 'rt_audio_out' | 'rt_input_audio';
  label: string;
  unit: string;
  color: 'amber' | 'violet' | 'sky';
  target: number;
  description: string;
  format?: (v: number) => string;
}> = [
  {
    // The provider's endpointing lag — the
    // S2S `endpointing_ms`. Only rendered once a turn carries the field.
    key: 'rt_endpointing',
    label: 'Endpt.',
    unit: 'ms',
    color: 'amber',
    target: 500,
    description: 'Provider endpointing — how late the S2S provider\'s VAD / transcription noticed the end of speech relative to the worker\'s own VAD stamp (provider_endpointing_ms). Waited through before TTFB starts.',
  },
  {
    key: 'rt_ttfb',
    label: 'TTFB',
    unit: 'ms',
    color: 'violet',
    // Realtime providers commit to first audio quickly; classic TTFB target
    // (400ms) is a reasonable upper bound for cloud S2S over fair networks.
    target: 600,
    description: 'Realtime S2S time-to-first-byte — wait from the end-of-speech anchor to the first audio delta from the provider (see the anchor badge on each turn). Lower = audio starts sooner.',
  },
  {
    key: 'rt_audio_out',
    label: 'Audio out',
    unit: 'ms',
    color: 'sky',
    target: 0, // informational — no "target", just a magnitude bar
    description: 'Total assistant audio duration emitted by the provider for the turn. Not a latency — shown so per-turn reply length is visible at a glance.',
  },
  {
    key: 'rt_input_audio',
    label: 'Input',
    unit: 'ms',
    color: 'amber',
    target: 0,
    description: 'User input audio duration committed to the provider for the turn. Useful to spot abnormally long / short utterances.',
  },
];

// Per-metric targets sourced from provider-declared descriptor budgets
// (`health.budget`) instead of the hardcoded literals, so a provider that
// declares its own budget drives the over-target coloring too. Falls back to
// the `PER_TURN_METRICS` defaults when a descriptor omits the budget (legacy
// captures). Maps descriptor field `key` → this chart's metric key.
const DESC_KEY_TO_METRIC: Record<string, string> = {
  ttft_ms: 'ttft', ttfb_ms: 'ttfb', rtf: 'rtf', provider_endpointing_ms: 'rt_endpointing',
};
function descriptorBudgets(
  descriptors?: import('../../../lib/transport/types').ComponentMetricDescriptor[],
): Record<string, number> {
  const out: Record<string, number> = {};
  for (const c of descriptors ?? []) {
    for (const f of c.fields ?? []) {
      const metric = DESC_KEY_TO_METRIC[f.key];
      const budget = f.health?.budget;
      if (metric && typeof budget === 'number') out[metric] = budget;
    }
  }
  return out;
}

const PerTurnDistribution = ({ utterances, isDark, isRealtime, budgets }: { utterances: Utterance[]; isDark: boolean; isRealtime: boolean; budgets?: Record<string, number> }) => {
  // Classic pipeline: pull TTFT/TTFB/RTF series directly from the
  // utterance summaries. Realtime S2S: pull TTFB / audio-out / input-audio
  // from each turn's `metric_snapshot` (the single fused `kind: "realtime"`
  // component). Drives the per-row MicroBarChart below.
  type ClassicKey = (typeof PER_TURN_METRICS)[number]['key'];
  type RealtimeKey = (typeof PER_TURN_METRICS_REALTIME)[number]['key'];

  const series: Record<string, (number | null)[]> = isRealtime
    ? (() => {
        // Realtime snapshot lives on `summary.latency_breakdown` when the
        // BFF persisted the per-turn `TurnMetricsSnapshot`; some BFFs
        // attach it to `orchTurn.detail.latency_breakdown` instead. Try
        // both \u2014 the helpers tolerate `undefined`.
        // (Per-utterance snapshot field name is harmonised on the BFF.)
        const snap = (u: Utterance) =>
          (u as unknown as { metric_snapshot?: import('../../../lib/transport/types').TurnMetricsSnapshot }).metric_snapshot
          ?? (u as unknown as { latency_breakdown?: import('../../../lib/transport/types').TurnMetricsSnapshot }).latency_breakdown;
        return {
          rt_endpointing: utterances.map(u => u.summary?.provider_endpointing_ms ?? providerEndpointingMs(snap(u)) ?? null),
          rt_ttfb: utterances.map(u => realtimeTtfbMs(snap(u)) ?? null),
          rt_audio_out: utterances.map(u => realtimeAudioOutputMs(snap(u)) ?? null),
          rt_input_audio: utterances.map(u => realtimeInputAudioMs(snap(u)) ?? null),
        } satisfies Record<RealtimeKey, (number | null)[]>;
      })()
    : (() => {
        const ttft = utterances.map(u => u.summary?.ttft_ms ?? null);
        const ttfb = utterances.map(u => u.summary?.ttfb_ms ?? null);
        const rtf = utterances.map(u => {
          const r = (u.tts_detail ?? u.tts_timing)?.rtf;
          return typeof r === 'number' ? Math.round(r * 100) / 100 : null;
        });
        return { ttft, ttfb, rtf } satisfies Record<ClassicKey, (number | null)[]>;
      })();
  // The S2S endpointing row exists only once the worker emits the field
  // (older captures would otherwise show an all-dash row).
  const rows = isRealtime
    ? PER_TURN_METRICS_REALTIME.filter(m => m.key !== 'rt_endpointing' || series.rt_endpointing.some(v => v != null))
    : PER_TURN_METRICS;
  // Stat helper: min/median/max, used for the per-row summary line so
  // the user sees concrete numbers without hovering each bar.
  const stats = (vs: (number | null)[], fmt: (v: number) => string) => {
    const xs = vs.filter((v): v is number => typeof v === 'number' && Number.isFinite(v));
    if (xs.length === 0) return null;
    const sorted = [...xs].sort((a, b) => a - b);
    const median = sorted[Math.floor(sorted.length / 2)];
    return { min: sorted[0], median, max: sorted[sorted.length - 1], n: xs.length, fmt };
  };
  const labelMuted = isDark ? 'text-slate-500' : 'text-gray-400';
  const labelStrong = isDark ? 'text-slate-300' : 'text-gray-700';
  // Label columns by utterance_id so a bar maps directly to its TurnCard
  // (which is headed `#<utterance_id>`), rather than a 0-based array index that
  // silently diverges when utterance ids aren't contiguous.
  const turnLabels: (number | string)[] = utterances.map((u, i) => u.utterance_id ?? i);

  // Density-adaptive labelling: with many turns the per-column width shrinks
  // below what the numbers need and the labels smear into each other (and
  // used to force the whole card wider than the viewport). Measure the real
  // chart width and only render what fits; exact values stay reachable via
  // the bar tooltips and the min/med/max column.
  const { ref: chartAreaRef, width: chartWidth } = useMeasuredWidth<HTMLDivElement>();
  const turnCount = Math.max(1, turnLabels.length);
  // Column pitch: n columns separated by 2px gaps.
  const colW = chartWidth > 0 ? (chartWidth - (turnCount - 1) * 2) / turnCount : Number.POSITIVE_INFINITY;
  // Widest value label across all rows decides uniformly — mixed rows
  // (some labelled, some not) read as a rendering bug.
  const maxValueChars = rows.reduce((m, row) => {
    const fmt = row.format ?? ((v: number) => String(Math.round(v)));
    for (const v of series[row.key]) {
      if (typeof v === 'number' && Number.isFinite(v)) m = Math.max(m, fmt(v).length);
    }
    return m;
  }, 1);
  const showValueLabels = colW >= maxValueChars * LABEL_CHAR_PX + 2;
  // Header turn ids: render every k-th so ticks never collide.
  const maxTickChars = turnLabels.reduce<number>((m, l) => Math.max(m, String(l).length), 1);
  const tickStep = Number.isFinite(colW)
    ? Math.max(1, Math.ceil((maxTickChars * LABEL_CHAR_PX + 4) / colW))
    : 1;
  return (
    <div className={`px-3 py-2 rounded-lg border ${isDark ? 'bg-slate-900/50 border-slate-800' : 'bg-gray-50 border-gray-100'}`}>
      {/* Turn-id header so users can tell which bar is which turn (matches the
          `#<id>` badge on each TurnCard). */}
      <div className="flex items-center gap-3 pb-1.5 mb-1 border-b border-dashed border-current/10">
        <span className={`text-xs w-14 shrink-0 ${labelMuted}`}>turn →</span>
        <div ref={chartAreaRef} className="flex-1 min-w-0">
          <div className="flex items-end gap-[2px] text-[10px] font-mono leading-tight">
            {turnLabels.map((label, i) => (
              <span key={i} className={`flex-1 min-w-[3px] text-center overflow-hidden ${labelMuted}`}>
                {i % tickStep === 0 ? label : ''}
              </span>
            ))}
          </div>
        </div>
        <span className={`text-[10px] w-40 shrink-0 ${labelMuted}`}>min · med · max (target)</span>
      </div>
      {rows.map(m => {
        const fmt = m.format ?? ((v: number) => String(Math.round(v)));
        const s = stats(series[m.key], fmt);
        // Prefer the provider-declared descriptor budget; else the built-in
        // target. `target=0` means "informational magnitude bar" — don't paint
        // over-budget red and don't print a `(≤N)` clause.
        const declared = budgets?.[m.key];
        const baseTarget = typeof declared === 'number' ? declared : m.target;
        const effectiveTarget = baseTarget > 0 ? baseTarget : undefined;
        return (
          <div key={m.key} className="flex items-center gap-3 py-1.5">
            <span className={`text-xs w-14 shrink-0 font-medium ${labelStrong}`} title={m.description}>
              {m.label}
            </span>
            <div className="flex-1 min-w-0">
              <MicroBarChart
                values={series[m.key]}
                unit={m.unit}
                color={m.color}
                target={effectiveTarget}
                isDark={isDark}
                formatValue={m.format}
                labels={turnLabels}
                showValueLabels={showValueLabels}
              />
            </div>
            <span className={`text-[10px] w-40 shrink-0 font-mono text-right ${labelMuted}`}>
              {s ? (
                <>
                  {fmt(s.min)} · <span className={labelStrong}>{fmt(s.median)}</span> · {fmt(s.max)} {m.unit}
                  {effectiveTarget !== undefined && (
                    <span className={`ml-1 ${labelMuted}`}>(≤{fmt(effectiveTarget)})</span>
                  )}
                </>
              ) : (
                <span className={labelMuted}>n/a</span>
              )}
            </span>
          </div>
        );
      })}
      {/* Footer: one-line legend explaining what each metric means.
          Keeps the chart self-documenting without a separate tooltip. */}
      <div className={`pt-2 mt-1 text-[10px] leading-snug border-t border-dashed border-current/10 ${labelMuted} space-y-0.5`}>
        {rows.map(m => (
          <div key={m.key}>
            <span className={`font-mono font-medium ${labelStrong}`}>{m.label}</span>
            <span className="mx-1">—</span>
            <span>{m.description}</span>
          </div>
        ))}
        <div>
          <span className={isDark ? 'text-rose-300' : 'text-rose-600'}>red bars</span> = over target
          ; <span className="font-mono">↑</span> prefix on a value = bar height clipped (axis capped at max(target×2, p90×1.5)
          so a single outlier doesn't squash the rest).
        </div>
        {!showValueLabels && (
          <div>
            Too many turns to print per-bar values — hover a bar for its exact value.
          </div>
        )}
      </div>
    </div>
  );
};

/**
 * Banner shown when session aggregates are computed from a paginated subset of
 * events. Until the full event log is loaded (or the backend ships
 * precomputed session-wide metrics), totals like avg TTFT, prompt tokens, and
 * barge-in counts may undercount. We surface that to the user instead of
 * silently showing wrong numbers.
 */
const PartialDataBanner = ({ loaded, total, isDark }: { loaded: number, total: number, isDark: boolean }) => (
  <div
    className={`flex items-start gap-2 text-xs px-3 py-2 mb-3 rounded border ${
      isDark
        ? 'bg-amber-500/10 border-amber-500/30 text-amber-300'
        : 'bg-amber-50 border-amber-200 text-amber-800'
    }`}
    role="status"
    title="Aggregates are based on the currently loaded events page. Open the Timeline view and use 'Load more' to include the full event log."
  >
    <span className="font-bold">Partial</span>
    <span>
      Aggregates based on {loaded.toLocaleString()} of {total.toLocaleString()} events. Totals may be incomplete.
    </span>
  </div>
);

const AggregatedMetricsPanel = ({ utterances, events, eventsTotal, sessionMetrics, isRealtime, isDark }: { utterances: Utterance[], events: SessionEvent[], eventsTotal: number, sessionMetrics?: SessionMetrics | null, isRealtime: boolean, isDark: boolean }) => {
  // Prefer backend-precomputed metrics — they're computed over the full
  // event log on disk so they're correct even when the client only loaded the
  // first events page. Fall back to client-side computation for compatibility
  // with older BFFs that don't return `session_metrics`.
  const metrics: AggregatedSessionMetrics | SessionMetrics =
    sessionMetrics ?? computeSessionMetrics(utterances, events);
  // Only show the partial banner when we're falling back to client-side metrics
  // AND the loaded events are less than the full set.
  const isPartial = !sessionMetrics && eventsTotal > events.length;

  return (
    <div className="space-y-4">
      {isPartial && <PartialDataBanner loaded={events.length} total={eventsTotal} isDark={isDark} />}
      <div>
        <h4 className={`text-xs font-bold uppercase tracking-wider mb-2 ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>Latency Averages</h4>
        <div className={`px-3 py-1 rounded-lg border ${isDark ? 'bg-slate-900/50 border-slate-800' : 'bg-gray-50 border-gray-100'}`}>
          {isRealtime ? (
            // Realtime S2S sessions: TTFT/Endpointing don't exist as
            // separate stages and `metrics.avgTtfbMs` is sourced from
            // `summary.ttfb_ms` which the realtime runner populates from
            // the same provider-reported value. Avg input audio is
            // computed locally from per-turn snapshots since the classic
            // aggregator never sees them.
            <>
              <MetricRow isDark={isDark} label="Time to First Byte (TTFB)" value={metrics.avgTtfbMs} unit="ms" />
              {/* The provider's endpointing lag (S2S `endpointing_ms`) —
                  shown once the worker emits it. */}
              {metrics.avgProviderEndpointingMs != null && (
                <MetricRow isDark={isDark} label="Provider endpointing" value={metrics.avgProviderEndpointingMs} unit="ms" />
              )}
              <MetricRow isDark={isDark} label="Barge-In RTT" value={metrics.avgBargeInRttMs} unit="ms" />
            </>
          ) : (
            <>
              <MetricRow isDark={isDark} label="Time to First Token (TTFT)" value={metrics.avgTtftMs} unit="ms" />
              <MetricRow isDark={isDark} label="Time to First Byte (TTFB)" value={metrics.avgTtfbMs} unit="ms" />
              <MetricRow isDark={isDark} label="Endpointing (VAD Silence)" value={metrics.avgEndpointingMs} unit="ms" />
              <MetricRow isDark={isDark} label="Barge-In RTT" value={metrics.avgBargeInRttMs} unit="ms" />
            </>
          )}
        </div>
      </div>

      <div>
        <h4 className={`text-xs font-bold uppercase tracking-wider mb-2 ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>Performance & Usage</h4>
        <div className={`px-3 py-1 rounded-lg border ${isDark ? 'bg-slate-900/50 border-slate-800' : 'bg-gray-50 border-gray-100'}`}>
          {isRealtime ? (
            <>
              {metrics.totalPromptTokens > 0 && <MetricRow isDark={isDark} label="Prompt Tokens" value={metrics.totalPromptTokens} />}
              {metrics.totalOutputTokens > 0 && <MetricRow isDark={isDark} label="Output Tokens" value={metrics.totalOutputTokens} />}
              <MetricRow isDark={isDark} label="Total Audio" value={metrics.totalAudioSeconds} unit="sec" />
            </>
          ) : (
            <>
              <MetricRow isDark={isDark} label="Prompt Tokens" value={metrics.totalPromptTokens > 0 ? metrics.totalPromptTokens : null} />
              <MetricRow isDark={isDark} label="Output Tokens" value={metrics.totalOutputTokens > 0 ? metrics.totalOutputTokens : null} />
              <MetricRow isDark={isDark} label="Avg Tok/s" value={metrics.avgTokensPerSec} />
              <MetricRow isDark={isDark} label="Avg RTF (Audio)" value={metrics.avgRtf} />
              <MetricRow isDark={isDark} label="Total Audio" value={metrics.totalAudioSeconds} unit="sec" />
            </>
          )}
          {/* Output throughput: the
              streaming-phase realtime factor — the number that was null for
              s2s sessions while a post-processor emitted audio at 0.6×. Only
              shown when at least one turn carries an `output` component. */}
          {metrics.avgEmitRtf != null && (
            <MetricRow
              isDark={isDark}
              label={`Output RTF (avg · max)${(metrics.turnsOverRealtime ?? 0) > 0 ? ` · ${metrics.turnsOverRealtime} turn${metrics.turnsOverRealtime === 1 ? '' : 's'} > realtime` : ''}`}
              value={`${metrics.avgEmitRtf.toFixed(2)} · ${(metrics.maxEmitRtf ?? metrics.avgEmitRtf).toFixed(2)}`}
              unit="×"
            />
          )}
        </div>
      </div>

      <div>
        <h4 className={`text-xs font-bold uppercase tracking-wider mb-2 ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>Interruptions</h4>
        <div className={`px-3 py-1 rounded-lg border ${isDark ? 'bg-slate-900/50 border-slate-800' : 'bg-gray-50 border-gray-100'}`}>
          <MetricRow isDark={isDark} label="Server VAD Barge-Ins" value={metrics.serverBargeIns} />
          <MetricRow isDark={isDark} label="Client Cancels" value={metrics.clientBargeIns} />
          <MetricRow isDark={isDark} label="Playback Barge-Ins" value={metrics.playbackBargeIns} />
          <MetricRow isDark={isDark} label="Terminated Turns" value={metrics.terminatedTurns} />
        </div>
      </div>
    </div>
  );
};

export const TelemetryPanel = ({ data }: { data: SessionData }) => {
  const { isDark } = useTheme();
  const isRealtime = isRealtimeSession(data.metadata);

  // Rows for the Session Metadata panel: registry/schema fields consumed
  // elsewhere in the UI are dropped (e.g. `metric_descriptors` flows through
  // `SessionDescriptorsContext` to drive per-turn metric rendering — dumping it
  // here just clutters the panel with a giant JSON blob; add new entries when a
  // backend field starts shipping reference data through `metadata` rather than
  // user-facing session info), and `formatMetadataValue` drops anything empty
  // (null, `[]`, `{}`) so the panel shows only fields that actually carry a value.
  const metadataRows = useMemo(() => {
    const hidden = ['metric_descriptors'];
    return Object.entries(data.metadata ?? {})
      .filter(([k]) => !hidden.includes(k))
      .map(([k, v]) => [k, formatMetadataValue(k, v)] as const)
      .filter((entry): entry is readonly [string, MetadataDisplayValue] => entry[1] !== null);
  }, [data.metadata]);

  return (
    <div className={`w-1/3 min-w-[360px] shrink-0 p-8 overflow-y-auto shadow-xl z-10 flex flex-col gap-6 ${isDark ? 'bg-[#0f172a] border-l border-slate-800' : 'bg-gray-50 border-l border-gray-200'}`}>
      <h2 className={`text-xl font-bold flex items-center gap-2 pb-4 ${isDark ? 'text-white border-b border-slate-800' : 'text-gray-900 border-b border-gray-200'}`}>
        <BarChart3 className="text-indigo-400"/>
        Telemetry
        {isRealtime && (
          <span
            className={`ml-2 text-[10px] font-medium px-1.5 py-0.5 rounded ring-1 ${isDark ? 'bg-violet-500/15 text-violet-300 ring-violet-500/30' : 'bg-violet-50 text-violet-700 ring-violet-300'}`}
            title="This session was driven by a realtime speech-to-speech provider (OpenAI Realtime / Gemini Live). The classic STT/LLM/TTS stages are collapsed into a single fused stage by the provider; metric rows reflect that."
          >
            Realtime S2S
          </span>
        )}
      </h2>

      {data.utterances && data.utterances.length > 0 && (
        <CollapsibleSection title="Session Performance">
          <AggregatedMetricsPanel
            utterances={data.utterances}
            events={data.events || []}
            eventsTotal={data.events_total ?? (data.events?.length ?? 0)}
            sessionMetrics={data.session_metrics}
            isRealtime={isRealtime}
            isDark={isDark}
          />
        </CollapsibleSection>
      )}

      {data.utterances && data.utterances.length > 1 && (
        <CollapsibleSection title="Per-turn distribution">
          <PerTurnDistribution utterances={data.utterances} isDark={isDark} isRealtime={isRealtime} budgets={descriptorBudgets(data.metadata?.metric_descriptors)} />
        </CollapsibleSection>
      )}

      <CollapsibleSection title="Telemetry Data" defaultOpen={false}>
        {data.telemetry && data.telemetry.length > 0 ? (
          <div className={`font-mono text-xs break-all max-h-96 overflow-y-auto w-full rounded ${isDark ? 'bg-slate-800/80 text-indigo-200' : 'bg-gray-50 text-indigo-700'}`}>
            {data.telemetry.map((t: Record<string, unknown>, i: number) => (
              <div key={i} className={`py-2 px-2 ${isDark ? 'border-b border-slate-700' : 'border-b border-gray-200'}`}>
                <span className={isDark ? 'text-slate-500' : 'text-gray-400'}>[{String(t.timestamp_ms ?? i)}] </span>
                {JSON.stringify(t.metrics || t.data || t.content || t)}
              </div>
            ))}
          </div>
        ) : (
          <div className={`text-sm italic ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>No telemetry data found.</div>
        )}
      </CollapsibleSection>

      {metadataRows.length > 0 && (
        <CollapsibleSection title="Session Metadata">
          {metadataRows.map(([k, v]) => (
            <div key={k} className={`flex justify-between items-start gap-3 text-sm py-1 last:border-0 ${isDark ? 'border-b border-slate-800' : 'border-b border-gray-100'}`}>
              <span className={`shrink-0 ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>{k}</span>
              {v.isBlock ? (
                <pre className={`font-mono text-xs whitespace-pre-wrap break-all max-h-48 overflow-y-auto text-right ${isDark ? 'text-white' : 'text-gray-900'}`}>{v.text}</pre>
              ) : (
                <span className={`break-words text-right ${isDark ? 'text-white' : 'text-gray-900'}`} title={v.title}>
                  <span className="font-mono">{v.text}</span>
                  {v.hint && (
                    <span className={`ml-1.5 font-normal ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>{v.hint}</span>
                  )}
                </span>
              )}
            </div>
          ))}
        </CollapsibleSection>
      )}

      <CollapsibleSection title="Metrics Guide" defaultOpen={false}>
        <MetricsGuide />
      </CollapsibleSection>
    </div>
  );
};
