// Audio playback service — queues and plays PCM i16 chunks from the server.

import type { AudioMeta, PlaybackGap, PlaybackQuality } from "../transport/types";
import { debug } from "../debug";

export interface PlaybackSchedulingOptions {
  limitScheduledAudio?: boolean;
  maxScheduledChunks?: number;
  /** Jitter buffer: hold the start of each utterance (and each restart
   *  after an underrun) until this much audio is queued, so playback
   *  keeps a cushion over the delivery head instead of chasing it.
   *  Providers that pace delivery at ~1× realtime (ElevenLabs agents,
   *  slow local TTS) otherwise underrun audibly on the first delivery
   *  hiccup — the bursting providers (OpenAI/Gemini) build a cushion
   *  naturally and are unaffected by this hold in practice. 0 = play
   *  each first chunk immediately (legacy behavior). A safety timeout
   *  (2× prebufferMs, min 250 ms) releases the hold even if delivery
   *  stalls below the threshold, and an end-of-utterance flag releases
   *  it immediately, so short replies are never delayed. */
  prebufferMs?: number;
}

/** Floor for the prebuffer-release safety timeout (ms). */
const PREBUFFER_TIMEOUT_MIN_MS = 250;

interface PendingPlaybackChunk {
  samples: Int16Array;
  meta: AudioMeta;
  seqNum: number;
  endOfResponse: boolean;
}

/** A queued audio chunk with its associated metadata. */
export interface PlaybackChunk {
  samples: Int16Array;
  meta: AudioMeta;
  /** Scheduled AudioContext time when this chunk starts playing. */
  startTime: number;
  /** Scheduled AudioContext time when this chunk finishes. */
  endTime: number;
}

export class AudioPlaybackService {
  private ctx: AudioContext | null = null;
  private nextStartTime = 0;
  private sampleRate: number;
  private _playing = false;
  private scheduling: Required<PlaybackSchedulingOptions>;
  /** Active AudioBufferSourceNodes — tracked so flush() can stop them immediately. */
  private activeSources: Set<AudioBufferSourceNode> = new Set();
  private pendingChunks: PendingPlaybackChunk[] = [];
  private pendingEndOfResponse = false;
  /** True while scheduling is held back to accumulate the jitter
   *  buffer (see {@link PlaybackSchedulingOptions.prebufferMs}). */
  private prebufferHold = false;
  /** Safety-timeout handle for the prebuffer hold. */
  private prebufferTimer: ReturnType<typeof setTimeout> | null = null;
  /** In-flight `AudioContext.close()` promise, or `null` when no close is
   *  pending.  `stop()` is synchronous from the caller's perspective but the
   *  underlying `close()` is async; rapid `stop()` → `start()` would otherwise
   *  briefly hold two AudioContexts.  Exposed via {@link isClosing} so callers
   *  / tests can detect the window. */
  private closingPromise: Promise<void> | null = null;

  /** Rolling RMS level 0..1 for visualisation. */
  level = 0;

  /** The most recently scheduled chunk — UI can read `.meta` for blendshapes, etc. */
  currentChunk: PlaybackChunk | null = null;

  /** Optional callback fired for every chunk right before it is scheduled.
   *  Use this to drive avatar blendshape animations in sync with audio. */
  onChunk: ((chunk: PlaybackChunk) => void) | null = null;

  // ── Playback position tracking ──────────────────────────

  /** Current utterance id being played (set externally). */
  currentUtteranceId = -1;

  /** Sequence number of the last audio chunk enqueued for this utterance. */
  lastEnqueuedSeqNum = 0;

  /** Total duration (ms) of audio enqueued for the current utterance. */
  totalEnqueuedDurationMs = 0;

  /** AudioContext time when the first chunk of the current utterance was scheduled.
   *  -1 means the utterance has not been scheduled yet (sentinel — distinct from 0,
   *  which is a valid AudioContext timestamp when the context was freshly created). */
  utteranceStartTime = -1;

  /** End time of the last scheduled AudioBufferSourceNode for the current utterance. */
  lastScheduledEndTime = 0;

  /** Whether we've fired onPlaybackStart for the current utterance. */
  private firedPlaybackStart = false;

  /** Underruns of the current utterance, in ideal-audio coordinates.
   *  Reset in beginUtterance. */
  private _gaps: PlaybackGap[] = [];

  /** `performance.now()` of the first / last non-empty audio frame handed to
   *  `enqueue()` for the current utterance. -1 = no frame received yet.
   *  Wall-clock, independent of the
   *  AudioContext clock, so `lastArrivalPerf − firstArrivalPerf` measures how
   *  fast the server *delivered* the audio, not how it was scheduled. */
  private firstArrivalPerf = -1;
  private lastArrivalPerf = -1;

  /** Callback: fires when the first chunk of a new utterance starts playing. */
  onPlaybackStart: ((utteranceId: number) => void) | null = null;

  /** Callback: fires when the last chunk of an utterance finishes playing. */
  onPlaybackEnd: ((utteranceId: number) => void) | null = null;

  /** Callback: fires when an asynchronous AudioContext operation rejects.
   *  Most importantly, this surfaces autoplay-policy failures (Safari/iOS:
   *  resume() rejects without a recent user gesture).  Without this, those
   *  failures are silent and the entire session produces no audio while the
   *  UI happily reports "speaking".  `context` is a short tag identifying the
   *  failing operation (e.g. "audiocontext_resume", "audiocontext_close"). */
  onError: ((error: unknown, context: string) => void) | null = null;

  constructor(sampleRate = 48000, scheduling: PlaybackSchedulingOptions = {}) {
    this.sampleRate = sampleRate;
    this.scheduling = {
      limitScheduledAudio: scheduling.limitScheduledAudio ?? false,
      maxScheduledChunks: Math.max(1, scheduling.maxScheduledChunks ?? 2),
      prebufferMs: Math.max(0, scheduling.prebufferMs ?? 0),
    };
  }

  get playing(): boolean {
    return this._playing;
  }

  /** True while a previous `stop()`'s `AudioContext.close()` promise is still
   *  pending.  Useful for tests and for callers that want to defer a `start()`
   *  until the previous context has fully released its hardware resources. */
  get isClosing(): boolean {
    return this.closingPromise !== null;
  }

  /** Resolves when any in-flight `close()` settles.  Resolves immediately if
   *  no close is pending. */
  awaitClose(): Promise<void> {
    return this.closingPromise ?? Promise.resolve();
  }

  /** Get the AudioContext (if active) — needed by CaptionSync. */
  get audioContext(): AudioContext | null {
    return this.ctx;
  }

  start(): void {
    if (this.ctx) return;
    this.ctx = new AudioContext({ sampleRate: this.sampleRate });
    // Browsers may create the context in "suspended" state due to autoplay
    // policy.  Resume it so scheduled buffers actually reach the speakers.
    // Failures here (most commonly Safari/iOS autoplay rejection without a
    // user gesture) MUST be surfaced — a silent rejection leaves the context
    // suspended forever, ctx.currentTime stuck at 0, and the entire pipeline
    // produces no audio while reporting "speaking".
    this.ctx.resume().catch((err) => {
      console.error("[playback] AudioContext.resume() rejected:", err);
      this.onError?.(err, "audiocontext_resume");
    });
    this.nextStartTime = 0;
    this._playing = true;
    this.drainPendingQueue();
  }

  stop(): void {
    this.stopAllSources();
    if (this.ctx) {
      // close() is asynchronous; capture the promise so callers can detect
      // that a previous context is still closing (see {@link isClosing}).
      // Surface failures so the host can react.
      const closingCtx = this.ctx;
      const promise = closingCtx.close().catch((err) => {
        console.error("[playback] AudioContext.close() rejected:", err);
        this.onError?.(err, "audiocontext_close");
      });
      // Only clear closingPromise if no NEW close has been started in the
      // meantime (would happen if stop() is called twice in a row).
      this.closingPromise = promise;
      void promise.finally(() => {
        if (this.closingPromise === promise) {
          this.closingPromise = null;
        }
      });
      this.ctx = null;
    }
    this.nextStartTime = 0;
    this._playing = false;
    this.level = 0;
    this.currentChunk = null;
    this.pendingChunks = [];
    this.pendingEndOfResponse = false;
    this.prebufferHold = false;
    this.clearPrebufferTimer();
    this.resetUtteranceTracking();
  }

  /** Begin tracking a new utterance. Call when a new turn starts. */
  beginUtterance(utteranceId: number): void {
    // Re-entry for the utterance that is already playing is a no-op. The
    // session FSM rotates back into `speaking` whenever an audio or
    // audio-meta frame arrives after the server has already said
    // `listening` — which, with realtime providers delivering audio at
    // well under real time, is the normal case: the whole response plus
    // the end-of-utterance sentinel and the `listening` control land while
    // seconds of audio are still queued here. Resetting on that re-entry
    // zeroed `totalEnqueuedDurationMs` and `_gaps` (so the playback_quality
    // frame built at the natural end reported no audio and was skipped —
    // 0 of 31 completed S2S turns ever produced one, 2026-09-03) and threw
    // away `pendingChunks` that had not been scheduled yet. A genuinely new
    // turn always carries a new id; the flush path resets the counters
    // itself before any legitimate same-id restart.
    if (
      utteranceId === this.currentUtteranceId &&
      (this.totalEnqueuedDurationMs > 0 || this.pendingChunks.length > 0 || this.activeSources.size > 0)
    ) {
      return;
    }
    this.currentUtteranceId = utteranceId;
    this.lastEnqueuedSeqNum = 0;
    this.totalEnqueuedDurationMs = 0;
    this.utteranceStartTime = -1;
    this.lastScheduledEndTime = 0;
    this.firedPlaybackStart = false;
    this._gaps = [];
    this.firstArrivalPerf = -1;
    this.lastArrivalPerf = -1;
    this.pendingEndOfResponse = false;
    this.pendingChunks = [];
    this.armPrebuffer();
  }

  // ── Jitter buffer (prebuffer) ───────────────────────────

  /** Current pre-buffer target in ms. */
  get prebufferMs(): number {
    return this.scheduling.prebufferMs;
  }

  /**
   * Change the pre-buffer target on a live service.
   *
   * The seam the adaptive-quality controller drives: when the link degrades,
   * the right response to late, uneven arrival is *more* buffer, not less.
   *
   * Takes effect from the next utterance. The one exception is lowering it
   * while a hold is active: the new, smaller target may already be satisfied,
   * so the hold is re-evaluated immediately and the queue drained rather than
   * holding audio the caller no longer wants held. Raising it never re-holds
   * audio that is already flowing — that would insert a gap mid-utterance,
   * which is the artefact the buffer exists to prevent.
   */
  setPrebufferMs(ms: number): void {
    const next = Math.max(0, ms);
    if (next === this.scheduling.prebufferMs) return;
    const lowered = next < this.scheduling.prebufferMs;
    this.scheduling.prebufferMs = next;
    if (!this.prebufferHold) return;
    if (next === 0) {
      // No hold is possible any more; release and let queued audio through.
      this.prebufferHold = false;
      this.clearPrebufferTimer();
      this.drainPendingQueue();
    } else if (lowered) {
      // A pending timeout was sized against the old, larger target.
      this.clearPrebufferTimer();
      if (!this.prebufferBlocks()) this.drainPendingQueue();
    }
  }

  private armPrebuffer(): void {
    this.clearPrebufferTimer();
    this.prebufferHold = this.scheduling.prebufferMs > 0;
  }

  private clearPrebufferTimer(): void {
    if (this.prebufferTimer !== null) {
      clearTimeout(this.prebufferTimer);
      this.prebufferTimer = null;
    }
  }

  private pendingDurationMs(): number {
    let samples = 0;
    for (const c of this.pendingChunks) samples += c.samples.length;
    return (samples / this.sampleRate) * 1000;
  }

  /** Whether the prebuffer hold currently blocks scheduling. Releases
   *  when enough audio is queued, when the utterance is already
   *  complete (no more audio is coming), or via the safety timeout. */
  private prebufferBlocks(): boolean {
    if (!this.prebufferHold) return false;
    const complete =
      this.pendingEndOfResponse || this.pendingChunks.some((c) => c.endOfResponse);
    if (complete || this.pendingDurationMs() >= this.scheduling.prebufferMs) {
      this.prebufferHold = false;
      this.clearPrebufferTimer();
      return false;
    }
    // Delivery is trickling below the threshold — make sure a timeout
    // eventually releases the hold so a stalled provider can't mute us.
    if (this.prebufferTimer === null && this.pendingChunks.length > 0) {
      const timeoutMs = Math.max(PREBUFFER_TIMEOUT_MIN_MS, this.scheduling.prebufferMs * 2);
      this.prebufferTimer = setTimeout(() => {
        this.prebufferTimer = null;
        this.prebufferHold = false;
        this.drainPendingQueue();
      }, timeoutMs);
    }
    return true;
  }

  /** Compute how many ms of audio were actually played before a flush. */
  getPlayedDurationMs(): number {
    if (!this.ctx || this.utteranceStartTime < 0) return 0;
    const now = this.ctx.currentTime;
    // Clamp to lastScheduledEndTime to avoid counting silence after underrun
    const effectiveNow = Math.min(now, this.lastScheduledEndTime);
    const playedSec = Math.max(0, effectiveNow - this.utteranceStartTime);
    return Math.round(playedSec * 1000);
  }

  /** Ms of audio actually *heard*, bounded by the audio that was enqueued.
   *
   *  `getPlayedDurationMs()` is wall-clock based (`ctx.currentTime` since the
   *  utterance start) and can over-count when the AudioContext clock advanced
   *  past the audio that was actually scheduled — buffered-ahead or silent
   *  underrun time. A `PlaybackReport` sent to the server must never claim the
   *  user heard more audio than was ever enqueued to the output, or the server
   *  truncates conversation history to text that was never spoken.
   *  Report paths use this clamped value; the raw wall-clock accessor is left
   *  intact for the caption/word-timing clock, which wants elapsed time. */
  getHeardDurationMs(): number {
    const played = this.getPlayedDurationMs();
    return this.totalEnqueuedDurationMs > 0
      ? Math.min(played, this.totalEnqueuedDurationMs)
      : played;
  }

  /** Wall ms from the first to the last audio frame *received* for the
   *  current utterance (0 until two frames have arrived). See
   *  {@link PlaybackQuality.arrivalWallMs}. */
  getArrivalWallMs(): number {
    if (this.firstArrivalPerf < 0 || this.lastArrivalPerf < 0) return 0;
    return Math.max(0, Math.round(this.lastArrivalPerf - this.firstArrivalPerf));
  }

  /** Per-turn playback quality: the accumulated underrun gap list plus
   *  ideal/heard/wall aggregates. Read at playback-complete (or right BEFORE
   *  `flush()` on an interruption — flush
   *  resets the counters), before the next beginUtterance resets the state.
   *
   *  `interrupted` and the annotation-lateness counters are owned by the
   *  session core (it knows about the flush path and the annotation stream);
   *  they are returned as their neutral defaults here. */
  getPlaybackQuality(utteranceId: number): PlaybackQuality {
    const audioMs = Math.round(this.totalEnqueuedDurationMs);
    const heardMs = Math.round(this.getHeardDurationMs());
    const wallMs = this.utteranceStartTime >= 0 && this.ctx
      ? Math.round((this.lastScheduledEndTime - this.utteranceStartTime) * 1000)
      : audioMs;
    const totalSilenceMs = this._gaps.reduce((s, g) => s + g.silenceMs, 0);
    return {
      utteranceId,
      underrunCount: this._gaps.length,
      totalSilenceMs,
      audioMs,
      heardMs,
      wallMs,
      arrivalWallMs: this.getArrivalWallMs(),
      interrupted: false,
      annotationLateCount: 0,
      annotationLateMsMax: 0,
      gaps: [...this._gaps],
    };
  }

  /** Offset (ms, from `utteranceStartTime`) at which the *next* chunk to be
   *  scheduled would actually begin playing.  This is the right anchor for
   *  word-timing fragments: it accounts for real-world gaps where the
   *  scheduler had to wait for `ctx.currentTime` to catch up because the
   *  previous chunk had finished and no new PCM had arrived yet.
   *
   *  `totalEnqueuedDurationMs` only sums chunk durations and therefore
   *  under-reports the next chunk's start when underruns occur — feeding it
   *  to the WordTimingTable causes the karaoke marker to race ahead of audio
   *  by exactly the underrun gap.
   *
   *  Before the first chunk is scheduled (`utteranceStartTime < 0`) we
   *  return 0 so the first fragment is anchored at the utterance origin. */
  getNextChunkStartOffsetMs(): number {
    if (!this.ctx) return 0;
    if (this.utteranceStartTime < 0) return 0;
    const startAt = Math.max(this.ctx.currentTime, this.nextStartTime);
    return Math.max(0, Math.round((startAt - this.utteranceStartTime) * 1000));
  }

  /** Enqueue a PCM i16 chunk (with metadata) for gapless playback. */
  enqueue(samples: Int16Array, meta: AudioMeta = {}, seqNum = 0, endOfUtterance = false): void {
    if (!this.ctx) return;

    if (samples.length === 0) {
      if (endOfUtterance) {
        this.pendingEndOfResponse = true;
        // Drain (not just maybeEmitPlaybackEnd): a prebuffer hold below
        // its threshold must release now — no more audio is coming.
        this.drainPendingQueue();
      }
      return;
    }

    // Arrival stamps (wall clock): first → last audio frame received. Only
    // frames that carry samples count — an empty end-of-utterance marker is
    // not audio and would otherwise stretch the span by the server's tail.
    const arrivedAt = performance.now();
    if (this.firstArrivalPerf < 0) this.firstArrivalPerf = arrivedAt;
    this.lastArrivalPerf = arrivedAt;

    // Convert i16 to float32
    const float32 = new Float32Array(samples.length);
    for (let i = 0; i < samples.length; i++) {
      float32[i] = samples[i] / 32768;
    }

    // Compute RMS for visualiser
    let sum = 0;
    for (let i = 0; i < float32.length; i++) {
      sum += float32[i] * float32[i];
    }
    this.level = Math.sqrt(sum / float32.length);

    this.pendingChunks.push({ samples, meta, seqNum, endOfResponse: endOfUtterance });
    this.drainPendingQueue();
  }

  /** Inspect current browser playback buffering. */
  getBufferStats(): { scheduledChunks: number; pendingChunks: number; scheduledAheadMs: number } {
    const scheduledAheadMs = this.ctx
      ? Math.max(0, this.nextStartTime - this.ctx.currentTime) * 1000
      : 0;
    return {
      scheduledChunks: this.activeSources.size,
      pendingChunks: this.pendingChunks.length,
      scheduledAheadMs: Math.round(scheduledAheadMs),
    };
  }

  private canScheduleMore(): boolean {
    return !this.scheduling.limitScheduledAudio || this.activeSources.size < this.scheduling.maxScheduledChunks;
  }

  private drainPendingQueue(): void {
    // Underrun: the scheduled audio ran out mid-utterance while more
    // chunks are still arriving. Re-arm the jitter buffer so we resume
    // with a cushion instead of chasing the delivery head again (which
    // would turn every jitter spike into another audible break).
    if (
      !this.prebufferHold &&
      this.scheduling.prebufferMs > 0 &&
      this.firedPlaybackStart &&
      this.ctx &&
      this.pendingChunks.length > 0 &&
      this.ctx.currentTime > this.nextStartTime + 0.005
    ) {
      this.armPrebuffer();
    }
    if (this.prebufferBlocks()) return;
    while (this.ctx && this.pendingChunks.length > 0 && this.canScheduleMore()) {
      const next = this.pendingChunks.shift();
      if (!next) break;
      this.scheduleChunk(next);
    }
    this.maybeEmitPlaybackEnd();
  }

  private scheduleChunk(chunkInput: PendingPlaybackChunk): void {
    if (!this.ctx) return;
    const ctx = this.ctx;

    // Create audio buffer and schedule
    const float32 = new Float32Array(chunkInput.samples.length);
    for (let i = 0; i < chunkInput.samples.length; i++) {
      float32[i] = chunkInput.samples[i] / 32768;
    }

    const buffer = ctx.createBuffer(1, float32.length, this.sampleRate);
    buffer.getChannelData(0).set(float32);

    const source = ctx.createBufferSource();
    source.buffer = buffer;
    source.connect(ctx.destination);

    const now = ctx.currentTime;
    const startAt = Math.max(now, this.nextStartTime);
    // Underrun detection: scheduled audio ran dry when the clock overtook the
    // schedule. Gate on firedPlaybackStart so the initial prebuffer hold is
    // not counted. Position the gap in the IDEAL timeline (pre-increment of
    // totalEnqueuedDurationMs below).
    const gapSec = startAt - this.nextStartTime;
    if (this.firedPlaybackStart && gapSec > 0.005) {
      this._gaps.push({
        atAudioMs: Math.round(this.totalEnqueuedDurationMs),
        silenceMs: Math.round(gapSec * 1000),
      });
    }
    source.start(startAt);
    this.activeSources.add(source);
    this.nextStartTime = startAt + buffer.duration;

    const chunkDurationMs = (chunkInput.samples.length / this.sampleRate) * 1000;

    // Track playback position
    this.lastEnqueuedSeqNum = chunkInput.seqNum;
    this.totalEnqueuedDurationMs += chunkDurationMs;
    this.lastScheduledEndTime = startAt + buffer.duration;

    if (!this.firedPlaybackStart) {
      this.firedPlaybackStart = true;
      this.utteranceStartTime = startAt;
      this.onPlaybackStart?.(this.currentUtteranceId);
    }

    // Build chunk record with timing info
    const chunk: PlaybackChunk = {
      samples: chunkInput.samples,
      meta: chunkInput.meta,
      startTime: startAt,
      endTime: startAt + buffer.duration,
    };
    this.currentChunk = chunk;
    this.onChunk?.(chunk);

    // Track playback end via onended on the last chunk if flagged as end-of-response.
    // The caller should mark this on the appropriate chunk.
    const uttId = this.currentUtteranceId;
    source.onended = () => {
      this.activeSources.delete(source);
      if (chunkInput.endOfResponse && this.currentUtteranceId === uttId) {
        this.onPlaybackEnd?.(uttId);
      }
      this.drainPendingQueue();
    };
  }

  private maybeEmitPlaybackEnd(): void {
    if (!this.pendingEndOfResponse) return;
    if (this.activeSources.size > 0 || this.pendingChunks.length > 0) return;
    this.pendingEndOfResponse = false;
    this.onPlaybackEnd?.(this.currentUtteranceId);
  }

  /** Flush queued audio (e.g. on barge-in). Returns ms of audio actually heard
   *  (clamped to enqueued audio — see {@link getHeardDurationMs}). This value
   *  is reported to the server for history truncation, so it must not exceed
   *  what was played out. */
  flush(): number {
    const playedMs = this.getHeardDurationMs();
    const sourceCount = this.activeSources.size;
    const pendingCount = this.pendingChunks.length;
    // Immediately stop all scheduled AudioBufferSourceNodes — this silences
    // playback synchronously, unlike AudioContext.close() which is async.
    this.stopAllSources();
    this.pendingChunks = [];
    this.pendingEndOfResponse = false;
    this.prebufferHold = false;
    this.clearPrebufferTimer();
    debug(`[playback] flush: stopped ${sourceCount} scheduled sources, dropped ${pendingCount} pending chunks, playedMs=${playedMs}`);
    if (this.ctx) {
      this.nextStartTime = this.ctx.currentTime;
      this.level = 0;
      this.currentChunk = null;
    }
    this.resetUtteranceTracking();
    return playedMs;
  }

  /** Stop all tracked AudioBufferSourceNodes immediately. */
  private stopAllSources(): void {
    for (const src of this.activeSources) {
      // CRITICAL: detach onended BEFORE stop() so the handler can't fire with
      // a stale `currentUtteranceId` after a flush/reset.  `src.stop()` queues
      // an `ended` event that would otherwise be delivered asynchronously —
      // by the time it fires, `currentUtteranceId` may already point at the
      // next utterance, causing onPlaybackEnd to be emitted for the wrong id.
      src.onended = null;
      try {
        src.stop();
      } catch {
        // already stopped
      }
      try {
        src.disconnect();
      } catch {
        // already disconnected
      }
    }
    this.activeSources.clear();
  }

  private resetUtteranceTracking(): void {
    this.lastEnqueuedSeqNum = 0;
    this.totalEnqueuedDurationMs = 0;
    this.utteranceStartTime = -1;
    this.lastScheduledEndTime = 0;
    this.firedPlaybackStart = false;
    this.firstArrivalPerf = -1;
    this.lastArrivalPerf = -1;
  }
}
