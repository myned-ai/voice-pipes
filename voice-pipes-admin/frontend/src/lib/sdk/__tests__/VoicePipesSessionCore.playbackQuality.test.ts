import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";

// Exactly one `playback_quality` frame per turn, emitted at
// SEND_PLAYBACK_COMPLETE, skipped for empty turns.
//
// The same frame is ALSO emitted from the flush path (barge-in
// FREEZE_BUBBLE_AT_POSITION, cancelAgent, flushLocalAudio,
// sendText interruption) with `interrupted = true` — read BEFORE
// `playback.flush()` resets the counters — under the same dedup, carries the
// annotation-lateness counters, and yields `clientArrivalRtf` on
// `playbackMetrics`.

const h = vi.hoisted(() => ({ fake: null as unknown as ReturnType<typeof makeFakeTransport> }));

vi.mock("../../transport", async (importActual) => {
  const actual = await importActual<typeof import("../../transport")>();
  return { ...actual, createTransport: () => h.fake };
});

import { VoicePipesSessionCore, type ClientPlaybackMetrics } from "../VoicePipesSessionCore";
import type { AudioAnnotation, PlaybackQuality } from "../../transport/types";
import type { PlaybackClock } from "../../audio/playback-clock";

function makeFakeTransport() {
  return {
    _state: "disconnected" as "disconnected" | "connecting" | "connected",
    onMessage: null as null | ((msg: unknown) => void),
    onStateChange: null as null | ((state: unknown, info?: unknown) => void),
    onError: null as null | ((err: Error) => void),
    get state() {
      return this._state;
    },
    async connect() {
      this._state = "connected";
      this.onStateChange?.("connected");
    },
    disconnect() {
      this._state = "disconnected";
      this.onStateChange?.("disconnected");
    },
    sendAudio() {},
    sendText() {},
    sendControl: vi.fn(),
    sendMessage() {},
    sendTimingReport: vi.fn(),
    sendPlaybackReport: vi.fn(),
    sendPlaybackComplete: vi.fn(),
    sendPlaybackQuality: vi.fn(),
  };
}

describe("VoicePipesSessionCore — playback_quality emission", () => {
  let core: VoicePipesSessionCore;
  let playbackMetrics: ClientPlaybackMetrics[];
  let events: { dir: string; type: string; summary: string }[];

  beforeEach(async () => {
    h.fake = makeFakeTransport();
    core = new VoicePipesSessionCore();
    await core.connect("ws://test:8800/ws", "websocket");
    playbackMetrics = [];
    events = [];
    core.on("playbackMetrics", (m) => playbackMetrics.push(m));
    core.on("eventsEmitted", (dir, type, summary) => events.push({ dir, type, summary }));
  });

  afterEach(() => {
    core.destroy();
  });

  /** Fake playback service. Like the real one, `flush()` resets the counters
   *  the quality frame is built from (audioMs → 0), so a frame read after
   *  the flush would be empty and skipped. */
  function stubQuality(q: Partial<PlaybackQuality>, opts: { scheduledAheadMs?: number } = {}) {
    let audioMs = q.audioMs ?? 0;
    const flush = vi.fn(() => {
      audioMs = 0;
      return 0;
    });
    core["playback"] = {
      getPlaybackQuality: (utteranceId: number): PlaybackQuality => ({
        utteranceId,
        underrunCount: 0,
        totalSilenceMs: 0,
        heardMs: 0,
        wallMs: 0,
        arrivalWallMs: 0,
        interrupted: false,
        annotationLateCount: 0,
        annotationLateMsMax: 0,
        gaps: [],
        ...q,
        audioMs,
      }),
      flush,
      stop: () => {},
      getBufferStats: () => ({ scheduledAheadMs: opts.scheduledAheadMs ?? 0, scheduledChunks: 0, pendingChunks: 0 }),
      getHeardDurationMs: () => 0,
      get totalEnqueuedDurationMs() {
        return audioMs;
      },
    } as never;
    return { flush };
  }

  function sentFrames(): PlaybackQuality[] {
    return h.fake.sendPlaybackQuality.mock.calls.map((c) => c[0] as PlaybackQuality);
  }

  it("emits once, with the playback service's quality object, on SEND_PLAYBACK_COMPLETE", () => {
    stubQuality({ audioMs: 300, underrunCount: 2, totalSilenceMs: 550, wallMs: 850, arrivalWallMs: 200, gaps: [{ atAudioMs: 100, silenceMs: 300 }, { atAudioMs: 200, silenceMs: 250 }] });
    core["executeFsmAction"]({ type: "SEND_PLAYBACK_COMPLETE", utteranceId: 4 });
    expect(h.fake.sendPlaybackQuality).toHaveBeenCalledTimes(1);
    const sent = sentFrames()[0];
    expect(sent.utteranceId).toBe(4);
    expect(sent.underrunCount).toBe(2);
    expect(sent.gaps).toHaveLength(2);
    expect(sent.arrivalWallMs).toBe(200);
    expect(sent.interrupted).toBe(false);
    expect(sent.annotationLateCount).toBe(0);
    expect(sent.annotationLateMsMax).toBe(0);
    // Same utterance again (duplicate FSM edge) → no second frame.
    core["executeFsmAction"]({ type: "SEND_PLAYBACK_COMPLETE", utteranceId: 4 });
    expect(h.fake.sendPlaybackQuality).toHaveBeenCalledTimes(1);
    // Next utterance → emits again.
    core["executeFsmAction"]({ type: "SEND_PLAYBACK_COMPLETE", utteranceId: 5 });
    expect(h.fake.sendPlaybackQuality).toHaveBeenCalledTimes(2);
  });

  it("skips empty turns (audioMs === 0)", () => {
    stubQuality({ audioMs: 0 });
    core["executeFsmAction"]({ type: "SEND_PLAYBACK_COMPLETE", utteranceId: 9 });
    expect(h.fake.sendPlaybackQuality).not.toHaveBeenCalled();
    expect(h.fake.sendPlaybackComplete).toHaveBeenCalledWith(9);
    expect(playbackMetrics).toHaveLength(0);
  });

  // ── flush path ──

  it("barge-in (FREEZE_BUBBLE_AT_POSITION): emits with interrupted=true BEFORE flush, and the later SEND_PLAYBACK_COMPLETE does not re-send", () => {
    const { flush } = stubQuality({ audioMs: 1200, arrivalWallMs: 2000, underrunCount: 3, totalSilenceMs: 900 });
    core["executeFsmAction"]({ type: "FREEZE_BUBBLE_AT_POSITION", utteranceId: 4 });

    expect(h.fake.sendPlaybackQuality).toHaveBeenCalledTimes(1);
    expect(flush).toHaveBeenCalledTimes(1);
    // The frame was read before flush() zeroed the counters …
    const sent = sentFrames()[0];
    expect(sent).toMatchObject({ utteranceId: 4, interrupted: true, audioMs: 1200, arrivalWallMs: 2000, underrunCount: 3 });
    // … and was sent before flush ran.
    expect(h.fake.sendPlaybackQuality.mock.invocationCallOrder[0]).toBeLessThan(flush.mock.invocationCallOrder[0]);

    // Dedup: the FSM may still reach SEND_PLAYBACK_COMPLETE for the flushed turn.
    core["executeFsmAction"]({ type: "SEND_PLAYBACK_COMPLETE", utteranceId: 4 });
    expect(h.fake.sendPlaybackQuality).toHaveBeenCalledTimes(1);

    // The next turn emits again.
    stubQuality({ audioMs: 500, arrivalWallMs: 100 });
    core["executeFsmAction"]({ type: "FREEZE_BUBBLE_AT_POSITION", utteranceId: 5 });
    expect(h.fake.sendPlaybackQuality).toHaveBeenCalledTimes(2);
    expect(sentFrames()[1]).toMatchObject({ utteranceId: 5, interrupted: true });
  });

  it("cancelAgent (explicit stop): emits interrupted=true for the speaking utterance before flushing", () => {
    const { flush } = stubQuality({ audioMs: 800, arrivalWallMs: 1600 });
    core["_speakingUtteranceId"] = 6;
    core["_pipelineState"] = "speaking";
    core.cancelAgent();
    expect(h.fake.sendControl).toHaveBeenCalledWith({ action: "cancel" });
    expect(h.fake.sendPlaybackQuality).toHaveBeenCalledTimes(1);
    expect(sentFrames()[0]).toMatchObject({ utteranceId: 6, interrupted: true, audioMs: 800, arrivalWallMs: 1600 });
    expect(h.fake.sendPlaybackQuality.mock.invocationCallOrder[0]).toBeLessThan(flush.mock.invocationCallOrder[0]);
    expect(h.fake.sendPlaybackComplete).toHaveBeenCalledWith(6);
  });

  it("flushLocalAudio (navigate away): emits interrupted=true; nothing when no utterance is speaking", () => {
    stubQuality({ audioMs: 800, arrivalWallMs: 400 });
    core["_speakingUtteranceId"] = null;
    core.flushLocalAudio();
    expect(h.fake.sendPlaybackQuality).not.toHaveBeenCalled();

    stubQuality({ audioMs: 800, arrivalWallMs: 400 });
    core["_speakingUtteranceId"] = 7;
    core.flushLocalAudio();
    expect(h.fake.sendPlaybackQuality).toHaveBeenCalledTimes(1);
    expect(sentFrames()[0]).toMatchObject({ utteranceId: 7, interrupted: true });
  });

  it("sendText while the agent is speaking: emits interrupted=true with the pre-text cancel", () => {
    if (typeof crypto.randomUUID !== "function") {
      Object.defineProperty(crypto, "randomUUID", { configurable: true, value: () => "00000000-0000-4000-8000-000000000000" });
    }
    const { flush } = stubQuality({ audioMs: 1000, arrivalWallMs: 500 }, { scheduledAheadMs: 300 });
    core["_speakingUtteranceId"] = 8;
    core["_pipelineState"] = "speaking";
    core.sendText("hello");
    expect(h.fake.sendControl).toHaveBeenCalledWith({ action: "cancel" });
    expect(h.fake.sendPlaybackQuality).toHaveBeenCalledTimes(1);
    expect(sentFrames()[0]).toMatchObject({ utteranceId: 8, interrupted: true, audioMs: 1000 });
    expect(h.fake.sendPlaybackQuality.mock.invocationCallOrder[0]).toBeLessThan(flush.mock.invocationCallOrder[0]);
  });

  it("flush of a turn that received no audio sends nothing (and leaves the dedup free for its natural end)", () => {
    stubQuality({ audioMs: 0 });
    core["executeFsmAction"]({ type: "FREEZE_BUBBLE_AT_POSITION", utteranceId: 4 });
    expect(h.fake.sendPlaybackQuality).not.toHaveBeenCalled();
    expect(core["lastQualityUtteranceId"]).toBeNull();
  });

  // ── annotation lateness ──────────────────────────────────────────────────

  function fakeClock(utteranceId: number | null, nowMs: number): PlaybackClock {
    return {
      getUtteranceId: () => utteranceId,
      getNowMs: () => nowMs,
      getTotalMs: () => nowMs + 500,
      isRunning: () => true,
      subscribe: () => () => {},
    };
  }

  function annotation(utteranceId: number, startMs: number): AudioAnnotation {
    return { utteranceId, startMs, durationMs: 160, kind: "blendshapes", extras: { fps: 30, frames: [[0.1]] } };
  }

  it("carries the per-utterance annotation lateness counters (late = clock.nowMs − startMs, > 0 only)", () => {
    stubQuality({ audioMs: 2000, arrivalWallMs: 1000 });
    core["playbackClock_"] = fakeClock(4, 500);
    core["handleAnnotationMessage"](annotation(4, 300)); // 200 ms late
    core["handleAnnotationMessage"](annotation(4, 450)); // 50 ms late
    core["handleAnnotationMessage"](annotation(4, 500)); // exactly on time — not late
    core["handleAnnotationMessage"](annotation(4, 900)); // ahead of playback — not late
    core["handleAnnotationMessage"](annotation(5, 0));   // clock is not on utterance 5 — never late

    core["executeFsmAction"]({ type: "SEND_PLAYBACK_COMPLETE", utteranceId: 4 });
    expect(sentFrames()[0]).toMatchObject({ utteranceId: 4, annotationLateCount: 2, annotationLateMsMax: 200 });

    // The counters belong to utterance 4 only.
    core["executeFsmAction"]({ type: "SEND_PLAYBACK_COMPLETE", utteranceId: 5 });
    expect(sentFrames()[1]).toMatchObject({ utteranceId: 5, annotationLateCount: 0, annotationLateMsMax: 0 });
  });

  it("annotation lateness rides the flush-path frame too", () => {
    stubQuality({ audioMs: 2000, arrivalWallMs: 3000 });
    core["playbackClock_"] = fakeClock(4, 1000);
    core["handleAnnotationMessage"](annotation(4, 100)); // 900 ms late
    core["executeFsmAction"]({ type: "FREEZE_BUBBLE_AT_POSITION", utteranceId: 4 });
    expect(sentFrames()[0]).toMatchObject({ utteranceId: 4, interrupted: true, annotationLateCount: 1, annotationLateMsMax: 900 });
  });

  // ── clientArrivalRtf → playbackMetrics ───────────────────────────────────

  it("derives clientArrivalRtf (arrivalWallMs / audioMs, 2 decimals) and re-emits playbackMetrics with the earlier timings kept", () => {
    stubQuality({ audioMs: 1200, arrivalWallMs: 2000 });
    core["lastPlaybackMetrics"] = { utteranceId: 4, clientReceiveToPlayMs: 480, clientTtfaMs: 1280 };
    core["executeFsmAction"]({ type: "SEND_PLAYBACK_COMPLETE", utteranceId: 4 });
    expect(playbackMetrics).toEqual([{ utteranceId: 4, clientReceiveToPlayMs: 480, clientTtfaMs: 1280, clientArrivalRtf: 1.67 }]);
    // Kept for the envelope of a `turn_metrics` that arrives afterwards.
    expect(core["lastPlaybackMetrics"]).toMatchObject({ utteranceId: 4, clientArrivalRtf: 1.67 });
    // Event Log row.
    const row = events.find((e) => e.type === "playback_quality");
    expect(row?.dir).toBe("out");
    expect(row?.summary).toContain("audio=1200ms arrival=2000ms");
  });

  it("clientArrivalRtf from the flush path: a fresh metrics object when the first-chunk timings belong to another utterance", () => {
    stubQuality({ audioMs: 500, arrivalWallMs: 250 });
    core["lastPlaybackMetrics"] = { utteranceId: 3, clientTtfaMs: 700 };
    core["executeFsmAction"]({ type: "FREEZE_BUBBLE_AT_POSITION", utteranceId: 4 });
    expect(playbackMetrics).toEqual([{ utteranceId: 4, clientArrivalRtf: 0.5 }]);
    expect(events.find((e) => e.type === "playback_quality")?.summary).toContain("(interrupted)");
  });
});
