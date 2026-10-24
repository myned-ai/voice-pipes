import { Trophy, AlertTriangle, Clock, Mic, MessageSquare, Cpu, FileText } from 'lucide-react';
import type { VariantSummary, PipelineConfigSummary } from './types';
import MetricTooltip from './MetricTooltip';
import TtsPanel from './TtsPanel';

interface Props {
  variantName: string;
  summary: VariantSummary;
  configSummary?: PipelineConfigSummary;
  isBest?: boolean;
  isDark: boolean;
  onViewConfig?: () => void;
  errorReason?: string;
}

function fmtMs(ms: number | null | undefined): string {
  if (ms == null) return '—';
  if (ms < 1000) return `${Math.round(ms)} ms`;
  return `${(ms / 1000).toFixed(1)} s`;
}

function fmtTps(tps: number | null | undefined): string {
  if (tps == null) return '—';
  return `${tps.toFixed(1)} tok/s`;
}

export default function VariantCard({ variantName, summary, configSummary, isBest, isDark, onViewConfig, errorReason }: Props) {
  const passRate = summary.scenarios_run > 0
    ? Math.round((summary.scenarios_passed / summary.scenarios_run) * 100)
    : 0;

  return (
    <div className={`rounded-xl p-6 border transition-all ${
      isBest
        ? isDark ? 'border-emerald-500/50 bg-emerald-500/5' : 'border-emerald-400 bg-emerald-50'
        : isDark ? 'border-slate-700 bg-slate-800' : 'border-gray-200 bg-white'
    }`}>
      {/* Error banner */}
      {errorReason && (
        <div className={`flex items-center gap-2 mb-4 px-3 py-2 rounded-lg text-xs font-medium ${
          isDark ? 'bg-red-500/10 text-red-400 border border-red-500/20' : 'bg-red-50 text-red-600 border border-red-200'
        }`}>
          <AlertTriangle size={14} className="shrink-0" />
          <span>{errorReason}</span>
        </div>
      )}

      <div className="flex items-center gap-2 mb-4">
        {isBest && <Trophy size={16} className="text-emerald-400" />}
        <h3 className={`text-lg font-bold ${isDark ? 'text-white' : 'text-gray-900'}`}>{variantName}</h3>
        {isBest && <span className="text-xs text-emerald-400 font-medium ml-auto">Best</span>}
      </div>

      {/* Config summary badges */}
      {configSummary && (
        <div className="flex flex-wrap gap-1.5 mb-4">
          {configSummary.stt_provider && <Badge label={`STT: ${configSummary.stt_provider}`} isDark={isDark} />}
          {configSummary.llm_model && <Badge label={`LLM: ${configSummary.llm_model}`} isDark={isDark} />}
          {configSummary.tts_provider && <Badge label={`TTS: ${configSummary.tts_provider}`} isDark={isDark} />}
          {configSummary.llm_temperature != null && <Badge label={`temp: ${configSummary.llm_temperature}`} isDark={isDark} />}
          {onViewConfig && (
            <button
              onClick={onViewConfig}
              className={`inline-flex items-center gap-1 px-2 py-0.5 rounded-full text-[10px] font-medium transition-colors ${
                isDark ? 'bg-indigo-500/15 text-indigo-300 hover:bg-indigo-500/25' : 'bg-indigo-50 text-indigo-600 hover:bg-indigo-100'
              }`}
            >
              <FileText size={10} /> Full config
            </button>
          )}
        </div>
      )}

      <div className="grid grid-cols-2 gap-y-3 gap-x-10">
        <MetricRow icon={<Clock size={14} />} label="P50 E2E" value={fmtMs(summary.p50_e2e_latency_ms)} isDark={isDark} />
        <MetricRow icon={<Clock size={14} />} label="Avg E2E" value={fmtMs(summary.avg_e2e_latency_ms)} isDark={isDark} />
        <MetricRow icon={<Clock size={14} />} label="P90 E2E" value={fmtMs(summary.p90_e2e_latency_ms)} isDark={isDark} />
        <MetricRow icon={<Mic size={14} />} label="Avg TTFT" value={fmtMs(summary.avg_ttft_ms)} isDark={isDark} />
        <MetricRow icon={<Clock size={14} />} label="P99 E2E" value={fmtMs(summary.p99_e2e_latency_ms)} isDark={isDark} />
        <MetricRow icon={<MessageSquare size={14} />} label="Avg TTFB" value={fmtMs(summary.avg_ttfb_ms)} isDark={isDark} />
      </div>

      {/* Per-component metrics (shown when available) */}
      {(summary.avg_stt_latency_ms != null || summary.avg_llm_generation_ms != null || summary.avg_tts_ttfb_ms != null || summary.avg_tokens_per_second != null) && (
        <div className={`mt-3 pt-3 border-t ${isDark ? 'border-slate-700' : 'border-gray-200'}`}>
          <div className="grid grid-cols-2 gap-y-3 gap-x-10">
            <MetricRow icon={<Cpu size={14} />} label="Avg STT" value={fmtMs(summary.avg_stt_latency_ms)} isDark={isDark} />
            <MetricRow icon={<Cpu size={14} />} label="Avg LLM Gen" value={fmtMs(summary.avg_llm_generation_ms)} isDark={isDark} />
            <MetricRow icon={<Cpu size={14} />} label="Avg TTS TTFB" value={fmtMs(summary.avg_tts_ttfb_ms)} isDark={isDark} />
            <MetricRow icon={<Cpu size={14} />} label="Tokens/s" value={fmtTps(summary.avg_tokens_per_second)} isDark={isDark} />
          </div>
        </div>
      )}

      {/* TTS-only / TTS-detail panel — hides itself when no fields populated */}
      <TtsPanel summary={summary} isDark={isDark} />

      {/* Conversation / barge-in aggregates — hides when no fields populated */}
      <ConversationPanel summary={summary} isDark={isDark} />

      <div className={`mt-4 pt-4 border-t ${isDark ? 'border-slate-700' : 'border-gray-200'} flex items-center justify-between`}>
        <div className="flex items-center gap-2">
          {summary.scenarios_failed > 0
            ? <AlertTriangle size={14} className="text-red-400" />
            : <div className="w-2 h-2 rounded-full bg-emerald-400" />
          }
          <span className={`text-sm ${isDark ? 'text-slate-300' : 'text-gray-600'}`}>
            {summary.scenarios_passed}/{summary.scenarios_run} passed
          </span>
        </div>
        <span className={`text-sm font-mono font-bold ${
          passRate === 100 ? 'text-emerald-400' : passRate >= 80 ? 'text-amber-400' : 'text-red-400'
        }`}>
          {passRate}%
        </span>
      </div>

      {summary.avg_wer != null && (
        <div
          data-testid="wer-badge"
          className={`mt-2 text-xs ${
            summary.avg_wer > 0.25
              ? 'text-red-400 font-semibold'
              : isDark ? 'text-slate-400' : 'text-gray-500'
          }`}
        >
          {summary.avg_wer > 0.25 && <AlertTriangle size={10} className="inline mr-1 -mt-0.5" />}
          WER: {(summary.avg_wer * 100).toFixed(1)}%
          {summary.avg_wer > 0.25 && ' (regression)'}
        </div>
      )}
    </div>
  );
}

function MetricRow({ icon, label, value, isDark }: { icon: React.ReactNode; label: string; value: string; isDark: boolean }) {
  return (
    <div className={`flex items-center gap-2 text-sm ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
      <span className={isDark ? 'text-slate-500' : 'text-gray-400'}>{icon}</span>
      <MetricTooltip metric={label}>{label}</MetricTooltip>
      <span className={`ml-auto font-mono font-medium ${isDark ? 'text-slate-200' : 'text-gray-800'}`}>{value}</span>
    </div>
  );
}

function Badge({ label, isDark }: { label: string; isDark: boolean }) {
  return (
    <span className={`inline-block px-2 py-0.5 rounded-full text-[10px] font-medium ${
      isDark ? 'bg-slate-700 text-slate-300' : 'bg-gray-100 text-gray-600'
    }`}>
      {label}
    </span>
  );
}

function ConversationPanel({ summary, isDark }: { summary: VariantSummary; isDark: boolean }) {
  const hasConversation = summary.conversation_scenarios != null
    || summary.avg_turns_completed != null
    || summary.avg_turn_latency_ms != null;
  const hasBargeIn = summary.barge_in_scenarios != null
    || summary.barge_in_success_rate != null
    || summary.p50_barge_in_rtt_ms != null
    || summary.p95_barge_in_rtt_ms != null
    || summary.p50_recovery_ms != null
    || summary.p95_recovery_ms != null;
  if (!hasConversation && !hasBargeIn) return null;

  return (
    <div className={`mt-3 pt-3 border-t ${isDark ? 'border-slate-700' : 'border-gray-200'}`}>
      <div className={`text-xs font-semibold uppercase tracking-wide mb-2 ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
        Conversation / Barge-in
      </div>
      <div className="grid grid-cols-2 gap-y-2 gap-x-10">
        {summary.conversation_scenarios != null && (
          <MetricRow icon={<MessageSquare size={14} />} label="Conv scenarios" value={String(summary.conversation_scenarios)} isDark={isDark} />
        )}
        {summary.avg_turns_completed != null && (
          <MetricRow icon={<MessageSquare size={14} />} label="Avg turns" value={summary.avg_turns_completed.toFixed(1)} isDark={isDark} />
        )}
        {summary.avg_turn_latency_ms != null && (
          <MetricRow icon={<Clock size={14} />} label="Avg turn lat" value={fmtMs(summary.avg_turn_latency_ms)} isDark={isDark} />
        )}
        {summary.barge_in_scenarios != null && (
          <MetricRow icon={<AlertTriangle size={14} />} label="Barge-in scenarios" value={String(summary.barge_in_scenarios)} isDark={isDark} />
        )}
        {summary.barge_in_success_rate != null && (
          <MetricRow icon={<AlertTriangle size={14} />} label="Barge-in success" value={`${(summary.barge_in_success_rate * 100).toFixed(0)}%`} isDark={isDark} />
        )}
        {summary.p50_barge_in_rtt_ms != null && (
          <MetricRow icon={<Clock size={14} />} label="P50 barge-in RTT" value={fmtMs(summary.p50_barge_in_rtt_ms)} isDark={isDark} />
        )}
        {summary.p95_barge_in_rtt_ms != null && (
          <MetricRow icon={<Clock size={14} />} label="P95 barge-in RTT" value={fmtMs(summary.p95_barge_in_rtt_ms)} isDark={isDark} />
        )}
        {summary.p50_recovery_ms != null && (
          <MetricRow icon={<Clock size={14} />} label="P50 recovery" value={fmtMs(summary.p50_recovery_ms)} isDark={isDark} />
        )}
        {summary.p95_recovery_ms != null && (
          <MetricRow icon={<Clock size={14} />} label="P95 recovery" value={fmtMs(summary.p95_recovery_ms)} isDark={isDark} />
        )}
      </div>
    </div>
  );
}
