import { BarChart3, Activity, Clock, Zap, MessageSquare, AlertTriangle, Radio, Check, XCircle, DollarSign } from 'lucide-react';
import { GLOBAL_SLO } from '../../lib/slo';
import { fmtMs, fmtNum, fmtUsd } from './format';
import { KpiCard } from './KpiCard';
import type { AnalyticsData, KPI } from './types';
import type { DashboardMetrics } from './useDashboardMetrics';

interface QuotaAlertsProps {
  isDark: boolean;
  data: AnalyticsData;
}

/** Banner for providers whose window spend has crossed 80% of their monthly cap. */
export function QuotaAlerts({ isDark, data }: QuotaAlertsProps) {
  const map = data?.kpi.spend_by_provider;
  if (!map) return null;
  const breaches = Object.entries(map)
    .filter(([, v]) => v.cap_pct != null && v.cap_pct >= 80)
    .sort((a, b) => (b[1].cap_pct ?? 0) - (a[1].cap_pct ?? 0));
  if (breaches.length === 0) return null;
  const worst = breaches[0][1].cap_pct ?? 0;
  const tone = worst >= 100
    ? (isDark ? 'bg-rose-950/30 border-rose-900/50 text-rose-200' : 'bg-rose-50 border-rose-200 text-rose-800')
    : (isDark ? 'bg-amber-950/30 border-amber-900/50 text-amber-200' : 'bg-amber-50 border-amber-200 text-amber-800');
  return (
    <div className={`rounded-xl border p-4 flex items-start gap-3 ${tone}`}>
      <AlertTriangle size={18} className="mt-0.5 flex-shrink-0" />
      <div className="flex-1 min-w-0">
        <p className="text-sm font-semibold">
          {worst >= 100 ? 'Provider over monthly cap' : 'Provider approaching monthly cap'}
        </p>
        <ul className="mt-1.5 space-y-0.5 text-xs">
          {breaches.map(([name, v]) => (
            <li key={name} className="tabular-nums">
              <span className="font-medium">{name}</span>
              {' · '}
              {fmtUsd(v.total)} of {fmtUsd(v.monthly_cap_usd)}
              {' · '}
              <span className="font-semibold">{(v.cap_pct ?? 0).toFixed(1)}%</span>
            </li>
          ))}
        </ul>
        <p className="text-xs mt-2 opacity-75">
          Caps configured in <code className="font-mono">backend/app/pricing.yaml</code>.
        </p>
      </div>
    </div>
  );
}

interface KpiCardsProps {
  isDark: boolean;
  data: AnalyticsData;
  days: number;
  selectedPipelines: Set<string>;
  kpi: KPI;
  avgEndpointingMs: number | null;
  avgProviderEndpointingMs: number | null;
  sloBreaches: DashboardMetrics['sloBreaches'];
  priorDeltas: DashboardMetrics['priorDeltas'];
  productionMode?: boolean;
}

/** The summary KPI cards. */
export function KpiCards({ isDark, data, days, selectedPipelines, kpi, avgEndpointingMs, avgProviderEndpointingMs, sloBreaches, priorDeltas, productionMode = false }: KpiCardsProps) {
  return (
    <div className={`grid grid-cols-2 ${productionMode ? 'lg:grid-cols-3 xl:grid-cols-4' : 'lg:grid-cols-4'} gap-4`}>
      <KpiCard
        icon={Activity}
        label="Sessions"
        value={String(kpi.total_sessions)}
        sub={
          (kpi.failed_sessions ?? 0) > 0
            ? `${kpi.failed_sessions} failed · last ${days === 1 ? 'day' : `${days} days`}`
            : `in last ${days === 1 ? 'day' : `${days} days`}`
        }
        iconColor="text-indigo-400"
        isDark={isDark}
        delta={productionMode ? { pct: priorDeltas.sessions } : undefined}
      />
      <KpiCard
        icon={MessageSquare}
        label="Turns"
        value={String(kpi.total_utterances)}
        sub={`${kpi.total_sessions > 0 ? Math.round(kpi.total_utterances / kpi.total_sessions) : 0} avg per session`}
        iconColor="text-emerald-400"
        isDark={isDark}
        delta={productionMode ? { pct: priorDeltas.turns } : undefined}
      />
      <KpiCard
        icon={XCircle}
        label="Error Rate"
        value={
          kpi.error_rate_pct != null
            ? `${kpi.error_rate_pct.toFixed(1)}%`
            : '—'
        }
        sub={(() => {
          const parts: string[] = [];
          if ((kpi.failed_turns ?? 0) > 0) parts.push(`${kpi.failed_turns} failed turns`);
          // Only show orchestrator overlay when looking at the unfiltered
          // global view — the overlay is not pipeline-scoped.
          if (
            selectedPipelines.size === 0 &&
            data?.kpi.orchestrator_errors_total != null &&
            data.kpi.orchestrator_errors_total > (kpi.failed_sessions ?? 0)
          ) {
            parts.push(`orch: ${data.kpi.orchestrator_errors_total} session err`);
          }
          return parts.length > 0 ? parts.join(' · ') : 'no failed turns';
        })()}
        iconColor={
          kpi.error_rate_pct != null && kpi.error_rate_pct > 1
            ? 'text-rose-400'
            : 'text-slate-400'
        }
        isDark={isDark}
        delta={productionMode ? { pct: priorDeltas.errorRate, lowerIsBetter: true } : undefined}
      />
      <KpiCard
        icon={Clock}
        label="Avg Latency"
        value={fmtMs(kpi.avg_latency_ms)}
        sub={(() => {
          const parts: string[] = [];
          if (kpi.p95_latency_ms != null) parts.push(`p95: ${fmtMs(kpi.p95_latency_ms)}`);
          if (productionMode && sloBreaches.latency != null && sloBreaches.latency > 0) {
            parts.push(`${sloBreaches.latency.toFixed(1)}% > ${(GLOBAL_SLO.total_p95_ms / 1000).toFixed(1)}s SLO`);
          }
          return parts.length > 0 ? parts.join(' · ') : undefined;
        })()}
        iconColor="text-amber-400"
        isDark={isDark}
        delta={productionMode ? { pct: priorDeltas.avgLatency, lowerIsBetter: true } : undefined}
      />
      {productionMode && (
        <>
          <KpiCard
            icon={Radio}
            label="Endpointing Delay"
            // Cascade VAD silence wait; when the window is S2S-only the
            // provider endpointing lag takes the headline slot.
            value={fmtMs(avgEndpointingMs ?? avgProviderEndpointingMs)}
            sub={(() => {
              const parts: string[] = [];
              if (avgEndpointingMs != null) {
                parts.push(
                  sloBreaches.endpointing != null && sloBreaches.endpointing > 0
                    ? `avg VAD silence wait · ${sloBreaches.endpointing.toFixed(1)}% > ${GLOBAL_SLO.endpointing_p95_ms}ms SLO`
                    : 'avg VAD silence wait',
                );
                if (avgProviderEndpointingMs != null) parts.push(`S2S provider: ${fmtMs(avgProviderEndpointingMs)}`);
              } else if (avgProviderEndpointingMs != null) {
                parts.push('avg S2S provider endpointing (provider VAD lag)');
              } else {
                parts.push('avg VAD silence wait');
              }
              return parts.join(' · ');
            })()}
            iconColor="text-rose-400"
            isDark={isDark}
          />
          <KpiCard
            icon={Zap}
            label="LLM Speed"
            value={kpi.avg_tokens_per_sec != null ? `${fmtNum(kpi.avg_tokens_per_sec)} tok/s` : '—'}
            sub={kpi.avg_tokens_per_sec != null ? 'generation throughput' : 'no LLM data'}
            iconColor="text-sky-400"
            isDark={isDark}
            delta={{ pct: priorDeltas.tps }}
          />
          {/* Spend KPI (P2 #11).  Backend reports spend across the entire
              day-window — it is *not* pipeline-filtered, so we always show
              the global figure here.  Sub line surfaces the top-spending
              provider plus its cap-utilisation %. */}
          <KpiCard
            icon={DollarSign}
            label={`Spend · ${days === 1 ? 'today' : `${days}d`}`}
            value={fmtUsd(kpi.spend_total_usd)}
            sub={(() => {
              const map = data?.kpi.spend_by_provider;
              if (!map || Object.keys(map).length === 0) return 'no priced usage';
              const top = Object.entries(map)
                .sort((a, b) => (b[1].total ?? 0) - (a[1].total ?? 0))[0];
              if (!top) return undefined;
              const [name, v] = top;
              const capPart = v.cap_pct != null ? ` · ${v.cap_pct.toFixed(0)}% of cap` : '';
              return `top: ${name} ${fmtUsd(v.total)}${capPart}`;
            })()}
            iconColor={(() => {
              const map = data?.kpi.spend_by_provider;
              if (!map) return 'text-emerald-400';
              const maxCap = Math.max(0, ...Object.values(map).map((v) => v.cap_pct ?? 0));
              if (maxCap >= 100) return 'text-rose-400';
              if (maxCap >= 80) return 'text-amber-400';
              return 'text-emerald-400';
            })()}
            isDark={isDark}
          />
        </>
      )}
    </div>
  );
}

interface AudioQualityKpisProps {
  isDark: boolean;
  filteredUtterances: DashboardMetrics['filteredUtterances'];
  kpi: KPI;
  audioQuality: DashboardMetrics['audioQuality'];
}

/** Client-perceived audio quality KPIs. */
export function AudioQualityKpis({ isDark, filteredUtterances, kpi, audioQuality }: AudioQualityKpisProps) {
  return (
    <div>
      <h3 className={`text-xs font-semibold uppercase tracking-wide mb-2 ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
        Audio Quality (client-perceived)
      </h3>
      <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-4 gap-4">
        <KpiCard
          icon={Radio}
          label="Avg Client TTFA"
          value={audioQuality.avgTtfaMs != null ? fmtMs(audioQuality.avgTtfaMs) : '—'}
          sub={`${audioQuality.ttfaSamples.toLocaleString()} sample${audioQuality.ttfaSamples === 1 ? '' : 's'}${audioQuality.avgBufferMs != null ? ` · client buffer ${fmtMs(audioQuality.avgBufferMs)}` : ''}`}
          iconColor="text-cyan-400"
          isDark={isDark}
        />
        <KpiCard
          icon={AlertTriangle}
          label="Turns w/ Audio Underruns"
          value={audioQuality.underrunPct != null ? `${audioQuality.underrunPct.toFixed(1)}%` : '—'}
          sub={`${audioQuality.underrunTurns.toLocaleString()} of ${filteredUtterances.length.toLocaleString()} turns`}
          iconColor={
            audioQuality.underrunPct == null
              ? (isDark ? 'text-slate-500' : 'text-gray-400')
              : audioQuality.underrunPct >= 5
                ? 'text-rose-400'
                : audioQuality.underrunPct >= 1
                  ? 'text-amber-400'
                  : 'text-emerald-400'
          }
          isDark={isDark}
        />
        <KpiCard
          icon={Zap}
          label="Avg Barge-in RTT"
          value={audioQuality.avgBargeRttMs != null ? fmtMs(audioQuality.avgBargeRttMs) : '—'}
          sub={`${audioQuality.rttSamples.toLocaleString()} sample${audioQuality.rttSamples === 1 ? '' : 's'}`}
          iconColor="text-violet-400"
          isDark={isDark}
        />
        {/* Output realtime factor:
            p90 of the per-turn `output.emit_rtf` over the filtered
            window — > 1.0 means the server emitted audio slower than
            it plays and the client starved after its prebuffer. The
            server rollup (`turns_over_realtime` / `avg_emit_rtf` /
            `max_emit_rtf`) is the fallback when the utterance rows
            predate the per-turn field. */}
        {(() => {
          const fromRows = audioQuality.emitRtfSamples > 0;
          const headline = fromRows ? audioQuality.emitRtfP90 : (kpi.avg_emit_rtf ?? null);
          const overRealtime = fromRows ? audioQuality.turnsOverRealtime : (kpi.turns_over_realtime ?? 0);
          const max = fromRows ? audioQuality.emitRtfMax : (kpi.max_emit_rtf ?? null);
          const subParts: string[] = [];
          if (fromRows) subParts.push(`${audioQuality.emitRtfSamples.toLocaleString()} turn${audioQuality.emitRtfSamples === 1 ? '' : 's'}`);
          else if (headline != null) subParts.push('window avg');
          if (headline != null) subParts.push(`${overRealtime.toLocaleString()} slower than realtime`);
          if (max != null) subParts.push(`max ${max.toFixed(2)}×`);
          return (
            <KpiCard
              icon={Radio}
              label={fromRows ? 'Output realtime factor (p90)' : 'Output realtime factor'}
              value={headline != null ? `${headline.toFixed(2)}×` : '—'}
              sub={subParts.length > 0 ? subParts.join(' · ') : 'no output throughput data'}
              iconColor={
                headline == null
                  ? (isDark ? 'text-slate-500' : 'text-gray-400')
                  : headline > 1.0 || overRealtime > 0
                    ? 'text-rose-400'
                    : headline > 0.9
                      ? 'text-amber-400'
                      : 'text-emerald-400'
              }
              isDark={isDark}
            />
          );
        })()}
      </div>
    </div>
  );
}

interface BusinessKpisProps {
  isDark: boolean;
  data: AnalyticsData;
  days: number;
  kpi: KPI;
  costEfficiency: NonNullable<DashboardMetrics['costEfficiency']>;
}

/** Business / cost-efficiency KPIs and session outcomes. */
export function BusinessKpis({ isDark, data, days, kpi, costEfficiency }: BusinessKpisProps) {
  return (
    <div className="space-y-4">
      <div>
        <h3 className={`text-xs font-semibold uppercase tracking-wide mb-2 ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
          Business · Cost efficiency
        </h3>
        <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-4">
          <KpiCard
            icon={DollarSign}
            label="Cost / session"
            value={fmtUsd(costEfficiency.costPerSession)}
            sub={`${kpi.total_sessions.toLocaleString()} session${kpi.total_sessions === 1 ? '' : 's'}`}
            iconColor="text-emerald-400"
            isDark={isDark}
          />
          <KpiCard
            icon={MessageSquare}
            label="Cost / turn"
            value={fmtUsd(costEfficiency.costPerTurn)}
            sub={`${kpi.total_utterances.toLocaleString()} turn${kpi.total_utterances === 1 ? '' : 's'}`}
            iconColor="text-teal-400"
            isDark={isDark}
          />
          <KpiCard
            icon={BarChart3}
            label="Projected monthly"
            value={costEfficiency.projectedMonthlySpend != null ? `$${Math.round(costEfficiency.projectedMonthlySpend).toLocaleString()}` : '—'}
            sub={`at the current ${days}-day run-rate`}
            iconColor="text-sky-400"
            isDark={isDark}
          />
        </div>
      </div>

      {/* Outcomes — sourced from the server KPI (session-level, so it
          reflects the server-side pipeline scope; hides when the
          captures carry no outcome tags). */}
      {data && (data.kpi.outcome_known ?? 0) > 0 && (
        <div>
          <h3 className={`text-xs font-semibold uppercase tracking-wide mb-2 ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
            Business · Outcomes
          </h3>
          <div className="grid grid-cols-1 sm:grid-cols-2 lg:grid-cols-3 gap-4">
            <KpiCard
              icon={Check}
              label="Containment rate"
              value={data.kpi.containment_rate_pct != null ? `${data.kpi.containment_rate_pct.toFixed(1)}%` : '—'}
              sub={`${(data.kpi.contained ?? 0).toLocaleString()} of ${(data.kpi.outcome_known ?? 0).toLocaleString()} resolved by AI`}
              iconColor="text-emerald-400"
              isDark={isDark}
            />
            <KpiCard
              icon={AlertTriangle}
              label="Escalation rate"
              value={data.kpi.escalation_rate_pct != null ? `${data.kpi.escalation_rate_pct.toFixed(1)}%` : '—'}
              sub={`${(data.kpi.escalated ?? 0).toLocaleString()} handed to a human`}
              iconColor={(data.kpi.escalation_rate_pct ?? 0) >= 20 ? 'text-amber-400' : 'text-slate-400'}
              isDark={isDark}
            />
            <KpiCard
              icon={Activity}
              label="Avg CSAT"
              value={data.kpi.avg_csat != null ? `${data.kpi.avg_csat.toFixed(2)} / 5` : '—'}
              sub={`${(data.kpi.csat_samples ?? 0).toLocaleString()} rating${(data.kpi.csat_samples ?? 0) === 1 ? '' : 's'}`}
              iconColor={
                data.kpi.avg_csat == null ? (isDark ? 'text-slate-500' : 'text-gray-400')
                  : data.kpi.avg_csat >= 4 ? 'text-emerald-400'
                    : data.kpi.avg_csat >= 3 ? 'text-amber-400'
                      : 'text-rose-400'
              }
              isDark={isDark}
            />
          </div>
        </div>
      )}
    </div>
  );
}
