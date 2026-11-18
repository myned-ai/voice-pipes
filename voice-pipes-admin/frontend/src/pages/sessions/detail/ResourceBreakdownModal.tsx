import { useEffect } from 'react';
import {
  X,
  Cpu,
  HardDrive,
  Layers,
  Activity,
  AlertTriangle,
  CheckCircle2,
  TrendingUp,
  Info,
  ShieldCheck,
  Server,
} from 'lucide-react';
import { useTheme } from '../../../contexts/ThemeContext';
import type { DeviceInfo, SessionData } from './types';
import { formatDeviceBadge, type DeviceTone } from './utils';

export interface ResourceBreakdownModalProps {
  isOpen: boolean;
  onClose: () => void;
  data: SessionData;
  sessionId: string;
}

interface ComponentCard {
  id: string;
  name: string;
  provider: string | null;
  model: string | null;
  meta: string | null;
  device?: DeviceInfo;
  color: string;
}

export function ResourceBreakdownModal({
  isOpen,
  onClose,
  data,
  sessionId,
}: ResourceBreakdownModalProps) {
  const { isDark } = useTheme();

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
  const providers = data.providers;

  // Extract turn-by-turn resource metrics
  interface TurnRow {
    turnIndex: number;
    latencyMs: number | null;
    rssMb: number | null;
    deltaMb: number | null;
    cpuPct: number | null;
    cpuMs: number | null;
  }

  const toTurnRow = (
    idx: number,
    latencyMs: number | null,
    res?: {
      rss_bytes?: number | null;
      rss_delta_bytes?: number | null;
      cpu_percent?: number | null;
      cpu_time_ms?: number | null;
    } | null,
  ): TurnRow => ({
    turnIndex: idx + 1,
    latencyMs,
    rssMb: res?.rss_bytes != null ? res.rss_bytes / (1024 * 1024) : null,
    deltaMb: res?.rss_delta_bytes != null ? res.rss_delta_bytes / (1024 * 1024) : null,
    cpuPct: res?.cpu_percent ?? null,
    cpuMs: res?.cpu_time_ms ?? null,
  });

  const turnRows: TurnRow[] = data.utterances && data.utterances.length > 0
    ? data.utterances.map((u, i) => toTurnRow(i, u.summary?.total_ms ?? u.summary?.e2e_perceived_ms ?? null, u.resource_usage))
    : (data.turns ?? []).map((t, i) => toTurnRow(i, t.total_ms ?? null, t.detail?.resource_usage));

  const validRss = turnRows.map((r) => r.rssMb).filter((v): v is number => v != null);
  const validCpuMs = turnRows.map((r) => r.cpuMs).filter((v): v is number => v != null);

  const peakRssMb = metadata?.peak_rss_bytes != null
    ? metadata.peak_rss_bytes / (1024 * 1024)
    : validRss.length > 0
    ? Math.max(...validRss)
    : null;
  const containerPeakMb = metadata?.container_peak_memory_bytes != null
    ? metadata.container_peak_memory_bytes / (1024 * 1024)
    : null;
  const avgCpuMs = metadata?.avg_turn_cpu_ms != null
    ? metadata.avg_turn_cpu_ms
    : validCpuMs.length > 0
    ? Math.round(validCpuMs.reduce((a, b) => a + b, 0) / validCpuMs.length)
    : null;

  // Calculate session drift only when turn RSS measurements exist (missing is null, never 0)
  const startRss = validRss.length > 0 ? validRss[0] : null;
  const endRss = validRss.length > 0 ? validRss[validRss.length - 1] : null;
  const sessionDriftMb = startRss != null && endRss != null ? endRss - startRss : null;
  const sessionDriftPct =
    sessionDriftMb != null && startRss != null && startRss > 0
      ? (sessionDriftMb / startRss) * 100
      : null;
  // Scale drift threshold with baseline RSS (at least 25 MB, or 2% for multi-GB ONNX/GGUF workers)
  // and only flag positive growth — memory reclamation (negative drift) is healthy.
  const driftThresholdMb = startRss != null ? Math.max(25, startRss * 0.02) : 25;
  const hasSignificantDrift = sessionDriftMb != null && sessionDriftMb > driftThresholdMb;

  // Recorded component identity & verified device info (no guessed models or memory sizes)
  const sttProvider = providers?.stt?.provider ?? metadata?.stt_provider ?? null;
  const sttModel = providers?.stt?.model ?? null;
  const sttMeta = [
    providers?.stt?.mode && providers.stt.mode !== 'unknown' ? `${providers.stt.mode} mode` : null,
    providers?.stt?.model_version ? `v:${providers.stt.model_version}` : null,
  ].filter(Boolean).join(' · ') || null;

  const llmProvider = providers?.llm?.provider ?? null;
  const llmModel = providers?.llm?.model ?? null;
  const llmMeta = [
    providers?.llm?.stream != null ? (providers.llm.stream ? 'streaming' : 'non-streaming') : null,
    providers?.llm?.model_version ? `v:${providers.llm.model_version}` : null,
  ].filter(Boolean).join(' · ') || null;

  const ttsProvider = providers?.tts?.provider ?? metadata?.tts_provider ?? null;
  const ttsModel = providers?.tts?.model ?? providers?.tts?.voice ?? null;
  const ttsMeta = [
    providers?.tts?.model && providers?.tts?.voice ? `voice: ${providers.tts.voice}` : null,
    providers?.tts?.model_version ? `v:${providers.tts.model_version}` : null,
  ].filter(Boolean).join(' · ') || null;

  const components: ComponentCard[] = [
    {
      id: 'stt',
      name: 'Speech-to-Text (STT)',
      provider: sttProvider,
      model: sttModel,
      meta: sttMeta,
      device: providers?.stt?.device,
      color: isDark ? 'text-sky-400' : 'text-sky-600',
    },
    {
      id: 'llm',
      name: 'Language Model (LLM)',
      provider: llmProvider,
      model: llmModel,
      meta: llmMeta,
      device: providers?.llm?.device,
      color: isDark ? 'text-amber-400' : 'text-amber-600',
    },
    {
      id: 'tts',
      name: 'Text-to-Speech (TTS)',
      provider: ttsProvider,
      model: ttsModel,
      meta: ttsMeta,
      device: providers?.tts?.device,
      color: isDark ? 'text-violet-400' : 'text-violet-600',
    },
  ];

  const deviceToneClasses = (tone: DeviceTone) => {
    switch (tone) {
      case 'gpu':
        return isDark
          ? 'bg-emerald-500/15 text-emerald-300 ring-emerald-500/30'
          : 'bg-emerald-50 text-emerald-700 ring-emerald-300';
      case 'unconfirmed':
        return isDark
          ? 'bg-amber-500/15 text-amber-300 ring-amber-500/30'
          : 'bg-amber-50 text-amber-700 ring-amber-300';
      case 'unknown':
        return isDark
          ? 'bg-slate-800 text-slate-400 ring-slate-700'
          : 'bg-gray-100 text-gray-500 ring-gray-200';
      default:
        return isDark
          ? 'bg-slate-700 text-slate-300 ring-slate-600'
          : 'bg-gray-200 text-gray-700 ring-gray-300';
    }
  };

  const peakRssTooltip = peakRssMb != null
    ? 'Peak resident RAM (RSS) of the worker OS process during this session (/proc/self/statm). Good: flat across turns (cloud workers ~40–100 MB; local ONNX/GGUF models higher). Bad: steady growth across turns indicates a memory leak.'
    : 'Peak resident RAM (RSS) of the worker OS process. Empty (—) because the worker did not emit process memory telemetry (e.g., non-Linux host or older worker build).';

  const containerPeakTooltip = containerPeakMb != null
    ? 'Peak memory of the entire Linux container cgroup (cgroups v2 memory.peak), including worker heap, ONNX/CUDA buffers, and OS page cache. Good: comfortably below your container memory limit. Bad: approaching the cgroup limit risks an OOM kill.'
    : 'Peak memory of the container cgroup (cgroups v2 memory.peak). Empty (—) because the worker is not running inside a Linux cgroups v2 container or the cgroup file was unreadable.';

  const avgCpuTooltip = avgCpuMs != null
    ? 'Mean active worker CPU time spent per turn (ms). Good: low on cloud/GPU pipelines (< 50 ms) or predictable on local CPU inference. Bad: rising or multi-second CPU time on cloud pipelines suggests heavy resampling/filtering or thread contention.'
    : 'Mean active worker CPU time per turn (ms). Empty (—) because per-turn CPU accounting (cpu_time_ms) was not recorded — normal for S2S/realtime sessions or captures without per-turn CPU sampling.';

  const driftTooltip = sessionDriftMb != null
    ? `Net worker RSS change from Turn 1 to the final turn (${sessionDriftMb >= 0 ? '+' : ''}${sessionDriftMb.toFixed(2)} MB${sessionDriftPct != null ? `, ${sessionDriftPct >= 0 ? '+' : ''}${sessionDriftPct.toFixed(1)}%` : ''}). Threshold: +${driftThresholdMb.toFixed(0)} MB (max of 25 MB or 2% of baseline RSS). Negative drift means memory was reclaimed; small positive steps on local ONNX/GGUF workers are normal tensor-arena (BFCArena) warmup. A true leak grows continuously across turns and sessions.`
    : 'Net worker RSS change from Turn 1 to the final turn. Empty (—) because fewer than 1 turn recorded process RSS.';

  const componentCardTooltip = (comp: ComponentCard): string => {
    if (!comp.provider && !comp.model) {
      return `${comp.name}: Not recorded (—) because this stage was not used or was not reported in session metadata.`;
    }
    if (!comp.provider && comp.model) {
      return `${comp.name} (${comp.model}): Provider is — because in speech-to-speech (S2S) pipelines a single realtime model handles STT, LLM, and TTS, so separate LLM provider credentials are not configured.`;
    }
    return `${comp.name} using ${comp.provider ?? 'unknown'}${comp.model ? ` (${comp.model})` : ''}. Cloud APIs and external sidecars run in separate address spaces and do not add to worker RSS.`;
  };

  const notRecordedCellTip = 'Not recorded for this turn (—). S2S/realtime sessions typically report Process RSS per turn without per-turn CPU or RSS delta.';

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-labelledby="resource-modal-title"
      className="fixed inset-0 z-50 flex items-center justify-center p-4 bg-black/60 backdrop-blur-sm animate-fade-in"
      onClick={onClose}
    >
      <div
        className={`w-full max-w-4xl max-h-[90vh] flex flex-col rounded-xl shadow-2xl border overflow-hidden transition-all ${
          isDark ? 'bg-slate-900 border-slate-700 text-slate-100' : 'bg-white border-gray-200 text-gray-900'
        }`}
        onClick={(e) => e.stopPropagation()}
      >
        {/* Modal Header */}
        <div
          className={`flex items-center justify-between px-6 py-4 border-b ${
            isDark ? 'border-slate-800 bg-slate-900/50' : 'border-gray-100 bg-gray-50/50'
          }`}
        >
          <div className="flex items-center gap-3">
            <div className={`p-2 rounded-lg ${isDark ? 'bg-cyan-500/20 text-cyan-400' : 'bg-cyan-50 text-cyan-600'}`}>
              <HardDrive size={20} />
            </div>
            <div>
              <div className="flex items-center gap-2">
                <h2 id="resource-modal-title" className="text-lg font-semibold tracking-tight">
                  System Resources & Turn Memory Profile
                </h2>
                <span
                  className={`text-[10px] font-mono px-2 py-0.5 rounded-full ring-1 ${
                    isDark ? 'bg-slate-800 text-slate-400 ring-slate-700' : 'bg-gray-100 text-gray-600 ring-gray-200'
                  }`}
                >
                  Session {sessionId.slice(0, 8)}
                </span>
              </div>
              <p className={`text-xs ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
                Measured OS process resident memory (RSS), Linux cgroups peak, and turn-by-turn CPU & memory deltas
              </p>
            </div>
          </div>
          <button
            onClick={onClose}
            className={`p-1.5 rounded-lg border transition-colors ${
              isDark
                ? 'border-slate-700 hover:bg-slate-800 text-slate-400 hover:text-slate-200'
                : 'border-gray-200 hover:bg-gray-100 text-gray-400 hover:text-gray-600'
            }`}
            title="Close (Esc)"
          >
            <X size={18} />
          </button>
        </div>

        {/* Modal Content */}
        <div className="overflow-y-auto p-6 space-y-6">
          {/* Executive KPI Cards (Measured Ground Truth) */}
          <div className="grid grid-cols-2 md:grid-cols-4 gap-4">
            {/* Peak Process RSS */}
            <div
              title={peakRssTooltip}
              className={`p-4 rounded-xl border flex flex-col justify-between cursor-help ${
                isDark ? 'bg-slate-800/40 border-slate-700/60' : 'bg-slate-50 border-gray-200'
              }`}
            >
              <div className="flex items-center justify-between">
                <span className={`text-xs font-medium ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
                  Peak Process RSS
                </span>
                <HardDrive size={16} className={isDark ? 'text-cyan-400' : 'text-cyan-600'} />
              </div>
              <div className="mt-2">
                <div className="text-2xl font-bold font-mono tracking-tight">
                  {peakRssMb != null ? `${peakRssMb.toFixed(1)} MB` : '—'}
                </div>
                <div className={`text-[11px] mt-0.5 ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
                  {peakRssMb != null ? 'Measured via /proc/self/statm' : 'Not recorded'}
                </div>
              </div>
            </div>

            {/* Peak Container Memory */}
            <div
              title={containerPeakTooltip}
              className={`p-4 rounded-xl border flex flex-col justify-between cursor-help ${
                isDark ? 'bg-slate-800/40 border-slate-700/60' : 'bg-slate-50 border-gray-200'
              }`}
            >
              <div className="flex items-center justify-between">
                <span className={`text-xs font-medium ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
                  Container Peak
                </span>
                <Server size={16} className={isDark ? 'text-indigo-400' : 'text-indigo-600'} />
              </div>
              <div className="mt-2">
                <div className="text-2xl font-bold font-mono tracking-tight">
                  {containerPeakMb != null ? `${containerPeakMb.toFixed(0)} MB` : '—'}
                </div>
                <div className={`text-[11px] mt-0.5 ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
                  {containerPeakMb != null ? 'cgroups v2 memory.peak' : 'Not in cgroup / unrecorded'}
                </div>
              </div>
            </div>

            {/* Avg Turn CPU */}
            <div
              title={avgCpuTooltip}
              className={`p-4 rounded-xl border flex flex-col justify-between cursor-help ${
                isDark ? 'bg-slate-800/40 border-slate-700/60' : 'bg-slate-50 border-gray-200'
              }`}
            >
              <div className="flex items-center justify-between">
                <span className={`text-xs font-medium ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
                  Avg Turn CPU
                </span>
                <Cpu size={16} className={isDark ? 'text-teal-400' : 'text-teal-600'} />
              </div>
              <div className="mt-2">
                <div className="text-2xl font-bold font-mono tracking-tight">
                  {avgCpuMs != null ? `${avgCpuMs} ms` : '—'}
                </div>
                <div className={`text-[11px] mt-0.5 ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
                  {avgCpuMs != null ? 'CPU time per turn' : 'Not recorded'}
                </div>
              </div>
            </div>

            {/* Session Memory Drift */}
            <div
              title={driftTooltip}
              className={`p-4 rounded-xl border flex flex-col justify-between cursor-help ${
                isDark ? 'bg-slate-800/40 border-slate-700/60' : 'bg-slate-50 border-gray-200'
              }`}
            >
              <div className="flex items-center justify-between">
                <span className={`text-xs font-medium ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
                  Net Memory Drift
                </span>
                <TrendingUp
                  size={16}
                  className={
                    sessionDriftMb == null
                      ? isDark
                        ? 'text-slate-500'
                        : 'text-gray-400'
                      : hasSignificantDrift
                      ? isDark
                        ? 'text-amber-400'
                        : 'text-amber-600'
                      : isDark
                      ? 'text-emerald-400'
                      : 'text-emerald-600'
                  }
                />
              </div>
              <div className="mt-2">
                <div
                  className={`text-2xl font-bold font-mono tracking-tight ${
                    sessionDriftMb == null
                      ? ''
                      : hasSignificantDrift
                      ? isDark
                        ? 'text-amber-300'
                        : 'text-amber-700'
                      : isDark
                      ? 'text-emerald-300'
                      : 'text-emerald-700'
                  }`}
                >
                  {sessionDriftMb != null
                    ? `${sessionDriftMb >= 0 ? `+${sessionDriftMb.toFixed(2)}` : sessionDriftMb.toFixed(2)} MB`
                    : '—'}
                </div>
                <div className={`text-[11px] mt-0.5 ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
                  {sessionDriftMb == null
                    ? 'Not recorded'
                    : hasSignificantDrift
                    ? `Growth observed (${sessionDriftPct != null ? `+${sessionDriftPct.toFixed(1)}%` : 'above threshold'})`
                    : sessionDriftMb < 0
                    ? `Reclaimed (${sessionDriftPct != null ? `${sessionDriftPct.toFixed(1)}%` : 'delta'})`
                    : `Turn 1 → final (${sessionDriftPct != null ? `+${sessionDriftPct.toFixed(1)}%` : 'stable'})`}
                </div>
              </div>
            </div>
          </div>

          {/* Recorded Pipeline Components & Execution Devices */}
          <div
            className={`p-5 rounded-xl border space-y-4 ${
              isDark ? 'bg-slate-800/20 border-slate-800' : 'bg-white border-gray-200'
            }`}
          >
            <div className="flex items-center justify-between flex-wrap gap-2">
              <div className="flex items-center gap-2">
                <Layers size={18} className={isDark ? 'text-cyan-400' : 'text-cyan-600'} />
                <h3 className="text-sm font-semibold">Recorded Pipeline Components & Execution Devices</h3>
              </div>
            </div>

            <div className="grid grid-cols-1 md:grid-cols-3 gap-3">
              {components.map((comp) => {
                const badge = formatDeviceBadge(comp.device);
                return (
                  <div
                    key={comp.id}
                    title={componentCardTooltip(comp)}
                    className={`p-3 rounded-lg border flex flex-col justify-between gap-1.5 cursor-help ${
                      isDark ? 'bg-slate-800/40 border-slate-700/50' : 'bg-gray-50 border-gray-200'
                    }`}
                  >
                    <div className="flex items-center justify-between gap-2">
                      <span className={`text-xs font-bold ${comp.color}`}>{comp.name}</span>
                      {badge && (
                        <span
                          className={`text-[10px] font-mono uppercase px-1.5 py-0.5 rounded ring-1 ${deviceToneClasses(badge.tone)}`}
                          title={badge.title}
                        >
                          {badge.label}
                        </span>
                      )}
                    </div>
                    <div className={`text-xs font-mono font-medium truncate ${isDark ? 'text-slate-200' : 'text-gray-800'}`} title={comp.model ?? undefined}>
                      {comp.model ?? '—'}
                    </div>
                    <div className={`text-[11px] font-mono flex items-center justify-between gap-2 ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
                      <span>provider: {comp.provider ?? '—'}</span>
                      {comp.meta && <span className="truncate">{comp.meta}</span>}
                    </div>
                  </div>
                );
              })}
            </div>

            <div className={`flex items-start gap-2 text-xs pt-1 ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
              <Info size={14} className="shrink-0 mt-0.5 text-cyan-400" />
              <span>
                Memory telemetry is measured at the OS process (`rss_bytes`) and container cgroup (`memory.peak`) level.
                Per-component heap allocations are not isolated within the shared worker process, and external sidecars
                or cloud APIs run in their own address spaces.
              </span>
            </div>
          </div>

          {/* Turn-by-Turn Memory Stability Table */}
          {turnRows.length > 0 && (
            <div
              className={`p-5 rounded-xl border space-y-3 ${
                isDark ? 'bg-slate-800/20 border-slate-800' : 'bg-white border-gray-200'
              }`}
            >
              <div className="flex items-center justify-between">
                <div className="flex items-center gap-2">
                  <Activity size={18} className={isDark ? 'text-teal-400' : 'text-teal-600'} />
                  <h3 className="text-sm font-semibold">Turn-by-Turn Memory & CPU Profile</h3>
                </div>
                {sessionDriftMb != null && (
                  <div className="flex items-center gap-2">
                    <span
                      title={`Session memory stability: 'Memory Stable' when positive growth <= ${driftThresholdMb.toFixed(0)} MB (max of 25 MB or 2% of Turn 1 RSS) or memory is reclaimed; 'Drift Detected' above that threshold.`}
                      className={`inline-flex items-center gap-1 text-[11px] font-mono px-2 py-0.5 rounded-full ring-1 cursor-help ${
                        !hasSignificantDrift
                          ? isDark
                            ? 'bg-emerald-500/15 text-emerald-300 ring-emerald-500/30'
                            : 'bg-emerald-50 text-emerald-700 ring-emerald-300'
                          : isDark
                          ? 'bg-amber-500/15 text-amber-300 ring-amber-500/30'
                          : 'bg-amber-50 text-amber-700 ring-amber-300'
                      }`}
                    >
                      <ShieldCheck size={12} />
                      {!hasSignificantDrift ? 'Memory Stable' : 'Drift Detected'}
                    </span>
                  </div>
                )}
              </div>

              <div className="overflow-x-auto rounded-lg border border-slate-700/50">
                <table className="w-full text-left text-xs font-mono">
                  <thead
                    className={`border-b ${
                      isDark ? 'bg-slate-800/80 border-slate-700 text-slate-300' : 'bg-gray-100 border-gray-200 text-gray-700'
                    }`}
                  >
                    <tr>
                      <th className="py-2 px-3 cursor-help" title="Utterance index in chronological order">Turn</th>
                      <th className="py-2 px-3 cursor-help" title="Total turn duration / perceived latency (ms). Good: <= 1500 ms; Poor: > 3000 ms (note: long agent replies also increase total_ms). Empty (—) if dropped or unmeasured.">Latency (E2E)</th>
                      <th className="py-2 px-3 cursor-help" title="Worker OS resident memory (RSS) sampled at the end of this turn. Good: flat or decreasing across turns. Empty (—) if not sampled.">Process RSS</th>
                      <th className="py-2 px-3 cursor-help" title="RSS change during this turn. Good: flat (< 0.05 MB) or < +10 MB. Warning: +10–25 MB (Growth). Bad: >= +25 MB (Spike). Empty (—) when per-turn delta was not recorded (common on S2S).">Turn Δ</th>
                      <th className="py-2 px-3 cursor-help" title="Worker CPU time consumed during this turn (ms). Empty (—) when per-turn CPU sampling was not recorded (common on S2S).">CPU Time</th>
                      <th className="py-2 px-3 cursor-help" title="Average worker CPU utilization during this turn (%). Empty (—) when per-turn CPU sampling was not recorded.">CPU %</th>
                      <th className="py-2 px-3 cursor-help" title="Per-turn memory status based on Turn Δ: Stable (< 10 MB), Growth (10–25 MB), Spike (>= 25 MB), Dropped (no turn metrics), or — (Turn Δ not recorded).">Status</th>
                    </tr>
                  </thead>
                  <tbody className={`divide-y ${isDark ? 'divide-slate-800' : 'divide-gray-100'}`}>
                    {turnRows.map((row) => {
                      const delta = row.deltaMb;
                      let statusBadge: React.ReactNode =
                        row.latencyMs == null && row.rssMb == null && row.cpuMs == null ? (
                          <span className={isDark ? 'text-slate-500' : 'text-gray-400'} title="Turn was dropped before producing latency or resource metrics.">Dropped</span>
                        ) : (
                          <span title={notRecordedCellTip}>—</span>
                        );
                      if (delta != null) {
                        if (delta >= 25) {
                          statusBadge = (
                            <span className="text-rose-400 flex items-center gap-1">
                              <AlertTriangle size={12} /> Spike
                            </span>
                          );
                        } else if (delta >= 10) {
                          statusBadge = (
                            <span className="text-amber-400 flex items-center gap-1">
                              <AlertTriangle size={12} /> Growth
                            </span>
                          );
                        } else {
                          statusBadge = (
                            <span className="text-emerald-500 flex items-center gap-1">
                              <CheckCircle2 size={12} /> Stable
                            </span>
                          );
                        }
                      }

                      return (
                        <tr
                          key={row.turnIndex}
                          className={`${
                            isDark ? 'hover:bg-slate-800/40' : 'hover:bg-gray-50'
                          } transition-colors`}
                        >
                          <td className="py-2 px-3 font-bold text-slate-300">Turn #{row.turnIndex}</td>
                          <td className="py-2 px-3">{row.latencyMs != null ? `${Math.round(row.latencyMs)} ms` : <span title="Latency not recorded for this turn.">—</span>}</td>
                          <td className="py-2 px-3 font-semibold">
                            {row.rssMb != null ? `${row.rssMb.toFixed(1)} MB` : <span title={notRecordedCellTip}>—</span>}
                          </td>
                          <td className="py-2 px-3">
                            {delta != null ? (
                              <span
                                className={`px-1.5 py-0.5 rounded text-[10px] font-medium ring-1 ${
                                  delta >= 25
                                    ? isDark
                                      ? 'bg-rose-500/20 text-rose-300 ring-rose-500/40'
                                      : 'bg-rose-50 text-rose-700 ring-rose-300'
                                    : delta >= 10
                                    ? isDark
                                      ? 'bg-amber-500/20 text-amber-300 ring-amber-500/40'
                                      : 'bg-amber-50 text-amber-700 ring-amber-300'
                                    : Math.abs(delta) >= 0.05
                                    ? isDark
                                      ? 'bg-cyan-500/20 text-cyan-300 ring-cyan-500/40'
                                      : 'bg-cyan-50 text-cyan-700 ring-cyan-300'
                                    : isDark
                                    ? 'bg-slate-800 text-slate-400 ring-slate-700'
                                    : 'bg-gray-100 text-gray-500 ring-gray-200'
                                }`}
                              >
                                {Math.abs(delta) < 0.05
                                  ? 'flat'
                                  : `${delta > 0 ? '+' : ''}${delta.toFixed(2)}M`}
                              </span>
                            ) : (
                              <span title={notRecordedCellTip}>—</span>
                            )}
                          </td>
                          <td className="py-2 px-3">{row.cpuMs != null ? `${row.cpuMs} ms` : <span title={notRecordedCellTip}>—</span>}</td>
                          <td className="py-2 px-3">{row.cpuPct != null ? `${row.cpuPct.toFixed(0)}%` : <span title={notRecordedCellTip}>—</span>}</td>
                          <td className="py-2 px-3">{statusBadge}</td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
              </div>
            </div>
          )}
        </div>

        {/* Modal Footer */}
        <div
          className={`flex items-center justify-end px-6 py-3 border-t ${
            isDark ? 'border-slate-800 bg-slate-900/50' : 'border-gray-100 bg-gray-50/50'
          }`}
        >
          <button
            onClick={onClose}
            className={`px-4 py-2 text-xs font-medium rounded-lg border transition-colors ${
              isDark
                ? 'border-slate-700 bg-slate-800 hover:bg-slate-700 text-slate-200'
                : 'border-gray-300 bg-white hover:bg-gray-50 text-gray-700'
            }`}
          >
            Close
          </button>
        </div>
      </div>
    </div>
  );
}
