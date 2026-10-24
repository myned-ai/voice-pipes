// Tests for createPlaybackSource — the AudioPlaybackService → PlaybackSource adapter.

import { describe, expect, it } from "vitest";
import type { AudioPlaybackService } from "./playback";
import { createPlaybackSource } from "./playback-source";

// Minimal stub satisfying the surface used by the adapter.
type Stub = Pick<
  AudioPlaybackService,
  | "getPlayedDurationMs"
  | "getBufferStats"
  | "currentUtteranceId"
  | "playing"
  | "totalEnqueuedDurationMs"
>;

function makeRef(stub: Partial<Stub> | null): { current: AudioPlaybackService | null } {
  return { current: stub as AudioPlaybackService | null };
}

describe("createPlaybackSource", () => {
  it("returns zeros when ref is null", () => {
    const src = createPlaybackSource(makeRef(null));
    expect(src.getNowMs()).toBe(0);
    expect(src.getTotalMs()).toBe(0);
    expect(src.getUtteranceId()).toBeNull();
    expect(src.isRunning()).toBe(false);
    expect(src.getScheduledAheadMs()).toBe(0);
  });

  it("forwards getPlayedDurationMs to nowMs", () => {
    const src = createPlaybackSource(
      makeRef({
        getPlayedDurationMs: () => 1234,
        getBufferStats: () => ({ scheduledChunks: 0, pendingChunks: 0, scheduledAheadMs: 0 }),
        currentUtteranceId: 5,
        playing: true,
        totalEnqueuedDurationMs: 0,
      }),
    );
    expect(src.getNowMs()).toBe(1234);
  });

  it("computes totalMs as nowMs + scheduledAheadMs when both > 0", () => {
    const src = createPlaybackSource(
      makeRef({
        getPlayedDurationMs: () => 600,
        getBufferStats: () => ({ scheduledChunks: 1, pendingChunks: 0, scheduledAheadMs: 400 }),
        currentUtteranceId: 1,
        playing: true,
        totalEnqueuedDurationMs: 999, // ignored — primary path wins
      }),
    );
    expect(src.getTotalMs()).toBe(1000);
  });

  it("falls back to totalEnqueuedDurationMs while played + ahead are both 0", () => {
    const src = createPlaybackSource(
      makeRef({
        getPlayedDurationMs: () => 0,
        getBufferStats: () => ({ scheduledChunks: 0, pendingChunks: 0, scheduledAheadMs: 0 }),
        currentUtteranceId: 1,
        playing: true,
        totalEnqueuedDurationMs: 750,
      }),
    );
    expect(src.getTotalMs()).toBe(750);
  });

  it("treats currentUtteranceId === -1 as null (pre-session sentinel)", () => {
    const src = createPlaybackSource(
      makeRef({
        getPlayedDurationMs: () => 0,
        getBufferStats: () => ({ scheduledChunks: 0, pendingChunks: 0, scheduledAheadMs: 0 }),
        currentUtteranceId: -1,
        playing: false,
        totalEnqueuedDurationMs: 0,
      }),
    );
    expect(src.getUtteranceId()).toBeNull();
  });

  it("returns utteranceId even if 0", () => {
    const src = createPlaybackSource(
      makeRef({
        getPlayedDurationMs: () => 0,
        getBufferStats: () => ({ scheduledChunks: 0, pendingChunks: 0, scheduledAheadMs: 0 }),
        currentUtteranceId: 0,
        playing: false,
        totalEnqueuedDurationMs: 0,
      }),
    );
    expect(src.getUtteranceId()).toBe(0);
  });

  it("returns positive utteranceId when active", () => {
    const src = createPlaybackSource(
      makeRef({
        getPlayedDurationMs: () => 0,
        getBufferStats: () => ({ scheduledChunks: 0, pendingChunks: 0, scheduledAheadMs: 0 }),
        currentUtteranceId: 42,
        playing: true,
        totalEnqueuedDurationMs: 0,
      }),
    );
    expect(src.getUtteranceId()).toBe(42);
  });

  it("forwards playing to isRunning", () => {
    const refTrue = makeRef({
      getPlayedDurationMs: () => 0,
      getBufferStats: () => ({ scheduledChunks: 0, pendingChunks: 0, scheduledAheadMs: 0 }),
      currentUtteranceId: 1,
      playing: true,
      totalEnqueuedDurationMs: 0,
    });
    expect(createPlaybackSource(refTrue).isRunning()).toBe(true);

    const refFalse = makeRef({
      getPlayedDurationMs: () => 0,
      getBufferStats: () => ({ scheduledChunks: 0, pendingChunks: 0, scheduledAheadMs: 0 }),
      currentUtteranceId: 1,
      playing: false,
      totalEnqueuedDurationMs: 0,
    });
    expect(createPlaybackSource(refFalse).isRunning()).toBe(false);
  });

  it("forwards scheduledAheadMs from buffer stats", () => {
    const src = createPlaybackSource(
      makeRef({
        getPlayedDurationMs: () => 100,
        getBufferStats: () => ({ scheduledChunks: 2, pendingChunks: 1, scheduledAheadMs: 250 }),
        currentUtteranceId: 1,
        playing: true,
        totalEnqueuedDurationMs: 0,
      }),
    );
    expect(src.getScheduledAheadMs()).toBe(250);
  });

  it("survives ref swap (service recreated on sample-rate renegotiation)", () => {
    const ref = makeRef({
      getPlayedDurationMs: () => 100,
      getBufferStats: () => ({ scheduledChunks: 0, pendingChunks: 0, scheduledAheadMs: 0 }),
      currentUtteranceId: 1,
      playing: true,
      totalEnqueuedDurationMs: 0,
    });
    const src = createPlaybackSource(ref);
    expect(src.getNowMs()).toBe(100);

    ref.current = {
      getPlayedDurationMs: () => 999,
      getBufferStats: () => ({ scheduledChunks: 0, pendingChunks: 0, scheduledAheadMs: 0 }),
      currentUtteranceId: 2,
      playing: true,
      totalEnqueuedDurationMs: 0,
    } as AudioPlaybackService;
    expect(src.getNowMs()).toBe(999);
    expect(src.getUtteranceId()).toBe(2);
  });
});
