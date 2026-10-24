// PlaybackClock — single source of truth for "where are we in audio playback?".
//
// All time math reads from `audioContext.currentTime` (the audio hardware
// clock), never `performance.now()` and never `setTimeout` for word timing.
// Subscribers receive snapshots at a cadence driven by `requestAnimationFrame`
// when the tab is visible, and a `setTimeout` fallback when the tab is hidden
// (browsers throttle RAF in background tabs to ~1 Hz).
//
// The clock is intentionally a *small* interface: any implementation that
// can provide `getNowMs()` and `subscribe()` can stand in for tests, for
// future WebRTC track-mode playback, or for an offline replay tool.
//
// Pure TypeScript — no React, no transport knowledge.

/** Snapshot of the playback position at a single moment. */
export interface ClockSnapshot {
  /** Server-assigned utterance ID currently being played, or `null` when
   *  no utterance is active. */
  utteranceId: number | null;
  /** Playback offset in milliseconds, measured from the start of the
   *  current utterance.  Capped at `lastScheduledEndMs` so it never
   *  advances past the end of scheduled audio (which would otherwise
   *  happen during underrun). */
  nowMs: number;
  /** Total duration scheduled so far (ms): `nowMs + scheduledAheadMs`. */
  totalMs: number;
  /** Whether playback is currently advancing. */
  isRunning: boolean;
  /** How many milliseconds of audio are scheduled ahead of `nowMs`.  When
   *  this hits 0 while `isRunning`, the clock has likely underrun. */
  scheduledAheadMs: number;
}

/** Subscriber callback. */
export type ClockSubscriber = (snapshot: ClockSnapshot) => void;

/** The minimal interface consumed by `useSynchronizedSpeech` and any other
 *  component that needs to follow playback progress.  Implementations may
 *  be backed by `AudioContext.currentTime`, by `<audio>.currentTime`, by an
 *  RTP timestamp from a `MediaStreamTrack`, or by a scripted timeline in
 *  tests. */
export interface PlaybackClock {
  /** Current playback offset in ms. */
  getNowMs(): number;
  /** Total scheduled duration (ms): `nowMs + scheduledAheadMs`. */
  getTotalMs(): number;
  /** Server-assigned utterance ID, or `null` when none is active. */
  getUtteranceId(): number | null;
  /** Whether playback is currently advancing. */
  isRunning(): boolean;
  /** Subscribe to snapshot updates.  Returns an unsubscribe function. */
  subscribe(cb: ClockSubscriber): () => void;
}

// ── Driver: pluggable tick source ──────────────────────────────────────────

/** Schedules a callback to fire at a steady cadence.  Returns a stop fn. */
export type TickDriver = (cb: () => void) => () => void;

/** Default driver: `requestAnimationFrame` while visible, `setTimeout(33)`
 *  while hidden.  Switches automatically on `visibilitychange` and emits one
 *  immediate tick on resume so subscribers can catch up in a single batch. */
export function createDefaultTickDriver(): TickDriver {
  return (cb: () => void) => {
    let stopped = false;
    let rafId: number | null = null;
    let timeoutId: ReturnType<typeof setTimeout> | null = null;

    const isHidden = () =>
      typeof document !== "undefined" && document.visibilityState === "hidden";

    const scheduleNext = () => {
      if (stopped) return;
      if (isHidden()) {
        timeoutId = setTimeout(() => {
          if (stopped) return;
          cb();
          scheduleNext();
        }, 33);
      } else {
        rafId = requestAnimationFrame(() => {
          if (stopped) return;
          cb();
          scheduleNext();
        });
      }
    };

    const onVisibilityChange = () => {
      // Cancel any in-flight schedule and restart with the appropriate driver.
      if (rafId != null) cancelAnimationFrame(rafId);
      if (timeoutId != null) clearTimeout(timeoutId);
      rafId = null;
      timeoutId = null;
      // Emit one immediate snapshot so subscribers catch up after throttling.
      cb();
      scheduleNext();
    };

    if (typeof document !== "undefined") {
      document.addEventListener("visibilitychange", onVisibilityChange);
    }

    scheduleNext();

    return () => {
      stopped = true;
      if (rafId != null) cancelAnimationFrame(rafId);
      if (timeoutId != null) clearTimeout(timeoutId);
      if (typeof document !== "undefined") {
        document.removeEventListener("visibilitychange", onVisibilityChange);
      }
    };
  };
}

// ── Source: the upstream "where am I" provider ─────────────────────────────

/** A `PlaybackSource` is the thing that *knows* the current playback offset.
 *  Adapters around `AudioPlaybackService`, `<audio>` elements, or
 *  `MediaStreamTrack`s implement this interface; the clock just polls it. */
export interface PlaybackSource {
  getNowMs(): number;
  getTotalMs(): number;
  getUtteranceId(): number | null;
  isRunning(): boolean;
  getScheduledAheadMs(): number;
}

// ── The default clock implementation ───────────────────────────────────────

export interface DefaultPlaybackClockOptions {
  source: PlaybackSource;
  /** Override the tick driver (use a manual driver in tests). */
  driver?: TickDriver;
}

/** A `PlaybackClock` backed by a `PlaybackSource` and a `TickDriver`.
 *
 *  The clock starts its driver lazily when the first subscriber attaches
 *  and stops it when the last subscriber detaches, so an idle clock costs
 *  zero CPU.  Snapshots are cheap value objects; subscribers receive them
 *  on every tick (typically 60 Hz when visible). */
export class DefaultPlaybackClock implements PlaybackClock {
  private readonly source: PlaybackSource;
  private readonly driver: TickDriver;
  private readonly subscribers = new Set<ClockSubscriber>();
  private stopDriver: (() => void) | null = null;

  constructor(opts: DefaultPlaybackClockOptions) {
    this.source = opts.source;
    this.driver = opts.driver ?? createDefaultTickDriver();
  }

  getNowMs(): number {
    return this.source.getNowMs();
  }

  getTotalMs(): number {
    return this.source.getTotalMs();
  }

  getUtteranceId(): number | null {
    return this.source.getUtteranceId();
  }

  isRunning(): boolean {
    return this.source.isRunning();
  }

  subscribe(cb: ClockSubscriber): () => void {
    this.subscribers.add(cb);
    if (this.subscribers.size === 1) {
      this.stopDriver = this.driver(() => this.tick());
    }
    // Emit one immediate snapshot so the new subscriber renders correct
    // state on the next paint without waiting a frame.
    cb(this.snapshot());

    return () => {
      this.subscribers.delete(cb);
      if (this.subscribers.size === 0 && this.stopDriver) {
        this.stopDriver();
        this.stopDriver = null;
      }
    };
  }

  /** Take a snapshot from the underlying source. */
  snapshot(): ClockSnapshot {
    return {
      utteranceId: this.source.getUtteranceId(),
      nowMs: this.source.getNowMs(),
      totalMs: this.source.getTotalMs(),
      isRunning: this.source.isRunning(),
      scheduledAheadMs: this.source.getScheduledAheadMs(),
    };
  }

  private tick(): void {
    if (this.subscribers.size === 0) return;
    const snap = this.snapshot();
    for (const cb of this.subscribers) cb(snap);
  }
}

// ── Manual driver (for tests) ──────────────────────────────────────────────

/** Returns a `{ driver, advance }` pair where `advance()` triggers exactly
 *  one tick.  Use in tests to drive the clock deterministically. */
export function createManualTickDriver(): {
  driver: TickDriver;
  advance: () => void;
} {
  let active: (() => void) | null = null;
  const driver: TickDriver = (cb) => {
    active = cb;
    return () => {
      if (active === cb) active = null;
    };
  };
  return {
    driver,
    advance: () => {
      if (active) active();
    },
  };
}
