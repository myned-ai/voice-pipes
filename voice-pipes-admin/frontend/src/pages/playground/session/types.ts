// Types shared across usePlaygroundSession and its sub-hooks.
// Consumers that previously imported from "hooks/usePlaygroundSession" keep
// working because that file re-exports everything from here.

import type { AudioProcessingOptions } from "../../../lib/audio/capture";
import type { AudioMeta, ComponentMetricDescriptor, PipelineMessage, PipelineState, ErrorSeverity, ErrorComponent, TurnMetricsSnapshot, QualityTier } from "../../../lib/transport/types";
import type { TransportType, SessionConfig } from "../../../lib/transport";
import type { PlaygroundExtensionSession } from "../../../lib/extensions/types";
import type { ClientE2eSource } from "../../../lib/metrics/anchorSource";

/** Client-side energy VAD used to timestamp the user's end of speech.
 *  Measurement only — it never gates
 *  audio. Defaults: enabled, enter 0.02 / exit 0.012 RMS, hangover 300 ms,
 *  min speech 150 ms (see `lib/audio/endpointing.ts`). */
export interface ClientVadOptions {
  enabled?: boolean;
  enterThreshold?: number;
  exitThreshold?: number;
  hangoverMs?: number;
  minSpeechMs?: number;
}

export interface PlaybackOptions {
  limitScheduledAudio: boolean;
  maxScheduledChunks: number;
  /** Jitter buffer: hold each utterance's (re)start until this much
   *  audio is queued so realtime-paced providers don't underrun on the
   *  first delivery hiccup. 0 = play immediately. */
  prebufferMs: number;
  /** Client VAD that stamps `timing_report speech_end` and anchors the
   *  client E2E. Omitted = enabled with defaults. */
  clientVad?: ClientVadOptions;
}

/** Default jitter buffer (ms).
 *
 *  Raised from 200 after session 5fbf767c: a Gemini S2S turn took 5 underruns
 *  and 1.8 s of inserted silence despite `arrival_rtf` 0.54 — the audio
 *  arrived at twice the rate it played. Averages hid the cause. Bursting
 *  providers deliver in spurts with generation pauses between them, and a
 *  200 ms cushion does not survive a pause longer than 200 ms. Every gap in
 *  that turn fell in the window before the client had accumulated a backlog;
 *  once all the audio had arrived (13.3 s into a 26.4 s playback) they stopped
 *  entirely.
 *
 *  The cost is ~200 ms of extra time-to-first-audio, which is a real trade
 *  against the latency work elsewhere in this SDK — but a cheap one against
 *  1.8 s of audible silence. Raise it further per-session via
 *  `PlaybackOptions.prebufferMs` rather than editing this. */
export const DEFAULT_PREBUFFER_MS = 400;

export type TranscriptEntryType = "chat" | "error" | "system" | "message" | "interrupted";

/** Info about which word is currently being spoken — drives karaoke highlighting. */
export interface HighlightInfo {
  /** Index into the transcript array for the entry being spoken. */
  entryIndex: number;
  /** Cumulative word index within the entry's full text. */
  wordIndex: number;
}

export interface TranscriptEntry {
  role: "user" | "agent" | "system";
  text: string;
  timestamp: number;
  /** Server utterance ID associated with this turn. Useful for routing text correctly. */
  utteranceId?: number;
  /** True if the text was completely populated by the final LLM text payload. */
  isFullTextFilled?: boolean;
  /** Snapshot of the text the user actually heard, set once at interrupt time
   *  by AGENT_FREEZE_AT_WORD.  When present, ChatBubble renders this instead
   *  of `text` so the full LLM text is never shown after a barge-in.
   *  `text` remains the authoritative LLM value and is updated freely by the
   *  server's TEXT_TRUNCATED for history purposes. */
  heardText?: string;
  /** Entry type for rendering variants. Defaults to "chat" if absent. */
  type?: TranscriptEntryType;
  /** For "error" entries: severity level. */
  severity?: ErrorSeverity;
  /** For "error" entries: error code. */
  errorCode?: string;
  /** For "error" entries: which component failed. */
  component?: ErrorComponent;
  /** For "message" entries: the full PipelineMessage. */
  pipelineMessage?: PipelineMessage;
  /** For `trigger_action` message entries: the client-tool result the
   *  tester sent for this call — persisted here (not in component
   *  state) so the card keeps showing what was chosen across
   *  re-renders and transcript updates. */
  clientToolOutcome?: {
    phase: "started" | "progress" | "completed" | "failed";
    result?: unknown;
    speak: boolean;
    timestamp: number;
  };
  /** Debug log for tracking UI writes and clears */
  debugLog?: string[];
}

export interface WireEvent {
  direction: "in" | "out";
  type: string;
  summary: string;
  detail?: unknown;
  timestamp: number;
}

/** Per-turn metrics received from the server (registry-driven snapshot)
 *  augmented with the client-measured timings the server can never know
 *  about (network RTT, speech_end → first audio E2E). */
export interface TurnMetricsEntry {
  /** Server-authoritative per-turn snapshot (utterance id, total ms,
   *  interrupted flag, ordered per-component slices). Per-stage timings
   *  live inside `snapshot.components[].extras` — read via the helpers
   *  in `src/lib/metrics/lookup.ts`. */
  snapshot: TurnMetricsSnapshot;
  /** Network round-trip time measured via Ping/Pong (ms). */
  rttMs?: number;
  /** Client-measured end-to-end: speech_end → first audio received (ms). */
  clientE2eMs?: number;
  /** Which stamp `clientE2eMs` starts from: `client_vad` (the client
   *  endpointer's end of speech — the honest anchor) or `listening` (the
   *  server's `listening` transition, i.e. mic open — the legacy fallback
   *  when no VAD stamp exists for the turn). */
  clientE2eSource?: ClientE2eSource;
  /** First audio byte received → first sample audible (jitter buffer + scheduling). */
  clientReceiveToPlayMs?: number;
  /** speech_end → first sample audible (= clientE2eMs + clientReceiveToPlayMs). */
  clientTtfaMs?: number;
  /** Server pacer emit (audio_meta.server_timestamp_ms) → audible, clock-offset corrected. */
  clientPlaybackLatencyMs?: number;
  /** Client arrival realtime factor (wall ms first → last audio frame received ÷
   *  audio ms received). > 1.0 = the server delivered slower than realtime.
   *  Known only once the turn ended (natural end or flush). */
  clientArrivalRtf?: number;
  /** Wall-clock instant the snapshot landed (epoch ms). */
  timestamp: number;
}

export type SessionState = "idle" | "connecting" | "active" | "error";

/** One observed pipeline state transition, recorded for the Playground
 *  state-timeline panel (T6). Captures the wall-clock instant a
 *  `state_changed` control arrived and the gap since the prior transition
 *  so operators can see lifecycle over time (not just a current-state badge)
 *  and spot stalls between e.g. `processing` and `speaking`. */
export interface StateTransition {
  /** The pipeline state the session moved into (null = idle, i.e. the
   *  session ended or was reset). */
  state: PipelineState | null;
  /** The state it moved from (null for the very first transition). */
  prevState: PipelineState | null;
  /** Wall-clock instant the transition was observed (epoch ms). */
  timestamp: number;
  /** Milliseconds since the previous transition (null for the first). */
  sincePrevMs: number | null;
}

export interface PlaygroundSession {
  sessionState: SessionState;
  pipelineState: PipelineState | null;
  transcript: TranscriptEntry[];
  events: WireEvent[];
  micActive: boolean;
  micLevel: number;
  playbackLevel: number;
  /** Latest blendshape / metadata from the most recent audio chunk. */
  currentAudioMeta: AudioMeta | null;
  /** Current word-level highlight info for karaoke captions. */
  highlightInfo: HighlightInfo | null;
  /** Index into the transcript array for the entry currently being spoken (-1 = none). */
  speakingEntryIndex: number;
  /** Human-readable error message when sessionState is "error". */
  errorMessage: string | null;
  connect: (url: string, transportType: TransportType, config?: SessionConfig, playbackOptions?: PlaybackOptions) => Promise<void>;
  disconnect: () => void;
  sendText: (text: string) => void;
  /** Start the microphone. `processing` overrides the browser's own echo
   *  cancellation, noise suppression and gain control, which default to on;
   *  pass them off when the audio itself is the subject. */
  startMic: (
    sampleRate?: number,
    processing?: AudioProcessingOptions,
  ) => Promise<void>;
  stopMic: () => void;
  /** Per-turn latency metrics received from the server. */
  turnMetrics: TurnMetricsEntry[];
  /** Per-component metric descriptors advertised on `session_ready`.
   *  Drives the generic per-turn metrics table. Empty until session_ready
   *  arrives or when the server does not advertise any descriptors. */
  metricDescriptors: ComponentMetricDescriptor[];
  /** Latest network round-trip time (ms) from Ping/Pong. */
  rttMs: number | null;
  /** Fidelity tier the server last reported (adaptive quality). `"full"`
   *  unless the feature is enabled server-side and the link degraded. */
  qualityTier: QualityTier;
  /** Ordered log of observed pipeline state transitions (T6 timeline). */
  stateTimeline: StateTransition[];
  /** Server utterance ID of the turn currently speaking/draining, or null.
   *  Used by the manual playback-trigger controls to target the live turn. */
  activeUtteranceId: number | null;
  /** Clear transcript, events, and metrics without disconnecting. */
  clearConversation: () => void;
  /** Flush locally buffered audio without cancelling the server pipeline.
   *  Call when navigating away so audio doesn't keep playing in the background. */
  flushLocalAudio: () => void;
  /** Explicit user cancel ("Stop" button) to interrupt agent. */
  cancelAgent: () => void;
  /** Manual trigger: send a `playback_report` for the active utterance at
   *  a chosen offset (ms), without flushing local audio. Lets QA drive
   *  server-side history truncation by hand. No-op if not connected. */
  sendManualPlaybackReport: (offsetMs: number) => void;
  /** Manual trigger: send a `playback_complete` for the active utterance.
   *  No-op if not connected. */
  sendManualPlaybackComplete: () => void;
  /** Report the outcome of a client-side tool call (`trigger_action`
   *  command) — the playground acts as the reference client-tool
   *  executor. No-op if not connected. */
  sendClientToolResult: (
    callId: string,
    phase: "started" | "progress" | "completed" | "failed",
    result?: unknown,
    speak?: boolean,
  ) => void;
  /** Narrow façade handed to Playground Extensions (see lib/extensions). Stable
   *  identity for the lifetime of the hook. */
  extensionSession: PlaygroundExtensionSession;
}
