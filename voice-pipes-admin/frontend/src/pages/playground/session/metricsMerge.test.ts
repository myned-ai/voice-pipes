import { describe, it, expect } from "vitest";
import { mergePlaybackMetrics } from "./metricsMerge";
import type { TurnMetricsEntry } from "./types";

function entry(utteranceId: number, extra: Partial<TurnMetricsEntry> = {}): TurnMetricsEntry {
  return {
    snapshot: { utterance_id: utteranceId, total_ms: 1000, interrupted: false, components: [] },
    clientE2eMs: 700,
    timestamp: 1,
    ...extra,
  };
}

describe("mergePlaybackMetrics", () => {
  it("merges the three fields onto the matching entry without touching the others", () => {
    const prev = [entry(1), entry(2)];
    const next = mergePlaybackMetrics(prev, { utteranceId: 2, clientReceiveToPlayMs: 210, clientTtfaMs: 910, clientPlaybackLatencyMs: 260 });
    expect(next).not.toBe(prev);
    expect(next[0]).toBe(prev[0]);
    expect(next[1]).toMatchObject({ clientE2eMs: 700, clientReceiveToPlayMs: 210, clientTtfaMs: 910, clientPlaybackLatencyMs: 260 });
    expect(prev[1].clientReceiveToPlayMs).toBeUndefined();
  });

  it("returns the same array reference when no entry matches", () => {
    const prev = [entry(1)];
    expect(mergePlaybackMetrics(prev, { utteranceId: 5, clientReceiveToPlayMs: 1 })).toBe(prev);
    expect(mergePlaybackMetrics([], { utteranceId: 5 })).toEqual([]);
  });

  it("the end-of-turn re-emission carries clientArrivalRtf onto the entry", () => {
    const first = mergePlaybackMetrics([entry(4)], { utteranceId: 4, clientReceiveToPlayMs: 100, clientTtfaMs: 800 });
    expect(first[0].clientArrivalRtf).toBeUndefined();
    // The core repeats the earlier fields when it adds the arrival RTF.
    const second = mergePlaybackMetrics(first, { utteranceId: 4, clientReceiveToPlayMs: 100, clientTtfaMs: 800, clientArrivalRtf: 1.67 });
    expect(second[0]).toMatchObject({ clientReceiveToPlayMs: 100, clientTtfaMs: 800, clientArrivalRtf: 1.67 });
  });

  // The anchor of the client E2E rides
  // along like clientArrivalRtf, but a playback event without one must not
  // erase the envelope's anchor.
  it("clientE2eSource is copied from the playback event and kept when the event lacks it", () => {
    const withSource = mergePlaybackMetrics([entry(6)], { utteranceId: 6, clientTtfaMs: 900, clientE2eSource: "client_vad" });
    expect(withSource[0].clientE2eSource).toBe("client_vad");
    const later = mergePlaybackMetrics(withSource, { utteranceId: 6, clientTtfaMs: 900, clientArrivalRtf: 1.1 });
    expect(later[0].clientE2eSource).toBe("client_vad");
    const fromEnvelope = mergePlaybackMetrics([entry(7, { clientE2eSource: "listening" })], { utteranceId: 7, clientReceiveToPlayMs: 10 });
    expect(fromEnvelope[0].clientE2eSource).toBe("listening");
  });

  it("a later duplicate overwrites (including clearing fields that are now undefined)", () => {
    const first = mergePlaybackMetrics([entry(3)], { utteranceId: 3, clientReceiveToPlayMs: 100, clientTtfaMs: 800, clientPlaybackLatencyMs: 150 });
    const second = mergePlaybackMetrics(first, { utteranceId: 3, clientReceiveToPlayMs: 120 });
    expect(second[0].clientReceiveToPlayMs).toBe(120);
    expect(second[0].clientTtfaMs).toBeUndefined();
    expect(second[0].clientPlaybackLatencyMs).toBeUndefined();
  });
});
