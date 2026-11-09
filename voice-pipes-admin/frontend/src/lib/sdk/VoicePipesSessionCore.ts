// VoicePipesSessionCore.ts — headless, framework-agnostic client SDK module.
// Coordinates WS transport state, NTP-lite clock sync, mic capture, and audio playback.

import { createTransport, type TransportType } from "../transport";
import { type TransportAdapter, type TransportState, type TransportCloseInfo } from "../transport/adapter";
import {
  type SessionConfig,
  type ServerMessage,
  type ServerControl,
  type AudioMeta,
  type AudioAnnotation,
  type PipelineState,
  type CaptionMeta,
  type PlaybackQuality,
  type QualityTier,
} from "../transport/types";
import { ClockSync } from "../transport/clock-sync";
import { AudioCaptureService } from "../audio/capture";
import { UtteranceIdPredictor } from "./utteranceIds";
import type { AudioProcessingOptions } from "../audio/capture";
import { EnergyEndpointer, type EndpointingEvent } from "../audio/endpointing";
import { AudioPlaybackService, type PlaybackChunk } from "../audio/playback";
import { CaptionSync } from "../audio/caption-sync";
import { DefaultPlaybackClock, type PlaybackClock } from "../audio/playback-clock";
import { createPlaybackSource } from "../audio/playback-source";
import { WordTimingTable, timingsForFragment } from "../audio/word-timing-table";
import { PureVoicePipesClientFsm, type ClientFsmEvent, type ClientFsmAction } from "./PureVoicePipesClientFsm";
import {
  initialInterruptState,
  interruptReducer,
  shouldDropAudio,
  shouldDropFinalText,
  type InterruptState,
  type InterruptAction,
} from "../../pages/playground/session/interruptReducer";
import { type ClientVadOptions, type PlaybackOptions, DEFAULT_PREBUFFER_MS } from "../../pages/playground/session/types";
import { type ClientE2eSource } from "../metrics/anchorSource";
import { debug } from "../debug";
import type {
  AnnotationLateness,
  ClientPlaybackMetrics,
  VoicePipesSessionEventMap,
} from "./session/types";
import {
  DEFAULT_MIC_SAMPLE_RATE,
  DEFAULT_PING_INTERVAL_MS,
  DEFAULT_PREBUFFER_BY_TIER,
  ERROR_TRANSPORT_CODE,
  INITIAL_SEQUENCE_NUMBER,
  NET_REPORT_INTERVAL_MS,
} from "./session/constants";
import { describeUnexpectedClose } from "./session/closeReason";
import {
  describePlaybackQuality,
  describePlaybackTimings,
  firstChunkPlaybackMetrics,
  netReportDeltas,
  untilAudibleMs,
  withArrivalRtf,
  type NetSnapshot,
} from "./session/playbackMetrics";
import { buildTurnMetricsEnvelope, describeTurnMetrics } from "./session/turnMetrics";
import { realtimeDeltaAudioMs, realtimeDeltaUtteranceId } from "./session/messageParsing";
import { trimWordIndex } from "./session/trimWordIndex";

export type {
  ClientPlaybackMetrics,
  TurnMetricsEnvelope,
  VoicePipesSessionEventMap,
} from "./session/types";
export {
  DEFAULT_MIC_SAMPLE_RATE,
  DEFAULT_PLAYBACK_SAMPLE_RATE,
  DEFAULT_PING_INTERVAL_MS,
  NET_REPORT_INTERVAL_MS,
  DEFAULT_PREBUFFER_BY_TIER,
  INITIAL_SEQUENCE_NUMBER,
  ERROR_TRANSPORT_CODE,
} from "./session/constants";
export { describeUnexpectedClose } from "./session/closeReason";

export class VoicePipesSessionCore {
  // Listener sets keyed by event. Stored with the widest function type the
  // typed `on`/`off` signatures narrow to — every `VoicePipesSessionEventMap[K]`
  // is assignable to it, and `emit` re-narrows per event.
  private listeners = new Map<keyof VoicePipesSessionEventMap, Set<(...args: never[]) => void>>();

  // Pure framework state machine
  private fsm = new PureVoicePipesClientFsm();

  // Underlying adapters & state properties
  private transport: TransportAdapter | null = null;
  /** Input rate the server chose in `session_ready`, if it named one.
   *  `startMic` prefers this over the caller's request. */
  private negotiatedInputRate: number | null = null;
  private netReportIntervalId: number | null = null;
  private _qualityTier: QualityTier = "full";
  /** Overridable per-tier jitter-buffer depths (see
   *  {@link DEFAULT_PREBUFFER_BY_TIER}). */
  prebufferByTier: Record<QualityTier, number> = { ...DEFAULT_PREBUFFER_BY_TIER };
  /** Cumulative playback counters at the previous `net_report`, so each report
   *  carries a delta rather than a running total. */
  private lastNetSnapshot: NetSnapshot | null = null;
  private capture: AudioCaptureService | null = null;
  private playback: AudioPlaybackService | null = null;
  private captionSync: CaptionSync | null = null;
  private playbackClock_: PlaybackClock | null = null;

  // Session states
  private _sessionState: "idle" | "connecting" | "active" | "error" = "idle";
  private _pipelineState: PipelineState | null = null;
  private _errorMessage: string | null = null;
  private _rttMs: number | null = null;
  private _currentAudioMeta: AudioMeta | null = null;
  private _config: SessionConfig | null = null;

  // NTP estimation and Pings
  private clockSync = new ClockSync();
  private lastPingSent = 0;
  private pingIntervalId = 0;

  // Event sequence counters and latch refs
  private seqNum = 0;

  // State machinery for Barge-in and Interruptions
  private interruptState: InterruptState = initialInterruptState;

  // Tracks whether the next transport `disconnected` is an *expected* close —
  // set when the client initiates disconnect, when the server sends a `closing`
  // control frame, or after a fatal error already surfaced. When the socket
  // drops without any of these, the disconnect is treated as an abnormal drop
  // (server crash, wrong-server port collision, network loss) and surfaced as
  // an error instead of silently going idle.
  private expectedClose = false;

  // References and caches for precise turn management
  /** `timing_report speech_start` sent for the current user turn (client VAD start). */
  private sentSpeechStart = false;
  /** First mic frame of the current user turn seen — gates the mic-open
   *  rotation (`pendingListeningRotation` → `resetFirstAudio`). */
  private micFrameSeen = false;
  private receivedFirstAudio = false;
  private pendingListeningRotation = false;
  private pendingFallbackText: { uttId: number; text: string } | null = null;
  /** performance.now() of the server's `listening` transition — the legacy
   *  (mic-open) E2E anchor, used only when the client VAD stamped nothing. */
  private speechEndTs = 0;
  private clientE2eMs: number | undefined = undefined;
  private clientE2eSource: ClientE2eSource | undefined = undefined;

  // Client-side end-of-speech endpointer. Measurement only: it stamps
  // `timing_report speech_start` / `speech_end` and anchors `clientE2eMs`;
  // it never gates or delays audio.
  private clientVadOptions: ClientVadOptions = {};
  private clientVad: EnergyEndpointer | null = null;
  /** performance.now() of the client VAD's end of speech for the current user
   *  turn (0 = none yet). Set exactly when `speech_end` is reported. */
  private vadSpeechEndPerf = 0;
  /** `timing_report speech_end` already sent for the current user turn. */
  private speechEndReported = false;
  /** The VAD's current speech segment started while the user's turn was open
   *  (listening/processing) — its end still belongs to the user's turn even if
   *  the server has meanwhile moved on to `speaking`. */
  private speechBeganInUserTurn = false;
  /** Predicts the id of the spoken turn about to start, which the `speech_*`
   *  reports are filed under. See `utteranceIds.ts` for why this reads the
   *  server's numbering rather than assuming it. */
  private utteranceIds = new UtteranceIdPredictor();
  /** performance.now() when the first audio byte of the current utterance arrived. */
  private firstAudioArrivalPerf: number | null = null;
  /** Last computed client playback timings; merged into the next turn_metrics envelope. */
  private lastPlaybackMetrics: ClientPlaybackMetrics | null = null;

  /** Utterances whose WordTimingTable is fed by server-authoritative
   *  caption `word_timings` (realtime providers with speech alignment,
   *  e.g. ElevenLabs). Estimate-based fragments — realtime text-delta
   *  seeds and the final-text fallback — must not append to these
   *  tables; the first authoritative caption evicts any estimate-seeded
   *  content. */
  private authoritativeTimingUtts = new Set<number>();
  /** Utterances whose transcript text arrives via realtime text-delta
   *  messages. Their captions carry the same words for *timing* only —
   *  re-emitting the caption text would duplicate the bubble. */
  private rtTranscriptUtts = new Set<number>();

  // Track the speaking entry ID and live states
  private _speakingUtteranceId: number | null = null;
  private speakingEntryIsLive = false;
  private placeholderUtteranceId: number | null = null;
  private agentTextReceivedForUtterance: number | null = null;
  private lastBargeInPlayedMs: number | null = null;
  /** Last utterance for which a playback_quality frame was emitted (dedup guard).
   *  Shared by the natural-end path (SEND_PLAYBACK_COMPLETE) and the flush
   *  path (barge-in / cancel) so a turn never sends two frames. */
  private lastQualityUtteranceId: number | null = null;
  /** Annotation lateness of the utterance the playback clock is on. A single
   *  record suffices: `late > 0` can only happen for the utterance currently
   *  playing, and the clock moves monotonically through utterances. */
  private annotationLate: AnnotationLateness | null = null;

  // Bounded map of per-utterance timing tables
  private tables = new Map<number, WordTimingTable>();

  // Clock ratchet state for karaoke emission
  private ratchetUtteranceId: number | null = null;
  private ratchetFloor = 0;

  // Unsubscribe function for PlaybackClock subscription
  private clockUnsubscribe: (() => void) | null = null;

  // Playback options matching the custom UI configurations
  private playbackOptions: PlaybackOptions = {
    limitScheduledAudio: false,
    maxScheduledChunks: 2,
    prebufferMs: DEFAULT_PREBUFFER_MS,
  };

  // Pub/Sub observer pattern
  on<K extends keyof VoicePipesSessionEventMap>(event: K, listener: VoicePipesSessionEventMap[K]): void {
    let set = this.listeners.get(event);
    if (!set) {
      set = new Set();
      this.listeners.set(event, set);
    }
    set.add(listener);
  }

  off<K extends keyof VoicePipesSessionEventMap>(event: K, listener: VoicePipesSessionEventMap[K]): void {
    this.listeners.get(event)?.delete(listener);
  }

  private emit<K extends keyof VoicePipesSessionEventMap>(event: K, ...args: Parameters<VoicePipesSessionEventMap[K]>): void {
    const set = this.listeners.get(event);
    if (set) {
      for (const listener of set) {
        try {
          // Re-narrow from the storage type to this event's signature. `on`
          // only ever admits `VoicePipesSessionEventMap[K]` for key K, so the
          // conversion is sound; TS cannot relate the two generically.
          (listener as unknown as (...a: Parameters<VoicePipesSessionEventMap[K]>) => void)(...args);
        } catch (e) {
          console.error(`Error in event listener for ${event}:`, e);
        }
      }
    }
  }

  // Getters for external read access
  get sessionState(): "idle" | "connecting" | "active" | "error" { return this._sessionState; }
  get pipelineState(): PipelineState | null {
    const s = this.fsm.getCurrentState();
    return s === "idle" ? null : s;
  }
  get errorMessage(): string | null { return this._errorMessage; }
  get rttMs(): number | null { return this._rttMs; }
  get currentAudioMeta(): AudioMeta | null { return this._currentAudioMeta; }
  get activeUtteranceId(): number | null { return this._speakingUtteranceId; }
  get micActive(): boolean { return this.capture?.active ?? false; }
  get micLevel(): number { return this.capture?.level ?? 0; }
  get playbackLevel(): number { return this.playback?.level ?? 0; }
  get config(): SessionConfig | null { return this._config; }

  // Expose the raw audio playback service reference for components needing context (like visualizer/clock)
  get playbackService(): AudioPlaybackService | null { return this.playback; }
  /** RAF-driven, multi-subscriber playback clock for the current session
   *  (null when idle). Snapshots: nowMs/totalMs/utteranceId/isRunning. */
  get playbackClock(): PlaybackClock | null { return this.playbackClock_; }

  private setSessionState(state: typeof this._sessionState): void {
    if (this._sessionState === state) return;
    this._sessionState = state;
    this.emit("sessionStateChanged", state);
  }

  private dispatchInterrupt(action: InterruptAction): void {
    this.interruptState = interruptReducer(this.interruptState, action);
  }

  private logEvent(dir: "in" | "out", type: string, summary: string, detail?: unknown): void {
    this.emit("eventsEmitted", dir, type, summary, detail);
  }

  // Connect to the orchestrator gateway
  async connect(
    url: string,
    transportType: TransportType,
    config?: SessionConfig,
    playbackOptions?: PlaybackOptions
  ): Promise<void> {
    this.stopAudioPipeline();
    this.resetSessionState();
    this.seqNum = INITIAL_SEQUENCE_NUMBER;

    if (playbackOptions) {
      this.playbackOptions = {
        limitScheduledAudio: playbackOptions.limitScheduledAudio,
        maxScheduledChunks: Math.max(1, playbackOptions.maxScheduledChunks),
        prebufferMs: Math.max(0, playbackOptions.prebufferMs ?? DEFAULT_PREBUFFER_MS),
      };
      this.clientVadOptions = { ...(playbackOptions.clientVad ?? {}) };
    }

    this._config = config ?? null;
    this.expectedClose = false;
    this.logEvent("out", "control", `connecting to ${url} via ${transportType}`, config);
    this.setSessionState("connecting");
    this._errorMessage = null;
    this._rttMs = null;

    try {
      this.transport = createTransport(transportType);
      this.transport.onMessage = (msg) => this.handleMessage(msg);
      this.transport.onStateChange = (state: TransportState, info?: TransportCloseInfo) => {
        if (state === "disconnected") {
          this.handleDisconnectFallback(info);
        }
      };
      this.transport.onError = (err: Error) => {
        this._errorMessage = err.message;
        this.setSessionState("error");
        this.emit("errorRaised", ERROR_TRANSPORT_CODE, err.message, "transport");
      };

      await this.transport.connect(url, config);

      if (this._sessionState === "idle") {
        // Connection completed after disconnect was called. Clean up and abort.
        this.transport?.disconnect();
        this.transport = null;
        return;
      }

      if (this._sessionState !== "active") {
        this.setSessionState("active");
      }

      this.pingIntervalId = window.setInterval(() => {
        if (this.transport?.state === "connected") {
          this.lastPingSent = performance.now();
          this.transport.sendControl({ action: "ping", timestamp_ms: Date.now() });
        }
      }, DEFAULT_PING_INTERVAL_MS);

      this.netReportIntervalId = window.setInterval(() => {
        this.sendNetReport();
      }, NET_REPORT_INTERVAL_MS);

    } catch (err) {
      if (this._sessionState === "idle") {
        // Silently swallow/ignore errors from cancelled connection attempts
        return;
      }
      const msg = err instanceof Error ? err.message : "Connection failed";
      this._errorMessage = msg;
      this.setSessionState("error");
      this.emit("errorRaised", ERROR_TRANSPORT_CODE, msg, "transport");
      throw err;
    }
  }

  // Disconnect the session cleanly
  disconnect(): void {
    // Client-initiated: the upcoming transport `disconnected` is expected, so
    // handleDisconnectFallback must not report it as an abnormal drop.
    this.expectedClose = true;
    this.transitionFsm({ type: "CONNECTION_LOST" });
    if (this.transport) {
      this.transport.disconnect();
      this.transport = null;
    }
    this.stopAudioPipeline();
    this.clearPingInterval();
    this.logEvent("out", "control", "disconnected");
    this.setSessionState("idle");
    this.resetSessionState();
    this.emit("sessionTerminated", "Clean client disconnect requested");
  }

  // Clear existing conversations but preserve connection
  clearConversation(): void {
    this.resetSessionState();
  }

  // Send typed text mid-speech (or under listening)
  sendText(text: string): void {
    if (!this.transport || this.transport.state !== "connected") return;

    const serverActive = this._pipelineState === "speaking" || this._pipelineState === "processing";
    const scheduledAheadMs = this.playback?.getBufferStats()?.scheduledAheadMs ?? 0;
    const hasBufferedAudio = scheduledAheadMs > 0;
    const isActive = serverActive || hasBufferedAudio;

    debug(`[SDK Core] sendText serverActive=${serverActive} scheduledAheadMs=${scheduledAheadMs} isActive=${isActive}`);

    const msgId = crypto.randomUUID();
    this.transport.sendText(text, msgId);
    this.emit("textDelta", {
      text,
      isFinal: true,
      role: "user",
      messageId: msgId,
    });

    if (isActive) {
      if (serverActive) {
        this.transport.sendControl({ action: "cancel" });
        this.logEvent("out", "control", "cancel (pre-text interruption)");
      }

      // Quality BEFORE flush — flush() resets the counters the frame reads.
      if (this._speakingUtteranceId != null) this.emitPlaybackQuality(this._speakingUtteranceId, true);
      const preFlushPlayedMs = this.playback?.flush() ?? 0;
      this.captionSync?.flush();

      this.dispatchInterrupt({
        type: "USER_INITIATED_FLUSH",
        utteranceId: this._speakingUtteranceId,
        heardMs: preFlushPlayedMs,
        reason: "user_text",
      });

      const flushedUttId = this._speakingUtteranceId;
      if (flushedUttId != null) {
        const finalTrimWordIndex = this.getTrimWordIndex(flushedUttId, preFlushPlayedMs);

        this.emit("audioFlushed", {
          utteranceId: flushedUttId,
          playedMs: preFlushPlayedMs,
          reason: "cancel",
          finalTrimWordIndex,
        });
      }
    }

    this.logEvent("out", "text", text);
  }

  // Create speaker audio playback service once sample-rates are negotiated
  private createPlaybackService(sampleRate: number): AudioPlaybackService {
    const pb = new AudioPlaybackService(sampleRate, this.playbackOptions);

    pb.onPlaybackStart = (uttId: number) => {
      this.handlePlaybackStart(uttId);
    };

    pb.onPlaybackEnd = (uttId: number) => {
      this.handlePlaybackEnd(uttId);
    };

    this.captionSync = new CaptionSync();

    pb.onChunk = (chunk) => {
      if (this.captionSync && !chunk.meta.caption) {
        const durationSec = chunk.endTime - chunk.startTime;
        this.captionSync.extendLastSentence(durationSec);
      }
      // Before audioChunk so extensions see playbackMetrics first.
      this.recordFirstChunkTimings(pb, chunk);
      this.emit("audioChunk", chunk);
    };

    this.playback = pb;

    // Attach PlaybackClock to observe real-time highlighted karaoke timing headlessly
    const getPlayback = () => this.playback;
    this.playbackClock_ = new DefaultPlaybackClock({
      source: createPlaybackSource({
        get current() {
          return getPlayback();
        }
      })
    });
    if (this.clockUnsubscribe) {
      this.clockUnsubscribe();
    }
    this.clockUnsubscribe = this.playbackClock_.subscribe((snap) => {
      this.tickHighlight(snap.nowMs, snap.utteranceId);
    });

    // After playbackClock_ is set so both are readable in the listener.
    this.emit("playbackChanged", pb);
    return pb;
  }

  /** On the first scheduled chunk of an utterance, derive the client playback
   *  timings. Runs synchronously inside `AudioPlaybackService.schedule()` right
   *  after `source.start(startAt)`, so `chunk.startTime − ctx.currentTime` is
   *  the remaining hold before the sample is audible; `ctx.outputLatency`
   *  (Chrome/Firefox only — Safari reports nothing, so the number then excludes
   *  the 10–40 ms output-device latency) is added on top. */
  private recordFirstChunkTimings(pb: AudioPlaybackService, chunk: PlaybackChunk): void {
    const arrival = this.firstAudioArrivalPerf;
    const ctx = pb.audioContext;
    if (arrival === null || !ctx) return;
    this.firstAudioArrivalPerf = null; // one measurement per utterance
    const utteranceId = chunk.meta.utteranceId ?? this._speakingUtteranceId;
    if (utteranceId == null) return;

    const untilAudible = untilAudibleMs(chunk.startTime, ctx);
    const nowPerfMs = performance.now();
    const serverTs = chunk.meta.serverTimestampMs;
    const delivery =
      serverTs !== undefined && this.clockSync.hasSamples
        ? { nowWallMs: Date.now(), clockOffsetMs: this.clockSync.offset, serverTimestampMs: serverTs }
        : null;

    const m = firstChunkPlaybackMetrics({
      utteranceId,
      untilAudibleMs: untilAudible,
      nowPerfMs,
      arrivalPerfMs: arrival,
      clientE2eMs: this.clientE2eMs,
      clientE2eSource: this.clientE2eSource,
      delivery,
    });
    this.lastPlaybackMetrics = m;
    this.logEvent("in", "control", describePlaybackTimings(m), m);
    this.emit("playbackMetrics", m);
  }

  /** Reveal every remaining word of `uttId` after its playback finished
   *  naturally. No-op when the utterance has no timing table, is already
   *  fully revealed, or when a NEWER utterance owns the highlight (a late
   *  `playbackEnd` must not yank the karaoke view back to an old bubble —
   *  non-active entries render fully styled anyway). */
  private flushHighlightToEnd(uttId: number): void {
    const table = this.tables.get(uttId);
    if (!table || table.wordCount === 0) return;
    if (this.ratchetUtteranceId !== null && this.ratchetUtteranceId !== uttId) return;
    if (this.ratchetUtteranceId === uttId && this.ratchetFloor >= table.wordCount) return;

    const wordIdx = table.wordCount - 1;
    this.ratchetUtteranceId = uttId;
    this.ratchetFloor = wordIdx + 1;
    const timing = table.timings[wordIdx];
    if (timing) {
      this.emit("wordHighlighted", {
        word: timing.word,
        startMs: timing.start_ms,
        endMs: timing.end_ms,
        utteranceId: uttId,
        wordIndex: wordIdx,
      });
    }
  }

  // Handle high-frequency tick transitions of the highlighted words
  private tickHighlight(nowMs: number, curUttId: number | null): void {
    if (curUttId === null) return;
    const table = this.tables.get(curUttId);
    if (!table || table.wordCount === 0) return;

    if (this.ratchetUtteranceId !== curUttId) {
      this.ratchetUtteranceId = curUttId;
      this.ratchetFloor = 0;
    }

    const wordsHeard = table.wordsHeardAtMs(nowMs, this.ratchetFloor);
    if (wordsHeard <= 0 || wordsHeard <= this.ratchetFloor) return;

    const wordIdx = Math.min(table.wordCount - 1, wordsHeard - 1);
    if (wordIdx >= this.ratchetFloor) {
      this.ratchetFloor = wordIdx + 1;
      const timing = table.timings[wordIdx];
      if (timing) {
        this.emit("wordHighlighted", {
          word: timing.word,
          startMs: timing.start_ms,
          endMs: timing.end_ms,
          utteranceId: curUttId,
          wordIndex: wordIdx,
        });
      }
    }
  }

  /** Report link conditions to the server (S3 + S5).
   *
   *  `playback_quality` says the same thing more accurately but only once a
   *  turn ends, which is too late to react during a long one. This samples the
   *  same counters periodically and sends deltas.
   *
   *  Uplink backlog is the half the server cannot see at all: it can time its
   *  own socket writes, but only we know how much mic audio is queued locally
   *  and not yet on the wire. */
  private sendNetReport(): void {
    if (this.transport?.state !== "connected") return;

    const now = performance.now();
    const q = this.playback?.getPlaybackQuality(0) ?? null;
    const { report, snapshot } = netReportDeltas(q, this.lastNetSnapshot, now);
    if (snapshot) this.lastNetSnapshot = snapshot;

    this.transport.sendControl({
      action: "net_report",
      ...report,
      uplink_backlog_bytes: this.transport.bufferedAmount ?? 0,
    });
  }

  /** Start capturing microphone audio.
   *
   *  `sampleRate` is a *request*. When the server named an `input_sample_rate`
   *  in `session_ready` that value wins, because the server is authoritative:
   *  it knows what the STT needs and the rate is fixed for the session. Capture
   *  resamples, so honouring it costs nothing and typically saves 3× on the
   *  uplink (48 kHz browser capture → 16 kHz STT native). */
  /**
   * Start the microphone.
   *
   * `processing` overrides the browser's own echo cancellation, noise
   * suppression and gain control, which are on by default. Pass them off when
   * the audio itself is the subject -- recording a corpus, or working out why
   * speech is not reaching the recogniser -- since they can gate quiet speech
   * to silence before anything here sees it.
   */
  async startMic(
    requestedRate = DEFAULT_MIC_SAMPLE_RATE,
    processing?: AudioProcessingOptions,
  ): Promise<number> {
    const sampleRate = this.negotiatedInputRate ?? requestedRate;
    if (this.capture?.active) return this.capture.actualSampleRate || sampleRate;

    const capture = new AudioCaptureService();
    this.capture = capture;
    // Client end-of-speech endpointer: default on; `clientVad.enabled: false`
    // keeps the legacy `listening`-anchored E2E and sends no speech reports.
    const { enabled, ...vadOpts } = this.clientVadOptions;
    this.clientVad = enabled === false ? null : new EnergyEndpointer(vadOpts);
    capture.onSamples = (samples: Int16Array) => {
      const seq = this.seqNum++;
      this.transport?.sendAudio(samples, seq);

      if (!this.micFrameSeen) {
        if (this.pendingListeningRotation) {
          this.pendingListeningRotation = false;
          this.resetFirstAudio();
        }
        this.micFrameSeen = true;
      }

      // `capture.level` is the RMS of exactly this frame (computed right
      // before the callback). The VAD only sees unmuted frames, so muting
      // freezes it rather than ending speech.
      const vadEvent = this.clientVad?.feed(capture.level, performance.now()) ?? null;
      if (vadEvent) this.handleClientVadEvent(vadEvent);
    };

    // `AudioCaptureService` resamples when the browser ignores the requested
    // rate, so `actualSampleRate` is always the rate we asked for. There is
    // deliberately no mid-session correction here: the server binds the input
    // rate once at the handshake and ignores a later `configure` (see
    // `handle_configure` in serve/runner/context.rs), so the old
    // mismatch-and-reconfigure path never took effect. The rate has to be
    // right in the initial SessionConfig instead.
    await this.capture.start(sampleRate, processing);
    const actual = this.capture.actualSampleRate;

    // Report the context rate and whether a conversion is in the path, not
    // just the rate we promised. When a cascade pipeline negotiates 16 kHz
    // and a speech-to-speech one keeps 48 kHz, the two take different routes
    // through capture -- and "microphone started (16000 Hz)" looks identical
    // either way, which makes a fault on one path invisible in the log.
    const ctxRate = this.capture.contextSampleRate;
    const via = this.capture.isResampling
      ? `context ${ctxRate} Hz, resampled`
      : `context ${ctxRate} Hz, direct`;
    this.logEvent(
      "out",
      "mic",
      `microphone started (${actual || sampleRate} Hz; ${via})`,
    );
    return actual || sampleRate;
  }

  // Stop capture microphone audio
  stopMic(): void {
    this.capture?.stop();
    this.capture = null;
    this.clientVad = null;
    this.logEvent("out", "mic", "microphone stopped");
  }

  /** The user's turn is open: the server is waiting for / working on the
   *  user's input, not playing the agent's reply. `null` = before the first
   *  `state_changed` (a session that has just become ready). */
  private userTurnOpen(): boolean {
    const s = this._pipelineState;
    return s === null || s === "listening" || s === "processing";
  }

  /** The server is still waiting on the user: no turn has been committed, so
   *  speech that resumes now continues the same utterance. */
  private turnNotYetCommitted(): boolean {
    return this._pipelineState === null || this._pipelineState === "listening";
  }

  /** Utterance id the client's `speech_*` timing reports are filed under:
   *  the spoken turn the user is about to trigger. */
  private predictedNextUtteranceId(): number {
    return this.utteranceIds.next();
  }

  private noteUtteranceId(id: number | null | undefined): void {
    this.utteranceIds.note(id);
  }

  /** Client VAD events.
   *
   *  `speech_start`: reported once per user turn at the VAD's start of speech
   *  (previously: on the first mic frame, i.e. "mic opened"). Sent in any
   *  pipeline state — during agent playback it precedes the barge-in decision.
   *
   *  `speech_end`: reported only while the user's turn is open (`listening`
   *  / `processing`, or a segment that began there and outlived a fast
   *  `speaking` transition). An end that lands during agent playback without
   *  the interrupt path having taken the turn (no `listening` yet) is
   *  suppressed — it would anchor nothing. The same instant becomes the
   *  `clientE2eMs` anchor (`client_vad`).
   *
   *  It may be reported MORE than once per turn, and the last one stands. A
   *  detector cannot tell a pause from the end of a turn; only the server's
   *  commit can. So when speech resumes while the server is still
   *  `listening`, the end already reported was a pause, and the next one
   *  supersedes it. This used to be "at most once", which kept the pause and
   *  discarded the real end: someone who said "…to predict the internal bond
   *  in, uh, particle boards" had their response time measured from the "uh",
   *  and a 1.1 s reply was reported as 7.7 s. */
  private handleClientVadEvent(event: EndpointingEvent): void {
    if (event === "speech_start") {
      if (this.userTurnOpen()) this.speechBeganInUserTurn = true;
      // Speaking again before the server committed the turn: the end already
      // reported was a pause. Re-arm so the real end is reported and replaces
      // it. Not once the server is `processing` -- that turn is committed,
      // and new speech is not part of it.
      if (this.speechEndReported && this.turnNotYetCommitted()) {
        this.speechEndReported = false;
        this.logEvent("out", "mic", "speech resumed before the turn committed; the earlier speech_end was a pause");
      }
      if (this.sentSpeechStart) return;
      this.sentSpeechStart = true;
      // During agent playback the start relates to the utterance being
      // interrupted (legacy semantics); otherwise to the upcoming user turn.
      const uttId = this._speakingUtteranceId ?? this.predictedNextUtteranceId();
      this.transport?.sendTimingReport?.(uttId, "speech_start");
      this.logEvent("out", "control", "speech_start (client vad)", { utterance_id: uttId, client_timestamp_ms: Date.now() });
      return;
    }

    const began = this.speechBeganInUserTurn;
    this.speechBeganInUserTurn = false;
    if (this.speechEndReported) {
      this.logEvent("out", "mic", "speech_end (client vad) suppressed: already reported for this turn");
      return;
    }
    if (!this.userTurnOpen() && !began) {
      this.logEvent("out", "mic", `speech_end (client vad) suppressed: agent ${this._pipelineState}`);
      return;
    }
    this.speechEndReported = true;
    this.vadSpeechEndPerf = performance.now();
    const uttId = this.predictedNextUtteranceId();
    this.transport?.sendTimingReport?.(uttId, "speech_end");
    this.logEvent("out", "control", "speech_end (client vad)", { utterance_id: uttId, client_timestamp_ms: Date.now() });
  }

  // Flush local audio playback
  flushLocalAudio(): void {
    // Quality BEFORE flush — flush() resets the counters the frame reads.
    if (this._speakingUtteranceId != null) this.emitPlaybackQuality(this._speakingUtteranceId, true);
    const playedMs = this.playback?.flush() ?? 0;
    this.captionSync?.flush();
    this.dispatchInterrupt({
      type: "USER_INITIATED_FLUSH",
      utteranceId: this._speakingUtteranceId,
      heardMs: playedMs,
      reason: "client_cancel",
    });

    const flushedUttId = this._speakingUtteranceId;
    if (flushedUttId != null) {
      const finalTrimWordIndex = this.getTrimWordIndex(flushedUttId, playedMs);

      this.emit("audioFlushed", {
        utteranceId: flushedUttId,
        playedMs,
        reason: "cancel",
        finalTrimWordIndex,
      });
    }
  }

  // Explicit user cancel ("Stop" button)
  cancelAgent(): void {
    if (!this.transport || this.transport.state !== "connected") return;

    const serverActive = this._pipelineState === "speaking" || this._pipelineState === "processing";
    const scheduledAheadMs = this.playback?.getBufferStats()?.scheduledAheadMs ?? 0;
    if (!serverActive && scheduledAheadMs === 0) return;

    if (serverActive) {
      this.transport.sendControl({ action: "cancel" });
      this.logEvent("out", "control", "cancel (explicit stop)");
    }

    // Quality BEFORE flush — flush() resets the counters the frame reads.
    if (this._speakingUtteranceId != null) this.emitPlaybackQuality(this._speakingUtteranceId, true);
    const playedMs = this.playback?.flush() ?? 0;
    this.captionSync?.flush();

    this.dispatchInterrupt({
      type: "USER_INITIATED_FLUSH",
      utteranceId: this._speakingUtteranceId,
      heardMs: playedMs,
      reason: "client_cancel",
    });

    const cancelUttId = this._speakingUtteranceId;
    if (cancelUttId != null) {
      this.transport.sendPlaybackReport?.(cancelUttId, playedMs);
      this.transport.sendPlaybackComplete?.(cancelUttId);

      const finalTrimWordIndex = this.getTrimWordIndex(cancelUttId, playedMs);

      this.emit("audioFlushed", {
        utteranceId: cancelUttId,
        playedMs,
        reason: "cancel",
        finalTrimWordIndex,
      });
    }
  }

  // Explicit manual triggers for QA operators
  sendManualPlaybackReport(offsetMs: number): void {
    if (!this.transport || this.transport.state !== "connected") return;
    const uttId = this._speakingUtteranceId;
    if (uttId == null) return;
    const safeOffset = Math.max(0, Math.round(offsetMs));
    this.transport.sendPlaybackReport?.(uttId, safeOffset);
    this.logEvent("out", "playback_report", `manual report utt=${uttId} offset=${safeOffset}ms`);
  }

  sendManualPlaybackComplete(): void {
    if (!this.transport || this.transport.state !== "connected") return;
    const uttId = this._speakingUtteranceId;
    if (uttId == null) return;
    this.transport.sendPlaybackComplete?.(uttId);
    this.logEvent("out", "playback_complete", `manual complete utt=${uttId}`);
  }

  // Close loops and helpers
  private handleDisconnectFallback(info?: TransportCloseInfo): void {
    const wasSpeaking = this._pipelineState === "speaking" || this._pipelineState === "processing";
    const playedMs = wasSpeaking ? (this.playback?.flush() ?? 0) : 0;

    if (wasSpeaking) {
      const flushedUttId = this._speakingUtteranceId;
      this.dispatchInterrupt({
        type: "TRANSPORT_DISCONNECTED",
        utteranceId: flushedUttId,
      });

      if (flushedUttId != null) {
        const finalTrimWordIndex = this.getTrimWordIndex(flushedUttId, playedMs);

        this.emit("audioFlushed", {
          utteranceId: flushedUttId,
          playedMs,
          reason: "disconnect",
          finalTrimWordIndex,
        });
      }
    }

    this.transitionFsm({ type: "CONNECTION_LOST" });
    this.stopAudioPipeline();

    if (this.expectedClose || this._sessionState === "error") {
      // Clean shutdown: a client-initiated disconnect, a server `closing`
      // frame, or a fatal error we already reported. Don't clobber an
      // already-surfaced error state with idle.
      if (this._sessionState !== "error") {
        this.setSessionState("idle");
      }
    } else {
      // The socket dropped with no `closing` frame, no client disconnect, and
      // no fatal error: the peer vanished (server crash, wrong-server port
      // collision, network loss). Surface it explicitly instead of silently
      // resetting to idle — that silent reset is indistinguishable from a clean
      // shutdown and is exactly what makes these failures hard to diagnose.
      const message = describeUnexpectedClose(info);
      this._errorMessage = message;
      this.logEvent("in", "control", message, info);
      this.setSessionState("error");
      this.emit("errorRaised", ERROR_TRANSPORT_CODE, message, "transport");
      this.emit("sessionTerminated", message);
    }
  }

  destroy(): void {
    this.clearPingInterval();
    this.stopAudioPipeline();
    if (this.transport) {
      this.transport.disconnect();
      this.transport = null;
    }
    this.listeners.clear();
  }

  private stopAudioPipeline(): void {
    this.capture?.stop();
    this.capture = null;
    const hadPlayback = this.playback !== null;
    this.playback?.stop();
    this.playback = null;
    this.captionSync?.flush();
    this.captionSync = null;

    if (this.clockUnsubscribe) {
      this.clockUnsubscribe();
      this.clockUnsubscribe = null;
    }
    this.playbackClock_ = null;
    // After the clock is nulled so listeners observe a consistent idle state.
    if (hadPlayback) this.emit("playbackChanged", null);
  }

  private clearPingInterval(): void {
    if (this.netReportIntervalId) {
      window.clearInterval(this.netReportIntervalId);
      this.netReportIntervalId = null;
      this.lastNetSnapshot = null;
    }
    if (this.pingIntervalId) {
      window.clearInterval(this.pingIntervalId);
      this.pingIntervalId = 0;
    }
  }

  /** Arm the "first audio of the next utterance" detector. Every site that
   *  used to flip `receivedFirstAudio` goes through here so the arrival stamp
   *  used by `recordFirstChunkTimings` can never outlive the utterance it
   *  belongs to. */
  private resetFirstAudio(): void {
    this.receivedFirstAudio = false;
    this.firstAudioArrivalPerf = null;
  }

  private resetSessionState(): void {
    this.fsm = new PureVoicePipesClientFsm();
    this._pipelineState = null;
    this.sentSpeechStart = false;
    this.micFrameSeen = false;
    this.resetFirstAudio();
    this.lastPlaybackMetrics = null;
    this.annotationLate = null;
    this.speechEndTs = 0;
    this.clientE2eMs = undefined;
    this.clientE2eSource = undefined;
    this.vadSpeechEndPerf = 0;
    this.speechEndReported = false;
    this.speechBeganInUserTurn = false;
    this.utteranceIds.reset();
    this.clientVad?.reset();
    this.placeholderUtteranceId = null;
    this.agentTextReceivedForUtterance = null;
    this._speakingUtteranceId = null;
    this.speakingEntryIsLive = false;
    this.pendingFallbackText = null;
    this.pendingListeningRotation = false;

    this.dispatchInterrupt({ type: "RESET" });
    this.captionSync?.flush();
    this.tables.clear();
    this.authoritativeTimingUtts.clear();
    this.rtTranscriptUtts.clear();
    this.ratchetUtteranceId = null;
    this.ratchetFloor = 0;
  }

  private handlePlaybackStart(_uttIdFromPlayback: number): void {
    const uttId = _uttIdFromPlayback;
    if (uttId != null) {
      this.transport?.sendTimingReport?.(uttId, "playback_start");
    }

    if (this._pipelineState !== "speaking") return;

    if (
      uttId != null &&
      !this.speakingEntryIsLive &&
      this.agentTextReceivedForUtterance !== uttId &&
      this.placeholderUtteranceId !== uttId
    ) {
      this.placeholderUtteranceId = uttId;
      this.speakingEntryIsLive = true;
    }
  }

  private handlePlaybackEnd(_uttId: number): void {
    // Natural end of playback: everything that was scheduled has been
    // heard, so any words the karaoke clock could not reach are revealed
    // now (estimated word timings routinely place the tail past the real
    // audio end, and the clock is clamped to played audio — without this
    // flush those words stay dim until the next turn dismisses the view).
    // Barge-ins never get here: `flush()` detaches `onended` before
    // stopping sources, so interrupted turns keep their trim behavior
    // and never over-reveal unspoken words.
    this.flushHighlightToEnd(_uttId);

    this.transitionFsm({ type: "PLAYBACK_END", utteranceId: _uttId });

    this.pendingFallbackText = null;
    this.speakingEntryIsLive = false;

    if (this._speakingUtteranceId === _uttId) {
      this._speakingUtteranceId = null;
    }

    if (this.pendingListeningRotation) {
      this.pendingListeningRotation = false;
      this.sentSpeechStart = false;
      this.micFrameSeen = false;
      this.resetFirstAudio();
    }
  }

  private transitionFsm(event: ClientFsmEvent): void {
    const oldState = this.fsm.getCurrentState();
    const actions = this.fsm.transition(event);
    const newState = this.fsm.getCurrentState();

    if (oldState !== newState) {
      this._pipelineState = newState === "idle" ? null : newState;
      this.emit("stateChanged", this._pipelineState);
    }

    for (const action of actions) {
      this.executeFsmAction(action);
    }
  }

  private executeFsmAction(action: ClientFsmAction): void {
    switch (action.type) {
      case "START_PLAYBACK_SYSTEM":
        if (this.playback) {
          this.playback.stop();
        }
        this.createPlaybackService(action.sampleRate);
        break;

      case "STOP_PLAYBACK_SYSTEM":
        this.stopAudioPipeline();
        break;

      case "OPEN_BOT_TYPING_BUBBLE":
        // Emitted via stateChanged transition to processing
        break;

      case "ROTATE_BUBBLE_TO_KARAOKE":
        this.resetFirstAudio();
        this.noteUtteranceId(action.utteranceId);
        this._speakingUtteranceId = action.utteranceId;
        this.playback?.beginUtterance(action.utteranceId);
        this.playback?.start();
        this.captionSync?.setAudioContext(this.playback?.audioContext ?? null);
        break;

      case "FREEZE_BUBBLE_AT_POSITION": {
        // Fallback timings calculation for caption-less TTS providers BEFORE flushing plays
        const pending = this.pendingFallbackText;
        if (pending && pending.uttId === action.utteranceId) {
          const pb = this.playback;
          const totalMs = pb?.totalEnqueuedDurationMs ?? 0;
          if (totalMs > 0) {
            this.recordFinalTextFallback(action.utteranceId, pending.text, totalMs);
          }
          this.pendingFallbackText = null;
        }

        // Quality BEFORE flush — flush() resets the counters the frame reads.
        // A starving turn is usually interrupted before it completes, so the
        // flush path is exactly where the arrival span matters most.
        this.emitPlaybackQuality(action.utteranceId, true);
        const freshPlayedMs = this.playback?.flush() ?? 0;
        const pendingHeardMs = this.interruptState.pendingHeardMs;
        if (pendingHeardMs !== null) {
          this.dispatchInterrupt({ type: "CONSUME_PENDING_HEARD_MS" });
        }
        const playedMs = pendingHeardMs && pendingHeardMs > 0 ? pendingHeardMs : freshPlayedMs;
        this.lastBargeInPlayedMs = playedMs;

        let fallbackTrimWordIndex = this.getTrimWordIndex(action.utteranceId, playedMs);
        if (fallbackTrimWordIndex === null && this._speakingUtteranceId !== null) {
          fallbackTrimWordIndex = this.getTrimWordIndex(this._speakingUtteranceId, playedMs);
        }

        this.placeholderUtteranceId = null;

        this.captionSync?.flush();
        this.dispatchInterrupt({
          type: "SERVER_CLEAR_AUDIO",
          utteranceId: action.utteranceId,
          reason: "user_speech",
        });

        this.emit("audioFlushed", {
          utteranceId: action.utteranceId,
          playedMs,
          reason: "barge_in",
          finalTrimWordIndex: fallbackTrimWordIndex ?? undefined,
        });
        break;
      }

      case "SEND_PLAYBACK_REPORT": {
        const playedMs = this.lastBargeInPlayedMs !== null
          ? this.lastBargeInPlayedMs
          // Report heard (enqueue-bounded) audio, not raw wall-clock, so
          // the server never truncates history to unspoken text.
          : (this.playback?.getHeardDurationMs() ?? 0);
        this.transport?.sendPlaybackReport?.(action.utteranceId, playedMs);
        this.lastBargeInPlayedMs = null; // reset
        break;
      }

      case "SEND_PLAYBACK_COMPLETE": {
        this.transport?.sendTimingReport?.(action.utteranceId, "playback_end");
        this.transport?.sendPlaybackComplete?.(action.utteranceId);
        // Playback-quality telemetry: one frame per turn, at the natural
        // end of playback (the flush path emits the same frame with
        // `interrupted = true`).
        this.emitPlaybackQuality(action.utteranceId, false);
        this.emit("playbackFinished", { utteranceId: action.utteranceId });
        break;
      }
    }
  }

  /** The turn's `PlaybackQuality` as the wire wants it: the playback
   *  service's counters plus the core-owned `interrupted` flag and the
   *  annotation-lateness counters of this utterance (zeros when no annotator
   *  is configured). `null` when there is no playback service. */
  private buildPlaybackQuality(utteranceId: number, interrupted: boolean): PlaybackQuality | null {
    const q = this.playback?.getPlaybackQuality(utteranceId);
    if (!q) return null;
    const late = this.annotationLate?.utteranceId === utteranceId ? this.annotationLate : null;
    return {
      ...q,
      interrupted,
      annotationLateCount: late?.count ?? 0,
      annotationLateMsMax: late?.maxMs ?? 0,
    };
  }

  /** Emit the `playback_quality` frame for `utteranceId` — exactly once per
   *  turn (`lastQualityUtteranceId` dedup: the FSM can reach
   *  SEND_PLAYBACK_COMPLETE from more than one edge, and a flushed turn may
   *  still reach it afterwards). Skips empty turns (no audio received).
   *  Must run BEFORE `playback.flush()` on the interruption paths, since
   *  flush resets the counters the frame is built from. Also derives
   *  `clientArrivalRtf` and re-emits `playbackMetrics` so the Playground
   *  entry / envelope pick it up. */
  private emitPlaybackQuality(utteranceId: number, interrupted: boolean): void {
    if (this.lastQualityUtteranceId === utteranceId) return;
    const q = this.buildPlaybackQuality(utteranceId, interrupted);
    if (!q || q.audioMs <= 0) return;
    this.lastQualityUtteranceId = utteranceId;
    this.transport?.sendPlaybackQuality?.(q);
    this.logEvent("out", "playback_quality", describePlaybackQuality(utteranceId, q, interrupted), q);

    const m = withArrivalRtf(this.lastPlaybackMetrics, utteranceId, q);
    this.lastPlaybackMetrics = m;
    this.emit("playbackMetrics", m);
  }

  // Core router for server messages
  private handleMessage(msg: ServerMessage): void {
    switch (msg.kind) {
      case "audio":
        this.handleAudioMessage(msg);
        break;
      case "text":
        this.handleTextMessage(msg);
        break;
      case "control":
        this.handleControlMessage(msg);
        break;
      case "data":
        this.logEvent("in", "data", `data: ${(msg.payload as { kind?: string })?.kind ?? "structured"}`, msg.payload);
        break;
      case "message":
        this.handlePipelineMessage(msg);
        break;
      case "annotation":
        this.handleAnnotationMessage(msg.annotation);
        break;
    }
  }

  /** `audio_annotation` frames ride the same ordered socket as audio, so a
   *  flushed utterance can still have annotations in flight after
   *  `clear_audio` — drop them exactly like late audio for that utterance. */
  private handleAnnotationMessage(annotation: AudioAnnotation): void {
    if (shouldDropAudio(this.interruptState, annotation.utteranceId)) {
      debug("[SDK Core barge-in] dropping annotation after flush, utteranceId=", annotation.utteranceId, "kind=", annotation.kind);
      return;
    }
    this.recordAnnotationLateness(annotation);
    this.logEvent(
      "in",
      "annotation",
      `annotation kind=${annotation.kind} utt=${annotation.utteranceId} start=${annotation.startMs}ms dur=${annotation.durationMs}ms`,
      annotation,
    );
    this.emit("audioAnnotation", annotation);
  }

  /** Annotation lateness: at receipt,
   *  `late = playbackClock.nowMs − annotation.startMs` when the clock is on
   *  the annotation's utterance. `late > 0` means the audio the annotation
   *  describes had already started playing — the consumer could not have
   *  used it in time (the client-side truth behind `annotator.lagging`).
   *  Annotations for an utterance the clock is not on (arrived ahead of
   *  playback, or the turn has not started) are never late. */
  private recordAnnotationLateness(annotation: AudioAnnotation): void {
    const clock = this.playbackClock_;
    if (!clock || clock.getUtteranceId() !== annotation.utteranceId) return;
    const late = clock.getNowMs() - annotation.startMs;
    if (!(late > 0)) return;
    const lateMs = Math.round(late);
    if (this.annotationLate?.utteranceId !== annotation.utteranceId) {
      this.annotationLate = { utteranceId: annotation.utteranceId, count: 0, maxMs: 0 };
    }
    this.annotationLate.count += 1;
    this.annotationLate.maxMs = Math.max(this.annotationLate.maxMs, lateMs);
  }

  // Sub-handlers for each frame type
  private handleAudioMessage(msg: ServerMessage & { kind: "audio" }): void {
    const packetUtteranceId = msg.meta?.utteranceId ?? this._speakingUtteranceId;
    this.noteUtteranceId(packetUtteranceId);

    if (packetUtteranceId != null) {
      this.transitionFsm({
        type: "AUDIO_CHUNK_ARRIVED",
        utteranceId: packetUtteranceId,
        isFirst: !this.receivedFirstAudio
      });
    }

    if (shouldDropAudio(this.interruptState, packetUtteranceId)) {
      debug("[SDK Core barge-in] dropping audio after flush, utteranceId=", packetUtteranceId, "seq=", msg.seqNum);
      return;
    }

    if (!this.receivedFirstAudio && msg.samples.length > 0) {
      this.receivedFirstAudio = true;
      this.firstAudioArrivalPerf = performance.now();
      this.markFirstAudioReceived();
      const activeUttId = packetUtteranceId;
      if (activeUttId != null) {
        this.transport?.sendTimingReport?.(activeUttId, "audio_receive");
      }
    }

    const preEnqueueMs = this.playback?.getNextChunkStartOffsetMs() ?? 0;
    this.playback?.enqueue(msg.samples, msg.meta, msg.seqNum, msg.endOfUtterance);
    this._currentAudioMeta = msg.meta;
    this.logEvent("in", "audio", `${msg.samples.length} samples, seq=${msg.seqNum}`, msg.meta);

    if (msg.meta.caption && this.playback?.currentChunk) {
      const chunk = this.playback.currentChunk;
      const durationSec = chunk.endTime - chunk.startTime;
      this.captionSync?.addSentence(msg.meta.caption, chunk.startTime, durationSec);
    }

    const captionText = msg.meta.caption?.text;
    if (captionText) {
      if (msg.meta.utteranceId != null) {
        const totalFragmentMs = msg.meta.total_fragment_duration_ms ?? 0;
        const anchorMs = preEnqueueMs;
        if (msg.meta?.caption) {
          // Server-authoritative word timings (realtime alignment):
          // the first such caption evicts any estimate-seeded table
          // for this utterance so exact and estimated fragments never
          // mix.
          if (
            (msg.meta.caption.word_timings?.length ?? 0) > 0 &&
            !this.authoritativeTimingUtts.has(msg.meta.utteranceId)
          ) {
            this.tables.delete(msg.meta.utteranceId);
            this.authoritativeTimingUtts.add(msg.meta.utteranceId);
          }
          if (msg.samples.length === 0 && totalFragmentMs > 0) {
            // This is a metadata-only chunk sent at the end of synthesis.
            // Do not record the caption text again to avoid duplication,
            // just update the duration.
            const table = this.tables.get(msg.meta.utteranceId);
            if (table) {
              table.updateLastFragmentDuration(totalFragmentMs);
            }
          } else {
            this.recordCaptionFragment(msg.meta.utteranceId, msg.meta.caption, totalFragmentMs, anchorMs);
          }
        }
      }

      // Realtime turns receive their transcript via text-delta messages —
      // the caption carries the same words for timing only, and re-emitting
      // them would duplicate the bubble text. Two markers are needed:
      // `rtTranscriptUtts` covers text-before-audio ordering, and
      // `authoritativeTimingUtts` covers audio-before-text (ElevenLabs
      // streams aligned audio chunks BEFORE `agent_response` arrives, so
      // the first caption would otherwise land in the transcript as a
      // phantom prefix of the text delta that follows). Word-timed
      // captions imply a realtime provider whose transcript arrives as
      // text deltas, so their text never feeds the bubble.
      const captionUttId = msg.meta.utteranceId ?? this._speakingUtteranceId;
      if (
        this._speakingUtteranceId != null &&
        (msg.meta.utteranceId == null || msg.meta.utteranceId === this._speakingUtteranceId) &&
        !(
          captionUttId != null &&
          (this.rtTranscriptUtts.has(captionUttId) ||
            this.authoritativeTimingUtts.has(captionUttId))
        )
      ) {
        const activeUttId = this._speakingUtteranceId;
        this.emit("textDelta", {
          text: captionText,
          isFinal: false,
          role: "assistant",
          messageId: `llm-${activeUttId}`,
        });
      }
    }

    if (this.playback?.currentChunk) {
      this.captionSync?.updateLastScheduledEndTime(this.playback.currentChunk.endTime);
    }
  }

  private handleTextMessage(msg: ServerMessage & { kind: "text" }): void {
    const role: "user" | "assistant" = msg.messageId?.startsWith("stt-") ? "user" : "assistant";
    if (role === "assistant" && msg.messageId && shouldDropFinalText(this.interruptState, msg.messageId)) {
      this.logEvent("in", "text", `[agent SKIP – blocked ${msg.messageId}] ${msg.text.slice(0, 40)}…`, { messageId: msg.messageId });
      return;
    }

    let serverTextUttId = -1;
    if (msg.messageId?.startsWith("llm-")) {
      const n = parseInt(msg.messageId.slice("llm-".length), 10);
      if (Number.isFinite(n)) serverTextUttId = n;
    }
    this.emit("textDelta", {
      text: msg.text,
      isFinal: true,
      role: role === "user" ? "user" : "assistant",
      messageId: msg.messageId,
    });

    if (role === "assistant" && serverTextUttId >= 0) {
      const pb = this.playback;
      const totalMs = (pb?.getPlayedDurationMs() ?? 0) +
        (pb?.getBufferStats().scheduledAheadMs ?? 0) ||
        (pb?.totalEnqueuedDurationMs ?? 0);
      this.recordFinalTextFallback(serverTextUttId, msg.text, totalMs);
      this.pendingFallbackText = { uttId: serverTextUttId, text: msg.text };
    }

    this.logEvent("in", "text", `[${role}] ${msg.text}`, { messageId: msg.messageId, role });
  }

  private handlePipelineMessage(msg: ServerMessage & { kind: "message" }): void {
    const pm = msg.message;
    const role = pm.role === "user" ? "user" : pm.role === "system" ? "system" : "assistant";

    if (role === "user" && pm.message_id?.startsWith("stt-")) {
      this.emit("textDelta", {
        text: pm.text ?? "",
        isFinal: true,
        role: "user",
        messageId: pm.message_id,
        turnId: pm.turn_id,
      });
      this.logEvent("in", "message", `[user STT] ${pm.text ?? ""}`, pm);
      return;
    }

    if (role === "assistant" && pm.kind === "text") {
      const audioMs = realtimeDeltaAudioMs(pm);
      const uttId = realtimeDeltaUtteranceId(pm);

      const deltaText = pm.text ?? "";
      if (audioMs !== null && uttId >= 0 && deltaText.length > 0) {
        this.noteUtteranceId(uttId);
        if (this._speakingUtteranceId !== uttId) {
          if (this._speakingUtteranceId != null) {
            this.dispatchInterrupt({ type: "TURN_FINALIZED", utteranceId: this._speakingUtteranceId });
          }
          this.transitionFsm({ type: "AUDIO_META_ARRIVED", utteranceId: uttId });
        }

        this.emit("textDelta", {
          text: deltaText,
          isFinal: false,
          role: "assistant",
          messageId: pm.message_id,
          turnId: pm.turn_id,
        });

        // This turn's transcript is text-delta-fed: caption text must
        // not re-enter the bubble. And when authoritative word
        // timings have arrived via captions, the estimate seed below
        // must not pollute the exact table.
        this.rtTranscriptUtts.add(uttId);
        if (!this.authoritativeTimingUtts.has(uttId)) {
          this.recordCaptionFragment(uttId, { text: deltaText }, 0, audioMs);
        }
        this.logEvent("in", "message", `[agent rt-delta] uttId=${uttId} audio_ms=${audioMs} text="${deltaText.slice(0, 60)}"`, pm);
        return;
      }
    }

    if (role === "system") {
      this.emit("textDelta", {
        text: pm.text ?? pm.kind,
        isFinal: true,
        role: "system",
        messageId: pm.message_id ?? "sys-msg",
        turnId: pm.turn_id,
      });
      this.logEvent("in", "message", `[${role}] ${pm.text ?? pm.kind}`, pm);
      return;
    }

    // Structured kinds — tool activity (`tool_call` / `tool_result`),
    // client commands (`trigger_action`), cards — surface to the UI as
    // activity panels in the transcript.
    if (pm.kind && pm.kind !== "text") {
      this.emit("pipelineMessage", pm);
    }

    this.logEvent("in", "message", `[${role}] ${pm.text ?? pm.kind}`, pm);
  }

  /** Report the outcome of a client-side tool call (`trigger_action`
   *  command) back to the server. `started`/`progress` are telemetry;
   *  `completed`/`failed` resolve the call — the result is injected
   *  into the model's context, spoken only when the tool's
   *  `speak_on_result` is configured or `speak` is set here. */
  sendClientToolResult(
    callId: string,
    phase: "started" | "progress" | "completed" | "failed",
    result?: unknown,
    speak?: boolean,
  ): void {
    if (!this.transport) return;
    this.transport.sendControl({
      action: "client_tool_result",
      call_id: callId,
      phase,
      result,
      speak,
    });
    this.logEvent(
      "out",
      "control",
      `client_tool_result ${phase} (call ${callId}${speak ? ", speak" : ""})`,
      { call_id: callId, phase, result, speak },
    );
  }

  private handleControlMessage(msg: ServerMessage & { kind: "control" }): void {
    const ctrl = msg.control as ServerControl;
    if (!("action" in ctrl)) return;

    switch (ctrl.action) {
      case "state_changed":
        this.handleStateChangedControl(ctrl);
        break;
      case "quality_update":
        this.handleQualityUpdateControl(ctrl);
        break;
      case "session_ready":
        this.handleSessionReadyControl(ctrl);
        break;
      case "closing":
        this.handleClosingControl(ctrl);
        break;
      case "error":
        this.handleErrorControl(ctrl);
        break;
      case "turn_metrics":
        this.handleTurnMetricsControl(ctrl);
        break;
      case "text_truncated":
        this.handleTextTruncatedControl(ctrl);
        break;
      case "utterance_dropped":
        this.handleUtteranceDroppedControl(ctrl);
        break;
      case "clear_audio":
        this.handleClearAudioControl(ctrl);
        break;
      case "pong":
        this.handlePongControl(ctrl);
        break;
      default:
        this.logEvent("in", "control", ctrl.action, ctrl);
    }
  }

  private handlePongControl(ctrl: Extract<ServerControl, { action: "pong" }>): void {
    const perfNow = performance.now();
    const wallNow = Date.now();
    this.logEvent("in", "control", `pong (rtt=${this._rttMs}ms)`);
    if (this.lastPingSent > 0) {
      const rtt = perfNow - this.lastPingSent;
      const rttRounded = Math.round(rtt);
      this._rttMs = rttRounded;
      this.clockSync.addSample(this.lastPingSent, ctrl.server_timestamp_ms, perfNow, wallNow);
      this.transport?.recordPongSample?.(this.lastPingSent, ctrl.server_timestamp_ms, perfNow, wallNow);
      this.lastPingSent = 0;
    }
  }

  private handleStateChangedControl(ctrl: Extract<ServerControl, { action: "state_changed" }>): void {
    this.logEvent("in", "control", `state → ${ctrl.state}`, ctrl);
    this.transitionFsm({ type: "SERVER_STATE_CHANGED", serverState: ctrl.state });

    if (ctrl.state === "listening") {
      // A new user turn opens: re-arm the once-per-turn speech reports and
      // the E2E anchors. The legacy `listening` stamp stays as the fallback
      // anchor for turns where the client VAD stamps nothing.
      this.sentSpeechStart = false;
      this.micFrameSeen = false;
      this.speechEndReported = false;
      this.vadSpeechEndPerf = 0;
      // The server took the turn while the user is still talking (barge-in
      // or a very early `listening`): that segment's end belongs to this
      // user turn even if the server has moved on by then.
      this.speechBeganInUserTurn = this.clientVad?.inSpeech ?? false;
      this.speechEndTs = performance.now(); // legacy client turn anchor

      if ((this.playback?.getBufferStats().scheduledAheadMs ?? 0) > 0) {
        this.pendingListeningRotation = true;
      } else {
        this.resetFirstAudio();
        this._speakingUtteranceId = null;
      }
      this.speakingEntryIsLive = false;
    }
  }

  /** The server moved the fidelity tier.
   *
   *  Two things follow: deepen the jitter buffer, since a degraded link is
   *  late and uneven rather than merely slower; and tell the UI, so a user
   *  sees "poor connection" instead of being left to interpret stuttering
   *  audio. Fires on transitions only — the server does not send this per
   *  measurement. */
  private handleQualityUpdateControl(
    ctrl: Extract<ServerControl, { action: "quality_update" }>,
  ): void {
    // A server predating adaptive quality sends no tier; treat that as full
    // rather than guessing from the coarse ping bucket.
    const tier: QualityTier = ctrl.tier ?? "full";
    this.logEvent("in", "control", `quality ${ctrl.quality} (tier ${tier})`, ctrl);
    if (tier === this._qualityTier) return;
    this._qualityTier = tier;

    const depth = this.prebufferByTier[tier];
    if (depth !== undefined) this.playback?.setPrebufferMs(depth);

    this.emit("qualityTierChanged", tier, ctrl.quality);
  }

  /** The fidelity tier the server last reported. `"full"` unless adaptive
   *  quality is enabled server-side. */
  get qualityTier(): QualityTier {
    return this._qualityTier;
  }

  private handleSessionReadyControl(ctrl: Extract<ServerControl, { action: "session_ready" }>): void {
    this.logEvent("in", "control", "session ready", ctrl);
    if (ctrl.negotiated) {
      // The server is authoritative for the input rate: it may have lowered
      // our proposal to the STT's native rate. `startMic` captures at this
      // instead of whatever the caller asked for, which is the whole point of
      // the `accepts_negotiated_input_rate` opt-in we sent at connect.
      this.negotiatedInputRate = ctrl.negotiated.input_sample_rate ?? null;
      // The server states where its utterance numbering starts and which
      // range is typed text; predicting turn ids from anything else is how
      // speech reports ended up filed under the wrong turn.
      this.utteranceIds.configure(ctrl.negotiated);
      const outRate = ctrl.negotiated.output_sample_rate ?? 48000;
      this.transitionFsm({ type: "SESSION_READY", outputRate: outRate });
      this.emit("sessionReady", ctrl.negotiated);
    }
    this.setSessionState("active");
  }

  private handleClosingControl(ctrl: Extract<ServerControl, { action: "closing" }>): void {
    // Server announced a graceful shutdown: the socket close that follows is
    // expected, not an abnormal drop.
    this.expectedClose = true;
    this.logEvent("in", "control", "session closing", ctrl);
    this.setSessionState("idle");
    this.emit("sessionTerminated", ctrl.reason);
  }

  private handleErrorControl(ctrl: Extract<ServerControl, { action: "error" }>): void {
    this.logEvent("in", "error", `[${ctrl.severity ?? "error"}] ${ctrl.component ? ctrl.component + ": " : ""}${ctrl.message}`);
    this.emit("errorRaised", parseInt(ctrl.code) || 500, ctrl.message, ctrl.component);

    if (ctrl.severity === "fatal") {
      // The error is already surfaced; the socket close we trigger next is an
      // expected consequence, not a separate abnormal drop.
      this.expectedClose = true;
      this._errorMessage = ctrl.message;
      this.setSessionState("error");
      this.transport?.disconnect();
    }
  }

  private handleTurnMetricsControl(ctrl: Extract<ServerControl, { action: "turn_metrics" }>): void {
    const snap = ctrl.snapshot;
    this.noteUtteranceId(snap.utterance_id);
    // Client playback timings for THIS utterance, if its first chunk has
    // already been scheduled. Not cleared here: for short answers
    // `playbackMetrics` fires after `turn_metrics` (the jitter buffer still
    // holds the audio) and the hook merges it into the entry instead.
    const pm = this.lastPlaybackMetrics?.utteranceId === snap.utterance_id ? this.lastPlaybackMetrics : null;
    const client = { clientE2eMs: this.clientE2eMs, clientE2eSource: this.clientE2eSource, pm };
    this.logEvent("in", "control", describeTurnMetrics(snap, client), ctrl);

    const metricsEnvelope = buildTurnMetricsEnvelope(snap, this._rttMs, client);

    this.emit("metricsEmitted", snap.utterance_id.toString(), metricsEnvelope);
    this.clientE2eMs = undefined;
    this.clientE2eSource = undefined;
  }

  private handleTextTruncatedControl(ctrl: Extract<ServerControl, { action: "text_truncated" }>): void {
    this.logEvent("in", "control", `[barge-in] text_truncated utterance=${ctrl.utterance_id} final_text="${ctrl.final_text}"`, ctrl);
  }

  /** Speech that made no turn. The transcript gets a system line for it:
   *  the user spoke, the pipeline went back to listening, and without the
   *  line the Playground shows nothing at all — after a barge-in that reads
   *  as an agent that was cut off and then refused to answer. */
  private handleUtteranceDroppedControl(ctrl: Extract<ServerControl, { action: "utterance_dropped" }>): void {
    this.logEvent("in", "control", `utterance_dropped utterance=${ctrl.utterance_id} reason=${ctrl.reason} audio_ms=${ctrl.audio_ms}`, ctrl);
    this.emit("textDelta", {
      text: describeDroppedUtterance(ctrl.reason, ctrl.audio_ms),
      isFinal: true,
      role: "system",
      messageId: `dropped-${ctrl.utterance_id}`,
    });
  }

  private handleClearAudioControl(ctrl: Extract<ServerControl, { action: "clear_audio" }>): void {
    const clearAudioReceivedAt = performance.now();

    if (this._speakingUtteranceId !== null && this._speakingUtteranceId !== ctrl.utterance_id) {
      debug("[SDK Core barge-in] ignoring stale clear_audio for utterance=", ctrl.utterance_id, "current is", this._speakingUtteranceId);
      return;
    }

    this.logEvent("in", "control", `clear_audio (utterance=${ctrl.utterance_id}, reason=${ctrl.reason})`, ctrl);

    this.transitionFsm({ type: "CLEAR_AUDIO", utteranceId: ctrl.utterance_id });

    debug(`[SDK Core barge-in] flushed via FSM, elapsed=${(performance.now() - clearAudioReceivedAt).toFixed(1)}ms`);
  }

  // NTP / Speech metrics helpers
  /** Client E2E = end of the user's speech → first audio byte received.
   *  Anchor precedence: the client VAD's `speech_end` stamp for this turn,
   *  else the legacy `listening` transition (mic open), which overstates
   *  the wait by however long the user took to start talking. */
  private markFirstAudioReceived(): void {
    if (this.clientE2eMs !== undefined) return;
    const now = performance.now();
    if (this.vadSpeechEndPerf > 0) {
      this.clientE2eMs = Math.round(now - this.vadSpeechEndPerf);
      this.clientE2eSource = "client_vad";
    } else if (this.speechEndTs > 0) {
      this.clientE2eMs = Math.round(now - this.speechEndTs);
      this.clientE2eSource = "listening";
    }
  }

  // Record an audio fragment description to the word timing table
  private recordCaptionFragment(serverUttId: number, caption: CaptionMeta, fragmentDurationMs: number, anchorMs?: number): void {
    const table = this.getOrCreateWordTimingTable(serverUttId);
    table.appendCaption(caption, fragmentDurationMs, anchorMs);
  }

  // Fallback: build one fragment from full text using character timings estimator
  private recordFinalTextFallback(serverUttId: number, text: string, totalDurationMs: number): void {
    if (serverUttId < 0 || totalDurationMs <= 0 || !text) return;
    if (this.authoritativeTimingUtts.has(serverUttId)) return;
    let table = this.tables.get(serverUttId);
    if (table && table.wordCount > 0) return;
    table = this.getOrCreateWordTimingTable(serverUttId);
    const fragment = timingsForFragment({ text }, totalDurationMs);
    table.append(fragment, totalDurationMs);
  }

  // Helper to lazily lookup or initialize a WordTimingTable
  private getOrCreateWordTimingTable(serverUttId: number): WordTimingTable {
    let table = this.tables.get(serverUttId);
    if (!table) {
      table = new WordTimingTable();
      this.tables.set(serverUttId, table);
    }
    return table;
  }

  private getTrimWordIndex(utteranceId: number, playedMs: number): number | null {
    if (utteranceId < 0) return null;
    return trimWordIndex(this.tables.get(utteranceId), playedMs);
  }
}

/** The transcript line for speech the server dropped, by the reason it
 *  gives: no words found, too few, or a recogniser that never answered. */
export function describeDroppedUtterance(reason: string, audioMs: number): string {
  const heard = `${(Math.max(audioMs, 0) / 1000).toFixed(1)} s of speech`;
  switch (reason) {
    case "empty_transcript":
      return `Heard ${heard} but recognised no words`;
    case "too_short":
      return `Heard ${heard}, too short to be a turn`;
    case "stt_timeout":
      return `The recogniser did not answer ${heard} and was restarted`;
    default:
      return `Heard ${heard}; no turn was made (${reason})`;
  }
}
