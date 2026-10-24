// Public types of the session core: the turn-metrics envelope, the client
// playback timings, and the typed event map listeners subscribe through.

import type {
  AudioAnnotation,
  NegotiatedConfig,
  PipelineMessage,
  PipelineState,
  QualityTier,
  TurnMetricsSnapshot,
} from "../../transport/types";
import type { AudioPlaybackService, PlaybackChunk } from "../../audio/playback";
import type { ClientE2eSource } from "../../metrics/anchorSource";

// Turn metrics envelope matching the plan specifications
export interface TurnMetricsEnvelope {
  snapshot: TurnMetricsSnapshot;
  rttMs?: number;
  /** Client end of speech → first audio byte received (ms). Anchored on the
   *  client VAD's `speech_end` when one was stamped for the turn, else on the
   *  server's `listening` transition (see `clientE2eSource`). */
  clientE2eMs?: number;
  /** Which stamp `clientE2eMs` / `clientTtfaMs` start from:
   *  `client_vad` (honest) or
   *  `listening` (legacy mic-open fallback). */
  clientE2eSource?: ClientE2eSource;
  /** First audio byte received → first sample audible (jitter buffer + scheduling). */
  clientReceiveToPlayMs?: number;
  /** speech_end → first sample audible (= clientE2eMs + clientReceiveToPlayMs). */
  clientTtfaMs?: number;
  /** Server pacer emit (audio_meta.server_timestamp_ms) → audible, clock-offset corrected. */
  clientPlaybackLatencyMs?: number;
  /** Client arrival realtime factor: wall ms first → last audio frame received ÷
   *  audio ms received (2 decimals). > 1.0 = the server delivered audio slower
   *  than it plays. */
  clientArrivalRtf?: number;
}

/** The client-side playback timings of one utterance, emitted as soon as its
 *  first chunk is scheduled, and once more when the turn ends (natural
 *  playback end or flush) with `clientArrivalRtf` filled in. */
export interface ClientPlaybackMetrics {
  utteranceId: number;
  /** First audio byte received → first sample audible (jitter buffer + scheduling). */
  clientReceiveToPlayMs?: number;
  /** speech_end → first sample audible; undefined for agent-initiated turns. */
  clientTtfaMs?: number;
  /** Anchor of `clientTtfaMs` (same as the envelope's `clientE2eSource`);
   *  undefined when there is no client anchor for the turn. */
  clientE2eSource?: ClientE2eSource;
  /** Server pacer emit → audible in server time; undefined until a Pong sample
   *  exists or when the chunk carries no `serverTimestampMs`. */
  clientPlaybackLatencyMs?: number;
  /** `arrivalWallMs / audioMs` of the turn's `playback_quality` frame, 2
   *  decimals; undefined until the turn ended and when no audio was received. */
  clientArrivalRtf?: number;
}

/** Per-utterance lateness of `audio_annotation` frames: how often (and by
 *  how much) an annotation arrived after the audio it describes had already
 *  started playing. */
export interface AnnotationLateness {
  utteranceId: number;
  count: number;
  maxMs: number;
}

export interface VoicePipesSessionEventMap {
  /** Fired when backend-authoritative turns state changes. */
  stateChanged: (state: PipelineState | null) => void;

  /** Incremental or final transcript text delta received. */
  textDelta: (args: {
    text: string;
    isFinal: boolean;
    role: "user" | "assistant" | "system";
    messageId: string;
    turnId?: string;
  }) => void;

  /** Highlight clock tick for karaoke playback (word-by-word reveal). */
  wordHighlighted: (args: {
    word: string;
    startMs: number;
    endMs: number;
    utteranceId: number;
    wordIndex: number;
  }) => void;

  /** Authoritative playback cancellation/barge-in flush. */
  audioFlushed: (args: {
    utteranceId: number;
    playedMs: number;
    reason: "barge_in" | "cancel" | "disconnect";
    finalTrimWordIndex?: number | null;
  }) => void;

  /** Fired when playback of an utterance naturally concludes. */
  playbackFinished: (args: { utteranceId: number }) => void;

  /** Session established with authoritative configuration returned by gateway. */
  sessionReady: (negotiated: NegotiatedConfig) => void;

  /** The server changed how much fidelity it is spending on the wire.
   *  Fires only on transitions, never per measurement. */
  qualityTierChanged: (tier: QualityTier, quality: string) => void;

  /** Endpoint latencies and metrics received from turn snapshots. */
  metricsEmitted: (turnId: string, metrics: TurnMetricsEnvelope) => void;

  /** Fired when an utterance's first chunk is scheduled (receive→play, TTFA,
   *  delivery) and again when the turn ends — natural playback end or flush —
   *  with `clientArrivalRtf` added (all earlier fields repeated). May arrive
   *  before OR after `metricsEmitted` for the same utterance (short answers
   *  finish server-side while the jitter buffer is still holding). */
  playbackMetrics: (metrics: ClientPlaybackMetrics) => void;

  /** Periodic server connection quality checks. */
  qualityChanged: (quality: "excellent" | "good" | "poor" | "lost") => void;

  /** Any system/pipeline or connection exception raised. */
  errorRaised: (code: number, message: string, component?: string) => void;

  /** Socket closed or terminated. */
  sessionTerminated: (reason: string) => void;

  /** Reactive session state changes for UI connection bars. */
  sessionStateChanged: (state: "idle" | "connecting" | "active" | "error") => void;

  /** Logging of incoming/outgoing payload frames for dry diagnostics. */
  eventsEmitted: (dir: "in" | "out", type: string, summary: string, detail?: unknown) => void;

  /** Structured (non-text) PipelineMessage received — tool activity
   *  (`tool_call` / `tool_result`), client commands (`trigger_action`),
   *  cards, etc. Rendered by the UI as activity/command panels, never
   *  as spoken content. */
  pipelineMessage: (message: PipelineMessage) => void;

  /** Every output audio chunk, fired right before it is scheduled on the
   *  AudioContext. `chunk.startTime`/`endTime` are AudioContext-clock seconds
   *  (read `playbackService.audioContext.currentTime` to compare);
   *  `chunk.meta.extras` carries post-processor payloads (e.g. blendshapes).
   *  High frequency (~20–25 Hz) — do NOT route into React state. */
  audioChunk: (chunk: PlaybackChunk) => void;

  /** One `audio_annotation` frame from an off-path `AudioAnnotator`
   *  (blendshapes, visemes, …). Lives in utterance-offset space: place
   *  `startMs + i·(1000/fps)` against `playbackClock` snapshots (`nowMs`)
   *  with the same `utteranceId` — never against `audioChunk.startTime`.
   *  May precede or follow the audio it describes. Annotations for an
   *  utterance that was flushed (barge-in / cancel) are dropped before
   *  this fires. High frequency (~6 Hz per annotator) — do NOT route into
   *  React state. */
  audioAnnotation: (annotation: AudioAnnotation) => void;

  /** The `AudioPlaybackService` was created for a session (non-null) or torn
   *  down (null). It is recreated on every session start, so anything that
   *  cached `playbackService` / `audioContext` must re-read it here. */
  playbackChanged: (playback: AudioPlaybackService | null) => void;
}
