import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { AudioPlaybackService } from "./playback";

// ── AudioContext / AudioBufferSourceNode mocks ─────────────

class MockAudioBufferSourceNode {
  buffer: unknown = null;
  onended: (() => void) | null = null;
  private _stopped = false;

  connect() {}
  disconnect() {}
  start() {}
  stop() {
    if (this._stopped) throw new DOMException("already stopped");
    this._stopped = true;
  }

  /** Simulate the source finishing naturally. */
  _fireEnded() {
    this.onended?.();
  }
}

class MockAudioBuffer {
  private data: Float32Array;
  readonly numberOfChannels = 1;
  readonly sampleRate: number;
  readonly duration: number;
  readonly length: number;

  constructor(_channels: number, length: number, sampleRate: number) {
    this.data = new Float32Array(length);
    this.sampleRate = sampleRate;
    this.duration = length / sampleRate;
    this.length = length;
  }

  getChannelData(): Float32Array {
    return this.data;
  }
}

let mockCurrentTime = 0;
let createdSources: MockAudioBufferSourceNode[] = [];

class MockAudioContext {
  readonly sampleRate: number;
  readonly destination = {};

  constructor(opts?: { sampleRate?: number }) {
    this.sampleRate = opts?.sampleRate ?? 48000;
  }

  get currentTime(): number {
    return mockCurrentTime;
  }

  createBuffer(channels: number, length: number, sampleRate: number) {
    return new MockAudioBuffer(channels, length, sampleRate);
  }

  createBufferSource() {
    const src = new MockAudioBufferSourceNode();
    createdSources.push(src);
    return src as unknown as AudioBufferSourceNode;
  }

  resume() {
    if (mockResumeRejection) return Promise.reject(mockResumeRejection);
    return Promise.resolve();
  }

  close() {
    if (mockCloseRejection) return Promise.reject(mockCloseRejection);
    return Promise.resolve();
  }
}

// Failure-injection knobs for AudioContext.resume() / close().  Set in tests
// to verify that promise rejections are surfaced via onError instead of being
// silently swallowed (regression guard for a silent autoplay-policy failure
// that left ctx.currentTime stuck at 0 and broke the karaoke clock).
let mockResumeRejection: unknown = null;
let mockCloseRejection: unknown = null;

// Replace the global AudioContext with our mock.
vi.stubGlobal("AudioContext", MockAudioContext);

// ── Tests ───────────────────────────────────────────────────

describe("AudioPlaybackService", () => {
  let svc: AudioPlaybackService;

  beforeEach(() => {
    mockCurrentTime = 0;
    createdSources = [];
    mockResumeRejection = null;
    mockCloseRejection = null;
    svc = new AudioPlaybackService(16000);
    svc.start();
    svc.beginUtterance(1);
  });

  function makeSamples(count: number, amplitude = 1000): Int16Array {
    const arr = new Int16Array(count);
    for (let i = 0; i < count; i++) arr[i] = amplitude;
    return arr;
  }

  describe("enqueue", () => {
    it("schedules audio and tracks source nodes", () => {
      svc.enqueue(makeSamples(1600)); // 100ms at 16kHz
      expect(createdSources.length).toBe(1);

      svc.enqueue(makeSamples(1600));
      expect(createdSources.length).toBe(2);
    });

    it("fires onPlaybackStart on the first chunk", () => {
      const cb = vi.fn();
      svc.onPlaybackStart = cb;

      svc.enqueue(makeSamples(1600));
      expect(cb).toHaveBeenCalledWith(1);

      svc.enqueue(makeSamples(1600));
      // Should only fire once
      expect(cb).toHaveBeenCalledTimes(1);
    });

    it("ignores empty samples", () => {
      svc.enqueue(new Int16Array(0));
      expect(createdSources.length).toBe(0);
    });
  });

  describe("prebuffer (jitter buffer)", () => {
    // 200 ms at 16 kHz = 3200 samples.
    function prebufferedSvc(): AudioPlaybackService {
      const s = new AudioPlaybackService(16000, { prebufferMs: 200 });
      s.start();
      s.beginUtterance(1);
      return s;
    }

    it("holds playback until the configured amount of audio is queued", () => {
      const s = prebufferedSvc();
      s.enqueue(makeSamples(1600)); // 100 ms — below threshold
      expect(createdSources.length).toBe(0);
      s.enqueue(makeSamples(1600)); // 200 ms total — threshold reached
      expect(createdSources.length).toBe(2);
    });

    it("fires onPlaybackStart only when the hold releases", () => {
      const s = prebufferedSvc();
      const cb = vi.fn();
      s.onPlaybackStart = cb;
      s.enqueue(makeSamples(1600));
      expect(cb).not.toHaveBeenCalled();
      s.enqueue(makeSamples(1600));
      expect(cb).toHaveBeenCalledWith(1);
    });

    it("releases immediately for a short utterance flagged end-of-utterance", () => {
      const s = prebufferedSvc();
      s.enqueue(makeSamples(800), {}, 1, true); // 50 ms, complete
      expect(createdSources.length).toBe(1);
    });

    it("releases when an empty end-of-utterance marker arrives", () => {
      const s = prebufferedSvc();
      s.enqueue(makeSamples(800)); // 50 ms — held
      expect(createdSources.length).toBe(0);
      s.enqueue(new Int16Array(0), {}, 2, true);
      expect(createdSources.length).toBe(1);
    });

    it("safety timeout releases a trickling hold", () => {
      vi.useFakeTimers();
      try {
        const s = prebufferedSvc();
        s.enqueue(makeSamples(800)); // 50 ms — held, timer armed
        expect(createdSources.length).toBe(0);
        vi.advanceTimersByTime(400); // 2 × prebufferMs
        expect(createdSources.length).toBe(1);
      } finally {
        vi.useRealTimers();
      }
    });

    it("re-arms after an underrun so playback resumes with a cushion", () => {
      const s = prebufferedSvc();
      s.enqueue(makeSamples(3200)); // 200 ms — releases, scheduled at t=0
      expect(createdSources.length).toBe(1);
      // Scheduled audio ends at t=0.2; the clock has moved well past it.
      mockCurrentTime = 0.5;
      s.enqueue(makeSamples(1600)); // 100 ms — held again (underrun re-arm)
      expect(createdSources.length).toBe(1);
      s.enqueue(makeSamples(1600)); // 200 ms buffered — resumes
      expect(createdSources.length).toBe(3);
    });

    it("beginUtterance re-arms the hold for the next turn", () => {
      const s = prebufferedSvc();
      s.enqueue(makeSamples(3200), {}, 1, true);
      expect(createdSources.length).toBe(1);
      s.beginUtterance(2);
      s.enqueue(makeSamples(1600)); // below threshold — held again
      expect(createdSources.length).toBe(1);
    });

    it("prebufferMs of 0 keeps immediate playback (legacy behavior)", () => {
      // The default service from beforeEach has no prebuffer.
      svc.enqueue(makeSamples(160)); // 10 ms
      expect(createdSources.length).toBe(1);
    });

    // The seam the adaptive-quality controller drives: a degraded link
    // wants a bigger cushion, not a smaller one.
    describe("setPrebufferMs", () => {
      it("raising it applies from the next utterance", () => {
        const s = prebufferedSvc();
        s.setPrebufferMs(400);
        expect(s.prebufferMs).toBe(400);
        s.enqueue(makeSamples(3200)); // 200 ms — below the new 400 ms target
        expect(createdSources.length).toBe(0);
        s.enqueue(makeSamples(3200)); // 400 ms total — releases
        expect(createdSources.length).toBe(2);
      });

      it("raising it never re-holds audio already flowing", () => {
        const s = prebufferedSvc();
        s.enqueue(makeSamples(3200)); // releases at the old 200 ms target
        expect(createdSources.length).toBe(1);
        s.setPrebufferMs(600);
        // Mid-utterance audio keeps flowing: re-holding here would insert the
        // very gap the buffer exists to prevent.
        s.enqueue(makeSamples(160));
        expect(createdSources.length).toBe(2);
      });

      it("lowering it below what is queued releases an active hold", () => {
        const s = prebufferedSvc();
        s.enqueue(makeSamples(1600)); // 100 ms — held at 200 ms
        expect(createdSources.length).toBe(0);
        s.setPrebufferMs(50); // already satisfied
        expect(createdSources.length).toBe(1);
      });

      it("lowering it to zero releases the hold entirely", () => {
        const s = prebufferedSvc();
        s.enqueue(makeSamples(800)); // 50 ms — held
        expect(createdSources.length).toBe(0);
        s.setPrebufferMs(0);
        expect(createdSources.length).toBe(1);
      });

      it("lowering it re-sizes the pending safety timeout", () => {
        vi.useFakeTimers();
        try {
          const s = prebufferedSvc();
          s.enqueue(makeSamples(160)); // 10 ms — held, timer armed at 2×200 ms
          expect(createdSources.length).toBe(0);
          s.setPrebufferMs(100); // still held (10 ms < 100 ms), timer re-armed
          expect(createdSources.length).toBe(0);
          // The old timer would have fired at 400 ms; the new one is
          // PREBUFFER_TIMEOUT_MIN_MS-floored, so it fires no later.
          vi.advanceTimersByTime(400);
          expect(createdSources.length).toBe(1);
        } finally {
          vi.useRealTimers();
        }
      });

      it("is a no-op when the value is unchanged", () => {
        const s = prebufferedSvc();
        s.enqueue(makeSamples(1600)); // 100 ms — held
        s.setPrebufferMs(200);
        expect(createdSources.length).toBe(0);
      });

      it("clamps negatives to zero", () => {
        const s = prebufferedSvc();
        s.setPrebufferMs(-100);
        expect(s.prebufferMs).toBe(0);
      });
    });
  });

  describe("flush", () => {
    it("never fires onPlaybackEnd — barge-in must not look like natural completion", () => {
      // The SDK reveals all remaining karaoke words on onPlaybackEnd
      // (natural end = everything scheduled was heard). A barge-in flush
      // must therefore NOT deliver that callback, or unspoken words would
      // light up. stopAllSources detaches onended before stopping.
      const ended: number[] = [];
      svc.onPlaybackEnd = (id) => ended.push(id);
      svc.enqueue(makeSamples(1600), {}, 1, true); // end-of-utterance chunk
      svc.flush();
      // Even if the stopped source's ended event still fires afterwards:
      createdSources[0]._fireEnded();
      expect(ended).toEqual([]);
    });

    it("stops all active source nodes", () => {
      svc.enqueue(makeSamples(1600));
      svc.enqueue(makeSamples(1600));
      svc.enqueue(makeSamples(1600));
      expect(createdSources.length).toBe(3);

      const stopSpies = createdSources.map((s) => vi.spyOn(s, "stop"));

      const playedMs = svc.flush();

      // All three sources should have been stopped
      for (const spy of stopSpies) {
        expect(spy).toHaveBeenCalledTimes(1);
      }

      // Returns a number
      expect(typeof playedMs).toBe("number");
    });

    it("does not throw if a source was already stopped", () => {
      svc.enqueue(makeSamples(1600));

      // Manually stop the first source (simulates natural end)
      createdSources[0]._fireEnded();

      // flush should not throw even if some sources are already stopped
      expect(() => svc.flush()).not.toThrow();
    });

    it("resets tracking state", () => {
      svc.enqueue(makeSamples(1600));
      expect(svc.lastEnqueuedSeqNum).toBe(0);
      svc.enqueue(makeSamples(1600), {}, 5);
      expect(svc.lastEnqueuedSeqNum).toBe(5);

      svc.flush();

      expect(svc.lastEnqueuedSeqNum).toBe(0);
      expect(svc.totalEnqueuedDurationMs).toBe(0);
      expect(svc.currentChunk).toBeNull();
      expect(svc.level).toBe(0);
    });

    it("allows new audio to be enqueued after flush", () => {
      svc.enqueue(makeSamples(1600));
      svc.flush();

      const prevCount = createdSources.length;

      svc.enqueue(makeSamples(1600));
      expect(createdSources.length).toBe(prevCount + 1);
    });

    it("does not recreate the AudioContext", () => {
      svc.enqueue(makeSamples(1600));
      const ctxBefore = svc.audioContext;

      svc.flush();

      // The context should be the same object (not recreated)
      expect(svc.audioContext).toBe(ctxBefore);
    });
  });

  describe("stop", () => {
    it("stops all sources and closes the context", () => {
      svc.enqueue(makeSamples(1600));
      svc.enqueue(makeSamples(1600));

      const stopSpies = createdSources.map((s) => vi.spyOn(s, "stop"));

      svc.stop();

      for (const spy of stopSpies) {
        expect(spy).toHaveBeenCalledTimes(1);
      }

      expect(svc.audioContext).toBeNull();
      expect(svc.playing).toBe(false);
    });
  });

  // ── Regression: AudioContext error surfacing ──────────────────────────────
  //
  // This was previously a silent failure: the AudioContext was suspended
  // (autoplay policy) and ctx.resume() was called as fire-and-forget (`void resume()`).
  // When resume() rejected, the rejection was swallowed and the entire
  // pipeline produced no audio while the UI reported "speaking".  These
  // tests guard the new onError contract so future regressions surface
  // instead of silently breaking playback.

  describe("AudioContext error surfacing", () => {
    it("invokes onError when ctx.resume() rejects", async () => {
      const errors: Array<{ err: unknown; context: string }> = [];
      // Build a fresh service so we control start() ourselves.
      const fresh = new AudioPlaybackService(16000);
      fresh.onError = (err, context) => errors.push({ err, context });

      const rejection = new Error("autoplay policy: no user gesture");
      mockResumeRejection = rejection;

      fresh.start();
      // Allow the rejected promise's catch handler to run.
      await Promise.resolve();
      await Promise.resolve();

      expect(errors).toHaveLength(1);
      expect(errors[0].context).toBe("audiocontext_resume");
      expect(errors[0].err).toBe(rejection);
    });

    it("invokes onError when ctx.close() rejects", async () => {
      const errors: Array<{ err: unknown; context: string }> = [];
      svc.onError = (err, context) => errors.push({ err, context });

      const rejection = new Error("close failed");
      mockCloseRejection = rejection;

      svc.stop();
      await Promise.resolve();
      await Promise.resolve();

      expect(errors).toHaveLength(1);
      expect(errors[0].context).toBe("audiocontext_close");
      expect(errors[0].err).toBe(rejection);
    });

    it("does NOT throw or interrupt start() when resume() rejects", () => {
      mockResumeRejection = new Error("rejected");
      const fresh = new AudioPlaybackService(16000);
      // start() must complete synchronously even if resume() will reject async.
      expect(() => fresh.start()).not.toThrow();
      expect(fresh.playing).toBe(true);
    });
  });

  describe("source lifecycle", () => {
    it("removes finished sources from tracking", () => {
      svc.enqueue(makeSamples(1600));
      svc.enqueue(makeSamples(1600));

      // Simulate first source finishing naturally
      createdSources[0]._fireEnded();

      // Now flush — should only have 1 source to stop (second one)
      const stopSpy1 = vi.spyOn(createdSources[1], "stop");
      svc.flush();
      expect(stopSpy1).toHaveBeenCalledTimes(1);
    });
  });

  // ── Regression: beginUtterance ordering ──────────────────────────────────
  //
  // Bug: beginUtterance() was called *after* enqueue() in handleAudioMessage.
  // enqueue() sets utteranceStartTime correctly inside drainPendingQueue, but
  // beginUtterance() immediately resets it to -1 (the sentinel).  getPlayedDurationMs()
  // guards on utteranceStartTime < 0 and returns 0, so the PlaybackClock always
  // reported nowMs = 0, wordsHeardAtMs(0) = 0, highlightInfo = null, and the
  // StreamingAgentBubble rendered <TypingDots> forever — last word never shown.
  //
  // Fix: call beginUtterance() *before* enqueue() so utteranceStartTime is
  // set by drainPendingQueue and never overwritten.
  //
  // See: voice-pipes-admin/frontend/src/lib/audio/REGRESSION-NOTES.md

  describe("beginUtterance ordering regression", () => {
    it("CORRECT ORDER: beginUtterance before enqueue preserves utteranceStartTime", () => {
      // Advance mock clock so startAt > 0 — confirms we're measuring audio time.
      mockCurrentTime = 1.0; // 1 second into AudioContext time

      svc.beginUtterance(42);                  // ← BEFORE enqueue (correct)
      svc.enqueue(makeSamples(1600));           // 100 ms at 16 kHz

      // utteranceStartTime must equal the AudioContext time at scheduling (≥ 1.0 s).
      expect(svc.utteranceStartTime).toBeGreaterThan(0);

      // Advance mock clock past the first chunk so getPlayedDurationMs returns > 0.
      mockCurrentTime = 1.05; // 50 ms into playback
      expect(svc.getPlayedDurationMs()).toBeGreaterThan(0);
    });

    it("WRONG ORDER (documents the bug): beginUtterance after enqueue resets utteranceStartTime to 0", () => {
      // This test documents the broken ordering so any future refactor that
      // moves the call back after enqueue will immediately fail with a clear signal.
      mockCurrentTime = 1.0;

      // Replicate the old (broken) call order:
      svc.enqueue(makeSamples(1600));           // ← BEFORE beginUtterance (broken)
      // utteranceStartTime is set correctly here by drainPendingQueue…
      expect(svc.utteranceStartTime).toBeGreaterThan(0); // temporarily correct

      svc.beginUtterance(42);                  // ← AFTER enqueue (broken — resets to -1)
      // …but beginUtterance() clobbers it.
      expect(svc.utteranceStartTime).toBe(-1);  // ← the bug: now permanently -1

      mockCurrentTime = 1.05;
      // getPlayedDurationMs guards on utteranceStartTime < 0 → always 0
      expect(svc.getPlayedDurationMs()).toBe(0); // ← broken: clock never advances
    });

    it("CORRECT ORDER: totalEnqueuedDurationMs after beginUtterance is 0 for first chunk anchor", () => {
      // preEnqueueMs must be 0 for the first fragment (so the WordTimingTable
      // anchors the first fragment at offset 0, not some stale leftover value).
      // beginUtterance resets totalEnqueuedDurationMs to 0, so capturing
      // preEnqueueMs *after* beginUtterance (but *before* enqueue) is always correct.
      mockCurrentTime = 0;

      svc.beginUtterance(1);
      const preEnqueueMs = svc.totalEnqueuedDurationMs; // capture after beginUtterance
      expect(preEnqueueMs).toBe(0);

      svc.enqueue(makeSamples(1600)); // 100 ms
      // After the first chunk, totalEnqueuedDurationMs advances.
      expect(svc.totalEnqueuedDurationMs).toBeGreaterThan(0);

      // Second chunk: preEnqueueMs should now equal the first chunk's duration.
      const preEnqueue2Ms = svc.totalEnqueuedDurationMs;
      svc.enqueue(makeSamples(1600));
      expect(svc.totalEnqueuedDurationMs).toBeGreaterThan(preEnqueue2Ms);
    });
  });

  // ── Regression: stop()/start() race + onended-after-stop hazard ──────────
  //
  // Two hazards this guards against:
  //   1. AudioContext.close() is async; rapid stop()→start() could leave two
  //      contexts alive briefly.  isClosing must report the in-flight close.
  //   2. AudioBufferSourceNode.onended fires asynchronously after stop().
  //      If we don't null it out first, a flush followed by a new utterance
  //      can deliver an `ended` event with a stale `currentUtteranceId`,
  //      causing onPlaybackEnd to fire for the wrong id.

  describe("AudioContext close/open lifecycle", () => {
    it("isClosing is true while close() is pending and clears once settled", async () => {
      svc.enqueue(makeSamples(1600));
      expect(svc.isClosing).toBe(false);

      svc.stop();
      expect(svc.isClosing).toBe(true);

      await svc.awaitClose();
      expect(svc.isClosing).toBe(false);
    });

    it("isClosing remains true when close() rejects, then clears", async () => {
      mockCloseRejection = new Error("device busy");
      svc.onError = () => {}; // swallow for assertion clarity
      svc.stop();
      expect(svc.isClosing).toBe(true);

      await svc.awaitClose();
      expect(svc.isClosing).toBe(false);
    });

    it("start() after stop() succeeds even before close() settles", async () => {
      svc.enqueue(makeSamples(1600));
      svc.stop();
      // Immediately start a new context — must not throw, must produce a fresh ctx.
      svc.start();
      expect(svc.audioContext).not.toBeNull();
      expect(svc.playing).toBe(true);
      // Pending close still drains in the background.
      await svc.awaitClose();
      expect(svc.isClosing).toBe(false);
    });
  });

  describe("stopAllSources detaches onended before stopping", () => {
    it("a late onended after flush does NOT fire onPlaybackEnd", () => {
      const endSpy = vi.fn();
      svc.onPlaybackEnd = endSpy;

      // Enqueue a chunk marked end-of-utterance.
      svc.enqueue(makeSamples(1600), {}, 0, true);
      const src = createdSources[createdSources.length - 1];

      // Flush — stopAllSources must clear src.onended so a late `ended` event
      // (the browser may dispatch it asynchronously after src.stop()) cannot
      // resurrect the now-stale onPlaybackEnd callback.
      svc.flush();

      // Simulate the late ended event the browser would deliver.
      src._fireEnded();

      expect(endSpy).not.toHaveBeenCalled();
    });

    it("a late onended after stop() does NOT fire onPlaybackEnd", () => {
      const endSpy = vi.fn();
      svc.onPlaybackEnd = endSpy;

      svc.enqueue(makeSamples(1600), {}, 0, true);
      const src = createdSources[createdSources.length - 1];

      svc.stop();
      src._fireEnded();

      expect(endSpy).not.toHaveBeenCalled();
    });
  });

  describe("getNextChunkStartOffsetMs (anchor for word-timing fragments)", () => {
    it("returns 0 before the first chunk is scheduled", () => {
      // utteranceStartTime = -1 sentinel — must return 0 even though the
      // ctx clock has been ticking.
      mockCurrentTime = 5;
      expect(svc.getNextChunkStartOffsetMs()).toBe(0);
    });

    it("returns the first chunk's duration after one chunk schedules without gaps", () => {
      // 16000 sample-rate × 1600 samples = 100 ms chunk.
      svc.enqueue(makeSamples(1600));
      // No gap — next chunk would start at nextStartTime = 0 + 0.1s.
      // utteranceStartTime captured at startAt of first chunk = 0.
      expect(svc.getNextChunkStartOffsetMs()).toBe(100);
    });

    it("INCLUDES real-world underrun gaps (the bug-fix invariant)", () => {
      // First chunk schedules at ctx.currentTime = 0 → utteranceStartTime = 0,
      // nextStartTime = 0.1s.
      svc.enqueue(makeSamples(1600));
      expect(svc.getNextChunkStartOffsetMs()).toBe(100);

      // Simulate: chunk 1 fully plays, then a 2 s gap with no PCM arriving.
      // ctx.currentTime jumps to 2.1s while nextStartTime is still 0.1s.
      mockCurrentTime = 2.1;

      // Next chunk's actual playback start = max(currentTime, nextStartTime)
      //   = max(2.1, 0.1) = 2.1 s → offset from utteranceStartTime = 2100 ms.
      // (Whereas totalEnqueuedDurationMs would still report only 100 — that's
      // the under-report that caused karaoke to race ahead of audio.)
      expect(svc.getNextChunkStartOffsetMs()).toBe(2100);
      expect(svc.totalEnqueuedDurationMs).toBe(100);
    });

    it("returns 0 after beginUtterance resets state", () => {
      svc.enqueue(makeSamples(1600));
      expect(svc.getNextChunkStartOffsetMs()).toBe(100);

      svc.beginUtterance(2);
      // utteranceStartTime back to -1, totalEnqueuedDurationMs = 0.
      expect(svc.getNextChunkStartOffsetMs()).toBe(0);
    });
  });

  describe("playback quality counters", () => {
    it("accumulates gaps with ideal-timeline positions when the clock overtakes the schedule", () => {
      // Chunk 1: 100 ms, schedules at t=0 → nextStartTime = 0.1.
      svc.enqueue(makeSamples(1600));
      // Underrun: clock jumps to 0.4 (300 ms dry) before chunk 2 arrives.
      mockCurrentTime = 0.4;
      svc.enqueue(makeSamples(1600)); // gap at ideal 100 ms, 300 ms silence
      // Second underrun: schedule now ends at 0.5; clock jumps to 0.75.
      mockCurrentTime = 0.75;
      svc.enqueue(makeSamples(1600)); // gap at ideal 200 ms, 250 ms silence

      const q = svc.getPlaybackQuality(1);
      expect(q.utteranceId).toBe(1);
      expect(q.underrunCount).toBe(2);
      expect(q.gaps).toEqual([
        { atAudioMs: 100, silenceMs: 300 },
        { atAudioMs: 200, silenceMs: 250 },
      ]);
      expect(q.totalSilenceMs).toBe(550);
      expect(q.audioMs).toBe(300);
      // wall = last scheduled end (0.75 + 0.1 = 0.85) − utterance start (0) = 850 ms
      // = audio (300) + silence (550).
      expect(q.wallMs).toBe(850);
      expect(q.wallMs).toBe(q.audioMs + q.totalSilenceMs);
    });

    it("does not count the initial prebuffer hold as an underrun", () => {
      const s = new AudioPlaybackService(16000, { prebufferMs: 200 });
      s.start();
      s.beginUtterance(7);
      // Prebuffer holds the first chunks; clock advances while nothing is scheduled.
      mockCurrentTime = 1.0;
      s.enqueue(makeSamples(1600));
      s.enqueue(makeSamples(1600)); // 200 ms reached → flush at t=1.0
      const q = s.getPlaybackQuality(7);
      expect(q.underrunCount).toBe(0);
      expect(q.gaps).toEqual([]);
      s.stop();
    });

    it("ignores sub-5ms scheduling jitter", () => {
      svc.enqueue(makeSamples(1600));
      mockCurrentTime = 0.104; // 4 ms past the schedule — jitter, not a break
      svc.enqueue(makeSamples(1600));
      expect(svc.getPlaybackQuality(1).underrunCount).toBe(0);
    });

    it("resets gap state on beginUtterance", () => {
      svc.enqueue(makeSamples(1600));
      mockCurrentTime = 0.4;
      svc.enqueue(makeSamples(1600));
      expect(svc.getPlaybackQuality(1).underrunCount).toBe(1);

      svc.beginUtterance(2);
      const q = svc.getPlaybackQuality(2);
      expect(q.underrunCount).toBe(0);
      expect(q.audioMs).toBe(0);
      expect(q.gaps).toEqual([]);
    });

    // Regression (2026-09-03): the session FSM re-runs ROTATE_BUBBLE_TO_KARAOKE
    // — and with it beginUtterance(sameId) — when any audio/meta frame lands
    // after the server's early `listening` control. Realtime providers push
    // the whole response at well under real time, so that is the normal
    // case, and the reset wiped the counters the natural-end
    // playback_quality frame is built from (0 of 31 completed S2S turns ever
    // reported one) and dropped chunks still waiting to be scheduled.
    it("beginUtterance for the utterance already playing keeps counters, gaps and queued chunks", () => {
      svc.enqueue(makeSamples(1600)); // 100 ms
      mockCurrentTime = 0.4;
      svc.enqueue(makeSamples(1600)); // underrun: gap at 100 ms, 300 ms silence
      const before = svc.getPlaybackQuality(1);
      expect(before.audioMs).toBe(200);
      expect(before.underrunCount).toBe(1);

      svc.beginUtterance(1); // re-entry for the same utterance

      const after = svc.getPlaybackQuality(1);
      expect(after.audioMs).toBe(200);
      expect(after.underrunCount).toBe(1);
      expect(after.gaps).toEqual(before.gaps);
      // A genuinely new utterance still resets.
      svc.beginUtterance(2);
      expect(svc.getPlaybackQuality(2).audioMs).toBe(0);
    });

    it("beginUtterance re-entry does not discard chunks held back by the scheduler", () => {
      const s = new AudioPlaybackService(16000, { limitScheduledAudio: true, maxScheduledChunks: 1 });
      s.start();
      s.beginUtterance(5);
      s.enqueue(makeSamples(1600)); // scheduled
      s.enqueue(makeSamples(1600)); // held in pendingChunks by the cap
      expect(s.getBufferStats().pendingChunks).toBe(1);

      s.beginUtterance(5);
      expect(s.getBufferStats().pendingChunks).toBe(1);
      s.stop();
    });

    it("beginUtterance after a flush of the same utterance starts clean", () => {
      svc.enqueue(makeSamples(1600));
      svc.flush();
      svc.beginUtterance(1);
      expect(svc.getPlaybackQuality(1).audioMs).toBe(0);
    });

    it("returns the core-owned fields as neutral defaults", () => {
      svc.enqueue(makeSamples(1600));
      const q = svc.getPlaybackQuality(1);
      expect(q.interrupted).toBe(false);
      expect(q.annotationLateCount).toBe(0);
      expect(q.annotationLateMsMax).toBe(0);
    });
  });

  // `arrivalWallMs` is the wall-clock span
  // between the first and the last audio frame *received* — stamped with
  // performance.now() in enqueue(), independent of the AudioContext clock — so
  // `arrivalWallMs / audioMs` (arrival RTF) tells whether the server delivered
  // audio slower than it plays.
  describe("arrival span", () => {
    let perfNow: ReturnType<typeof vi.spyOn>;

    beforeEach(() => {
      perfNow = vi.spyOn(performance, "now").mockReturnValue(0);
    });

    afterEach(() => {
      perfNow.mockRestore();
    });

    it("spans first → last received frame in wall time, independent of the AudioContext clock", () => {
      // Three 100 ms chunks delivered 250 ms apart: 300 ms of audio took
      // 500 ms to arrive → arrival RTF 1.67 (server slower than realtime).
      perfNow.mockReturnValue(1000);
      svc.enqueue(makeSamples(1600));
      perfNow.mockReturnValue(1250);
      svc.enqueue(makeSamples(1600));
      perfNow.mockReturnValue(1500);
      svc.enqueue(makeSamples(1600));
      const q = svc.getPlaybackQuality(1);
      expect(q.audioMs).toBe(300);
      expect(q.arrivalWallMs).toBe(500);
      // The AudioContext clock never moved, so the scheduled wall span is
      // just the audio — the two measure different things.
      expect(q.wallMs).toBe(300);
    });

    it("is 0 for a single frame and before any audio arrived", () => {
      expect(svc.getArrivalWallMs()).toBe(0);
      perfNow.mockReturnValue(1000);
      svc.enqueue(makeSamples(1600));
      expect(svc.getPlaybackQuality(1).arrivalWallMs).toBe(0);
    });

    it("ignores empty end-of-utterance markers (they are not audio frames)", () => {
      perfNow.mockReturnValue(1000);
      svc.enqueue(makeSamples(1600));
      perfNow.mockReturnValue(1100);
      svc.enqueue(makeSamples(1600));
      // The server's tail marker arrives much later — must not stretch the span.
      perfNow.mockReturnValue(4000);
      svc.enqueue(new Int16Array(0), {}, 3, true);
      expect(svc.getPlaybackQuality(1).arrivalWallMs).toBe(100);
    });

    it("stamps arrival even while the prebuffer holds scheduling", () => {
      const s = new AudioPlaybackService(16000, { prebufferMs: 200 });
      s.start();
      s.beginUtterance(7);
      perfNow.mockReturnValue(10);
      s.enqueue(makeSamples(800)); // 50 ms — held, nothing scheduled yet
      expect(createdSources.length).toBe(0);
      perfNow.mockReturnValue(90);
      s.enqueue(makeSamples(800)); // still held
      expect(s.getArrivalWallMs()).toBe(80);
      s.stop();
    });

    it("resets on beginUtterance and on flush", () => {
      perfNow.mockReturnValue(1000);
      svc.enqueue(makeSamples(1600));
      perfNow.mockReturnValue(1400);
      svc.enqueue(makeSamples(1600));
      expect(svc.getArrivalWallMs()).toBe(400);

      svc.beginUtterance(2);
      expect(svc.getArrivalWallMs()).toBe(0);
      perfNow.mockReturnValue(2000);
      svc.enqueue(makeSamples(1600));
      perfNow.mockReturnValue(2050);
      svc.enqueue(makeSamples(1600));
      // Next turn measures only its own frames.
      expect(svc.getArrivalWallMs()).toBe(50);

      svc.flush();
      expect(svc.getArrivalWallMs()).toBe(0);
    });
  });
});
