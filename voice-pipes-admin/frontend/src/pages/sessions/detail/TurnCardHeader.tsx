import { AlertCircle, Clock } from 'lucide-react';
import { useTheme } from '../../../contexts/ThemeContext';
import { arrivalRtfOf, type TurnDisplayState, type TurnSummary, type OrchTurn, type Utterance, type ReportAnomaly } from './types';
import { lastPipelineStage } from './utils';
import { StatusBadge } from '../../../components/StatusBadge';
import { audioPostComponents, chainOverheadMs, emitRtf, pacerWaitMs, sourceRtf } from '../../../lib/metrics/lookup';
import { describeAnchorSource } from '../../../lib/metrics/anchorSource';

/** S2S provider endpointing budget (report `targets.endpointing_ms`). */
const PROVIDER_ENDPOINTING_BUDGET_MS = 500;

interface Props {
  utt: Utterance;
  /**
   * Single derived UI state for the turn. Drives header styling and which
   * interruption chip (if any) is shown. Computed via `deriveTurnDisplayState`.
   */
  displayState: TurnDisplayState;
  /** Derived turn status string ('completed' | 'terminated' | orchTurn.status). */
  turnStatus?: string;
  summary?: TurnSummary;
  orchTurn?: OrchTurn;
  /**
   * Server-detected anomalies for this turn.
   * Sourced from `/report-for-llm?format=json`'s `anomalies[]` and
   * pre-grouped by `utt` in `SessionDetail`.  Renders one chip per item;
   * `info`-level rows are advisory and styled muted.
   */
  anomalies?: ReportAnomaly[];
  /**
   * U-2: this turn is the session's cold-start turn (smallest utterance_id).
   * Renders a muted "Cold start" badge so its latencies aren't compared
   * against subsequent warmed-up turns.
   */
  isColdStart?: boolean;
  /**
   * True when this turn belongs to a realtime speech-to-speech session.
   * Controls the fallback label used by `lastPipelineStage` when no
   * classic stt/llm/tts artefact is present — realtime turns collapse
   * those stages into a single fused provider stage, so the default
   * "start" label would be misleading for terminated realtime turns.
   */
  isRealtime?: boolean;
}

// Map anomaly `kind` (matches `_ANOMALY_COMPONENT` in main.py) → short
// human-readable label for the chip.  Keep terse — full detail goes in the
// tooltip.  Unknown kinds fall back to the raw key, which is still useful
// during development of new anomaly types.
const ANOMALY_LABEL: Record<string, string> = {
  'llm.ttft_high':         'TTFT high',
  'llm.throughput_low':    'tok/s low',
  'tts.ttfb_high':         'TTFB high',
  'tts.rtf_high':          'RTF high',
  'tts.audio_truncated':   'audio cut',
  'stt.processing_high':   'STT slow',
  'stt.endpointing_high':  'VAD slow',
  'bi.likely_self_barge':  'self-barge?',
  'bi.rtt_high':           'RTT high',
  'turn.total_outlier':    'total outlier',
  // Throughput / post-processor / client
  // arrival kinds, and the annotator (metadata-only) kinds.
  'output.slower_than_realtime': 'output < realtime',
  'audio_post.over_budget':      'post-proc over budget',
  'client.starvation':           'starvation',
  'client.jitter':               'jitter',
  'annotator.lagging':           'annotator lag',
  'annotator.degraded':          'annotator degraded',
  'annotator.unavailable':       'annotator down',
};

function anomalyChipColor(kind: string, level: string, isDark: boolean): string {
  if (level === 'info') {
    return isDark ? 'bg-slate-700/60 text-slate-300 ring-slate-600' : 'bg-gray-100 text-gray-600 ring-gray-300';
  }
  // Per-component palette so the eye can scan a long card list quickly.
  const comp = kind.split('.')[0];
  switch (comp) {
    case 'llm':
      return isDark ? 'bg-amber-500/15 text-amber-300 ring-amber-500/30' : 'bg-amber-50 text-amber-700 ring-amber-300';
    case 'tts':
    case 'realtime':
    case 'output':
    case 'audio_post':
      // Realtime / output / post-processor anomalies share TTS's violet
      // hue — they are all the "audio output" stage from the user's
      // perspective, so a single colour helps spot it without a separate
      // legend entry.
      return isDark ? 'bg-violet-500/15 text-violet-300 ring-violet-500/30' : 'bg-violet-50 text-violet-700 ring-violet-300';
    case 'stt':
    case 'client':
      // Client-side arrival diagnosis (starvation / jitter) — sky, like the
      // other client-measured numbers in the Playground.
      return isDark ? 'bg-sky-500/15 text-sky-300 ring-sky-500/30' : 'bg-sky-50 text-sky-700 ring-sky-300';
    case 'annotator':
      // Metadata only — audio unaffected. Indigo keeps it visibly apart
      // from the speech-path palette.
      return isDark ? 'bg-indigo-500/15 text-indigo-300 ring-indigo-500/30' : 'bg-indigo-50 text-indigo-700 ring-indigo-300';
    case 'bi':
    case 'turn':
    default:
      return isDark ? 'bg-rose-500/15 text-rose-300 ring-rose-500/30' : 'bg-rose-50 text-rose-700 ring-rose-300';
  }
}

/**
 * Header row for a TurnCard — turn number badge, status badge, and metric chips.
 *
 * Chip precedence is enforced by `displayState` so a single turn never shows
 * both "Terminated" and "Playback barge-in" simultaneously.
 */
export function TurnCardHeader({ utt, displayState, turnStatus, summary, orchTurn, anomalies, isColdStart, isRealtime }: Props) {
  const { isDark } = useTheme();
  const isTerminated = displayState === 'terminated';
  // Agent-initiated turn (idle prompt, filler follow-up, proactive
  // speech): no user input, latencies anchored at turn-open.
  const isAgentTurn = utt.metric_snapshot?.initiator === 'agent';

  return (
    <div className={`flex items-center justify-between pb-3 border-b ${isDark ? 'border-slate-700' : 'border-gray-200'}`}>
      {/* ── Left: turn number + status + anomaly chips ── */}
      <h4 className={`font-bold flex items-center gap-2 flex-wrap ${isDark ? 'text-white' : 'text-gray-900'}`}>
        <span className={`text-xs font-mono px-2 py-0.5 rounded ${isTerminated ? 'bg-orange-500/20 text-orange-300' : 'bg-indigo-500/20 text-indigo-300'}`}>
          #{utt.utterance_id}
        </span>
        {isTerminated ? `Terminated after ${lastPipelineStage(utt, isRealtime ?? false)}` : 'Turn'}
        {turnStatus && <StatusBadge status={turnStatus} />}
        {/* Agent-initiated turn badge: the pipeline spoke without user
            input. Latency chips on this row are turn-open-anchored, not
            end-of-user-speech-anchored — not comparable to user turns. */}
        {isAgentTurn && (
          <span
            className={`text-[10px] font-medium px-1.5 py-0.5 rounded ring-1 ${isDark ? 'bg-indigo-500/15 text-indigo-300 ring-indigo-500/30' : 'bg-indigo-50 text-indigo-700 ring-indigo-300'}`}
            title="Agent-initiated turn (idle prompt, tool follow-up, or proactive speech) — no user input preceded it; latencies are measured from turn open, not end of user speech."
          >
            Agent
          </span>
        )}
        {/* U-2: cold-start badge.  Muted styling on purpose — it's an
            informational marker, not a problem indicator.  Tooltip explains
            why these latencies aren't comparable to warmed turns. */}
        {isColdStart && (
          <span
            className={`text-[10px] font-medium px-1.5 py-0.5 rounded ring-1 ${isDark ? 'bg-slate-700/60 text-slate-300 ring-slate-600' : 'bg-gray-100 text-gray-600 ring-gray-300'}`}
            title="First turn of the session — includes one-time TLS handshake, model load, and voice-model load costs that subsequent turns avoid."
          >
            Cold start
          </span>
        )}
        {/* Output-throughput badge: the
            server emitted this turn's audio slower than it plays
            (`output.emit_rtf > 1.0`). Shown as playback speed (1 / emit_rtf)
            because "0.60× realtime" is what the listener experiences once the
            prebuffer drains. Tooltip splits provider vs post-processor chain. */}
        {(() => {
          const rtf = emitRtf(utt.metric_snapshot);
          if (rtf == null || rtf <= 1.0) return null;
          const src = sourceRtf(utt.metric_snapshot);
          const chain = chainOverheadMs(utt.metric_snapshot);
          const pacer = pacerWaitMs(utt.metric_snapshot);
          const hasPosts = audioPostComponents(utt.metric_snapshot).length > 0;
          const severe = rtf >= 1.2;
          const cls = severe
            ? (isDark ? 'bg-red-500/15 text-red-300 ring-red-500/40' : 'bg-red-50 text-red-700 ring-red-300')
            : (isDark ? 'bg-violet-500/15 text-violet-300 ring-violet-500/40' : 'bg-violet-50 text-violet-700 ring-violet-300');
          // A source/emit gap is the chain's only when a processor ran;
          // otherwise it is pacing or the transport.
          const culprit = src != null && rtf - src > 0.3
            ? (hasPosts
              ? 'the post-processor chain, not the provider, is the cause'
              : 'the provider delivered in time and no post-processor ran; the delay is between receiving the audio and sending it')
            : src != null && src > 1.0
              ? 'the provider itself delivers slower than realtime'
              : null;
          const tip = [
            `Server output throughput: the streaming phase emitted audio at ${(1 / rtf).toFixed(2)}× playback speed (emit_rtf ${rtf.toFixed(2)}; > 1.0 = slower than realtime). The client hears it fine until its prebuffer drains, then playback breaks up.`,
            src != null ? `provider alone: source_rtf ${src.toFixed(2)}` : null,
            chain != null && hasPosts ? `post-processor chain: +${Math.round(chain)}ms` : null,
            pacer ? `pacer wait: ${Math.round(pacer)}ms` : null,
            culprit,
          ].filter(Boolean).join(' · ');
          return (
            <span className={`text-[10px] font-medium px-1.5 py-0.5 rounded ring-1 ${cls}`} title={tip}>
              output {(1 / rtf).toFixed(2)}× realtime
            </span>
          );
        })()}
        {/* Playback-health chip: client-reported underruns + inserted
            silence. Severity mirrors the
            verdict palette: amber for audible breaks, red when the silence
            dominates the turn. Reconstructed from client playback timing, and
            adds the cause — *starvation*
            (audio arrived slower than realtime: server output or network
            throughput) vs *jitter* (arrivals kept up on average; bursty
            delivery vs the prebuffer) — from `playback_health.arrival_wall_ms`,
            and marks frames sent from the barge-in/cancel flush path. */}
        {(() => {
          const ph = utt.playback_health;
          const underruns = summary?.audio_underruns ?? ph?.underrun_count;
          if (underruns == null || underruns <= 0) return null;
          const silenceMs = summary?.total_silence_ms ?? ph?.total_silence_ms ?? 0;
          const severe = silenceMs >= 3000;
          const cls = severe
            ? (isDark ? 'bg-red-500/15 text-red-300 ring-red-500/40' : 'bg-red-50 text-red-700 ring-red-300')
            : (isDark ? 'bg-amber-500/15 text-amber-300 ring-amber-500/40' : 'bg-amber-50 text-amber-700 ring-amber-300');
          const wallMs = summary?.wall_clock_ms ?? ph?.wall_ms;
          const audioMs = summary?.audio_duration_ms ?? ph?.audio_ms;
          const ratio = wallMs != null && audioMs != null && audioMs > 0 && wallMs > audioMs * 1.1
            ? ` · ${(wallMs / audioMs).toFixed(1)}× slower than the audio`
            : '';
          const arrivalRtf = arrivalRtfOf(ph);
          const cause = arrivalRtf == null ? null : arrivalRtf > 1.0 ? 'starvation' : 'jitter';
          const causeTip = arrivalRtf == null
            ? ''
            : cause === 'starvation'
              ? ` · starvation: audio arrived slower than it plays (arrival_rtf ${arrivalRtf.toFixed(2)}) — server output or network throughput, not the prebuffer`
              : ` · jitter: arrivals kept up with playback on average (arrival_rtf ${arrivalRtf.toFixed(2)}) — bursty delivery vs the client prebuffer`;
          const interrupted = ph?.interrupted === true;
          const interruptedTip = interrupted
            ? ' · interrupted: reported from the barge-in/cancel flush, covers the audio delivered until then'
            : '';
          return (
            <span
              className={`text-[10px] font-medium px-1.5 py-0.5 rounded ring-1 ${cls}`}
              title={`Client playback health (reconstructed from client playback timing, not a recording): ${underruns} underrun break${underruns !== 1 ? 's' : ''}, ${(silenceMs / 1000).toFixed(1)}s of inserted silence${ratio}${causeTip}${interruptedTip}`}
            >
              ⚠ {underruns} break{underruns !== 1 ? 's' : ''} · {(silenceMs / 1000).toFixed(1)}s silent{cause ? ` · ${cause}` : ''}{interrupted ? ' · interrupted' : ''}
            </span>
          );
        })()}
        {/* Anomaly chips from server-side `_detect_anomalies`.  Only render
            warn-level chips inline — info-level rows are appended muted. */}
        {anomalies && anomalies.length > 0 && (
          <span className="flex items-center gap-1 flex-wrap">
            {anomalies.map((a, i) => {
              const label = ANOMALY_LABEL[a.kind] ?? a.kind;
              // v2: an off-critical-path breach (e.g. STT processing that
              // overlaps speech) or a negligible-severity one is advisory —
              // render it muted like an info row so it doesn't read as a defect.
              const advisory = a.on_critical_path === false || a.severity === 'negligible';
              const cls = anomalyChipColor(a.kind, advisory ? 'info' : a.level, isDark);
              const tip = [
                a.detail,
                a.observed != null && a.target != null
                  ? `observed=${a.observed} target=${a.target}`
                  : null,
                a.severity ? `severity=${a.severity}` : null,
                a.on_path_observed != null ? `user-facing≈${a.on_path_observed}ms (overlap-adjusted)` : null,
                a.on_critical_path === false ? 'off critical path (overlaps speech)' : null,
                `kind=${a.kind} (${a.level})`,
              ].filter(Boolean).join(' · ');
              return (
                <span
                  key={`${a.kind}-${i}`}
                  className={`text-[10px] font-medium px-1.5 py-0.5 rounded ring-1 ${cls}`}
                  title={tip}
                >
                  {label}
                </span>
              );
            })}
          </span>
        )}
      </h4>

      {/* ── Right: metric chips ── */}
      <div className={`flex items-center gap-3 text-xs ${isDark ? 'text-slate-400' : 'text-gray-500'}`}>
        {/* Server-VAD or client-cancel barge-in (server-side interruption) */}
        {displayState === 'interrupted' && (
          <span
            className="flex items-center gap-1 text-amber-400"
            title={
              summary?.interrupt_trigger === 'server_vad'
                ? 'Interrupted by server VAD (barge-in detected)'
                : summary?.interrupt_trigger === 'client_cancel'
                ? 'Interrupted by client cancel'
                : 'Interrupted'
            }
          >
            <AlertCircle size={12} />
            {summary?.interrupt_trigger === 'server_vad'
              ? 'VAD barge-in'
              : summary?.interrupt_trigger === 'client_cancel'
              ? 'Client cancel'
              : 'Interrupted'}
          </span>
        )}

        {/* Client-side playback barge-in (server completed; client stopped audio early).
            This is a successful turn with client-side interruption context, NOT a failure. */}
        {displayState === 'playback_barge_in' && (
          <span
            className="flex items-center gap-1 text-amber-400/70"
            title="Server finished sending audio, but user spoke before client finished playing it"
          >
            <AlertCircle size={12} />
            Playback barge-in
          </span>
        )}

        {/* Cascade endpointing (VAD silence wait). Hidden when the S2S
            provider-endpointing chip below carries the same quantity. */}
        {summary?.endpointing_ms != null && summary.provider_endpointing_ms == null && (
          <span
            className={`flex items-center gap-1 ${isDark ? 'text-rose-400' : 'text-rose-500'}`}
            title="VAD silence wait (endpointing delay)"
          >
            VAD {Math.round(summary.endpointing_ms)}ms
          </span>
        )}

        {/* S2S provider endpointing: how
            late the provider's VAD / transcription noticed the end of speech
            relative to the worker's own stamp. User-felt wait on the critical
            path; rose over the 500 ms budget. */}
        {summary?.provider_endpointing_ms != null && (() => {
          const v = Math.round(summary.provider_endpointing_ms);
          const over = v > PROVIDER_ENDPOINTING_BUDGET_MS;
          return (
            <span
              className={`flex items-center gap-1 ${over ? (isDark ? 'text-rose-400' : 'text-rose-500') : ''}`}
              title={`Provider endpointing (S2S): the provider's end-of-speech anchor landed ${v} ms after the worker's own VAD saw the user stop — the provider's VAD / transcription lag, waited through before TTFB starts (budget ${PROVIDER_ENDPOINTING_BUDGET_MS} ms)${over ? ' — over budget' : ''}`}
            >
              Endpointing {v}ms
            </span>
          );
        })()}

        {displayState === 'interrupted' && summary?.barge_in_rtt_ms != null && (
          <span
            className={`flex items-center gap-1 ${isDark ? 'text-rose-300' : 'text-rose-600'}`}
            title="Barge-in round-trip time"
          >
            RTT {Math.round(summary.barge_in_rtt_ms)}ms
          </span>
        )}

        {summary?.ttft_ms != null && (
          <span className="flex items-center gap-1" title="Time to First Token">
            TTFT {Math.round(summary.ttft_ms)}ms
          </span>
        )}

        {summary?.ttfb_ms != null && (() => {
          const anchor = summary.ttfb_anchor_source ? describeAnchorSource(summary.ttfb_anchor_source) : null;
          const tip = [
            'Time to First Byte (audio): end-of-speech anchor → first audio byte',
            anchor ? `anchor: ${anchor.label} (${anchor.detail})` : null,
            summary.first_text_ms != null ? `first transcript delta at ${Math.round(summary.first_text_ms)} ms` : null,
          ].filter(Boolean).join(' · ');
          return (
            <span className="flex items-center gap-1" title={tip}>
              TTFB {Math.round(summary.ttfb_ms)}ms
            </span>
          );
        })()}

        {/* Anchor provenance badge: says
            what the TTFB / E2E t=0 actually is. Amber for the late transcript
            proxy, rose when there is no anchor at all. */}
        {summary?.ttfb_anchor_source && (() => {
          const a = describeAnchorSource(summary.ttfb_anchor_source);
          const cls = a.tone === 'proxy'
            ? (isDark ? 'bg-amber-500/15 text-amber-300 ring-amber-500/40' : 'bg-amber-50 text-amber-700 ring-amber-300')
            : a.tone === 'none'
              ? (isDark ? 'bg-rose-500/15 text-rose-300 ring-rose-500/40' : 'bg-rose-50 text-rose-700 ring-rose-300')
              : (isDark ? 'bg-slate-700/60 text-slate-300 ring-slate-600' : 'bg-gray-100 text-gray-600 ring-gray-300');
          return (
            <span
              className={`text-[10px] font-medium px-1.5 py-0.5 rounded ring-1 ${cls}`}
              title={`End-of-speech anchor for this turn's TTFB: ${a.detail} (ttfb_anchor_source=${summary.ttfb_anchor_source})`}
            >
              anchor: {a.label}
            </span>
          );
        })()}

        {/* Wall-clock turn duration — the single home for this metric.
            Suppress when total is 0/absent (interrupted turns record
            total_ms=0; the timeline then shows a derived "(est.)" instead). */}
        {(() => {
          const total = summary?.total_ms ?? orchTurn?.total_ms;
          return total != null && total > 0 ? (
            <span
              className="flex items-center gap-1"
              title="Wall-clock turn duration: from utterance commit (end of user speech) to the last audio byte produced — includes the full TTS tail. The Response/TTFB span is the time to the first audio byte within this."
            >
              <Clock size={12} /> {Math.round(total)}ms
            </span>
          ) : null;
        })()}

        {summary?.e2e_perceived_ms != null && (() => {
          const anchor = summary.e2e_anchor_source ? describeAnchorSource(summary.e2e_anchor_source) : null;
          const base = summary.e2e_estimated
            ? "Perceived E2E latency (server-side estimate): user finished speaking to first audio byte at the server; excludes network delivery to the client"
            : "Perceived E2E latency: time from user finished speaking to first audio received by client";
          const tip = anchor ? `${base} · anchor: ${anchor.label} (${anchor.detail})` : base;
          return (
            <span
              className={`flex items-center gap-1 font-semibold ${isDark ? 'text-emerald-400' : 'text-emerald-600'}`}
              title={tip}
            >
              E2E {summary.e2e_estimated ? '~' : ''}{Math.round(summary.e2e_perceived_ms)}ms
            </span>
          );
        })()}

        {/* Per-turn process physical memory and CPU load */}
        {(() => {
          const res = orchTurn?.detail?.resource_usage ?? utt.resource_usage;
          if (!res) return null;
          const mb = res.rss_bytes != null ? (res.rss_bytes / (1024 * 1024)).toFixed(1) : null;
          const deltaMb = res.rss_delta_bytes != null ? (res.rss_delta_bytes / (1024 * 1024)).toFixed(2) : null;
          const cpuPct = res.cpu_percent != null ? `${res.cpu_percent.toFixed(0)}%` : null;
          const cpuMs = res.cpu_time_ms != null ? `${res.cpu_time_ms}ms` : null;
          const cgroupMb = res.container_peak_memory_bytes != null ? `${(res.container_peak_memory_bytes / (1024 * 1024)).toFixed(0)} MB` : null;

          const tipParts = [
            mb ? `Process physical resident memory (RSS): ${mb} MB` : null,
            deltaMb ? `Turn memory delta: ${Number(deltaMb) > 0 ? '+' : ''}${deltaMb} MB` : null,
            cpuPct ? `Turn CPU utilization: ${cpuPct} (${cpuMs ?? ''} CPU time)` : null,
            cgroupMb ? `Container peak memory: ${cgroupMb}` : null,
            'Deterministic Rust runtime with zero GC pauses',
          ].filter(Boolean).join(' · ');

          return (
            <>
              {mb && (
                <span
                  className={`text-[10px] font-mono font-medium px-1.5 py-0.5 rounded ring-1 ${
                    isDark ? 'bg-cyan-500/15 text-cyan-300 ring-cyan-500/30' : 'bg-cyan-50 text-cyan-700 ring-cyan-300'
                  }`}
                  title={tipParts}
                >
                  RAM {mb}M{deltaMb && Number(deltaMb) !== 0 ? ` (${Number(deltaMb) > 0 ? '+' : ''}${deltaMb})` : ''}
                </span>
              )}
              {cpuPct && (
                <span
                  className={`text-[10px] font-mono font-medium px-1.5 py-0.5 rounded ring-1 ${
                    isDark ? 'bg-teal-500/15 text-teal-300 ring-teal-500/30' : 'bg-teal-50 text-teal-700 ring-teal-300'
                  }`}
                  title={tipParts}
                >
                  CPU {cpuPct}
                </span>
              )}
            </>
          );
        })()}

        {orchTurn?.otel_trace_id && (
          <span
            className={`font-mono text-[10px] ${isDark ? 'text-slate-600' : 'text-gray-400'}`}
            title="OTEL Trace ID"
          >
            {orchTurn.otel_trace_id.slice(0, 12)}…
          </span>
        )}
      </div>
    </div>
  );
}
