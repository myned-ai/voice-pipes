// Geometry of the fragment timeline must mirror the backend's as-heard
// splice (`get_turn_as_heard`): silence inserted *before* the audio at
// `at_audio_ms`, cumulative, ideal audio clamped to the barge-in first, a
// gap past the clamp appended at the end.

import { describe, it, expect } from "vitest";
import { idealToHeardMs, layoutTimeline, segmentAt, gapAt, idealTotalMs } from "./heardTimeline";
import type { FragmentInfo } from "./types";

// 500 ms + 250 ms + 250 ms of ideal audio.
const frags: FragmentInfo[] = [
  { kind: "tts_frag_000", offset_ms: 0, duration_ms: 500 },
  { kind: "tts_frag_001", offset_ms: 500, duration_ms: 250 },
  { kind: "tts_frag_002", offset_ms: 750, duration_ms: 250 },
];
const noHeard = { gaps: [], cutMs: null };

describe("idealToHeardMs", () => {
  it("shifts audio at or after a gap by its silence, cumulatively", () => {
    const gaps = [
      { at_audio_ms: 500, silence_ms: 250 },
      { at_audio_ms: 800, silence_ms: 100 },
    ];
    expect(idealToHeardMs(499, gaps)).toBe(499);
    expect(idealToHeardMs(500, gaps)).toBe(750);
    expect(idealToHeardMs(799, gaps)).toBe(1049);
    expect(idealToHeardMs(800, gaps)).toBe(1150);
  });

  it("does not care about gap order", () => {
    const gaps = [
      { at_audio_ms: 800, silence_ms: 100 },
      { at_audio_ms: 500, silence_ms: 250 },
    ];
    expect(idealToHeardMs(800, gaps)).toBe(1150);
  });
});

describe("layoutTimeline — server track", () => {
  it("lays fragments at their ideal offsets with zero-width gap markers", () => {
    const layout = layoutTimeline(frags, "server", { gaps: [{ at_audio_ms: 500, silence_ms: 250 }], cutMs: null });
    expect(layout.totalMs).toBe(1000);
    expect(idealTotalMs(frags)).toBe(1000);
    expect(layout.segments.map(s => [s.startMs, s.endMs])).toEqual([[0, 500], [500, 750], [750, 1000]]);
    expect(layout.gaps).toEqual([{ startMs: 500, endMs: 500, silenceMs: 250, atAudioMs: 500 }]);
    expect(layout.cutMs).toBeNull();
  });

  it("marks fragments starting at or after the barge-in as not delivered", () => {
    const layout = layoutTimeline(frags, "server", { gaps: [], cutMs: 600 });
    expect(layout.segments.map(s => s.afterCut)).toEqual([false, false, true]);
    expect(layout.cutMs).toBe(600);
    // Still the full ideal length — the server track has all the audio.
    expect(layout.totalMs).toBe(1000);
  });
});

describe("layoutTimeline — as-heard track", () => {
  it("opens a gap block before the audio at its position and shifts what follows", () => {
    const layout = layoutTimeline(frags, "heard", { gaps: [{ at_audio_ms: 500, silence_ms: 250 }], cutMs: null });
    expect(layout.segments.map(s => [s.startMs, s.endMs])).toEqual([[0, 500], [750, 1000], [1000, 1250]]);
    expect(layout.gaps).toEqual([{ startMs: 500, endMs: 750, silenceMs: 250, atAudioMs: 500 }]);
    expect(layout.totalMs).toBe(1250);
  });

  it("a gap inside a fragment widens that segment", () => {
    const layout = layoutTimeline(frags, "heard", { gaps: [{ at_audio_ms: 100, silence_ms: 300 }], cutMs: null });
    expect(layout.segments[0]).toMatchObject({ startMs: 0, endMs: 800 });
    expect(layout.segments[1]).toMatchObject({ startMs: 800, endMs: 1050 });
    expect(layout.gaps[0]).toMatchObject({ startMs: 100, endMs: 400 });
  });

  it("clamps to the barge-in, truncating the straddling fragment and dropping later ones", () => {
    const layout = layoutTimeline(frags, "heard", { gaps: [{ at_audio_ms: 500, silence_ms: 250 }], cutMs: 600 });
    expect(layout.segments.map(s => [s.startMs, s.endMs])).toEqual([[0, 500], [750, 850]]);
    expect(layout.totalMs).toBe(850);
    expect(layout.cutMs).toBe(850);
  });

  it("a gap reported past the barge-in becomes trailing silence, as the backend appends it", () => {
    const layout = layoutTimeline(frags, "heard", { gaps: [{ at_audio_ms: 700, silence_ms: 200 }], cutMs: 600 });
    expect(layout.segments.map(s => [s.startMs, s.endMs])).toEqual([[0, 500], [500, 600]]);
    expect(layout.gaps).toEqual([{ startMs: 600, endMs: 800, silenceMs: 200, atAudioMs: 600 }]);
    expect(layout.totalMs).toBe(800);
  });

  it("a gap exactly on a fragment boundary sits between the two segments", () => {
    const layout = layoutTimeline(frags, "heard", { gaps: [{ at_audio_ms: 750, silence_ms: 100 }], cutMs: null });
    expect(layout.segments[1]).toMatchObject({ startMs: 500, endMs: 750 });
    expect(layout.segments[2]).toMatchObject({ startMs: 850, endMs: 1100 });
  });
});

describe("hit testing", () => {
  const layout = layoutTimeline(frags, "heard", { gaps: [{ at_audio_ms: 500, silence_ms: 250 }], cutMs: null });

  it("finds the segment under a position", () => {
    expect(segmentAt(layout, 0)?.idx).toBe(0);
    expect(segmentAt(layout, 499)?.idx).toBe(0);
    expect(segmentAt(layout, 750)?.idx).toBe(1);
    expect(segmentAt(layout, 1249)?.idx).toBe(2);
    expect(segmentAt(layout, 1250)).toBeNull();
  });

  it("reports a gap block rather than a segment inside the silence", () => {
    expect(segmentAt(layout, 600)).toBeNull();
    expect(gapAt(layout, 600)?.silenceMs).toBe(250);
    expect(gapAt(layout, 100)).toBeNull();
  });

  it("server-mode gap markers are not hit targets", () => {
    const server = layoutTimeline(frags, "server", { gaps: [{ at_audio_ms: 500, silence_ms: 250 }], cutMs: null });
    expect(gapAt(server, 500)).toBeNull();
    expect(segmentAt(server, 500)?.idx).toBe(1);
  });

  it("no fragments → empty layout", () => {
    expect(layoutTimeline([], "server", noHeard).totalMs).toBe(0);
  });
});
