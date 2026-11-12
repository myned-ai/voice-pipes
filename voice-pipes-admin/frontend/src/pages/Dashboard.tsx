import { useEffect, useState, useRef } from 'react';
import { Activity, AlertTriangle, Trash2, X } from 'lucide-react';
import { useNavigate } from 'react-router-dom';
import { useTheme } from '../contexts/ThemeContext';
import { GLOBAL_SLO } from '../lib/slo';
import PipelinesHealth from '../components/PipelinesHealth';
import SystemStatusStrip from '../components/SystemStatusStrip';
import type { ActivityTab, Agg, Bucket } from './dashboard/types';
import {
  DASHBOARD_DEFAULT_HIDDEN,
  DASHBOARD_PRODUCTION_MODE,
  DASHBOARD_PRODUCTION_ONLY_IDS,
  DASHBOARD_SECTION_IDS,
} from './dashboard/sections';
import { useCollapsed, usePagedTable } from './dashboard/tableState';
import { clearDashboardMetrics, useDashboardData } from './dashboard/useDashboardData';
import { useDashboardMetrics } from './dashboard/useDashboardMetrics';
import { DashboardHeader } from './dashboard/DashboardHeader';
import { AudioQualityKpis, BusinessKpis, KpiCards, QuotaAlerts, ResourceEfficiencyKpis } from './dashboard/KpiSections';
import { FailedSessionsTable, PipelineMatrixTable, ProviderSpendTable, TokenUsageTable } from './dashboard/DashboardTables';
import { CostByPipelineChart, LatencyByPipelineChart } from './dashboard/BucketedCharts';
import { PipelineFilter, ProviderChipFilters } from './dashboard/Filters';
import { EndToEndLatencyChart, LlmPerformanceChart, TokenUsageChart } from './dashboard/TimeSeriesCharts';
import { ActivityFeed } from './dashboard/ActivityFeed';

/* ── main Dashboard page ────────────────────────────────── */

export default function Dashboard({ productionMode = DASHBOARD_PRODUCTION_MODE }: { productionMode?: boolean }) {
  const { isDark } = useTheme();
  const navigate = useNavigate();
  const [days, setDays] = useState(productionMode ? 7 : 1);
  const [refreshToken, setRefreshToken] = useState(0);

  // Clear Metrics modal state
  const [clearModalOpen, setClearModalOpen] = useState(false);
  const [clearScopePipeline, setClearScopePipeline] = useState<string>('');
  const [deleteCaptures, setDeleteCaptures] = useState(false);
  const [clearing, setClearing] = useState(false);
  const [clearError, setClearError] = useState<string | null>(null);
  const [dismissedSessionIds, setDismissedSessionIds] = useState<Set<string>>(() => new Set());
  const [dismissError, setDismissError] = useState<string | null>(null);

  // Activity-feed tab: persisted so toggling `days` (or re-render) keeps the
  // user's last view.  'anomalies' is the historical default before tabs.
  const [activityTab, setActivityTab] = useState<ActivityTab>(() => {
    try {
      const v = localStorage.getItem('dashboard.activityTab');
      return v === 'issues' ? 'issues' : 'anomalies';
    } catch {
      return 'anomalies';
    }
  });
  useEffect(() => {
    try { localStorage.setItem('dashboard.activityTab', activityTab); } catch { /* ignore */ }
  }, [activityTab]);
  const [bucket, setBucket] = useState<Bucket>('auto');
  const [agg, setAgg] = useState<Agg>('p95');

  // Section visibility — persisted per operator so the dashboard can be tailored
  // to what each user cares about.  Stored as the set of HIDDEN ids (so sections
  // added in future default to visible).  Hiding heavy chart sections also cuts
  // client-side render cost (recharts is expensive), which helps perceived load.
  const [hiddenSections, setHiddenSections] = useState<Set<string>>(() => {
    try {
      const raw = localStorage.getItem('dashboard.hiddenSections');
      if (!raw) return new Set(DASHBOARD_DEFAULT_HIDDEN);
      const parsed: unknown = JSON.parse(raw);
      if (Array.isArray(parsed)) {
        return new Set(parsed.filter((x): x is string => typeof x === 'string' && DASHBOARD_SECTION_IDS.has(x)));
      }
      return new Set(DASHBOARD_DEFAULT_HIDDEN);
    } catch {
      return new Set(DASHBOARD_DEFAULT_HIDDEN);
    }
  });
  useEffect(() => {
    try { localStorage.setItem('dashboard.hiddenSections', JSON.stringify(Array.from(hiddenSections))); } catch { /* ignore */ }
  }, [hiddenSections]);
  const isVisible = (id: string) => {
    if (!productionMode && DASHBOARD_PRODUCTION_ONLY_IDS.has(id)) return false;
    return !hiddenSections.has(id);
  };
  const [customizeOpen, setCustomizeOpen] = useState(false);
  const customizeMenuRef = useRef<HTMLDivElement | null>(null);

  // Empty set means "all pipelines" (matches the legacy 'all' behavior).
  const [selectedPipelines, setSelectedPipelines] = useState<Set<string>>(() => new Set());
  const [pipelineMenuOpen, setPipelineMenuOpen] = useState(false);
  const pipelineMenuRef = useRef<HTMLDivElement | null>(null);

  // P3 #15 — Provider / model chip filters.  Empty Set means "all".  Applied
  // alongside the pipeline filter inside `filteredUtterances`.
  const [selectedSttProviders, setSelectedSttProviders] = useState<Set<string>>(() => new Set());
  const [selectedTtsProviders, setSelectedTtsProviders] = useState<Set<string>>(() => new Set());
  const [selectedLlmModels, setSelectedLlmModels] = useState<Set<string>>(() => new Set());

  // Recharts onClick passes its event-state object containing activePayload
  // when a data point or its band is clicked. We extract the session_id
  // from the underlying chartData row and navigate to SessionDetail.
  const handleChartClick = (e: unknown) => {
    if (!e || typeof e !== 'object') return;
    const payload = (e as { activePayload?: Array<{ payload?: { session_id?: string } }> }).activePayload;
    const sid = payload?.[0]?.payload?.session_id;
    if (sid) navigate(`/sessions/${sid}`);
  };

  // Close the multi-select on outside click.
  useEffect(() => {
    if (!pipelineMenuOpen) return;
    const handler = (e: MouseEvent) => {
      if (pipelineMenuRef.current && !pipelineMenuRef.current.contains(e.target as Node)) {
        setPipelineMenuOpen(false);
      }
    };
    document.addEventListener('mousedown', handler);
    return () => document.removeEventListener('mousedown', handler);
  }, [pipelineMenuOpen]);

  // Close the customize menu on outside click.
  useEffect(() => {
    if (!customizeOpen) return;
    const handler = (e: MouseEvent) => {
      if (customizeMenuRef.current && !customizeMenuRef.current.contains(e.target as Node)) {
        setCustomizeOpen(false);
      }
    };
    document.addEventListener('mousedown', handler);
    return () => document.removeEventListener('mousedown', handler);
  }, [customizeOpen]);

  // Close the Clear Metrics modal on Escape key.
  useEffect(() => {
    if (!clearModalOpen) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape' && !clearing) {
        setClearModalOpen(false);
      }
    };
    document.addEventListener('keydown', handler);
    return () => document.removeEventListener('keydown', handler);
  }, [clearModalOpen, clearing]);

  const { data, issues, issuesError, error, priorData } = useDashboardData(
    days,
    selectedPipelines,
    productionMode,
    refreshToken,
  );
  // Full-page loader only before the first successful load; later refetches
  // (days / pipeline filter) update the data in place. Derived rather than
  // stored so the fetch effect never has to set state synchronously.
  const loading = data === null && error === null;


  const {
    pipelineNames,
    sttProviderOptions,
    ttsProviderOptions,
    llmModelOptions,
    filteredUtterances,
    chartData,
    e2eYDomain,
    kpi,
    costEfficiency,
    avgEndpointingMs,
    avgProviderEndpointingMs,
    audioQuality,
    sloBreaches,
    priorDeltas,
    failedSessionsInView,
    pipelineMatrix,
    bucketedSeries,
    bucketedCost,
    tokensByPipeline,
    failedSessionsSorted,
    spendRows,
    anomalySessions,
  } = useDashboardMetrics({
    data,
    priorData,
    days,
    bucket,
    agg,
    selectedPipelines,
    selectedSttProviders,
    selectedTtsProviders,
    selectedLlmModels,
  });

  const openClearModal = (presetPipeline = '') => {
    setClearScopePipeline(presetPipeline);
    setDeleteCaptures(false);
    setClearError(null);
    setClearModalOpen(true);
  };

  const handleConfirmClear = async () => {
    setClearing(true);
    setClearError(null);
    try {
      await clearDashboardMetrics({
        pipeline: clearScopePipeline || null,
        deleteCaptures,
      });
      setClearModalOpen(false);
      setRefreshToken((t) => t + 1);
    } catch (err) {
      setClearError(err instanceof Error ? err.message : 'Failed to clear metrics');
    } finally {
      setClearing(false);
    }
  };

  const handleDismissSession = async (sessionId: string) => {
    setDismissError(null);
    setDismissedSessionIds((prev) => {
      const next = new Set(prev);
      next.add(sessionId);
      return next;
    });
    try {
      await clearDashboardMetrics({
        sessionId,
        deleteCaptures: false,
      });
      setRefreshToken((t) => t + 1);
    } catch (err) {
      setDismissedSessionIds((prev) => {
        const next = new Set(prev);
        next.delete(sessionId);
        return next;
      });
      setDismissError(
        err instanceof Error
          ? `Failed to dismiss session (${err.message}). If you recently updated the backend, restart the backend server on port 8000.`
          : 'Failed to dismiss session.',
      );
    }
  };

  const visibleFailedSessionsInView = failedSessionsInView.filter(
    (fs) => !dismissedSessionIds.has(fs.session_id),
  );
  const visibleFailedSessionsSorted = failedSessionsSorted.filter(
    (fs) => !dismissedSessionIds.has(fs.session_id),
  );


  const axisStroke = isDark ? '#475569' : '#d1d5db';
  const tickFill = isDark ? '#94a3b8' : '#6b7280';
  const gridStroke = isDark ? '#1e293b' : '#f3f4f6';


  const dayOptions = [
    { value: 1, label: 'Today' },
    { value: 7, label: '7 days' },
    { value: 30, label: '30 days' },
    { value: 90, label: '90 days' },
  ];


  const [failedCollapsed, toggleFailed] = useCollapsed('dashboard.collapse.failedSessions');
  const [matrixCollapsed, toggleMatrix] = useCollapsed('dashboard.collapse.pipelineMatrix');
  const [tokenCollapsed, toggleToken] = useCollapsed('dashboard.collapse.tokenTable');
  const [spendCollapsed, toggleSpend] = useCollapsed('dashboard.collapse.providerSpend');

  const failedPaged = usePagedTable(visibleFailedSessionsSorted, 25);
  const matrixPaged = usePagedTable(pipelineMatrix, 15);
  const tokenPaged = usePagedTable(tokensByPipeline, 15);
  const spendPaged = usePagedTable(spendRows, 15);

  const anomalyPaged = usePagedTable(anomalySessions, 15);
  const issuesPaged = usePagedTable(issues, 20);

  /* ── render ──────────────────────────────────────────── */

  return (
    <div className={`p-8 h-full flex flex-col overflow-y-auto transition-colors ${isDark ? 'bg-[#0f172a] text-slate-200' : 'bg-gray-50 text-gray-800'}`}>
      <div className="max-w-7xl mx-auto w-full flex flex-col gap-6 flex-1">
        {/* Header */}
        <DashboardHeader
          isDark={isDark}
          days={days}
          setDays={setDays}
          hiddenSections={hiddenSections}
          setHiddenSections={setHiddenSections}
          customizeOpen={customizeOpen}
          setCustomizeOpen={setCustomizeOpen}
          customizeMenuRef={customizeMenuRef}
          dayOptions={dayOptions}
          productionMode={productionMode}
          onOpenClearModal={() => openClearModal('')}
        />

        {/* L0 — glanceable "is the system healthy?" verdict synthesised from the
            window KPIs, shown above the detail so the answer is one-glance. */}
        {productionMode && kpi && <SystemStatusStrip kpi={kpi} slo={GLOBAL_SLO} latencyBreachPct={sloBreaches.latency} />}

        {/* Live pipelines & component health — independent of historical analytics. */}
        {isVisible('health') && <PipelinesHealth productionMode={productionMode} />}

        {loading ? (
          <div className="flex justify-center items-center p-20">
            <Activity className="animate-spin text-indigo-500" size={32} />
          </div>
        ) : error ? (
          <div className={`rounded-xl p-12 text-center border ${isDark ? 'bg-slate-800/50 border-slate-700' : 'bg-white border-gray-200'}`}>
            <AlertTriangle className="mx-auto mb-3 text-amber-500" size={32} />
            <p className={isDark ? 'text-slate-400' : 'text-gray-500'}>{error}</p>
          </div>
        ) : data && kpi ? (
          <>
            {/* ── Quota alerts (P2 #13) ───────────────────────
                Surfaces any provider whose window-scoped spend has crossed
                80% of its configured monthly cap.  Red at >=100%, amber
                otherwise.  Hidden when no providers are at risk. */}
            {productionMode && <QuotaAlerts isDark={isDark} data={data} />}

            {/* KPI Cards */}
            {isVisible('kpis') && (
            <KpiCards
              isDark={isDark}
              data={data}
              days={days}
              selectedPipelines={selectedPipelines}
              kpi={kpi}
              avgEndpointingMs={avgEndpointingMs}
              avgProviderEndpointingMs={avgProviderEndpointingMs}
              sloBreaches={sloBreaches}
              priorDeltas={priorDeltas}
              productionMode={productionMode}
            />
            )}

            {/* ── Resource efficiency (CPU & RAM) ─────────────
                Worker process memory footprint (RSS) and active pipeline
                compute per turn. Toggleable in the Customize menu and
                persisted in localStorage. */}
            {isVisible('resources') && (kpi.avg_peak_rss_bytes != null || kpi.avg_turn_cpu_ms != null || kpi.max_container_peak_bytes != null) && (
              <ResourceEfficiencyKpis
                isDark={isDark}
                kpi={kpi}
              />
            )}

            {/* ── Audio quality KPIs (P3 #16) ───────────────
                Client-perceived metrics extracted from per-utterance fields.
                Hidden when no samples exist for any of the three (e.g.
                non-WebRTC pipelines never emit `client_ttfa_ms`). */}
            {isVisible('audioQuality') && (audioQuality.ttfaSamples > 0 || audioQuality.rttSamples > 0 || audioQuality.underrunTurns > 0 || audioQuality.emitRtfSamples > 0 || (kpi.turns_over_realtime ?? 0) > 0 || (filteredUtterances.length > 0 && audioQuality.underrunPct != null)) && (
              <AudioQualityKpis
                isDark={isDark}
                filteredUtterances={filteredUtterances}
                kpi={kpi}
                audioQuality={audioQuality}
              />
            )}

            {/* Business / cost-efficiency: cost-per-call is the headline
                voice-ops business metric; per-pipeline when a filter is active. */}
            {isVisible('business') && costEfficiency && kpi.total_sessions > 0 && (
              <BusinessKpis
                isDark={isDark}
                data={data}
                days={days}
                kpi={kpi}
                costEfficiency={costEfficiency}
              />
            )}

            {dismissError && (
              <div className={`text-xs rounded-lg border px-4 py-2.5 ${isDark ? 'bg-rose-950/40 border-rose-800 text-rose-300' : 'bg-rose-50 border-rose-200 text-rose-700'}`}>
                {dismissError}
              </div>
            )}

            {/* Failed sessions panel — only when there is anything to show. */}
            {isVisible('failedSessions') && visibleFailedSessionsInView.length > 0 && (
              <FailedSessionsTable
                isDark={isDark}
                kpi={kpi}
                failedSessionsInView={visibleFailedSessionsInView}
                failedCollapsed={failedCollapsed}
                toggleFailed={toggleFailed}
                failedPaged={failedPaged}
                onDismissSession={handleDismissSession}
              />
            )}

            {/* ── Per-pipeline matrix ────────────────────────── */}
            {isVisible('pipelineMatrix') && pipelineMatrix.length > 0 && (
              <PipelineMatrixTable
                isDark={isDark}
                pipelineMatrix={pipelineMatrix}
                matrixCollapsed={matrixCollapsed}
                toggleMatrix={toggleMatrix}
                matrixPaged={matrixPaged}
                onClearPipeline={(pipelineName) => openClearModal(pipelineName)}
              />
            )}

            {/* ── Bucketed latency by pipeline ───────────────── */}
            {isVisible('latencyByPipeline') && bucketedSeries.rows.length > 0 && bucketedSeries.pipelines.length > 0 && (
              <LatencyByPipelineChart
                isDark={isDark}
                bucket={bucket}
                setBucket={setBucket}
                agg={agg}
                setAgg={setAgg}
                bucketedSeries={bucketedSeries}
                axisStroke={axisStroke}
                tickFill={tickFill}
                gridStroke={gridStroke}
              />
            )}

            {/* ── Bucketed LLM cost by pipeline (P2 #11) ───────
                Stacked area: each band is one pipeline's LLM spend per
                bucket.  STT/TTS cost is session-scoped and not chartable on
                the same time axis, so this surfaces the per-turn cost trend
                only.  Only renders when at least one priced LLM call was
                observed in the window. */}
            {isVisible('costByPipeline') && bucketedCost.rows.length > 0 && bucketedCost.pipelines.length > 0 && (
              <CostByPipelineChart
                isDark={isDark}
                bucketedCost={bucketedCost}
                axisStroke={axisStroke}
                tickFill={tickFill}
                gridStroke={gridStroke}
              />
            )}

            {/* Pipeline multi-select filter (replaces the legacy provider-changes timeline) */}
            {isVisible('filters') && pipelineNames.length > 1 && (
              <PipelineFilter
                isDark={isDark}
                selectedPipelines={selectedPipelines}
                setSelectedPipelines={setSelectedPipelines}
                pipelineMenuOpen={pipelineMenuOpen}
                setPipelineMenuOpen={setPipelineMenuOpen}
                pipelineMenuRef={pipelineMenuRef}
                pipelineNames={pipelineNames}
              />
            )}

            {/* ── Provider / model chip filters (P3 #15) ─────
                Three independent multi-selects rendered as compact chip
                rows.  Visible only when the option set has 2+ values for
                that dimension (single-value filters are useless).  Empty
                selection means "all". */}
            {productionMode && isVisible('filters') && (sttProviderOptions.length > 1 || ttsProviderOptions.length > 1 || llmModelOptions.length > 1) && (
              <ProviderChipFilters
                isDark={isDark}
                selectedSttProviders={selectedSttProviders}
                setSelectedSttProviders={setSelectedSttProviders}
                selectedTtsProviders={selectedTtsProviders}
                setSelectedTtsProviders={setSelectedTtsProviders}
                selectedLlmModels={selectedLlmModels}
                setSelectedLlmModels={setSelectedLlmModels}
                sttProviderOptions={sttProviderOptions}
                ttsProviderOptions={ttsProviderOptions}
                llmModelOptions={llmModelOptions}
              />
            )}

            {chartData.length === 0 ? (
              productionMode && (
                <div className={`rounded-xl p-12 text-center border ${isDark ? 'bg-slate-800/50 border-slate-700' : 'bg-white border-gray-200'}`}>
                  <p className={isDark ? 'text-slate-400' : 'text-gray-500'}>No turn data available for the selected period.</p>
                </div>
              )
            ) : (
              <>
                {/* ── End-to-End Latency Chart ───────────────── */}
                {isVisible('latencyChart') && (
                <EndToEndLatencyChart
                  isDark={isDark}
                  data={data}
                  handleChartClick={handleChartClick}
                  chartData={chartData}
                  e2eYDomain={e2eYDomain}
                  axisStroke={axisStroke}
                  tickFill={tickFill}
                  gridStroke={gridStroke}
                />
                )}

                {/* ── LLM Performance Chart ──────────────────── */}
                {isVisible('llmChart') && (
                <LlmPerformanceChart
                  isDark={isDark}
                  data={data}
                  handleChartClick={handleChartClick}
                  chartData={chartData}
                  axisStroke={axisStroke}
                  tickFill={tickFill}
                  gridStroke={gridStroke}
                />
                )}

                {/* ── Token Usage Chart ──────────────────────── */}
                {isVisible('tokenChart') && (
                <TokenUsageChart
                  isDark={isDark}
                  data={data}
                  handleChartClick={handleChartClick}
                  chartData={chartData}
                  axisStroke={axisStroke}
                  tickFill={tickFill}
                  gridStroke={gridStroke}
                />
                )}
                {/* ── Token Usage by Pipeline ───────────────── */}
                {isVisible('tokenTable') && tokensByPipeline.length > 0 && (
                  <TokenUsageTable
                    isDark={isDark}
                    tokenCollapsed={tokenCollapsed}
                    toggleToken={toggleToken}
                    tokenPaged={tokenPaged}
                  />
                )}

                {/* ── Provider Spend (P2 #12) ───────────────────
                    Driven by ``kpi.spend_by_provider`` — covers the whole
                    day-window (not pipeline-filtered, since pricing is a
                    provider-level concern).  Cap chip turns amber at 80% and
                    rose at 100%. */}
                {data?.kpi.spend_by_provider && Object.keys(data.kpi.spend_by_provider).length > 0 && isVisible('providerSpend') && (
                  <ProviderSpendTable
                    isDark={isDark}
                    days={days}
                    spendCollapsed={spendCollapsed}
                    toggleSpend={toggleSpend}
                    spendPaged={spendPaged}
                  />
                )}

                {/* ── Activity feed (tabbed) ─────────────────
                    Combines "Anomalies in window" (per-window rubric) and
                    "Recent issues" (SQLite-persisted cross-cutting feed) into
                    a single card with tab navigation.  Failed-sessions stays
                    standalone above so its red-alert prominence is preserved.
                    Tab selection persists in localStorage. */}
                {isVisible('activity') && (
                  <ActivityFeed
                    isDark={isDark}
                    navigate={navigate}
                    data={data}
                    issues={issues}
                    issuesError={issuesError}
                    days={days}
                    activityTab={activityTab}
                    setActivityTab={setActivityTab}
                    kpi={kpi}
                    anomalyPaged={anomalyPaged}
                    issuesPaged={issuesPaged}
                  />
                )}
              </>
            )}
          </>
        ) : null}
      </div>

      {/* Clear Metrics Confirmation Modal */}
      {clearModalOpen && (
        <div
          role="dialog"
          aria-modal="true"
          aria-labelledby="clear-metrics-title"
          className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4"
          onClick={(e) => {
            if (e.target === e.currentTarget && !clearing) setClearModalOpen(false);
          }}
        >
          <div
            className={`w-full max-w-md rounded-xl border p-6 shadow-2xl ${
              isDark ? 'bg-slate-800 border-slate-700 text-slate-200' : 'bg-white border-gray-200 text-gray-800'
            }`}
          >
            <div className="flex items-center justify-between mb-4">
              <h2 id="clear-metrics-title" className="text-lg font-semibold flex items-center gap-2">
                <Trash2 size={18} className="text-rose-400" />
                Clear Dashboard Metrics
              </h2>
              <button
                type="button"
                aria-label="Close modal"
                disabled={clearing}
                onClick={() => setClearModalOpen(false)}
                className={`p-1 rounded-lg ${isDark ? 'text-slate-400 hover:bg-slate-700' : 'text-gray-400 hover:bg-gray-100'}`}
              >
                <X size={16} />
              </button>
            </div>

            <p className={`text-sm mb-4 ${isDark ? 'text-slate-400' : 'text-gray-600'}`}>
              Reset historical metrics and failed session alerts on the dashboard so broken test runs no longer clutter your view.
            </p>

            <div className="mb-4">
              <label
                htmlFor="clear-scope-select"
                className={`block text-xs font-semibold uppercase tracking-wide mb-1.5 ${
                  isDark ? 'text-slate-400' : 'text-gray-500'
                }`}
              >
                Scope
              </label>
              <select
                id="clear-scope-select"
                value={clearScopePipeline}
                onChange={(e) => setClearScopePipeline(e.target.value)}
                disabled={clearing}
                className={`w-full rounded-lg border px-3 py-2 text-sm outline-none ${
                  isDark
                    ? 'bg-slate-900 border-slate-700 text-slate-200'
                    : 'bg-gray-50 border-gray-300 text-gray-800'
                }`}
              >
                <option value="">All pipelines (reset entire dashboard)</option>
                {pipelineNames.map((p) => (
                  <option key={p} value={p}>
                    Pipeline: {p}
                  </option>
                ))}
              </select>
            </div>

            <label
              className={`flex items-start gap-2.5 p-3 rounded-lg border mb-4 cursor-pointer text-sm ${
                deleteCaptures
                  ? isDark
                    ? 'bg-rose-950/30 border-rose-800/70 text-rose-200'
                    : 'bg-rose-50 border-rose-300 text-rose-800'
                  : isDark
                    ? 'bg-slate-900/50 border-slate-700 text-slate-300'
                    : 'bg-gray-50 border-gray-200 text-gray-700'
              }`}
            >
              <input
                type="checkbox"
                checked={deleteCaptures}
                onChange={(e) => setDeleteCaptures(e.target.checked)}
                disabled={clearing}
                className="mt-0.5 rounded border-gray-400 text-rose-600 focus:ring-rose-500"
              />
              <div>
                <div className="font-medium">Also delete recorded session capture files from disk</div>
                <div className={`text-xs mt-0.5 ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
                  If unchecked, session recordings remain available in the Sessions tab while dashboard metrics start fresh from now.
                </div>
              </div>
            </label>

            {clearError && (
              <div className={`text-xs rounded-lg border px-3 py-2 mb-4 ${isDark ? 'bg-rose-950/40 border-rose-800 text-rose-300' : 'bg-rose-50 border-rose-200 text-rose-700'}`}>
                {clearError}
              </div>
            )}

            <div className="flex items-center justify-end gap-2">
              <button
                type="button"
                disabled={clearing}
                onClick={() => setClearModalOpen(false)}
                className={`px-4 py-2 text-sm font-medium rounded-lg border transition-colors ${
                  isDark
                    ? 'bg-slate-800 border-slate-700 text-slate-300 hover:bg-slate-700'
                    : 'bg-white border-gray-300 text-gray-700 hover:bg-gray-100'
                }`}
              >
                Cancel
              </button>
              <button
                type="button"
                disabled={clearing}
                onClick={handleConfirmClear}
                className="px-4 py-2 text-sm font-medium rounded-lg bg-rose-600 text-white hover:bg-rose-500 disabled:opacity-50 transition-colors"
              >
                {clearing ? 'Clearing…' : 'Confirm Clear'}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
