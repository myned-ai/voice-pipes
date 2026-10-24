// Wire protocol types — TypeScript equivalents of voice-pipes transport types.

// ── Binary frame constants ──────────────────────────────────

export const MSG_TYPE_AUDIO_PCM_I16 = 0x01;
export const FLAG_END_OF_UTTERANCE = 0x01;
export const FLAG_END_OF_RESPONSE = 0x02;
export const HEADER_SIZE = 6;

// ── Binary frame header ─────────────────────────────────────

export interface BinaryHeader {
  msgType: number;
  flags: number;
  seqNum: number;
}

export function encodeBinaryFrame(
  header: BinaryHeader,
  payload: Uint8Array,
): ArrayBuffer {
  const buf = new ArrayBuffer(HEADER_SIZE + payload.byteLength);
  const view = new DataView(buf);
  view.setUint8(0, header.msgType);
  view.setUint8(1, header.flags);
  view.setUint32(2, header.seqNum, true); // little-endian
  new Uint8Array(buf, HEADER_SIZE).set(payload);
  return buf;
}

export function decodeBinaryFrame(
  data: ArrayBuffer,
): { header: BinaryHeader; payload: Uint8Array } {
  if (data.byteLength < HEADER_SIZE) {
    throw new Error(`Binary frame too short: ${data.byteLength} bytes`);
  }
  const view = new DataView(data);
  const header: BinaryHeader = {
    msgType: view.getUint8(0),
    flags: view.getUint8(1),
    seqNum: view.getUint32(2, true),
  };
  const payload = new Uint8Array(data, HEADER_SIZE);
  return { header, payload };
}

export function makeAudioHeader(
  seqNum: number,
  endOfUtterance = false,
  endOfResponse = false,
): BinaryHeader {
  let flags = 0;
  if (endOfUtterance) flags |= FLAG_END_OF_UTTERANCE;
  if (endOfResponse) flags |= FLAG_END_OF_RESPONSE;
  return { msgType: MSG_TYPE_AUDIO_PCM_I16, flags, seqNum };
}

// ── PCM i16 codec ───────────────────────────────────────────

export function encodePcmI16(samples: Int16Array): Uint8Array {
  return new Uint8Array(samples.buffer, samples.byteOffset, samples.byteLength);
}

export function decodePcmI16(data: Uint8Array): Int16Array {
  if (data.byteLength % 2 !== 0) {
    throw new Error(`PCM i16 data has odd byte count: ${data.byteLength}`);
  }
  return new Int16Array(
    data.buffer,
    data.byteOffset,
    data.byteLength / 2,
  );
}

// ── JSON envelope ───────────────────────────────────────────

export interface JsonEnvelope {
  type: string;
  message_id?: string;
  timestamp_ms?: number;
  payload?: unknown;
}

// ── Session config ──────────────────────────────────────────

export interface SessionConfig {
  audio_format?: string;
  sample_rate?: number;
  /** Preferred output (playback) sample rate — server may honour or override. */
  output_sample_rate?: number;
  channels?: number;
  language?: string;
  voice?: string;
  options?: Record<string, unknown>;
  /** Arbitrary application data passed opaquely to the server. */
  app_data?: unknown;
  /** Session mode — determines which pipeline components are active. */
  mode?: SessionMode;
  /** This client will capture at whatever `input_sample_rate` the server
   *  returns in `session_ready`, rather than at the `sample_rate` it proposed.
   *
   *  Lets the server drop the mic to the STT's native rate — usually 16 kHz
   *  against a browser's 48 kHz, a 3× uplink saving on bandwidth the server
   *  discards anyway. The server only lowers the rate when this is set,
   *  because the input rate is fixed at the handshake: a client that ignored
   *  the negotiated value would have its audio misread with nothing able to
   *  detect it.
   *
   *  Safe for this SDK — `AudioCaptureService` resamples to whatever rate it
   *  is given. */
  accepts_negotiated_input_rate?: boolean;
}

/** Session mode — determines which pipeline components are active. */
export type SessionMode = "voice" | "text" | "hybrid" | "voice_in_text_out" | "text_in_voice_out";

/** How much fidelity the server is currently spending on the wire.
 *
 *  Ordered best-first. Clients should degrade in step — a deeper jitter
 *  buffer, a lower avatar update rate, a "poor connection" hint — rather than
 *  letting the user interpret broken audio. Absent on servers predating
 *  adaptive quality, and always `full` while the feature is disabled. */
export type QualityTier = "full" | "reduced" | "minimal" | "critical";

// ── Pipeline state ──────────────────────────────────────────

export type PipelineState = "listening" | "processing" | "speaking" | "draining";

// ── Client → Server ─────────────────────────────────────────

/** Lifecycle phase of a client-side tool execution.
 *  `started`/`progress` are telemetry;
 *  `completed`/`failed` resolve the call server-side. */
export type ClientToolPhase = "started" | "progress" | "completed" | "failed";

export type ClientControl =
  | { action: "cancel" }
  | { action: "ping"; timestamp_ms: number }
  | { action: "net_report"; playback_buffer_ms?: number; underruns?: number; audio_received_ms?: number; wall_ms?: number; uplink_backlog_bytes?: number }
  | { action: "configure"; audio_format?: string; sample_rate?: number; output_sample_rate?: number; channels?: number; language?: string; voice?: string; options?: Record<string, unknown>; app_data?: unknown; mode?: string; accepts_negotiated_input_rate?: boolean }
  | { action: "resume"; session_token: string }
  | { action: "playback_report"; utterance_id: number; last_played_offset_ms: number; last_played_text?: string }
  | { action: "timing_report"; utterance_id: number; event: string; client_timestamp_ms: number; detail?: unknown }
  | { action: "playback_complete"; utterance_id: number }
  | { action: "client_tool_result"; call_id: string; phase: ClientToolPhase; result?: unknown; speak?: boolean }
  | {
      action: "playback_quality";
      utterance_id: number;
      underrun_count: number;
      total_silence_ms: number;
      audio_ms: number;
      heard_ms: number;
      wall_ms: number;
      /** Wall ms from the first to the last audio frame *received* by the
       *  client. `arrival_wall_ms /
       *  audio_ms` = arrival_rtf: > 1 means the server delivered audio slower
       *  than it plays. */
      arrival_wall_ms: number;
      /** True when the frame was emitted from a flush (barge-in / cancel)
       *  rather than the natural end of playback. */
      interrupted: boolean;
      /** Annotations that arrived after the audio they describe had already
       *  started playing (0 when no annotator is configured). */
      annotation_late_count: number;
      annotation_late_ms_max: number;
      gaps: { at_audio_ms: number; silence_ms: number }[];
    };

export interface TextMessage {
  text: string;
  message_id: string;
}

// ── Negotiated config (server-authoritative) ────────────────

/** The rates and providers the server will actually use.
 *  Client MUST respect `input_sample_rate` for capture and
 *  `output_sample_rate` for playback. */
export interface NegotiatedConfig {
  audio_format: string;
  input_sample_rate: number;
  output_sample_rate: number;
  channels: number;
  language?: string;
  voice?: string;
  stt_provider?: string;
  tts_provider?: string;
  app_data?: unknown;
  options?: Record<string, unknown>;
  /** Per-component metric descriptors collected from each component's
   *  `Component::turn_descriptor()` at session build time. Drives the
   *  generic per-turn metrics table; absent / empty for older servers. */
  metric_descriptors?: ComponentMetricDescriptor[];
  /** `options` keys this client sent that the server refused because
   *  they are not in the worker's `[realtime_provider] client_overrides`.
   *  Omitted when everything was accepted. */
  ignored_options?: string[];
  /** Id the server gives the session's first spoken utterance. */
  first_utterance_id?: number;
  /** Ids above this are typed-text turns, numbered from a range of their own. */
  text_utterance_id_base?: number;
}

// ── Session info ────────────────────────────────────────────

export interface SessionInfo {
  session_id: string;
  session_token?: string;
  resumed?: boolean;
  /** Echo of the client's original request. */
  client_config?: SessionConfig;
  /** Server-authoritative negotiated config. */
  negotiated?: NegotiatedConfig;
}

export type ServerControl =
  | { action: "session_ready"; session_id: string; session_token?: string; negotiated?: NegotiatedConfig; client_config?: SessionConfig }
  | { action: "pong"; client_timestamp_ms: number; server_timestamp_ms: number }
  | { action: "state_changed"; state: PipelineState }
  | { action: "quality_update"; quality: string; tier?: QualityTier }
  | { action: "error"; code: string; message: string; severity?: ErrorSeverity; turn_id?: string; component?: ErrorComponent }
  | { action: "closing"; reason: string }
  | { action: "session_summary"; turns: number; interrupted_turns: number; avg_ttft_ms?: number; avg_ttfb_ms?: number; avg_total_ms?: number; total_audio_duration_ms: number }
  | { action: "turn_metrics"; snapshot: TurnMetricsSnapshot }
  | { action: "clear_audio"; utterance_id: number; reason: string }
  | { action: "text_truncated"; utterance_id: number; final_text: string }
  /** Speech that made no turn: the recogniser returned no words
   *  (`empty_transcript`), too few (`too_short`) or never answered
   *  (`stt_timeout`). `utterance_id` is the id the speech would have
   *  become; the next committed utterance takes it. */
  | { action: "utterance_dropped"; utterance_id: number; reason: string; audio_ms: number };

// ── Per-turn metrics registry ────────────────────────

/**
 * Per-turn metrics snapshot — mirrors `pipeline::TurnMetricsSnapshot`
 * on the backend. Carries the utterance id, total wall-clock, the
 * interrupted flag, and an ordered list of per-component slices.
 *
 * Per-stage timings (TTFT, TTFB, STT latency, LLM generation, TTS
 * synthesis sub-metrics) live inside `components[].extras` rather than
 * as typed top-level fields. Use the helpers in
 * `src/lib/metrics/lookup.ts` (`ttftMs`, `ttfbMs`, `componentByKind`,
 * …) to read well-known stages without hardcoding key strings.
 */
export interface TurnMetricsSnapshot {
  utterance_id: number;
  total_ms: number;
  interrupted: boolean;
  /** Who opened the turn. Absent (older engines / user turns — the wire
   *  omits the default) means "user". Agent-initiated turns (idle
   *  prompts, filler follow-ups, proactive speech) have no user input:
   *  latencies anchor at turn-open and there is no STT component. */
  initiator?: 'user' | 'agent' | string;
  components: TurnComponentMetrics[];
}

/** One pipeline component's slice of a per-turn snapshot. */
export interface TurnComponentMetrics {
  component_id: string;
  /** Coarse classification — e.g. "stt" / "llm" / "tts" / "translator". */
  kind: string;
  started_at_ms: number;
  finished_at_ms?: number;
  latency_ms?: number;
  provider?: string;
  cold_start?: boolean;
  error?: string;
  /** Free-form per-component values (typed by `MetricFieldDescriptor`). */
  extras?: Record<string, unknown>;
}

/** Descriptor advertised on `session_ready.metric_descriptors`. */
export interface ComponentMetricDescriptor {
  component_id: string;
  kind: string;
  label: string;
  fields: MetricFieldDescriptor[];
}

export interface MetricFieldDescriptor {
  key: string;
  label: string;
  unit: MetricUnit;
  /** Optional numeric format hint, e.g. "0.00". */
  format?: string;
  /** Optional provider-declared health semantics (see
   *  docs/reference/vpo-metrics.md). Absent on display-only metrics and on
   *  captures predating it. */
  health?: MetricHealth;
  start_anchor?: string;
  end_anchor?: string;
  includes?: string;
  owner?: string;
  contained_in?: string;
}

/** Provider-declared health semantics for a metric field. All optional. */
export interface MetricHealth {
  role?: "latency" | "throughput" | "quality" | "diagnostic" | string;
  direction?: "lower_is_better" | "higher_is_better" | string;
  /** Target/budget in the metric's unit — the source for per-metric UI targets. */
  budget?: number;
  on_critical_path?: boolean | null;
  severity_bands?: { warn: number; moderate: number; severe: number };
}

export type MetricUnit =
  | "ms"
  | "percent"
  | "ratio"
  | "count"
  | "tokens_per_sec"
  | "bytes"
  | "raw"
  /** Decibels: dBFS for levels, plain dB for differences (SNR). */
  | "db";

// ── Audio metadata carried alongside PCM chunks ─────────────

/**
 * Extensible metadata envelope that rides alongside each audio chunk.
 * Start with timing info; avatar pipelines will add blendshapes, etc.
 */
/** One playback break (jitter-buffer underrun), positioned in the IDEAL
 *  (gapless) audio timeline — `atAudioMs` indexes straight into the
 *  server-side fragment concat, which is what makes the as-heard splice a
 *  pure server-side reconstruction. */
export interface PlaybackGap {
  atAudioMs: number;
  silenceMs: number;
}

/** Client-computed per-turn playback quality, emitted once per utterance via
 *  the `playback_quality` control frame — at playback-complete, or from the
 *  flush path (barge-in / cancel) with `interrupted = true`. */
export interface PlaybackQuality {
  utteranceId: number;
  /** = gaps.length */
  underrunCount: number;
  /** = Σ gaps[].silenceMs */
  totalSilenceMs: number;
  /** Ideal audio delivered (Σ chunk durations). */
  audioMs: number;
  /** Audio actually heard (enqueue-bounded; see getHeardDurationMs). */
  heardMs: number;
  /** Client wall-clock span of the turn's playback. */
  wallMs: number;
  /** Wall ms from the first to the last audio frame *received* (enqueued)
   *  for the utterance — `performance.now()` deltas, independent of the
   *  AudioContext clock. `arrivalWallMs / audioMs` is the client-side
   *  arrival realtime factor. */
  arrivalWallMs: number;
  /** True when emitted from a flush instead of the natural playback end. */
  interrupted: boolean;
  /** Annotations received after their `startMs` had already played
   *  (`late = playbackClock.nowMs − startMs > 0`); 0 without annotators. */
  annotationLateCount: number;
  /** Largest such lateness (ms). */
  annotationLateMsMax: number;
  gaps: PlaybackGap[];
}

export interface AudioMeta {
  /** Server-side wall-clock timestamp (epoch ms) for this chunk. */
  timestampMs?: number;
  /** Duration of this chunk in milliseconds. */
  durationMs?: number;
  /** The total duration of the entire sentence/fragment in milliseconds. */
  total_fragment_duration_ms?: number;
  /** @deprecated — never populated by the transport; use `extras`. */
  blendshapes?: Record<string, number>;
  /** Post-processor payload (`AudioPostProcessor` → `ProcessResponse.extras_json`),
   *  delivered verbatim by the server on this chunk's `audio_meta`. Per-chunk:
   *  never inherited by continuation chunks. Shape is processor-defined, e.g.
   *  `{ blendshapes: { fps: 30, names?: string[], frames: number[][] } }`. */
  extras?: Record<string, unknown>;
  /** Server wall-clock (epoch ms) when the chunk left the pacer. */
  serverTimestampMs?: number;
  /** Caption metadata for the sentence this audio belongs to. */
  caption?: CaptionMeta;
  /** Server-side utterance ID.  Present on audio chunks that carry caption
   *  data.  Used by the client to guard against caption contamination when
   *  paced audio from a previous turn arrives after state→listening. */
  utteranceId?: number;
  /** Full LLM response text — present ONLY on the end-of-utterance sentinel.
   *  Travels through the same ordered WebSocket stream as audio chunks so it
   *  cannot race with per-fragment caption data.  The client emits a synthetic
   *  text message from this field in the same event-loop tick as the EOU audio
   *  event, guaranteeing captions always precede the full-text confirmation. */
  fullText?: string;
  /** Arbitrary provider-specific extras. */
  [key: string]: unknown;
}

// ── Audio annotations (off-the-critical-path metadata) ──────

/**
 * One `audio_annotation` frame — per-audio metadata (blendshapes, visemes,
 * phonemes, emotion, …) produced by an `AudioAnnotator` running OFF the audio
 * path. Unlike `AudioMeta.extras` (inline processors, bound to one chunk)
 * an annotation describes a span of one
 * utterance in **utterance-offset space**: `[startMs, startMs + durationMs)`.
 *
 * Consumer contract: place `startMs + i·(1000/fps)` against `playbackClock`
 * snapshots (`nowMs`) with the same `utteranceId`; annotations may precede or
 * follow the audio they describe. Never compare against AudioContext seconds
 * or chunk boundaries — the offset clock is the only shared timeline.
 */
export interface AudioAnnotation {
  /** Server-assigned utterance the span belongs to (same id space as
   *  `AudioMeta.utteranceId` / `ClockSnapshot.utteranceId`). */
  utteranceId: number;
  /** Span start, ms from the start of the utterance's audio. */
  startMs: number;
  /** Span length in ms (`Σ durationMs` over a turn ≈ its audio duration). */
  durationMs: number;
  /** Free-form annotator kind, e.g. `"blendshapes"`. vp never interprets it. */
  kind: string;
  /** Opaque annotator payload, e.g. `{ fps: 30, names?: string[], frames: number[][] }`. */
  extras: Record<string, unknown>;
  /** Server wall-clock (epoch ms) when the annotation was emitted. */
  serverTimestampMs?: number;
}

// ── Caption types ───────────────────────────────────────────

/** Word-level timing for synchronized caption highlighting. */
export interface WordTiming {
  word: string;
  /** Offset from the start of the sentence's audio, in milliseconds. */
  start_ms: number;
  /** End offset in milliseconds. */
  end_ms: number;
}

/** Caption metadata attached to the first audio chunk of each sentence. */
export interface CaptionMeta {
  /** The sentence text that produced this audio. */
  text: string;
  /** Estimated total duration (ms) of audio for this sentence. */
  estimated_duration_ms?: number;
  /** Word-level timing, if the TTS provider returned it. */
  word_timings?: WordTiming[];
}

// ── Pipeline message (structured bidirectional) ─────────────

export interface PipelineMessage {
  kind: string;
  text?: string;
  payload?: unknown;
  binary?: number[];
  role: "user" | "assistant" | "system";
  turn_id?: string;
  message_id: string;
  timestamp_ms?: number;
}

// ── Error classification ────────────────────────────────────

export type ErrorSeverity = "warning" | "error" | "fatal";
export type ErrorComponent = "stt" | "llm" | "tts" | "pipeline" | "transport";

// ── Unified message types for the hook layer ────────────────

export type ServerMessage =
  | { kind: "audio"; samples: Int16Array; seqNum: number; endOfUtterance: boolean; endOfResponse: boolean; meta: AudioMeta }
  | { kind: "text"; text: string; messageId: string }
  | { kind: "control"; control: ServerControl }
  | { kind: "data"; payload: unknown }
  | { kind: "message"; message: PipelineMessage }
  | { kind: "annotation"; annotation: AudioAnnotation };

export type ClientMessage =
  | { kind: "audio"; samples: Int16Array; seqNum: number }
  | { kind: "text"; text: string; messageId: string }
  | { kind: "control"; control: ClientControl };
