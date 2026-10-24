import { describe, it, expect, vi } from "vitest";
import {
  createBlendshapeMeter,
  keyframesFromChunk,
  keyframesFromAnnotation,
  frameAt,
  frameAtOffset,
  pickFrame,
  type OffsetTimeline,
} from "./blendshape-meter";
import type { PlaybackChunk } from "../../audio/playback";
import type { ClockSnapshot, ClockSubscriber, PlaybackClock } from "../../audio/playback-clock";
import type { AudioAnnotation } from "../../transport/types";
import type { PlaygroundExtensionHost, PlaygroundExtensionEventMap } from "../types";

function annotation(extras: Record<string, unknown>, utteranceId = 12, startMs = 160, kind = "blendshapes"): AudioAnnotation {
  return { utteranceId, startMs, durationMs: 160, kind, extras };
}

function snap(utteranceId: number | null, nowMs: number): ClockSnapshot {
  return { utteranceId, nowMs, totalMs: nowMs + 500, isRunning: true, scheduledAheadMs: 500 };
}

function chunk(startTime: number, endTime: number, extras?: Record<string, unknown>, utteranceId = 3): PlaybackChunk {
  return {
    samples: new Int16Array(0),
    startTime,
    endTime,
    meta: { utteranceId, ...(extras ? { extras } : {}) },
  };
}

describe("keyframesFromChunk / frameAt", () => {
  it("expands fps/frames into absolute AudioContext-time keyframes", () => {
    const { frames, fps } = keyframesFromChunk(
      chunk(10, 10.1, { blendshapes: { fps: 20, names: ["jawOpen", "mouthSmile"], frames: [[0.1, 0.2], [0.3, 0.4]] } }),
    );
    expect(fps).toBe(20);
    expect(frames.map((f) => f.t)).toEqual([10, 10.05]);
    expect(frames[1].weights.get("jawOpen")).toBe(0.3);
    expect(frames[1].weights.get("mouthSmile")).toBe(0.4);
    expect(frames[0].utteranceId).toBe(3);
  });

  it("accepts a flat weight map as one keyframe at chunk start, and ignores non-object extras", () => {
    const { frames } = keyframesFromChunk(chunk(2, 2.5, { blendshapes: { jawOpen: 0.7 } }));
    expect(frames).toHaveLength(1);
    expect(frames[0].t).toBe(2);
    expect(frames[0].weights.get("jawOpen")).toBe(0.7);
    expect(keyframesFromChunk(chunk(2, 2.5)).frames).toEqual([]);
    expect(keyframesFromChunk(chunk(2, 2.5, { blendshapes: "nope" })).frames).toEqual([]);
  });

  it("frameAt picks the last keyframe at or before the playhead", () => {
    const { frames } = keyframesFromChunk(
      chunk(0, 0.1, { blendshapes: { fps: 10, frames: [[0], [1], [2]] } }),
    );
    expect(frameAt(frames, -1)).toBeNull();
    expect(frameAt(frames, 0.05)?.weights.get("#0")).toBe(0);
    expect(frameAt(frames, 0.1)?.weights.get("#0")).toBe(1);
    expect(frameAt(frames, 5)?.weights.get("#0")).toBe(2);
  });
});

// Annotations live in utterance-offset space and are read against
// `playbackClock` snapshots, never AudioContext seconds.
describe("keyframesFromAnnotation / frameAtOffset / pickFrame", () => {
  it("expands an annotation into offset keyframes at startMs + i * 1000 / fps (both payload placements)", () => {
    // Nested `extras.blendshapes` — the documented shape.
    const nested = keyframesFromAnnotation(
      annotation({ blendshapes: { fps: 20, names: ["jawOpen", "mouthSmile"], frames: [[0.1, 0.2], [0.3, 0.4], [0.5, 0.6]] } }),
    );
    expect(nested.fps).toBe(20);
    expect(nested.frames.map((f) => f.tMs)).toEqual([160, 210, 260]);
    expect(nested.frames.every((f) => f.utteranceId === 12)).toBe(true);
    expect(nested.frames[1].weights.get("jawOpen")).toBe(0.3);
    expect(nested.frames[1].weights.get("mouthSmile")).toBe(0.4);

    // Payload directly in `extras`.
    const direct = keyframesFromAnnotation(annotation({ fps: 10, frames: [[0], [1]] }, 5, 1000));
    expect(direct.fps).toBe(10);
    expect(direct.frames.map((f) => f.tMs)).toEqual([1000, 1100]);
    expect(direct.frames[1].utteranceId).toBe(5);
    expect(direct.frames[1].weights.get("#0")).toBe(1);

    // `extras.blendshapes` wins when both are present.
    const both = keyframesFromAnnotation(
      annotation({ fps: 10, frames: [[9], [9], [9]], blendshapes: { fps: 50, frames: [[0.5]] } }),
    );
    expect(both.fps).toBe(50);
    expect(both.frames).toHaveLength(1);
    expect(both.frames[0].weights.get("#0")).toBe(0.5);

    // Flat weight map → one keyframe at startMs; other kinds → nothing.
    const flat = keyframesFromAnnotation(annotation({ blendshapes: { jawOpen: 0.7 } }, 12, 480));
    expect(flat.frames.map((f) => f.tMs)).toEqual([480]);
    expect(flat.frames[0].weights.get("jawOpen")).toBe(0.7);
    expect(keyframesFromAnnotation(annotation({ fps: 30, frames: [[1]] }, 12, 0, "visemes")).frames).toEqual([]);
    expect(keyframesFromAnnotation(annotation({})).frames).toEqual([]);
  });

  it("selects the frame at the clock offset and switches source per utterance (annotation wins, inline fallback)", () => {
    const ann = keyframesFromAnnotation(annotation({ fps: 10, frames: [[0], [1], [2]] }, 12, 160));
    const timelines = new Map<number, OffsetTimeline>([[12, { frames: ann.frames, fps: ann.fps }]]);
    // Inline timeline (AudioContext seconds) for utterance 11.
    const inline = keyframesFromChunk(chunk(10, 10.2, { blendshapes: { fps: 10, frames: [[7], [8]] } }, 11));

    // frameAtOffset: last keyframe at or before nowMs.
    expect(frameAtOffset(ann.frames, 159)).toBeNull();
    expect(frameAtOffset(ann.frames, 160)?.weights.get("#0")).toBe(0);
    expect(frameAtOffset(ann.frames, 299)?.weights.get("#0")).toBe(1);
    expect(frameAtOffset(ann.frames, 5000)?.weights.get("#0")).toBe(2);

    // Clock on utterance 12 with annotation frames → annotation source at nowMs.
    const a = pickFrame(snap(12, 265), timelines, inline, 10.15);
    expect(a.source).toBe("annotation");
    expect(a.frame?.weights.get("#0")).toBe(1);
    expect(a.count).toBe(3);
    expect(a.fps).toBe(10);
    expect(a.at).toBe("t=265ms");
    // Before the first annotation frame: still the annotation source, neutral (null) face.
    expect(pickFrame(snap(12, 100), timelines, inline, 10.15).frame).toBeNull();

    // Clock on utterance 11 (no annotation frames) → inline source at AudioContext time.
    const b = pickFrame(snap(11, 150), timelines, inline, 10.15);
    expect(b.source).toBe("inline");
    expect(b.frame?.weights.get("#0")).toBe(8);
    expect(b.at).toBe("t=10.15s");
    // No clock at all (playback not started) → inline.
    expect(pickFrame(null, timelines, inline, 10.0).source).toBe("inline");
    expect(pickFrame(null, timelines, inline, 10.0).frame?.weights.get("#0")).toBe(7);
  });
});

describe("blendshape-meter extension", () => {
  function makeClock() {
    const subs = new Set<ClockSubscriber>();
    let current = snap(null, 0);
    const clock: PlaybackClock = {
      getNowMs: () => current.nowMs,
      getTotalMs: () => current.totalMs,
      getUtteranceId: () => current.utteranceId,
      isRunning: () => current.isRunning,
      subscribe: (cb) => { subs.add(cb); cb(current); return () => { subs.delete(cb); }; },
    };
    const tick = (s: ClockSnapshot) => { current = s; subs.forEach((cb) => cb(s)); };
    return { clock, tick, subs };
  }

  function makeHost(playbackClock: PlaybackClock | null = null) {
    const listeners = new Map<string, Set<(...a: unknown[]) => void>>();
    const fakeCtx = { currentTime: 0 } as unknown as AudioContext;
    const host: PlaygroundExtensionHost = {
      apiVersion: 1,
      session: {
        sessionState: "active",
        pipelineState: null,
        playbackService: { audioContext: fakeCtx } as never,
        playbackClock,
        activeUtteranceId: null,
        on: <K extends keyof PlaygroundExtensionEventMap>(ev: K, l: PlaygroundExtensionEventMap[K]) => {
          if (!listeners.has(ev)) listeners.set(ev, new Set());
          listeners.get(ev)!.add(l as (...a: unknown[]) => void);
          return () => { listeners.get(ev)?.delete(l as (...a: unknown[]) => void); };
        },
        sendClientToolResult: () => {},
      },
      theme: { isDark: true, onChange: () => () => {} },
      options: { maxBars: 8 },
      baseUrl: "builtin:blendshape-meter",
      log: vi.fn(),
    };
    const emit = (ev: string, ...args: unknown[]) => listeners.get(ev)?.forEach((l) => l(...args));
    return { host, emit, listeners };
  }

  it("mounts into jsdom (getContext → null) without throwing, consumes chunks, and unmounts cleanly", () => {
    const { host, emit, listeners } = makeHost();
    const el = document.createElement("div");
    const ext = createBlendshapeMeter();
    expect(() => ext.mount(el, host)).not.toThrow();
    expect(el.querySelector("canvas")).not.toBeNull();
    expect(() => emit("audioChunk", chunk(0, 0.5, { blendshapes: { fps: 30, frames: [[0.5]] } }))).not.toThrow();
    expect(() => emit("audioChunk", chunk(0.5, 1.0))).not.toThrow();
    expect(() => emit("audioFlushed", { utteranceId: 3, playedMs: 100, reason: "barge_in" })).not.toThrow();
    ext.unmount();
    expect(el.querySelector("canvas")).toBeNull();
    for (const set of listeners.values()) expect(set.size).toBe(0);
  });

  it("subscribes to the playback clock, consumes annotations, clears on flush, and releases the clock on unmount", () => {
    const { clock, tick, subs } = makeClock();
    const { host, emit, listeners } = makeHost(clock);
    const el = document.createElement("div");
    const ext = createBlendshapeMeter();
    ext.mount(el, host);
    expect(subs.size).toBe(1);
    expect(listeners.get("audioAnnotation")?.size).toBe(1);
    expect(() => emit("audioAnnotation", annotation({ fps: 30, frames: [[0.5]] }, 12, 0))).not.toThrow();
    expect(() => emit("audioAnnotation", annotation({ fps: 30, frames: [[0.5]] }, 12, 0, "visemes"))).not.toThrow();
    expect(() => tick(snap(12, 40))).not.toThrow();
    expect(() => tick(snap(13, 0))).not.toThrow(); // newer utterance → offset timeline for 12 pruned
    expect(() => emit("audioFlushed", { utteranceId: 13, playedMs: 0, reason: "barge_in" })).not.toThrow();
    ext.unmount();
    expect(subs.size).toBe(0);
    for (const set of listeners.values()) expect(set.size).toBe(0);
  });

  // The header dogfoods `emit_rtf` of the last turn via `metricsEmitted`
  // (and `playbackMetrics` for the timings).
  it("subscribes to metricsEmitted / playbackMetrics for the header and survives snapshots without an output component", () => {
    const { host, emit, listeners } = makeHost();
    const el = document.createElement("div");
    const ext = createBlendshapeMeter();
    ext.mount(el, host);
    expect(listeners.get("metricsEmitted")?.size).toBe(1);
    expect(listeners.get("playbackMetrics")?.size).toBe(1);
    const withOutput = {
      snapshot: {
        utterance_id: 12,
        total_ms: 60000,
        interrupted: false,
        components: [{ component_id: "output", kind: "output", started_at_ms: 0, extras: { emit_rtf: 1.67, source_rtf: 0.53, chain_overhead_ms: 41360 } }],
      },
    };
    expect(() => emit("metricsEmitted", "12", withOutput)).not.toThrow();
    expect(() => emit("playbackMetrics", { utteranceId: 12, clientTtfaMs: 900, clientArrivalRtf: 1.6 })).not.toThrow();
    // Old worker: no `output` component at all.
    expect(() => emit("metricsEmitted", "13", { snapshot: { utterance_id: 13, total_ms: 1000, interrupted: false, components: [] } })).not.toThrow();
    ext.unmount();
    for (const set of listeners.values()) expect(set.size).toBe(0);
  });
});
