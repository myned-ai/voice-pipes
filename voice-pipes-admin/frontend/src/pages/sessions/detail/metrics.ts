import type { Utterance, SessionEvent } from './types';
import type { TurnMetricsSnapshot } from '../../../lib/transport/types';
import { emitRtf, providerEndpointingMs, realtimeAudioOutputMs, ttsAudioDurationMs, ttsRtf } from '../../../lib/metrics/lookup';
import { normalizeLlmTiming } from './utils';

export interface AggregatedSessionMetrics {
  completedTurns: number;
  terminatedTurns: number;
  totalTurns: number;
  
  avgTtftMs: number | null;
  avgTtfbMs: number | null;
  avgEndpointingMs: number | null;
  avgBargeInRttMs: number | null;
  
  serverBargeIns: number;
  clientBargeIns: number;
  playbackBargeIns: number;
  
  totalPromptTokens: number;
  totalOutputTokens: number;
  avgTokensPerSec: number | null;
  
  totalAudioSeconds: number;
  avgRtf: number | null;

  /** Output throughput: mean / max of the
   *  per-turn `output.emit_rtf` (streaming-phase wall time ÷ audio duration;
   *  > 1.0 = emitted slower than realtime). Null when no turn carries an
   *  `output` component (captures from older workers). */
  avgEmitRtf: number | null;
  maxEmitRtf: number | null;
  /** Turns with `emit_rtf > 1.0`. */
  turnsOverRealtime: number;

  /** S2S provider endpointing lag
   *  (`provider_endpointing_ms`): mean over completed turns, from the summary
   *  or the realtime snapshot extra. Null for cascade sessions and captures
   *  predating the field. */
  avgProviderEndpointingMs: number | null;
}

/**
 * Generically computes aggregated session metrics from an array of utterances and events.
 * This can be used for a single session detail view or across multiple sessions in an analytics dashboard.
 */
export function computeSessionMetrics(utterances: Utterance[], events: SessionEvent[]): AggregatedSessionMetrics {
  const validUtterances = utterances.filter(u => !u.summary?.terminated);
  
  const ttftVals = validUtterances.map(u => u.summary?.ttft_ms).filter((v): v is number => typeof v === 'number');
  const ttfbVals = validUtterances.map(u => u.summary?.ttfb_ms).filter((v): v is number => typeof v === 'number');
  const endptVals = validUtterances.map(u => u.summary?.endpointing_ms).filter((v): v is number => typeof v === 'number');
  const rttVals = validUtterances.map(u => u.summary?.barge_in_rtt_ms).filter((v): v is number => typeof v === 'number');
  
  const avg = (arr: number[]) => arr.length > 0 ? arr.reduce((a, b) => a + b, 0) / arr.length : null;

  // Interruptions
  const serverBargeIns = validUtterances.filter(u => u.summary?.interrupted && u.summary?.interrupt_trigger === 'server_vad').length;
  const clientBargeIns = validUtterances.filter(u => u.summary?.interrupted && u.summary?.interrupt_trigger === 'client_cancel').length;
  const playbackBargeIns = validUtterances.filter(u => !u.summary?.interrupted && u.summary?.client_interrupted).length;

  // Tokens (using llm_detail preferred over llm_timing)
  const llmEvents = events.filter(e => (e.event_type || e.type) === 'llm_detail');
  // Fallback to llm_timing if no llm_detail events exist
  const llmFallback = llmEvents.length === 0 ? events.filter(e => (e.event_type || e.type) === 'llm_timing') : [];
  
  const allLlmTimings = [...llmEvents, ...llmFallback].map(normalizeLlmTiming);
  const totalPromptTokens = allLlmTimings.reduce((acc, t) => acc + (t.prompt_tokens || 0), 0);
  const totalOutputTokens = allLlmTimings.reduce((acc, t) => acc + (t.output_tokens || 0), 0);
  
  const validTokSec = allLlmTimings.map(t => t.tokens_per_sec).filter((v): v is number => typeof v === 'number');
  
  const snap = (u: Utterance): TurnMetricsSnapshot | undefined =>
    (u as unknown as { metric_snapshot?: TurnMetricsSnapshot }).metric_snapshot
    ?? (u as unknown as { latency_breakdown?: TurnMetricsSnapshot }).latency_breakdown;

  // TTS stats — derive from canonical turn snapshots; fallback to tts_detail (preferred) or tts_timing (if no tts_detail), never summing both.
  const snapAudioMs = validUtterances
    .map(u => ttsAudioDurationMs(snap(u)))
    .filter((v): v is number => typeof v === 'number');
  const snapRtf = validUtterances
    .map(u => ttsRtf(snap(u)))
    .filter((v): v is number => typeof v === 'number');

  let totalAudioSeconds = 0;
  let validRtf: number[] = [];

  if (snapAudioMs.length > 0) {
    totalAudioSeconds = snapAudioMs.reduce((acc, ms) => acc + ms / 1000, 0);
    validRtf = snapRtf;
  } else {
    // `payload` is an untyped event bag; only numeric fields count.
    const numField = (e: SessionEvent, key: string): number | null => {
      const v = e.payload?.[key];
      return typeof v === 'number' ? v : null;
    };
    const ttsEvents = events.filter(
      e => (e.event_type || e.type) === 'tts_detail' || (e.event_type || e.type) === 'tts_timing'
    );
    totalAudioSeconds = ttsEvents.reduce((acc, e) => acc + (numField(e, 'audio_duration_s') || 0), 0);
    validRtf = ttsEvents.map(e => numField(e, 'rtf')).filter((v): v is number => v !== null);
  }

  // Realtime S2S fallback: provider-native sessions (Gemini Live / OpenAI
  // Realtime) emit no tts_detail/tts_timing events. Their per-turn audio
  // duration lives in each turn's `kind: "realtime"` snapshot extra
  // `audio_output_ms`. Sum it so "Total Audio" is non-zero. Mirrors the
  // backend _compute_session_metrics fallback. See lib/metrics/lookup.ts.
  if (totalAudioSeconds === 0) {
    totalAudioSeconds = validUtterances.reduce((acc, u) => {
      const ms = realtimeAudioOutputMs(snap(u));
      return acc + (typeof ms === 'number' ? ms / 1000 : 0);
    }, 0);
  }

  // Output throughput: every turn (interrupted ones included — the `output`
  // component covers the audio emitted so far) that carries an `output`
  // component contributes its `emit_rtf`.
  const emitRtfs = utterances.map(u => emitRtf(snap(u))).filter((v): v is number => typeof v === 'number');
  const avgEmitRtf = avg(emitRtfs);
  const maxEmitRtf = emitRtfs.length > 0 ? Math.max(...emitRtfs) : null;
  const turnsOverRealtime = emitRtfs.filter(v => v > 1.0).length;

  const providerEndpointing = validUtterances
    .map(u => u.summary?.provider_endpointing_ms ?? providerEndpointingMs(snap(u)))
    .filter((v): v is number => typeof v === 'number' && Number.isFinite(v));
  const avgProviderEndpointing = avg(providerEndpointing);

  return {
    completedTurns: utterances.length - utterances.filter(u => u.summary?.terminated).length,
    terminatedTurns: utterances.filter(u => u.summary?.terminated).length,
    totalTurns: utterances.length,
    
    avgTtftMs: avg(ttftVals) ? Math.round(avg(ttftVals)!) : null,
    avgTtfbMs: avg(ttfbVals) ? Math.round(avg(ttfbVals)!) : null,
    avgEndpointingMs: avg(endptVals) ? Math.round(avg(endptVals)!) : null,
    avgBargeInRttMs: avg(rttVals) ? Math.round(avg(rttVals)!) : null,
    
    serverBargeIns,
    clientBargeIns,
    playbackBargeIns,
    
    totalPromptTokens,
    totalOutputTokens,
    avgTokensPerSec: avg(validTokSec) ? Number(avg(validTokSec)!.toFixed(1)) : null,
    
    totalAudioSeconds: Number(totalAudioSeconds.toFixed(2)),
    avgRtf: avg(validRtf) ? Number(avg(validRtf)!.toFixed(2)) : null,

    avgEmitRtf: avgEmitRtf != null ? Number(avgEmitRtf.toFixed(2)) : null,
    maxEmitRtf: maxEmitRtf != null ? Number(maxEmitRtf.toFixed(2)) : null,
    turnsOverRealtime,

    avgProviderEndpointingMs: avgProviderEndpointing != null ? Math.round(avgProviderEndpointing) : null,
  };
}
