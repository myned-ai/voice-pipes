import { useState } from 'react';
import axios from 'axios';
import { CheckCircle, XCircle, AlertTriangle, Clock, ClipboardCopy, Check } from 'lucide-react';
import { useTheme } from '../../contexts/ThemeContext';
import type { VariantResults, ScenarioRunStatus } from './types';
import MetricTooltip from './MetricTooltip';
import { buildScenarioReportForLlm } from './scenarioReportForLlm';
import { API_BASE } from '../../config/api';

function fmtMs(ms: number | null | undefined): string {
  if (ms == null) return '—';
  if (ms < 1000) return `${Math.round(ms)} ms`;
  return `${(ms / 1000).toFixed(1)} s`;
}

function StatusIcon({ status }: { status: ScenarioRunStatus }) {
  switch (status) {
    case 'passed':  return <CheckCircle size={14} className="text-emerald-400" />;
    case 'failed':  return <XCircle size={14} className="text-red-400" />;
    case 'timeout': return <Clock size={14} className="text-amber-400" />;
    case 'error':   return <AlertTriangle size={14} className="text-red-400" />;
  }
}

interface Props {
  variantResults: VariantResults[];
  scenarioId: string;
  /** Optional context used to build the per-(variant,scenario) LLM
   *  diagnostic report.  When provided, a small clipboard button is
   *  rendered next to each variant name in the table header. */
  experimentId?: string;
  experimentName?: string;
  scenarioName?: string;
  variantConfigs?: Record<string, { config_path?: string | null; config_hash?: string | null }>;
}

export default function ScenarioTable({
  variantResults,
  scenarioId,
  experimentId,
  experimentName,
  scenarioName,
  variantConfigs,
}: Props) {
  const { isDark } = useTheme();
  // Tracks which variant's copy button is showing the success tick. We
  // key by variant_id so two side-by-side variants don't clobber each
  // other's transient "Copied!" state.
  const [copiedVariantId, setCopiedVariantId] = useState<string | null>(null);

  const scenarios = variantResults.map(vr => ({
    variantId: vr.variant_id,
    variantName: vr.variant_name,
    result: vr.scenarios.find(s => s.scenario_id === scenarioId),
  }));

  if (scenarios.every(s => !s.result)) return null;

  const copyReport = async (variantId: string, variantName: string) => {
    if (!experimentId) return;
    const result = scenarios.find(s => s.variantId === variantId)?.result;
    if (!result) return;
    const cfg = variantConfigs?.[variantId];
    const sessionIds = result.session_ids ?? [];
    const sessionsMissingArtifacts = (await Promise.all(
      sessionIds.map(async (sessionId) => {
        try {
          const res = await axios.get(`${API_BASE}/api/v1/sessions/${encodeURIComponent(sessionId)}`, {
            // We only need a lightweight existence signal.
            params: { events_offset: 0, events_limit: 1 },
          });
          const d = res.data?.data;
          const hasUtterances = Array.isArray(d?.utterances) && d.utterances.length > 0;
          const hasEvents = Array.isArray(d?.events) && d.events.length > 0;
          const hasTurns = Array.isArray(d?.turns) && d.turns.length > 0;
          return hasUtterances || hasEvents || hasTurns ? null : sessionId;
        } catch {
          // If session detail is unreachable or missing, treat it as unavailable
          // so the copied report carries a concrete diagnostic breadcrumb.
          return sessionId;
        }
      }),
    )).filter((v): v is string => Boolean(v));

    const markdown = buildScenarioReportForLlm({
      experimentId,
      experimentName,
      scenarioId,
      scenarioName,
      variantId,
      variantName,
      configPath: cfg?.config_path,
      configHash: cfg?.config_hash,
      sessionsMissingArtifacts,
      result,
    });
    try {
      await navigator.clipboard.writeText(markdown);
      setCopiedVariantId(variantId);
      setTimeout(() => setCopiedVariantId(prev => (prev === variantId ? null : prev)), 2000);
    } catch (err) {
      console.error('Failed to copy scenario report:', err);
    }
  };

  const headerCell = `text-xs font-medium uppercase tracking-wide px-4 py-3 text-left ${
    isDark ? 'text-slate-400' : 'text-gray-500'
  }`;
  const cell = `px-4 py-3 text-sm font-mono ${isDark ? 'text-slate-200' : 'text-gray-800'}`;
  const rowBorder = isDark ? 'border-slate-700' : 'border-gray-200';

  const rows: { label: string; values: (string | React.ReactNode)[] }[] = [
    {
      label: 'Status',
      values: scenarios.map(s =>
        s.result ? (
          <span className="flex items-center gap-1.5">
            <StatusIcon status={s.result.status} />
            {s.result.status}
          </span>
        ) : '—'
      ),
    },
    {
      label: 'E2E Latency',
      values: scenarios.map(s => fmtMs(s.result?.metrics.e2e_latency_ms)),
    },
    {
      label: 'TTFT',
      values: scenarios.map(s => fmtMs(s.result?.metrics.ttft_ms)),
    },
    {
      label: 'TTFB',
      values: scenarios.map(s => fmtMs(s.result?.metrics.ttfb_ms)),
    },
  ];

  // Per-component metrics (only show rows when any variant has data).
  if (scenarios.some(s => s.result?.metrics.stt_latency_ms != null)) {
    rows.push({
      label: 'STT Latency',
      values: scenarios.map(s => fmtMs(s.result?.metrics.stt_latency_ms)),
    });
  }
  if (scenarios.some(s => s.result?.metrics.llm_generation_ms != null)) {
    rows.push({
      label: 'LLM Generation',
      values: scenarios.map(s => fmtMs(s.result?.metrics.llm_generation_ms)),
    });
  }
  if (scenarios.some(s => s.result?.metrics.llm_tokens != null)) {
    rows.push({
      label: 'LLM Tokens',
      values: scenarios.map(s => s.result?.metrics.llm_tokens?.toString() ?? '—'),
    });
  }
  if (scenarios.some(s => s.result?.metrics.tts_ttfb_ms != null)) {
    rows.push({
      label: 'TTS TTFB',
      values: scenarios.map(s => fmtMs(s.result?.metrics.tts_ttfb_ms)),
    });
  }

  // Only add WER row if any variant has it
  if (scenarios.some(s => s.result?.scoring?.wer != null)) {
    rows.push({
      label: 'WER',
      values: scenarios.map(s =>
        s.result?.scoring?.wer != null ? (s.result.scoring.wer * 100).toFixed(1) + '%' : '—'
      ),
    });
  }

  rows.push(
    {
      label: 'STT Text',
      values: scenarios.map(s => (
        <span className="font-sans text-xs whitespace-pre-wrap">
          {s.result?.metrics.stt_text ?? '—'}
        </span>
      )),
    },
    {
      label: 'Response',
      values: scenarios.map(s => (
        <span className="font-sans text-xs whitespace-pre-wrap max-w-xs block">
          {s.result?.metrics.response_text ?? '—'}
        </span>
      )),
    },
  );

  // Add expectation failures if any
  if (scenarios.some(s => s.result?.scoring?.expectation_failures?.length)) {
    rows.push({
      label: 'Failures',
      values: scenarios.map(s => {
        const failures = s.result?.scoring?.expectation_failures ?? [];
        return failures.length > 0
          ? <span className="text-red-400 text-xs">{failures.join(', ')}</span>
          : <span className="text-emerald-400 text-xs">None</span>;
      }),
    });
  }

  // Add error message row if any scenario has an error
  if (scenarios.some(s => s.result?.error_message)) {
    rows.push({
      label: 'Error',
      values: scenarios.map(s =>
        s.result?.error_message
          ? <span className="text-red-400 text-xs">{s.result.error_message}</span>
          : <span className={isDark ? 'text-slate-500' : 'text-gray-400'}>—</span>
      ),
    });
  }

  // Add failed repetitions row if any scenario has it
  if (scenarios.some(s => s.result?.repetitions_failed != null && s.result.repetitions_failed > 0)) {
    rows.push({
      label: 'Failed Reps',
      values: scenarios.map(s =>
        s.result?.repetitions_failed != null && s.result.repetitions_failed > 0
          ? <span className="text-amber-400 text-xs font-mono">{s.result.repetitions_failed}</span>
          : <span className={isDark ? 'text-slate-500' : 'text-gray-400'}>0</span>
      ),
    });
  }

  // ── Conversation: per-turn breakdown ──
  if (scenarios.some(s => s.result?.turns && s.result.turns.length > 0)) {
    rows.push({
      label: 'Conversation Turns',
      values: scenarios.map(s => {
        const turns = s.result?.turns;
        if (!turns || turns.length === 0) return <span className={isDark ? 'text-slate-500' : 'text-gray-400'}>—</span>;
        return (
          <div className="text-xs space-y-0.5">
            {turns.map(t => (
              <div key={t.turn_id} className="font-mono">
                <span className={isDark ? 'text-slate-500' : 'text-gray-400'}>{t.turn_id}</span>{' '}
                <span className={t.completed ? 'text-emerald-400' : 'text-amber-400'}>
                  {t.completed ? '✓' : '…'}
                </span>{' '}
                <span>{fmtMs(t.e2e_latency_ms)}</span>
                {t.expectation_failures && t.expectation_failures.length > 0 && (
                  <span className="text-red-400 ml-1">{t.expectation_failures.join('; ')}</span>
                )}
              </div>
            ))}
          </div>
        );
      }),
    });
  }

  // ── Barge-in: dedicated metrics ──
  if (scenarios.some(s => s.result?.barge_in)) {
    rows.push(
      {
        label: 'Interrupted',
        values: scenarios.map(s => {
          const b = s.result?.barge_in;
          if (!b) return <span className={isDark ? 'text-slate-500' : 'text-gray-400'}>—</span>;
          return b.interrupted
            ? <span className="text-emerald-400">yes ({b.trigger ?? '?'})</span>
            : <span className="text-red-400">no</span>;
        }),
      },
      {
        label: 'Barge-in RTT',
        values: scenarios.map(s => fmtMs(s.result?.barge_in?.barge_in_rtt_ms)),
      },
      {
        label: 'Recovery',
        values: scenarios.map(s => fmtMs(s.result?.barge_in?.recovery_ms)),
      },
      {
        label: 'Follow-up Completed',
        values: scenarios.map(s => {
          const b = s.result?.barge_in;
          if (!b) return <span className={isDark ? 'text-slate-500' : 'text-gray-400'}>—</span>;
          return b.follow_up_completed
            ? <span className="text-emerald-400 text-xs">yes</span>
            : <span className={isDark ? 'text-slate-500 text-xs' : 'text-gray-400 text-xs'}>—</span>;
        }),
      },
    );
  }

  // ── Gym: domain + goal status + composite reward + tool calls ──
  if (scenarios.some(s => s.result?.gym)) {
    rows.push(
      {
        label: 'Gym Domain',
        values: scenarios.map(s => {
          const g = s.result?.gym;
          if (!g) return <span className={isDark ? 'text-slate-500' : 'text-gray-400'}>—</span>;
          return <span className="font-mono text-xs uppercase px-1.5 py-0.5 rounded bg-amber-500/10 text-amber-400 font-semibold">{g.domain}</span>;
        }),
      },
      {
        label: 'Goal Status',
        values: scenarios.map(s => {
          const g = s.result?.gym;
          if (!g) return <span className={isDark ? 'text-slate-500' : 'text-gray-400'}>—</span>;
          return (
            <div className="text-xs">
              <span className={`font-semibold ${g.evaluation.success ? 'text-emerald-400' : 'text-rose-400'}`}>
                {g.evaluation.success ? '✓ Passed' : '✗ Failed'}
              </span>
              <div className={`mt-0.5 text-[11px] ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
                {g.evaluation.message}
              </div>
            </div>
          );
        }),
      },
      {
        label: 'RL Reward (R)',
        values: scenarios.map(s => {
          const g = s.result?.gym;
          if (!g) return <span className={isDark ? 'text-slate-500' : 'text-gray-400'}>—</span>;
          const r = g.reward;
          const positive = r.total_reward >= 1.0;
          return (
            <div className="text-xs space-y-0.5">
              <span className={`font-mono font-bold px-1.5 py-0.5 rounded ${
                positive ? 'bg-emerald-500/20 text-emerald-400' : 'bg-rose-500/20 text-rose-400'
              }`}>
                {r.total_reward >= 0 ? `+${r.total_reward.toFixed(2)}` : r.total_reward.toFixed(2)}
              </span>
              <div className={`text-[10px] space-x-1 font-mono ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
                <span>T:{r.task_reward.toFixed(1)}</span>
                <span>S:{r.slot_reward.toFixed(1)}</span>
                <span>C:{r.turn_reward.toFixed(1)}</span>
                <span>L:-{r.latency_penalty.toFixed(2)}</span>
              </div>
            </div>
          );
        }),
      },
      {
        label: 'Tool Calls',
        values: scenarios.map(s => {
          const g = s.result?.gym;
          if (!g) return <span className={isDark ? 'text-slate-500' : 'text-gray-400'}>—</span>;
          return <span className="font-mono text-xs">{g.tool_calls_executed}</span>;
        }),
      },
    );
  }

  // ── Gym: domain + goal status + composite reward + tool calls ──
  if (scenarios.some(s => s.result?.gym)) {
    rows.push(
      {
        label: 'Gym Domain',
        values: scenarios.map(s => {
          const g = s.result?.gym;
          if (!g) return <span className={isDark ? 'text-slate-500' : 'text-gray-400'}>—</span>;
          return <span className="font-mono text-xs uppercase px-1.5 py-0.5 rounded bg-amber-500/10 text-amber-400 font-semibold">{g.domain}</span>;
        }),
      },
      {
        label: 'Goal Status',
        values: scenarios.map(s => {
          const g = s.result?.gym;
          if (!g) return <span className={isDark ? 'text-slate-500' : 'text-gray-400'}>—</span>;
          return (
            <div className="text-xs">
              <span className={`font-semibold ${g.evaluation.success ? 'text-emerald-400' : 'text-rose-400'}`}>
                {g.evaluation.success ? '✓ Passed' : '✗ Failed'}
              </span>
              <div className={`mt-0.5 text-[11px] ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
                {g.evaluation.message}
              </div>
            </div>
          );
        }),
      },
      {
        label: 'RL Reward (R)',
        values: scenarios.map(s => {
          const g = s.result?.gym;
          if (!g) return <span className={isDark ? 'text-slate-500' : 'text-gray-400'}>—</span>;
          const r = g.reward;
          const positive = r.total_reward >= 1.0;
          return (
            <div className="text-xs space-y-0.5">
              <span className={`font-mono font-bold px-1.5 py-0.5 rounded ${
                positive ? 'bg-emerald-500/20 text-emerald-400' : 'bg-rose-500/20 text-rose-400'
              }`}>
                {r.total_reward >= 0 ? `+${r.total_reward.toFixed(2)}` : r.total_reward.toFixed(2)}
              </span>
              <div className={`text-[10px] space-x-1 font-mono ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
                <span>T:{r.task_reward.toFixed(1)}</span>
                <span>S:{r.slot_reward.toFixed(1)}</span>
                <span>C:{r.turn_reward.toFixed(1)}</span>
                <span>L:-{r.latency_penalty.toFixed(2)}</span>
              </div>
            </div>
          );
        }),
      },
      {
        label: 'Tool Calls',
        values: scenarios.map(s => {
          const g = s.result?.gym;
          if (!g) return <span className={isDark ? 'text-slate-500' : 'text-gray-400'}>—</span>;
          return <span className="font-mono text-xs">{g.tool_calls_executed}</span>;
        }),
      },
    );
  }

  // ── Synthetic caller: stop reason + barge-in stats + per-turn list ──
  if (scenarios.some(s => s.result?.synthetic_caller)) {
    rows.push(
      {
        label: 'Stop Reason',
        values: scenarios.map(s => {
          const r = s.result?.synthetic_caller;
          if (!r) return <span className={isDark ? 'text-slate-500' : 'text-gray-400'}>—</span>;
          return <span className="font-mono text-xs">{r.stop_reason ?? '—'}</span>;
        }),
      },
      {
        label: 'Caller Duration',
        values: scenarios.map(s => fmtMs(s.result?.synthetic_caller?.total_duration_ms)),
      },
      {
        label: 'Barge-in Attempts',
        values: scenarios.map(s => {
          const r = s.result?.synthetic_caller;
          if (!r) return <span className={isDark ? 'text-slate-500' : 'text-gray-400'}>—</span>;
          if (r.barge_in_attempts === 0) return <span className={isDark ? 'text-slate-500 text-xs' : 'text-gray-400 text-xs'}>—</span>;
          return <span className="font-mono text-xs">{r.barge_in_successes}/{r.barge_in_attempts}</span>;
        }),
      },
      {
        label: 'Caller Turns',
        values: scenarios.map(s => {
          const r = s.result?.synthetic_caller;
          if (!r || r.turns.length === 0) return <span className={isDark ? 'text-slate-500' : 'text-gray-400'}>—</span>;
          return (
            <div className="text-xs space-y-0.5 max-h-48 overflow-y-auto">
              {r.turns.map((t, i) => (
                <div key={i} className="font-mono">
                  <span className={t.speaker === 'caller' ? 'text-cyan-400' : 'text-emerald-400'}>
                    {t.speaker[0].toUpperCase()}
                  </span>
                  <span className={isDark ? 'text-slate-500' : 'text-gray-400'}> #{t.turn_index} </span>
                  <span className="font-sans">{t.transcript ?? '—'}</span>
                  {t.latency_ms != null && (
                    <span className={isDark ? 'text-slate-500 ml-1' : 'text-gray-400 ml-1'}>({fmtMs(t.latency_ms)})</span>
                  )}
                  {t.barge_in && <span className="text-rose-400 ml-1">⚡</span>}
                </div>
              ))}
            </div>
          );
        }),
      },
    );
  }

  return (
    <div className="overflow-x-auto">
      <table className="w-full">
        <thead>
          <tr className={`border-b ${rowBorder}`}>
            <th className={headerCell}>Metric</th>
            {scenarios.map(s => (
              <th key={s.variantId} className={headerCell}>
                <div className="flex items-center gap-2">
                  <span>{s.variantName}</span>
                  {experimentId && s.result && (
                    <button
                      type="button"
                      onClick={() => copyReport(s.variantId, s.variantName)}
                      title="Copy a compact diagnostic report (markdown + JSON) suitable for pasting into an LLM"
                      className={`inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-[10px] font-normal normal-case tracking-normal transition-colors ${
                        isDark
                          ? 'text-slate-400 hover:text-white hover:bg-slate-700'
                          : 'text-gray-500 hover:text-gray-900 hover:bg-gray-100'
                      }`}
                    >
                      {copiedVariantId === s.variantId
                        ? <><Check size={12} /> copied</>
                        : <><ClipboardCopy size={12} /> LLM report</>}
                    </button>
                  )}
                </div>
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((row, i) => (
            <tr key={row.label} className={`border-b ${rowBorder} ${i % 2 === 0 ? (isDark ? 'bg-slate-800/50' : 'bg-gray-50/50') : ''}`}>
              <td className={`${cell} font-sans font-medium ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
                <MetricTooltip metric={row.label}>{row.label}</MetricTooltip>
              </td>
              {row.values.map((v, j) => (
                <td key={j} className={cell}>{v}</td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
