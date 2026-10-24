// Typed interfaces for the session detail data model.
// These replace the `any` casts scattered through SessionDetail.tsx and make
// the shape of the server response explicit and checkable.

import type { ComponentMetricDescriptor, TurnMetricsSnapshot } from "../../../lib/transport/types";

/**
 * Display state for an agent turn in the Session History Details UI.
 *
 * Distinct from the boolean flags on `TurnSummary` because several flags can be
 * set at the same time (e.g. `terminated && client_interrupted`) and the UI
 * needs a single, well-defined state per turn.
 *
 * Precedence (highest first):
 *   1. `terminated`          — turn aborted before normal completion (failure path).
 *   2. `interrupted`         — server-side barge-in (server_vad / client_cancel)
 *                              that stopped LLM/TTS generation.
 *   3. `playback_barge_in`   — client-only interruption: server completed the turn
 *                              but the client stopped audio playback early.
 *   4. `completed`           — turn finished normally with no interruption.
 *
 * Note: `playback_barge_in` is a *successful* turn from the server's perspective
 * and must NOT be styled as a failure.
 */
export type TurnDisplayState =
  | 'completed'
  | 'interrupted'
  | 'playback_barge_in'
  | 'terminated';

export interface TurnSummary {
  interrupted?: boolean;
  client_interrupted?: boolean;
  /** Which agent triggered the barge-in: 'server_vad' | 'client_cancel' | string */
  interrupt_trigger?: string;
  barge_in_rtt_ms?: number;
  /** Milliseconds of audio that was actually played before interruption. */
  last_played_offset_ms?: number;
  /** STT endpointing (canonical; derived from the per-turn metric snapshot). */
  endpointing_ms?: number;
  ttft_ms?: number;
  /** True when `ttft_ms` was filled from `llm_detail.first_token_ms` because
   *  the live metric was not captured (typical for the cold-start turn). */
  ttft_estimated?: boolean;
  /** Time to first audio byte — the runner-observed (perceived) TTFB the engine
   *  writes onto the TTS component. Canonical; sourced from the metric snapshot. */
  ttfb_ms?: number;
  /** TTS pipeline-internal (pre-pacer) TTFB: first LLM fragment → first audio
   *  sample. Distinct from `ttfb_ms`; drives the timeline's TTS-startup segment. */
  tts_internal_ttfb_ms?: number;
  /** End-to-end turn duration (canonical). */
  total_ms?: number;
  /** STT processing latency (canonical). */
  processing_ms?: number;
  /** LLM generation duration (canonical). */
  generation_ms?: number;
  /** Total TTS synthesis span — first LLM text fragment to last audio chunk (pre-pacer). */
  synthesis_ms?: number;
  e2e_perceived_ms?: number;
  /** True when e2e_perceived_ms is a server-side estimate (realtime S2S
   *  speech-end-anchored TTFB) rather than the wallclock
   *  user-speech-end → first-audio-at-client measurement. */
  e2e_estimated?: boolean;
  /** Which end-of-speech stamp `e2e_perceived_ms` starts from
   *  (precedence: client › vp › provider):
   *  `client_speech_end` | `vp_vad` | `provider_speech_stopped` |
   *  `input_transcript_proxy` | … Absent on older BFFs / captures. */
  e2e_anchor_source?: string;
  /** S2S: anchor → first output-transcript delta (ms) — "the model started";
   *  diagnostic, the audio may follow much later. */
  first_text_ms?: number;
  /** S2S: provenance of the TTFB anchor (`provider_speech_stopped` |
   *  `input_transcript_proxy` | `vp_vad` | `client_speech_end` |
   *  `text_submit` | `turn_start` = no anchor, TTFB ≈ 0 is not a latency). */
  ttfb_anchor_source?: string;
  /** S2S: provider anchor − vp VAD stamp (ms) — how late the provider noticed
   *  the end of speech. The S2S `endpointing_ms`; budget 500 ms. */
  provider_endpointing_ms?: number;
  tts_provider?: string;
  stt_provider?: string;
  /** STT execution mode resolved by the BFF: 'streaming' | 'batch' | 'unknown'.
   *  Streaming providers (AssemblyAI, Deepgram, OpenAI Realtime) deliver the
   *  transcript incrementally during the user's speech, so `processing_ms`
   *  is not directly comparable to batch providers. */
  stt_mode?: 'streaming' | 'batch' | 'unknown' | string;
  audio_underruns?: number;
  /** Ideal audio duration of the turn (ms). Projected by the BFF from the
   *  metric snapshot or, failing that, from `playback_health.audio_ms`. */
  audio_duration_ms?: number;
  /** Total client-side silence inserted by underruns (ms) — from
   *  `playback_health`. */
  total_silence_ms?: number;
  /** Client wall-clock span of the turn's playback (ms). */
  wall_clock_ms?: number;
  /** Each underrun positioned in the ideal (gapless) audio timeline. */
  playback_gaps?: { at_audio_ms: number; silence_ms: number }[];
  terminated?: boolean;
}

/** Client-reported per-turn playback health (`NNN_playback.json`). */
/** One TTS fragment's position in the ideal audio (BFF `utt.fragments`). */
export interface FragmentInfo {
  /** Audio-file kind, e.g. `tts_frag_012` — the key into `audio_files`. */
  kind: string;
  offset_ms: number;
  duration_ms: number;
}

export interface PlaybackHealth {
  underrun_count: number;
  total_silence_ms: number;
  audio_ms: number;
  heard_ms: number;
  wall_ms: number;
  gaps: { at_audio_ms: number; silence_ms: number }[];
  /** Wall ms from the first to the last audio frame *received* by the client.
   *  `arrival_wall_ms / audio_ms` =
   *  arrival RTF: > 1.0 means audio arrived slower than it plays
   *  (starvation); ≤ 1.0 with underruns means bursty delivery (jitter).
   *  Absent on captures predating the field. */
  arrival_wall_ms?: number;
  /** True when the client sent the frame from the barge-in / cancel flush
   *  path rather than at the natural end of playback. */
  interrupted?: boolean;
  /** Annotations that arrived after the audio they describe had started
   *  playing (0 without annotators). */
  annotation_late_count?: number;
  annotation_late_ms_max?: number;
}

/** `arrival_wall_ms / audio_ms` of a playback-health frame, or `null` when
 *  the capture predates the field or the turn received no audio. */
export function arrivalRtfOf(ph: PlaybackHealth | undefined): number | null {
  if (!ph || ph.arrival_wall_ms == null || !(ph.audio_ms > 0)) return null;
  return ph.arrival_wall_ms / ph.audio_ms;
}

export interface SttResult {
  text?: string;
  latency_ms?: number;
}

export interface LlmOutput {
  text: string;
}

export interface LlmTiming {
  total_ms?: number;
  elapsed_ms?: number;
  generate_ms?: number;
  cache_reset_ms?: number;
  prompt_ingest_ms?: number;
  /** Time within the generate() call to produce the first token. */
  first_token_ms?: number;
  /** Item 5: cloud-LLM HTTP connect / first-byte latency (request → response
   *  headers received).  Currently emitted by the openai-compat streaming
   *  path; absent for local providers.  When present, `first_token_ms -
   *  connect_ms` is the model-side decode latency. */
  connect_ms?: number;
  prompt_tokens?: number;
  output_tokens?: number;
  tokens_per_sec?: number;
  device?: string;
  prefill_cache?: boolean;
  rtf?: number;
  first_sentence_chars?: number;
}

export interface TtsTiming {
  elapsed_ms?: number;
  chars?: number;
  audio_duration_s?: number;
  rtf?: number;
  sample_rate?: number;
  voice?: string;
}

export interface SttTiming {
  elapsed_ms?: number;
  endpointing_ms?: number;
}

/** Speech under a turn's id that made no turn: the recogniser returned no
 *  words (`empty_transcript`), too few (`too_short`) or never answered
 *  (`stt_timeout`). `file` names the microphone recording kept for it in
 *  the capture, when there is one. */
export interface DroppedUtterance {
  reason: string;
  text: string;
  had_interim_text: boolean;
  audio_ms: number | null;
  at_ms?: number | null;
  file?: string;
  duration_ms?: number;
}

export interface Utterance {
  utterance_id: number;
  /** Speech thrown away under this id before the turn committed, oldest
   *  first. A session the recogniser never answered has utterances with
   *  nothing else. */
  dropped_before?: DroppedUtterance[];
  stt?: SttResult;
  user_text?: string;
  llm_input?: string;
  llm_output?: LlmOutput;
  /** Server-truncated text retained after interruption (by word count). */
  retained_text?: string;
  summary?: TurnSummary;
  audio_files?: Record<string, string>;
  /** Sentence text of each TTS fragment, keyed like its audio file
   *  (`tts_frag_000`). The BFF attaches the `.txt` sidecar's contents
   *  under the fragment's kind; realtime deltas have no sidecar. */
  [fragment: `tts_frag_${string}`]: string | undefined;
  /** Where each TTS fragment sits in the ideal (server) audio, in fragment
   *  order. Offsets are cumulative, matching how `tts_concat.wav` is
   *  assembled, so they double as seek targets on that track. */
  fragments?: FragmentInfo[];
  /** Speech region inside the user's mic capture (BFF `speech_bounds`).
   *  `null` = the clip carries no speech (typed turn / silence); absent = no
   *  mic capture, or a BFF that predates the field. S2S captures keep the mic
   *  open across the whole turn, so the raw clip is mostly silence. */
  user_speech?: { kind: string; start_ms: number; end_ms: number } | null;
  /**
   * Per-utterance telemetry, normalized server-side.
   * The BFF matches `events.jsonl` payloads to utterances using the
   * lifecycle markers in `pipeline_events.jsonl`. Frontend
   * code should consume these directly instead of pairing flat event
   * arrays by index.
   */
  stt_timing?: SttTiming;
  llm_timing?: LlmTiming;
  llm_detail?: LlmTiming;
  tts_timing?: TtsTiming;
  tts_detail?: TtsTiming;
  /** Canonical per-turn metric snapshot (single source of truth for
   *  latencies and the turn initiator). Present when the capture carries
   *  the `turn_metrics` control frame. */
  metric_snapshot?: TurnMetricsSnapshot;
  /** Client-reported playback health (underruns / inserted silence). */
  playback_health?: PlaybackHealth;
}

/**
 * Per-turn latency breakdown surfaced by the orchestrator API. Mirrors
 * the `TurnMetricsSnapshot` struct on the backend `TurnRecord` and is
 * the authoritative source of timing data for sessions whose local
 * captures directory is empty (e.g. remote workers, cleared captures).
 *
 * Use the helpers in `src/lib/metrics/lookup.ts` (`sttLatencyMs`,
 * `llmGenerationMs`, `ttsTtfbMs`, …) to read well-known stages.
 */

export interface OrchTurnDetail {
  pipeline_name?: string;
  input?: { text?: string } | null;
  stt?: { provider?: string; text?: string; latency_ms?: number };
  tts?: { provider?: string; latency_ms?: number; audio_duration_s?: number };
  text_latency_ms?: number;
  latency_breakdown?: TurnMetricsSnapshot;
}

export interface OrchTurn {
  id?: string | number;
  status?: string;
  total_ms?: number;
  otel_trace_id?: string;
  events?: SessionEvent[];
  text_preview?: string;
  stt_provider?: string;
  tts_provider?: string;
  started_at?: string;
  error?: string;
  /** Full per-turn detail from the orchestrator (mirrors backend TurnRecord). */
  detail?: OrchTurnDetail;
}

export interface SessionMetadata {
  status?: string;
  pipeline_name?: string;
  turn_count?: number;
  started_at?: string;
  /** Older format — alias for started_at. */
  start_time?: string;
  ended_at?: string;
  /** Provider identity captured at session start (legacy fallback for the
   *  resolved `SessionProviders` block). */
  stt_provider?: string;
  tts_provider?: string;
  /** Per-component metric descriptors persisted from `session_ready`.
   *  Drives the descriptor-driven `<TurnComponentMetricsTable>` on the
   *  session-detail page. Empty for older sessions captured before
   *  descriptors were persisted. */
  metric_descriptors?: ComponentMetricDescriptor[];
}

export interface EventTimestamp {
  secs_since_epoch: number;
  nanos_since_epoch: number;
}

export interface SessionEvent {
  event_type?: string;
  type?: string;
  payload?: Record<string, unknown>;
  component?: string;
  content?: unknown;
  timestamp?: EventTimestamp;
}

export interface SessionData {
  metadata?: SessionMetadata;
  utterances?: Utterance[];
  events?: SessionEvent[];
  turns?: OrchTurn[];
  events_total?: number;
  telemetry?: Record<string, unknown>[];
  captures_missing?: boolean;
  /** Lines of the capture's event files that were not valid JSON and were
   *  skipped. Non-zero means the record of this session is incomplete. */
  unreadable_event_lines?: number;
  /**
   * Resolved provider identity for the session, computed by the BFF from
   * the richest event payload available (`llm_detail`, `llm_timing`,
   * `tts_detail`, `stt_timing`). Surfaces model names and STT mode so the
   * UI can render a header strip without re-implementing the lookup.
   */
  providers?: SessionProviders;
  /**
   * Pre-computed session-level health verdict from the BFF (item 8 of
   * the diagnostic-metrics plan). The UI renders this as a coloured
   * chip in the header so the operator can triage at a glance without
   * round-tripping through the LLM report endpoint.
   */
  verdict?: SessionVerdict | null;
  /**
   * Session-wide aggregates precomputed by the BFF over the full event log.
   * Frontend should prefer this over recomputing from the paginated
   * `events` array. Mirrors the `AggregatedSessionMetrics` shape from
   * `metrics.ts`, plus an `eventsAggregated` provenance field.
   */
  session_metrics?: SessionMetrics | null;
  /**
   * Client-side tool activity: one row per
   * tool call the model routed to the client, paired with the client's
   * reported outcome. Aggregated by the BFF from the worker's
   * `client_tool_call` / `client_tool_result` trace events.
   */
  client_tool_calls?: ClientToolCallRecord[];
}

/** One client-side tool call + its resolution (see `client_tool_calls`). */
export interface ClientToolCallRecord {
  call_id: string;
  name?: string;
  arguments?: unknown;
  /** Server-declared policy: a completed result is spoken as a generated confirmation. */
  speak_on_result?: boolean;
  called_at_ms?: number | null;
  status: 'pending' | 'completed' | 'failed';
  result?: unknown;
  /** Final speech decision for the resolution (policy OR client request). */
  spoken?: boolean;
  /** The client's own speak opt-in on the result event — distinct from
   *  `spoken`, which the tool's `speak_on_result` policy can force. */
  speak_requested?: boolean;
  resolved_at_ms?: number | null;
  /** Chronological slot in the turns view: the last utterance whose
   *  output started before this call was dispatched (BFF-computed).
   *  `null` = before the first turn / unplaceable. */
  after_utterance_id?: number | null;
}

export interface SessionVerdict {
  /** `fair` is the graded (v2) intermediate: over-target metrics but the
   *  perceived experience held up. Present only under the v2 health model. */
  level: 'healthy' | 'fair' | 'degraded' | 'broken' | string;
  primary_culprit: 'llm' | 'stt' | 'tts' | 'transport' | 'turn' | 'none' | string;
  headline: string;
  supporting_anomaly_kinds: string[];
  unstable_components: string[];
  /** Present when the session was graded by the operator's own rules: every
   *  value that differs from its default, and where it was set. The headline
   *  spells the same thing out; this is for showing it without parsing text. */
  custom_grading?: {
    name: string;
    value: number;
    default: number | null;
    source: 'console' | 'pipeline config' | 'request' | string;
  }[];
  /** v2 only: which health model produced this verdict + the experience anchor.
   *  `critical_path_*` = server-side estimate of the stopped-talking → first-audio
   *  wait (NOT the client-provenance perceived latency). */
  model?: 'v2' | string;
  critical_path_p50_ms?: number | null;
  critical_path_p90_ms?: number | null;
}

/**
 * One row from the BFF's `_detect_anomalies` output.  Keys mirror
 * `voice-pipes-admin/backend/app/main.py::_detect_anomalies`.
 *
 * `kind` is a dotted prefix:  `llm.*` / `stt.*` / `tts.*` / `bi.*` / `turn.*`
 * `level` is `'warn'` (counts toward verdict) or `'info'` (advisory).
 */
export interface ReportAnomaly {
  utt: number | string | null;
  kind: string;
  level: 'warn' | 'info' | string;
  observed?: number | string | null;
  target?: number | string | null;
  detail?: string;
  /** Graded (v2) magnitude: negligible | minor | moderate | severe. */
  severity?: 'negligible' | 'minor' | 'moderate' | 'severe' | string;
  /** v2: false when the breach is off the user-perceived critical path
   *  (e.g. STT processing that overlaps speech) — advisory, not degrading. */
  on_critical_path?: boolean;
  /** v2: overlap-adjusted, user-facing magnitude (e.g. STT critical-path share
   *  vs raw processing_ms) — severity is graded on this. */
  on_path_observed?: number;
}

/** Compute device a component ran on, resolved once at session setup and
 *  persisted to metadata.json (backend `_extract_provider_identity`). Absent on
 *  captures predating device capture, and on components that don't report one.
 *  `source` conveys confidence: `config`/`static` are authoritative, while
 *  `env-requested` means "configured but unconfirmed" (a request, not proof). */
export interface DeviceInfo {
  kind: string;              // 'cuda' | 'cpu' | 'metal' | 'unknown'
  detail?: string;           // e.g. '28/28 layers', 'VP_ORT_EP=cuda'
  source?: string;           // 'config' | 'static' | 'probe:/props' | 'env-requested' | 'unknown'
}

export interface SessionProviders {
  /** `model`/`model_version` for each component are resolved once at session
   *  setup and persisted to metadata.json (see backend
   *  `_extract_provider_identity`), so the strip shows the model actually
   *  running — even when a provider is a separate process. `device` is added
   *  the same way when the component reports one. */
  llm?: { provider?: string; model?: string; model_version?: string; stream?: boolean; device?: DeviceInfo };
  stt?: { provider?: string; mode?: 'streaming' | 'batch' | 'unknown' | string; model?: string; model_version?: string; device?: DeviceInfo };
  tts?: { provider?: string; voice?: string; model?: string; model_version?: string; device?: DeviceInfo };
}

export interface SessionMetrics {
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
  /** Turns with at least one client-reported playback underrun. */
  turnsWithUnderruns?: number;
  /** Total client-reported underruns across the session. */
  totalUnderruns?: number;
  /** Total client-side inserted silence across the session (ms). */
  totalSilenceMs?: number;
  /** Output throughput: mean / max
   *  `output.emit_rtf` over the session's turns and the number of turns
   *  emitted slower than realtime (`emit_rtf > 1.0`). Absent on older BFFs. */
  avgEmitRtf?: number | null;
  maxEmitRtf?: number | null;
  turnsOverRealtime?: number;
  /** S2S provider endpointing lag averaged over the session's turns.
   *  Absent on older BFFs. */
  avgProviderEndpointingMs?: number | null;
  /** Number of events the BFF aggregated to produce these metrics. */
  eventsAggregated: number;
}
