import { Mic, MessageSquare, Clock } from 'lucide-react';
import { useTheme } from '../../../contexts/ThemeContext';
import type { OrchTurn, SessionEvent } from './types';
import { StatusBadge } from '../../../components/StatusBadge';
import { LlmTimingCard, EventCardRenderer } from './EventCards';
import { PipelineTimeline } from './PipelineTimeline';
import { TurnComponentMetricsTable } from '../../../components/metrics/TurnComponentMetricsTable';
import { useSessionDescriptors } from '../../playground/SessionDescriptorsContext';
import {
  endpointingMs,
  llmGenerationMs,
  llmTokens,
  sttLatencyMs,
  ttfbMs,
  ttftMs,
  ttsAudioDurationMs,
  ttsChars,
  ttsProvider,
  ttsRtf,
  ttsSynthesisMs,
  ttsTtfbMs,
} from '../../../lib/metrics/lookup';

// ── Orchestrator Turn Card ──────────────────────────────────
//
// Renders a single per-turn card sourced exclusively from the orchestrator
// API (the `turn.detail` payload), used when the local captures directory
// has no per-utterance files for this session.  Displays the same conceptual
// layout as `TurnCard` (transcript → response → pipeline timeline → stats),
// minus audio playback (which requires the captured WAV fragments).

export const OrchestratorTurnCard = ({ turn, index, sessionSttProvider, sessionTtsProvider }: { turn: OrchTurn & { text_preview?: string }; index: number; sessionSttProvider?: string; sessionTtsProvider?: string }) => {
  const { isDark } = useTheme();
  const descriptors = useSessionDescriptors();
  const turnStatus = turn.status ?? 'completed';
  const events: SessionEvent[] = turn.events ?? [];
  const llmTimingEvt = events.find((e) => (e.event_type || e.type) === 'llm_detail') || events.find((e) => (e.event_type || e.type) === 'llm_timing');
  const llmResponseEvt = events.find((e) => (e.event_type || e.type) === 'llm_response');

  // Detail payload from the orchestrator carries authoritative per-turn data.
  const detail = turn.detail;
  const lb = detail?.latency_breakdown;
  const userText = detail?.stt?.text || detail?.input?.text || null;
  // Best-effort extraction of agent response text (orch may carry it in
  // events, otherwise fall back to text_preview).
  const llmResponseText = (() => {
    if (llmResponseEvt && typeof llmResponseEvt.payload === 'object' && llmResponseEvt.payload !== null) {
      const p = llmResponseEvt.payload as Record<string, unknown>;
      if (typeof p.text === 'string') return p.text;
    }
    return turn.text_preview ?? null;
  })();

  // Timeline values: prefer authoritative latency_breakdown (snapshot),
  // fall back to flat orch fields, then to events.
  const sttMs = sttLatencyMs(lb) ?? detail?.stt?.latency_ms ?? 0;
  const llmMs = llmGenerationMs(lb) ?? 0;
  const ttsSynth = ttsSynthesisMs(lb);
  const ttsMs = ttsSynth ?? detail?.tts?.latency_ms ?? 0;
  const hasTimeline = sttMs > 0 || llmMs > 0 || ttsMs > 0;
  const ttsRtfVal = ttsRtf(lb);
  const ttsCharsVal = ttsChars(lb);
  const ttsAudioMs = ttsAudioDurationMs(lb);
  const ttsProviderVal = ttsProvider(lb);
  const llmTokensVal = llmTokens(lb);
  const interrupted = lb?.interrupted ?? false;

  return (
    <div className={`rounded-xl p-5 space-y-4 border ${isDark ? 'bg-slate-800 border-slate-700' : 'bg-white border-gray-200'}`}>
      {/* Header */}
      <div className={`flex items-center justify-between pb-3 border-b ${isDark ? 'border-slate-700' : 'border-gray-200'}`}>
        <h4 className={`font-bold flex items-center gap-2 ${isDark ? 'text-white' : 'text-gray-900'}`}>
          <span className="bg-indigo-500/20 text-indigo-300 text-xs font-mono px-2 py-0.5 rounded">
            #{index + 1}
          </span>
          Turn
          {<StatusBadge status={turnStatus} />}
        </h4>
        <div className={`flex items-center gap-3 text-xs ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
          {turn.total_ms != null && (
            <span className="flex items-center gap-1">
              <Clock size={12} /> {Math.round(turn.total_ms)}ms
            </span>
          )}
          {turn.otel_trace_id && (
            <span className={`font-mono text-[10px] ${isDark ? 'text-slate-600' : 'text-gray-400'}`} title="OTEL Trace ID">
              {turn.otel_trace_id.slice(0, 12)}…
            </span>
          )}
        </div>
      </div>

      {/* Pipeline Timeline (sourced from orch latency_breakdown) */}
      {hasTimeline && (
        <PipelineTimeline
          sttMs={sttMs}
          llmMs={llmMs}
          ttsMs={ttsMs}
          ttftMs={ttftMs(lb)}
          ttfbMs={ttfbMs(lb)}
          totalMs={lb?.total_ms ?? turn.total_ms}
          llmGenerationMs={llmGenerationMs(lb)}
          ttsTtfbMs={ttsTtfbMs(lb)}
          endpointingMs={endpointingMs(lb)}
          llmOutputTokens={llmTokensVal}
        />
      )}

      {/* Per-component metrics table (descriptor-driven). Renders one
          section per component in `latency_breakdown.components`. When
          the session has no descriptors yet, only header rows render
          (provider / latency / cold-start / error / running). */}
      {lb && lb.components.length > 0 && (
        <TurnComponentMetricsTable snapshot={lb} descriptors={descriptors} />
      )}

      {/* User transcript */}
      {userText && (
        <div className="flex items-start gap-3">
          <Mic size={14} className={`mt-0.5 shrink-0 ${isDark ? 'text-sky-300' : 'text-sky-600'}`} />
          <span className={`text-sm leading-relaxed ${isDark ? 'text-sky-300' : 'text-sky-800'}`}>
            {userText}
          </span>
        </div>
      )}

      {/* Agent response */}
      {llmResponseText && (
        <div className="flex items-start gap-3">
          <MessageSquare size={14} className={`mt-0.5 shrink-0 ${isDark ? 'text-emerald-300' : 'text-emerald-600'}`} />
          <span className={`text-sm leading-relaxed ${isDark ? 'text-emerald-100' : 'text-emerald-800'}`}>
            {llmResponseText}
          </span>
        </div>
      )}

      {/* LLM Timing (from turn events) — only when orch latency_breakdown
          didn't already drive the timeline above. */}
      {llmTimingEvt && !hasTimeline && <LlmTimingCard event={llmTimingEvt} />}

      {/* Inline events list — only if nothing else rendered above. */}
      {events.length > 0 && !llmTimingEvt && !llmResponseEvt && !hasTimeline && (
        <div className="space-y-3">
          {events.map((evt, i) => (
            <EventCardRenderer key={i} event={evt} />
          ))}
        </div>
      )}

      {/* Stats footer: TTS/LLM details from latency_breakdown */}
      {(lb || turn.stt_provider || turn.tts_provider) && (
        <div className={`flex flex-wrap gap-x-5 gap-y-1 text-[10px] pt-3 border-t ${isDark ? 'text-slate-500 border-slate-700/50' : 'text-gray-400 border-gray-200'}`}>
          {llmTokensVal != null && <span>{llmTokensVal} tok</span>}
          {ttsRtfVal != null && (
            <span className={`font-mono font-semibold ${isDark ? 'text-violet-400/80' : 'text-violet-600'}`}>
              {ttsRtfVal.toFixed(2)}x RTF
            </span>
          )}
          {ttsSynth != null && (
            <span title="Total TTS synthesis: first text fragment → last audio chunk (pre-pacer)">
              TTS {Math.round(ttsSynth)}ms
            </span>
          )}
          {ttsCharsVal != null && <span>{ttsCharsVal} chars</span>}
          {ttsAudioMs != null && (
            <span>{(ttsAudioMs / 1000).toFixed(2)}s audio</span>
          )}
          {/* Suppress provider when it matches the session-level provider. */}
          {(ttsProviderVal ?? turn.tts_provider) && (ttsProviderVal ?? turn.tts_provider) !== sessionTtsProvider && (
            <span>TTS: {ttsProviderVal ?? turn.tts_provider}</span>
          )}
          {turn.stt_provider && turn.stt_provider !== sessionSttProvider && <span>STT: {turn.stt_provider}</span>}
          {turn.started_at && <span>{new Date(turn.started_at).toLocaleTimeString()}</span>}
          {turn.error && <span className="text-red-400">Error: {turn.error}</span>}
          {interrupted && (
            <span className={isDark ? 'text-amber-400' : 'text-amber-600'}>interrupted</span>
          )}
        </div>
      )}
    </div>
  );
};

