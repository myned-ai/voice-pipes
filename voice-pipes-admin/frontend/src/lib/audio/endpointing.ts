// Client-side energy endpointer.
//
// A tiny, pure VAD state machine over per-frame RMS levels. It exists to
// *timestamp* the user's end of speech on the client — the anchor for the
// `timing_report speech_end` frame and the client-measured E2E — not to
// gate or segment audio. It deliberately mirrors the server-side observer
// (same hangover / min-speech semantics) so the two stamps are comparable.
//
//   Idle ──(rms ≥ enter)──▶ Speech ──(rms < exit)──▶ Trailing ──(hangover
//   elapsed)──▶ End (emits `speech_end`, back to Idle)
//
// `speech_start` is emitted once a candidate segment has lasted
// `minSpeechMs` (a shorter burst — a click, a cough — is dropped without
// any event). Speech resuming inside the hangover cancels Trailing (no
// `speech_end`). No DOM, no timers: the caller feeds `(rms, nowMs)`.

export interface EndpointingOptions {
  /** RMS (0..1) at or above which silence turns into speech. */
  enterThreshold?: number;
  /** RMS (0..1) below which speech turns into trailing silence — lower than
   *  `enterThreshold` so a level hovering around the threshold does not flap. */
  exitThreshold?: number;
  /** Silence needed after the last speech frame before `speech_end` fires. */
  hangoverMs?: number;
  /** Minimum segment length before it counts as speech (`speech_start`). */
  minSpeechMs?: number;
}

export const DEFAULT_ENDPOINTING: Required<EndpointingOptions> = {
  enterThreshold: 0.02,
  exitThreshold: 0.012,
  hangoverMs: 300,
  minSpeechMs: 150,
};

export type EndpointingEvent = "speech_start" | "speech_end";
export type EndpointingState = "idle" | "speech" | "trailing";

export class EnergyEndpointer {
  readonly options: Required<EndpointingOptions>;
  private _state: EndpointingState = "idle";
  /** True once the current segment passed `minSpeechMs` (a real utterance). */
  private confirmed = false;
  private segmentStartMs = 0;
  private lastSpeechMs = 0;

  constructor(options: EndpointingOptions = {}) {
    const enter = options.enterThreshold ?? DEFAULT_ENDPOINTING.enterThreshold;
    this.options = {
      enterThreshold: enter,
      // A misconfigured exit above enter would make the machine flap; clamp.
      exitThreshold: Math.min(enter, options.exitThreshold ?? DEFAULT_ENDPOINTING.exitThreshold),
      hangoverMs: Math.max(0, options.hangoverMs ?? DEFAULT_ENDPOINTING.hangoverMs),
      minSpeechMs: Math.max(0, options.minSpeechMs ?? DEFAULT_ENDPOINTING.minSpeechMs),
    };
  }

  get state(): EndpointingState {
    return this._state;
  }

  /** True while inside a confirmed utterance (speech or its hangover). */
  get inSpeech(): boolean {
    return this._state !== "idle" && this.confirmed;
  }

  /** Timestamp (caller's clock) of the last frame classified as speech in
   *  the current / most recent segment; 0 before any speech. */
  get lastSpeechAtMs(): number {
    return this.lastSpeechMs;
  }

  /** Feed one frame. `rms` is the frame's RMS level (0..1), `nowMs` the
   *  caller's monotonic clock. Returns the event this frame triggered. */
  feed(rms: number, nowMs: number): EndpointingEvent | null {
    const { enterThreshold, exitThreshold, hangoverMs, minSpeechMs } = this.options;
    switch (this._state) {
      case "idle":
        if (rms >= enterThreshold) {
          this._state = "speech";
          this.segmentStartMs = nowMs;
          this.lastSpeechMs = nowMs;
          this.confirmed = false;
          if (minSpeechMs === 0) {
            this.confirmed = true;
            return "speech_start";
          }
        }
        return null;

      case "speech":
        if (rms >= exitThreshold) {
          this.lastSpeechMs = nowMs;
          if (!this.confirmed && nowMs - this.segmentStartMs >= minSpeechMs) {
            this.confirmed = true;
            return "speech_start";
          }
          return null;
        }
        // Dropped below the exit threshold.
        if (!this.confirmed) {
          // Too short to be speech — a click. Forget it silently.
          this._state = "idle";
          return null;
        }
        this._state = "trailing";
        return null;

      case "trailing":
        if (rms >= enterThreshold) {
          // Speech resumed inside the hangover: same utterance, no end.
          this._state = "speech";
          this.lastSpeechMs = nowMs;
          return null;
        }
        if (nowMs - this.lastSpeechMs >= hangoverMs) {
          this._state = "idle";
          this.confirmed = false;
          return "speech_end";
        }
        return null;
    }
  }

  reset(): void {
    this._state = "idle";
    this.confirmed = false;
    this.segmentStartMs = 0;
    this.lastSpeechMs = 0;
  }
}
