import { useState } from 'react';
import { useParams, Link, useLocation } from 'react-router-dom';
import { ArrowLeft, Loader2, FlaskConical, Settings, BarChart3, BarChart2 } from 'lucide-react';
import { useTheme } from '../../contexts/ThemeContext';
import type { VariantResults } from '../../components/benchmark/types';
import { useExperiment, useExperimentResults, useSuite } from './useBenchmarkApi';
import VariantCard from '../../components/benchmark/VariantCard';
import ConfigComparisonTable from '../../components/benchmark/ConfigComparisonTable';
import ConfigDrawer from '../../components/benchmark/ConfigDrawer';
import { LatencyPercentilesChart, TtftTtfbChart, LatencySamplesChart } from '../../components/benchmark/LatencyChart';
import ScenarioTable from '../../components/benchmark/ScenarioTable';
import ExperimentConfig from '../../components/benchmark/ExperimentConfig';
import VariantSetupCard from '../../components/benchmark/VariantSetupCard';
import ScenariosTable from '../../components/benchmark/ScenariosTable';
import TtsEntriesTable from '../../components/benchmark/TtsEntriesTable';
import TtsCompareTable from '../../components/benchmark/TtsCompareTable';
import { buildCompareVariants } from '../../components/benchmark/ttsCompareHelpers';

const STATUS_BADGE: Record<string, { bg: string; text: string; label: string }> = {
  draft:    { bg: 'bg-slate-500/20', text: 'text-slate-400', label: 'Draft' },
  running:  { bg: 'bg-amber-500/20', text: 'text-amber-400', label: 'Running' },
  complete: { bg: 'bg-emerald-500/20', text: 'text-emerald-400', label: 'Complete' },
  failed:   { bg: 'bg-red-500/20', text: 'text-red-400', label: 'Failed' },
};

type TabId = 'setup' | 'results';

function findBestVariant(results: VariantResults[]): string | null {
  let bestId: string | null = null;
  let bestP50 = Infinity;
  for (const vr of results) {
    const p50 = vr.summary.p50_e2e_latency_ms;
    if (p50 != null && p50 < bestP50) {
      bestP50 = p50;
      bestId = vr.variant_id;
    }
  }
  return bestId;
}

export default function BenchmarkDetail() {
  const { id } = useParams<{ id: string }>();
  const { isDark } = useTheme();
  const location = useLocation();

  const { experiment, loading: expLoading } = useExperiment(id);
  const { results, loading: resultsLoading } = useExperimentResults(id);
  const { suite } = useSuite(experiment?.test_suite_id);

  // Scenario the user explicitly picked; null until they click a tab.
  const [selectedScenario, setSelectedScenario] = useState<string | null>(null);
  const [drawerVariant, setDrawerVariant] = useState<{ id: string; name: string } | null>(null);

  // Tab from URL hash, default based on status
  const hashTab = location.hash.replace('#', '') as TabId;
  const defaultTab: TabId = experiment?.status === 'draft' || experiment?.status === 'running' ? 'setup' : 'results';
  const activeTab: TabId = (hashTab === 'setup' || hashTab === 'results') ? hashTab : defaultTab;

  const loading = expLoading || resultsLoading;

  // Active scenario: the user's pick, else the first scenario of the first
  // variant once results are in (derived, so no effect is needed).
  const activeScenario: string | null =
    selectedScenario ?? results?.variant_results?.[0]?.scenarios?.[0]?.scenario_id ?? null;

  if (loading) {
    return (
      <div className="flex items-center justify-center h-full">
        <Loader2 size={24} className="animate-spin text-indigo-400" />
      </div>
    );
  }

  if (!experiment) {
    return (
      <div className="p-8">
        <Link to="/benchmarks" className={`flex items-center gap-2 text-sm mb-4 ${isDark ? 'text-slate-400 hover:text-white' : 'text-gray-500 hover:text-gray-900'}`}>
          <ArrowLeft size={16} /> Back to Benchmarks
        </Link>
        <p className={isDark ? 'text-slate-400' : 'text-gray-500'}>Experiment not found.</p>
      </div>
    );
  }

  const badge = STATUS_BADGE[experiment.status] ?? STATUS_BADGE.draft;
  const variantResults = results?.variant_results ?? [];
  const bestVariantId = findBestVariant(variantResults);

  // Collect unique scenario IDs across all variants
  const scenarioIds = [...new Set(variantResults.flatMap(vr => vr.scenarios.map(s => s.scenario_id)))];
  const scenarioNames = new Map<string, string>();
  for (const vr of variantResults) {
    for (const s of vr.scenarios) {
      scenarioNames.set(s.scenario_id, s.scenario_name);
    }
  }

  const tabs: { id: TabId; label: string; icon: React.ReactNode }[] = [
    { id: 'setup', label: 'Setup', icon: <Settings size={14} /> },
    { id: 'results', label: 'Results', icon: <BarChart3 size={14} /> },
  ];

  return (
    <div className="p-8 overflow-y-auto h-full">
      {/* Header */}
      <Link to="/benchmarks" className={`flex items-center gap-2 text-sm mb-4 ${isDark ? 'text-slate-400 hover:text-white' : 'text-gray-500 hover:text-gray-900'}`}>
        <ArrowLeft size={16} /> Back to Benchmarks
      </Link>

      <div className="flex items-center gap-3 mb-1">
        <FlaskConical size={24} className="text-indigo-400" />
        <h1 className={`text-2xl font-bold ${isDark ? 'text-white' : 'text-gray-900'}`}>{experiment.name}</h1>
        <span className={`px-2 py-0.5 rounded-full text-xs font-medium ${badge.bg} ${badge.text}`}>
          {badge.label}
          {experiment.status === 'running' && <Loader2 size={10} className="inline ml-1 animate-spin" />}
        </span>
      </div>

      {experiment.description && (
        <p className={`text-sm mb-2 ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>{experiment.description}</p>
      )}
      <p className={`text-xs mb-6 ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
        {experiment.variants.length} variant{experiment.variants.length !== 1 ? 's' : ''} · {experiment.repetitions} rep{experiment.repetitions !== 1 ? 's' : ''} · Created {new Date(experiment.created_at).toLocaleDateString()}
      </p>

      {/* Tab bar */}
      <div className={`flex gap-1 mb-6 border-b ${isDark ? 'border-slate-700' : 'border-gray-200'}`}>
        {tabs.map(tab => (
          <Link
            key={tab.id}
            to={`#${tab.id}`}
            className={`flex items-center gap-1.5 px-4 py-2.5 text-sm font-medium border-b-2 transition-colors -mb-px ${
              activeTab === tab.id
                ? isDark
                  ? 'border-indigo-400 text-white'
                  : 'border-indigo-500 text-gray-900'
                : isDark
                  ? 'border-transparent text-slate-400 hover:text-white hover:border-slate-600'
                  : 'border-transparent text-gray-500 hover:text-gray-900 hover:border-gray-300'
            }`}
          >
            {tab.icon}
            {tab.label}
          </Link>
        ))}
      </div>

      {/* ── Setup Tab ──────────────────────────────────── */}
      {activeTab === 'setup' && (
        <div className="space-y-6">
          {/* Experiment configuration */}
          <ExperimentConfig experiment={experiment} suiteName={suite?.name} />

          {/* Variant setup cards */}
          {experiment.variants.length > 0 && (
            <div>
              <h3 className={`text-sm font-semibold mb-3 ${isDark ? 'text-white' : 'text-gray-900'}`}>
                Variants ({experiment.variants.length})
              </h3>
              <div className={`grid gap-4 ${
                experiment.variants.length === 1 ? 'grid-cols-1 max-w-md'
                : experiment.variants.length === 2 ? 'grid-cols-2'
                : 'grid-cols-3'
              }`}>
                {experiment.variants.map(v => (
                  <VariantSetupCard
                    key={v.id}
                    variant={v}
                    onViewConfig={() => setDrawerVariant({ id: v.id, name: v.name })}
                  />
                ))}
              </div>
            </div>
          )}

          {experiment.variants.length === 0 && (
            <div className={`text-center py-10 rounded-xl border ${isDark ? 'bg-slate-800 border-slate-700 text-slate-500' : 'bg-white border-gray-200 text-gray-400'}`}>
              No variants defined
            </div>
          )}

          {/* Test scenarios from suite */}
          {suite && (
            <ScenariosTable scenarios={suite.scenarios} />
          )}

          {!suite && experiment.test_suite_id && (
            <div className={`text-center py-10 rounded-xl border ${isDark ? 'bg-slate-800 border-slate-700 text-slate-500' : 'bg-white border-gray-200 text-gray-400'}`}>
              <Loader2 size={16} className="inline animate-spin mr-2" />
              Loading test suite…
            </div>
          )}
        </div>
      )}

      {/* ── Results Tab ────────────────────────────────── */}
      {activeTab === 'results' && (
        <>
          {/* No results yet */}
          {variantResults.length === 0 && (
            <div className={`flex flex-col items-center justify-center py-24 rounded-xl border gap-3 ${isDark ? 'bg-slate-800/50 border-slate-700' : 'bg-white border-gray-200'}`}>
              {experiment.status === 'running' ? (
                <>
                  <div className={`w-14 h-14 rounded-2xl flex items-center justify-center ${isDark ? 'bg-slate-700/60' : 'bg-gray-100'}`}>
                    <Loader2 size={24} className="text-indigo-400 animate-spin" />
                  </div>
                  <span className={`text-base font-semibold ${isDark ? 'text-slate-300' : 'text-gray-600'}`}>Experiment running</span>
                  <span className={`text-sm ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>Results will appear as scenarios complete</span>
                </>
              ) : (
                <>
                  <div className={`w-14 h-14 rounded-2xl flex items-center justify-center ${isDark ? 'bg-slate-700/60' : 'bg-gray-100'}`}>
                    <BarChart2 size={24} className={isDark ? 'text-slate-500' : 'text-gray-400'} />
                  </div>
                  <span className={`text-base font-semibold ${isDark ? 'text-slate-300' : 'text-gray-600'}`}>No results yet</span>
                  <span className={`text-sm text-center max-w-xs leading-relaxed ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
                    Run the experiment from the{' '}
                    <Link to="#setup" className="text-indigo-400 hover:underline">Setup tab</Link>{' '}
                    to generate benchmark results
                  </span>
                </>
              )}
            </div>
          )}

          {/* Variant summary cards */}
          {variantResults.length > 0 && (
            <>
              <div className={`grid gap-4 mb-8 ${variantResults.length === 1 ? 'grid-cols-1 max-w-md' : variantResults.length === 2 ? 'grid-cols-2' : 'grid-cols-3'}`}>
                {variantResults.map(vr => (
                  <VariantCard
                    key={vr.variant_id}
                    variantName={vr.variant_name}
                    summary={vr.summary}
                    configSummary={vr.config_summary}
                    isBest={vr.variant_id === bestVariantId && variantResults.length > 1}
                    isDark={isDark}
                    onViewConfig={() => setDrawerVariant({ id: vr.variant_id, name: vr.variant_name })}
                    errorReason={vr.error_reason}
                  />
                ))}
              </div>

              {/* Config comparison table */}
              {variantResults.length > 1 && (
                <div className="mb-8">
                  <ConfigComparisonTable variantResults={variantResults} />
                </div>
              )}

              {/* Charts */}
              <div className="grid grid-cols-1 lg:grid-cols-2 gap-4 mb-8">
                <LatencyPercentilesChart variantResults={variantResults} />
                <TtftTtfbChart variantResults={variantResults} />
              </div>

              {/* Per-scenario breakdown */}
              {scenarioIds.length > 0 && (
                <div className={`rounded-xl border ${isDark ? 'bg-slate-800 border-slate-700' : 'bg-white border-gray-200'}`}>
                  <div className={`flex items-center border-b ${isDark ? 'border-slate-700' : 'border-gray-200'}`}>
                    <h3 className={`text-sm font-semibold px-5 py-4 ${isDark ? 'text-white' : 'text-gray-900'}`}>
                      Per-Scenario Breakdown
                    </h3>
                  </div>

                  {/* Scenario tabs */}
                  {scenarioIds.length > 1 && (
                    <div className={`flex gap-1 px-5 pt-3 border-b ${isDark ? 'border-slate-700' : 'border-gray-200'}`}>
                      {scenarioIds.map(sid => (
                        <button
                          key={sid}
                          onClick={() => setSelectedScenario(sid)}
                          className={`px-3 py-2 text-xs font-medium rounded-t-lg transition-colors ${
                            activeScenario === sid
                              ? isDark ? 'bg-slate-700 text-white' : 'bg-gray-100 text-gray-900'
                              : isDark ? 'text-slate-400 hover:text-white' : 'text-gray-500 hover:text-gray-900'
                          }`}
                        >
                          {scenarioNames.get(sid) ?? sid}
                        </button>
                      ))}
                    </div>
                  )}

                  {/* Active scenario content */}
                  {activeScenario && (
                    <div className="p-5">
                      <ScenarioTable
                        variantResults={variantResults}
                        scenarioId={activeScenario}
                        experimentId={experiment.id}
                        experimentName={experiment.name}
                        scenarioName={scenarioNames.get(activeScenario)}
                        variantConfigs={Object.fromEntries(
                          (experiment.variants ?? []).map(v => [v.id, { config_path: v.config_path, config_hash: v.config_hash }])
                        )}
                      />
                      <div className="mt-4">
                        <LatencySamplesChart variantResults={variantResults} scenarioId={activeScenario} />
                      </div>
                      {/* Per-entry TTS table — one block per variant that
                          recorded entries for this scenario. */}
                      {id && variantResults.map((vr) => {
                        const sc = vr.scenarios.find((s) => s.scenario_id === activeScenario);
                        if (!sc?.tts_entries?.length) return null;
                        return (
                          <div key={vr.variant_id} className="mt-6">
                            <h4 className={`text-xs font-semibold uppercase tracking-wide mb-2 ${
                              isDark ? 'text-slate-400' : 'text-gray-500'
                            }`}>
                              TTS entries — {vr.variant_name}
                            </h4>
                            <TtsEntriesTable
                              experimentId={id}
                              variantId={vr.variant_id}
                              scenarioId={activeScenario}
                              entries={sc.tts_entries}
                              isDark={isDark}
                            />
                          </div>
                        );
                      })}
                      {/* Side-by-side TTS audio compare. Only renders
                          when ≥2 variants produced entries. */}
                      {id && (() => {
                        const compareVariants = buildCompareVariants(variantResults, activeScenario);
                        if (compareVariants.length < 2) return null;
                        return (
                          <div className="mt-8">
                            <h4 className={`text-xs font-semibold uppercase tracking-wide mb-2 ${
                              isDark ? 'text-slate-400' : 'text-gray-500'
                            }`}>
                              Side-by-side audio
                            </h4>
                            <TtsCompareTable
                              experimentId={id}
                              scenarioId={activeScenario}
                              variants={compareVariants}
                              isDark={isDark}
                            />
                          </div>
                        );
                      })()}
                    </div>
                  )}
                </div>
              )}
            </>
          )}
        </>
      )}

      {/* Config drawer (slide-out panel with full TOML) */}
      {id && (
        <ConfigDrawer
          experimentId={id}
          variantId={drawerVariant?.id ?? ''}
          variantName={drawerVariant?.name ?? ''}
          open={!!drawerVariant}
          onClose={() => setDrawerVariant(null)}
        />
      )}
    </div>
  );
}
