import { describe, it, expect, beforeEach } from "vitest";
import { VoicePipesSessionCore } from "../VoicePipesSessionCore";
import type { ClientControl } from "../../transport/types";

/** Minimal transport that records the control frames the core sends. */
function fakeTransport(bufferedAmount = 0) {
  const sent: ClientControl[] = [];
  return {
    sent,
    adapter: {
      state: "connected" as const,
      bufferedAmount,
      sendControl(c: ClientControl) {
        sent.push(c);
      },
    },
  };
}

/** Playback stub returning cumulative counters, as the real service does. */
function fakePlayback(audioMs: number, heardMs: number, underrunCount: number) {
  return {
    getPlaybackQuality: () => ({
      utteranceId: 0,
      audioMs,
      heardMs,
      underrunCount,
      totalSilenceMs: 0,
      wallMs: 0,
      arrivalWallMs: 0,
      interrupted: false,
      annotationLateCount: 0,
      annotationLateMsMax: 0,
      gaps: [],
    }),
  };
}

/* eslint-disable @typescript-eslint/no-explicit-any */
function report(core: VoicePipesSessionCore): void {
  (core as any).sendNetReport();
}
function setDeps(core: VoicePipesSessionCore, transport: unknown, playback: unknown) {
  (core as any).transport = transport;
  (core as any).playback = playback;
}
/* eslint-enable @typescript-eslint/no-explicit-any */

describe("net_report", () => {
  let core: VoicePipesSessionCore;
  beforeEach(() => {
    core = new VoicePipesSessionCore();
  });

  it("reports the uplink backlog the server cannot see", () => {
    const t = fakeTransport(65536);
    setDeps(core, t.adapter, fakePlayback(0, 0, 0));
    report(core);

    expect(t.sent).toHaveLength(1);
    expect(t.sent[0]).toMatchObject({
      action: "net_report",
      uplink_backlog_bytes: 65536,
    });
  });

  it("sends deltas, not running totals", () => {
    const t = fakeTransport();
    // First tick establishes the baseline and claims nothing.
    setDeps(core, t.adapter, fakePlayback(1000, 900, 1));
    report(core);
    expect(t.sent[0]).toMatchObject({ audio_received_ms: 0, underruns: 0 });

    // Second tick reports only what accrued since.
    setDeps(core, t.adapter, fakePlayback(2500, 2400, 3));
    report(core);
    expect(t.sent[1]).toMatchObject({ audio_received_ms: 1500, underruns: 2 });
  });

  // Playback counters reset at a turn boundary. A naive delta would go
  // negative and read as a wildly healthy link at the worst possible moment.
  it("re-baselines instead of reporting a negative delta after a reset", () => {
    const t = fakeTransport();
    setDeps(core, t.adapter, fakePlayback(5000, 4800, 4));
    report(core);
    setDeps(core, t.adapter, fakePlayback(200, 100, 0)); // new turn
    report(core);

    expect(t.sent[1]).toMatchObject({ audio_received_ms: 0, underruns: 0 });

    // ...and the next tick measures from the new baseline.
    setDeps(core, t.adapter, fakePlayback(900, 800, 1));
    report(core);
    expect(t.sent[2]).toMatchObject({ audio_received_ms: 700, underruns: 1 });
  });

  it("reports buffer depth as delivered-but-not-yet-heard", () => {
    const t = fakeTransport();
    setDeps(core, t.adapter, fakePlayback(1000, 820, 0));
    report(core);
    expect(t.sent[0]).toMatchObject({ playback_buffer_ms: 180 });
  });

  it("sends nothing while disconnected", () => {
    const t = fakeTransport();
    setDeps(core, { ...t.adapter, state: "disconnected" }, fakePlayback(1000, 900, 0));
    report(core);
    expect(t.sent).toHaveLength(0);
  });

  it("still reports when there is no playback service yet", () => {
    const t = fakeTransport(1024);
    setDeps(core, t.adapter, null);
    report(core);
    expect(t.sent[0]).toMatchObject({
      action: "net_report",
      uplink_backlog_bytes: 1024,
      playback_buffer_ms: 0,
    });
  });
});
