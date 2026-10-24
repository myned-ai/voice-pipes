import { useEffect } from 'react';
import {
  X,
  Activity,
  Zap,
  Clock,
  Cpu,
  Volume2,
  Mic,
  Bot,
  AlertTriangle,
  CheckCircle2,
  Info,
  Check,
  ClipboardCopy,
} from 'lucide-react';
import { useTheme } from '../../../contexts/ThemeContext';
import type { SessionData, ReportAnomaly, DeviceInfo } from './types';
import { formatDuration, formatStarted, formatDeviceBadge, type DeviceTone } from './utils';

export interface SessionSummaryDialogProps {
  isOpen: boolean;
  onClose: () => void;
  data: SessionData;
  sessionId: string;
  anomaliesByUtt?: Record<string, ReportAnomaly[]>;
  onCopyReportForLlm?: () => void;
  reportCopyState?: 'idle' | 'loading' | 'copied' | 'error';
}

export function SessionSummaryDialog({
  isOpen,
  onClose,
  data,
  sessionId,
  anomaliesByUtt = {},
  onCopyReportForLlm,
  reportCopyState = 'idle',
}: SessionSummaryDialogProps) {
  const { isDark } = useTheme();

  // Close on Escape key press
  useEffect(() => {
    if (!isOpen) return;
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [isOpen, onClose]);

  if (!isOpen) return null;

  const metadata = data.metadata;
  const metrics = data.session_metrics;
  const verdict = data.verdict;
  const providers = data.providers;

  // Flatten all unique anomalies
  const allAnomalies: ReportAnomaly[] = Object.values(anomaliesByUtt).flat();
  const criticalAnomalies = allAnomalies.filter((a) => a.on_critical_path !== false && a.severity !== 'negligible');
  const diagnosticAnomalies = allAnomalies.filter((a) => a.on_critical_path === false || a.severity === 'negligible');

  // Palette for verdict level
  const verdictPalette: Record<string, { bg: string; text: string; ring: string; border: string; icon: string }> = {
    healthy: {
      bg: isDark ? 'bg-emerald-500/10' : 'bg-emerald-50',
      text: isDark ? 'text-emerald-300' : 'text-emerald-800',
      ring: isDark ? 'ring-emerald-500/30' : 'ring-emerald-300',
      border: isDark ? 'border-emerald-500/30' : 'border-emerald-200',
      icon: '✓',
    },
    fair: {
      bg: isDark ? 'bg-teal-500/10' : 'bg-teal-50',
      text: isDark ? 'text-teal-300' : 'text-teal-800',
      ring: isDark ? 'ring-teal-500/30' : 'ring-teal-300',
      border: isDark ? 'border-teal-500/30' : 'border-teal-200',
      icon: '≈',
    },
    degraded: {
      bg: isDark ? 'bg-amber-500/10' : 'bg-amber-50',
      text: isDark ? 'text-amber-300' : 'text-amber-800',
      ring: isDark ? 'ring-amber-500/30' : 'ring-amber-300',
      border: isDark ? 'border-amber-500/30' : 'border-amber-200',
      icon: '⚠',
    },
    broken: {
      bg: isDark ? 'bg-red-500/10' : 'bg-red-50',
      text: isDark ? 'text-red-300' : 'text-red-800',
      ring: isDark ? 'ring-red-500/30' : 'ring-red-300',
      border: isDark ? 'border-red-500/30' : 'border-red-200',
      icon: '✕',
    },
  };

  const vp = verdict ? verdictPalette[verdict.level] ?? verdictPalette.degraded : null;

  const deviceToneClass = (tone: DeviceTone) => {
    switch (tone) {
      case 'gpu':
        return isDark ? 'text-emerald-400 bg-emerald-500/10 border-emerald-500/30' : 'text-emerald-700 bg-emerald-50 border-emerald-300';
      case 'unconfirmed':
        return isDark ? 'text-amber-400 bg-amber-500/10 border-amber-500/30' : 'text-amber-700 bg-amber-50 border-amber-300';
      default:
        return isDark ? 'text-slate-400 bg-slate-800 border-slate-700' : 'text-gray-600 bg-gray-100 border-gray-200';
    }
  };

  const renderDeviceBadge = (device?: DeviceInfo) => {
    const badge = formatDeviceBadge(device);
    if (!badge) return null;
    return (
      <span
        className={`text-[10px] uppercase font-mono px-1.5 py-0.5 rounded border ${deviceToneClass(badge.tone)}`}
        title={badge.title}
      >
        {badge.label}
      </span>
    );
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-xs p-3 sm:p-4 overflow-hidden"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
      role="dialog"
      aria-modal="true"
      aria-label="Session summary"
    >
      <div
        className={`w-full max-w-2xl max-h-[90vh] sm:max-h-[86vh] rounded-2xl border shadow-2xl flex flex-col overflow-hidden transition-colors ${
          isDark ? 'bg-[#0f172a] border-slate-800 text-slate-200' : 'bg-white border-gray-200 text-gray-800'
        }`}
      >
        {/* Pinned Header */}
        <div
          className={`shrink-0 px-4 sm:px-5 py-3 sm:py-3.5 border-b flex items-start justify-between gap-4 ${
            isDark ? 'border-slate-800' : 'border-gray-200'
          }`}
        >
          <div className="space-y-1 min-w-0">
            <div className="flex items-center gap-2">
              <div className={`p-1.5 rounded-lg shrink-0 ${isDark ? 'bg-indigo-500/20 text-indigo-300' : 'bg-indigo-50 text-indigo-600'}`}>
                <Activity size={17} />
              </div>
              <h2 className={`text-base sm:text-lg font-bold truncate ${isDark ? 'text-white' : 'text-gray-900'}`}>
                Session Summary & Key Facts
              </h2>
            </div>
            <p className={`text-xs ${isDark ? 'text-slate-400' : 'text-gray-500'} flex items-center gap-1.5 sm:gap-2 flex-wrap`}>
              <span className="font-semibold text-indigo-400">{metadata?.pipeline_name ?? 'pipeline'}</span>
              <span>·</span>
              <span className="font-mono text-[11px] truncate max-w-[180px] sm:max-w-[240px]" title={sessionId}>
                {sessionId}
              </span>
              {metadata?.started_at && (
                <>
                  <span>·</span>
                  <span>{formatStarted(metadata.started_at).abs}</span>
                </>
              )}
              {metadata?.started_at && metadata?.ended_at && (
                <>
                  <span>·</span>
                  <span>{formatDuration(metadata.started_at, metadata.ended_at)}</span>
                </>
              )}
            </p>
          </div>
          <button
            onClick={onClose}
            className={`p-1.5 rounded-lg border transition-colors shrink-0 ${
              isDark
                ? 'bg-slate-800 border-slate-700 text-slate-400 hover:text-white hover:bg-slate-700'
                : 'bg-gray-100 border-gray-200 text-gray-500 hover:text-gray-900 hover:bg-gray-200'
            }`}
            title="Close summary"
          >
            <X size={16} />
          </button>
        </div>

        {/* Scrollable Content Body */}
        <div className="flex-1 min-h-0 overflow-y-auto px-4 sm:px-5 py-3.5 sm:py-4 space-y-4">
          {/* Verdict Banner */}
          {verdict && vp && (
            <div className={`rounded-xl p-3 sm:p-3.5 border flex flex-col gap-2 ${vp.bg} ${vp.border}`}>
              <div className="flex items-center justify-between gap-2 flex-wrap">
                <div className="flex items-center gap-2 flex-wrap">
                  <span className={`inline-flex items-center gap-1.5 px-2.5 py-0.5 rounded-full text-xs font-bold ring-1 ${vp.bg} ${vp.text} ${vp.ring}`}>
                    <span>{vp.icon}</span>
                    <span className="uppercase tracking-wider">{verdict.level}</span>
                    {verdict.primary_culprit && verdict.primary_culprit !== 'none' && (
                      <span className="opacity-80">· Culprit: {verdict.primary_culprit.toUpperCase()}</span>
                    )}
                  </span>
                  {(verdict.custom_grading?.length ?? 0) > 0 && (
                    <span className={`text-[10px] px-1.5 py-0.5 rounded-md font-medium ${isDark ? 'bg-slate-800 text-slate-300' : 'bg-white text-gray-700 border border-gray-200'}`}>
                      Custom grading
                    </span>
                  )}
                </div>
                {verdict.critical_path_p90_ms != null && (
                  <div className={`text-xs font-mono tabular-nums ${vp.text}`}>
                    Critical-Path p90: <span className="font-bold">{(verdict.critical_path_p90_ms / 1000).toFixed(2)}s</span>
                    {verdict.critical_path_p50_ms != null && (
                      <span className="opacity-75"> (p50: {(verdict.critical_path_p50_ms / 1000).toFixed(2)}s)</span>
                    )}
                  </div>
                )}
              </div>
              <p className={`text-xs font-medium leading-relaxed ${vp.text}`}>
                {verdict.headline}
              </p>
            </div>
          )}

          {/* Core KPI Metrics Grid */}
          <div className="space-y-1.5">
            <h3 className={`text-[11px] uppercase font-bold tracking-wider ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
              Core Performance Benchmarks
            </h3>
            <div className="grid grid-cols-2 sm:grid-cols-3 gap-2.5">
              {/* Response Latency (p90) */}
              <div className={`p-2.5 sm:p-3 rounded-xl border flex flex-col gap-0.5 ${isDark ? 'bg-slate-800/60 border-slate-700/60' : 'bg-gray-50 border-gray-200'}`}>
                <div className="flex items-center gap-1.5 text-xs text-indigo-400 font-medium">
                  <Clock size={13} />
                  <span>Caller Wait (p90)</span>
                </div>
                <div className={`text-base sm:text-lg font-bold font-mono tabular-nums ${isDark ? 'text-white' : 'text-gray-900'}`}>
                  {verdict?.critical_path_p90_ms != null
                    ? `${(verdict.critical_path_p90_ms / 1000).toFixed(2)}s`
                    : metrics?.avgTtfbMs != null
                      ? `${(metrics.avgTtfbMs / 1000).toFixed(2)}s`
                      : '—'}
                </div>
                <span className={`text-[10px] leading-tight ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
                  Speech-end → first audio
                </span>
              </div>

              {/* LLM TTFT */}
              <div className={`p-2.5 sm:p-3 rounded-xl border flex flex-col gap-0.5 ${isDark ? 'bg-slate-800/60 border-slate-700/60' : 'bg-gray-50 border-gray-200'}`}>
                <div className="flex items-center gap-1.5 text-xs text-amber-400 font-medium">
                  <Zap size={13} />
                  <span>LLM TTFT</span>
                </div>
                <div className={`text-base sm:text-lg font-bold font-mono tabular-nums ${isDark ? 'text-white' : 'text-gray-900'}`}>
                  {metrics?.avgTtftMs != null ? `${Math.round(metrics.avgTtftMs)} ms` : '—'}
                </div>
                <span className={`text-[10px] leading-tight ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
                  Time-to-first-token
                </span>
              </div>

              {/* TTS TTFB */}
              <div className={`p-2.5 sm:p-3 rounded-xl border flex flex-col gap-0.5 ${isDark ? 'bg-slate-800/60 border-slate-700/60' : 'bg-gray-50 border-gray-200'}`}>
                <div className="flex items-center gap-1.5 text-xs text-violet-400 font-medium">
                  <Volume2 size={13} />
                  <span>TTS TTFB</span>
                </div>
                <div className={`text-base sm:text-lg font-bold font-mono tabular-nums ${isDark ? 'text-white' : 'text-gray-900'}`}>
                  {metrics?.avgTtfbMs != null ? `${Math.round(metrics.avgTtfbMs)} ms` : '—'}
                </div>
                <span className={`text-[10px] leading-tight ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
                  Commit → first audio chunk
                </span>
              </div>

              {/* Turn Endpointing */}
              <div className={`p-2.5 sm:p-3 rounded-xl border flex flex-col gap-0.5 ${isDark ? 'bg-slate-800/60 border-slate-700/60' : 'bg-gray-50 border-gray-200'}`}>
                <div className="flex items-center gap-1.5 text-xs text-sky-400 font-medium">
                  <Mic size={13} />
                  <span>Turn Endpointing</span>
                </div>
                <div className={`text-base sm:text-lg font-bold font-mono tabular-nums ${isDark ? 'text-white' : 'text-gray-900'}`}>
                  {metrics?.avgEndpointingMs != null ? `${Math.round(metrics.avgEndpointingMs)} ms` : '—'}
                </div>
                <span className={`text-[10px] leading-tight ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
                  Silence timeout + STT commit
                </span>
              </div>

              {/* LLM Throughput */}
              <div className={`p-2.5 sm:p-3 rounded-xl border flex flex-col gap-0.5 ${isDark ? 'bg-slate-800/60 border-slate-700/60' : 'bg-gray-50 border-gray-200'}`}>
                <div className="flex items-center gap-1.5 text-xs text-emerald-400 font-medium">
                  <Bot size={13} />
                  <span>LLM Throughput</span>
                </div>
                <div className={`text-base sm:text-lg font-bold font-mono tabular-nums ${isDark ? 'text-white' : 'text-gray-900'}`}>
                  {metrics?.avgTokensPerSec != null ? `${metrics.avgTokensPerSec.toFixed(1)} tok/s` : '—'}
                </div>
                <span className={`text-[10px] leading-tight ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
                  {metrics?.totalOutputTokens ? `${metrics.totalOutputTokens} generated tokens` : 'Generation speed'}
                </span>
              </div>

              {/* Audio & Realtime Factor */}
              <div className={`p-2.5 sm:p-3 rounded-xl border flex flex-col gap-0.5 ${isDark ? 'bg-slate-800/60 border-slate-700/60' : 'bg-gray-50 border-gray-200'}`}>
                <div className="flex items-center gap-1.5 text-xs text-cyan-400 font-medium">
                  <Cpu size={13} />
                  <span>TTS Realtime Factor</span>
                </div>
                <div className={`text-base sm:text-lg font-bold font-mono tabular-nums ${isDark ? 'text-white' : 'text-gray-900'}`}>
                  {metrics?.avgRtf != null ? `${metrics.avgRtf.toFixed(2)}x` : '—'}
                </div>
                <span className={`text-[10px] leading-tight ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
                  {metrics?.totalAudioSeconds != null ? `${metrics.totalAudioSeconds.toFixed(1)}s speech audio` : '< 1.0x faster than RT'}
                </span>
              </div>
            </div>
          </div>

          {/* Pipeline Components Snapshot */}
          <div className="space-y-1.5">
            <h3 className={`text-[11px] uppercase font-bold tracking-wider ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
              Pipeline Stack Architecture
            </h3>
            <div className="grid grid-cols-1 sm:grid-cols-3 gap-2.5">
              {/* STT */}
              <div className={`p-2.5 rounded-xl border ${isDark ? 'bg-slate-800/40 border-slate-700/40' : 'bg-gray-50 border-gray-200'} space-y-1`}>
                <div className="flex items-center justify-between text-xs font-semibold text-sky-400 gap-1">
                  <span className="truncate">STT: {providers?.stt?.provider ?? metadata?.stt_provider ?? 'Unknown'}</span>
                  {renderDeviceBadge(providers?.stt?.device)}
                </div>
                <p className={`text-xs font-mono truncate ${isDark ? 'text-slate-300' : 'text-gray-700'}`} title={providers?.stt?.model}>
                  {providers?.stt?.model ?? 'Default Model'}
                </p>
                {providers?.stt?.mode && (
                  <span className={`inline-block text-[10px] px-1.5 py-0.2 rounded ${isDark ? 'bg-slate-700 text-slate-300' : 'bg-gray-200 text-gray-700'}`}>
                    {providers.stt.mode} mode
                  </span>
                )}
              </div>

              {/* LLM */}
              <div className={`p-2.5 rounded-xl border ${isDark ? 'bg-slate-800/40 border-slate-700/40' : 'bg-gray-50 border-gray-200'} space-y-1`}>
                <div className="flex items-center justify-between text-xs font-semibold text-amber-400 gap-1">
                  <span className="truncate">LLM: {providers?.llm?.provider ?? 'Text Processor'}</span>
                  {renderDeviceBadge(providers?.llm?.device)}
                </div>
                <p className={`text-xs font-mono truncate ${isDark ? 'text-slate-300' : 'text-gray-700'}`} title={providers?.llm?.model}>
                  {providers?.llm?.model ?? 'In-process Model'}
                </p>
                {metrics?.totalPromptTokens != null && (
                  <span className="block text-[10px] text-slate-400 truncate">
                    {metrics.totalPromptTokens} prompt / {metrics.totalOutputTokens ?? 0} out
                  </span>
                )}
              </div>

              {/* TTS */}
              <div className={`p-2.5 rounded-xl border ${isDark ? 'bg-slate-800/40 border-slate-700/40' : 'bg-gray-50 border-gray-200'} space-y-1`}>
                <div className="flex items-center justify-between text-xs font-semibold text-violet-400 gap-1">
                  <span className="truncate">TTS: {providers?.tts?.provider ?? metadata?.tts_provider ?? 'Unknown'}</span>
                  {renderDeviceBadge(providers?.tts?.device)}
                </div>
                <p className={`text-xs font-mono truncate ${isDark ? 'text-slate-300' : 'text-gray-700'}`} title={providers?.tts?.model}>
                  {providers?.tts?.model ?? providers?.tts?.voice ?? 'Default Voice'}
                </p>
                <span className="block text-[10px] text-slate-400 truncate">
                  {metrics?.totalUnderruns === 0 ? '0 underruns (clean)' : `${metrics?.totalUnderruns ?? 0} underruns`}
                </span>
              </div>
            </div>
          </div>

          {/* Key Observations / Anomalies Section */}
          <div className="space-y-1.5">
            <h3 className={`text-[11px] uppercase font-bold tracking-wider ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
              Operational Findings ({allAnomalies.length} flag{allAnomalies.length === 1 ? '' : 's'})
            </h3>
            {allAnomalies.length === 0 ? (
              <div className={`p-2.5 rounded-xl border flex items-center gap-2 ${isDark ? 'bg-emerald-500/10 border-emerald-500/20 text-emerald-300' : 'bg-emerald-50 border-emerald-200 text-emerald-800'}`}>
                <CheckCircle2 size={15} className="shrink-0" />
                <span className="text-xs">
                  Clean run: all conversational turns completed within SLA targets with zero performance anomalies or audio dropouts.
                </span>
              </div>
            ) : (
              <div className="space-y-1.5">
                {criticalAnomalies.map((a, idx) => (
                  <div
                    key={`crit-${idx}`}
                    className={`p-2 rounded-lg border text-xs flex items-start gap-2 ${
                      isDark ? 'bg-amber-500/10 border-amber-500/20 text-amber-200' : 'bg-amber-50 border-amber-200 text-amber-800'
                    }`}
                  >
                    <AlertTriangle size={13} className="shrink-0 mt-0.5" />
                    <div className="flex-1 min-w-0">
                      <div className="flex items-center gap-1.5 flex-wrap">
                        <span className="font-semibold uppercase text-[10px] tracking-wide px-1.5 py-0.2 rounded bg-amber-500/20">
                          {a.severity ?? 'warn'}
                        </span>
                        <span className="font-mono">{a.kind}</span>
                        {a.observed != null && a.target != null && (
                          <span className="opacity-80">
                            (Observed: {typeof a.observed === 'number' ? Math.round(a.observed) : a.observed} ms / Target: {a.target} ms)
                          </span>
                        )}
                      </div>
                      {a.detail && <p className="mt-0.5 opacity-90 text-[11px]">{a.detail}</p>}
                    </div>
                  </div>
                ))}
                {diagnosticAnomalies.map((a, idx) => (
                  <div
                    key={`diag-${idx}`}
                    className={`p-2 rounded-lg border text-xs flex items-start gap-2 ${
                      isDark ? 'bg-slate-800/50 border-slate-700/50 text-slate-300' : 'bg-gray-50 border-gray-200 text-gray-700'
                    }`}
                  >
                    <Info size={13} className="shrink-0 mt-0.5 text-sky-400" />
                    <div className="flex-1 min-w-0">
                      <div className="flex items-center gap-1.5 flex-wrap">
                        <span className="font-semibold uppercase text-[10px] tracking-wide px-1.5 py-0.2 rounded bg-sky-500/20 text-sky-400">
                          diagnostic
                        </span>
                        <span className="font-mono">{a.kind}</span>
                        {a.observed != null && (
                          <span className="opacity-80">
                            ({typeof a.observed === 'number' ? Math.round(a.observed) : a.observed} ms)
                          </span>
                        )}
                      </div>
                      {a.detail && <p className="mt-0.5 opacity-80 text-[11px]">{a.detail}</p>}
                    </div>
                  </div>
                ))}
              </div>
            )}
          </div>
        </div>

        {/* Pinned Footer */}
        <div
          className={`shrink-0 px-4 sm:px-5 py-2.5 sm:py-3 border-t flex items-center justify-between gap-3 flex-wrap ${
            isDark ? 'border-slate-800' : 'border-gray-200'
          }`}
        >
          <div className="text-xs text-slate-400">
            <span>{metadata?.turn_count ?? metrics?.totalTurns ?? 0} total turns</span>
            {metrics?.serverBargeIns ? ` · ${metrics.serverBargeIns} barge-in(s)` : ''}
          </div>
          <div className="flex items-center gap-2">
            {onCopyReportForLlm && (
              <button
                onClick={onCopyReportForLlm}
                disabled={reportCopyState === 'loading'}
                className={`flex items-center gap-1.5 h-7 sm:h-8 px-2.5 sm:px-3 text-xs font-medium rounded-lg border transition-colors ${
                  reportCopyState === 'copied'
                    ? 'bg-emerald-600 border-emerald-600 text-white'
                    : isDark
                      ? 'bg-slate-800 border-slate-700 text-slate-300 hover:bg-slate-700'
                      : 'bg-white border-gray-300 text-gray-700 hover:bg-gray-100'
                }`}
              >
                {reportCopyState === 'copied' ? (
                  <>
                    <Check size={13} />
                    <span>Report Copied</span>
                  </>
                ) : (
                  <>
                    <ClipboardCopy size={13} />
                    <span>Copy Full Diagnostic</span>
                  </>
                )}
              </button>
            )}
            <button
              onClick={onClose}
              className={`h-7 sm:h-8 px-3 sm:px-4 text-xs font-medium rounded-lg transition-colors ${
                isDark ? 'bg-indigo-600 text-white hover:bg-indigo-500' : 'bg-indigo-600 text-white hover:bg-indigo-700'
              }`}
            >
              Close
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
