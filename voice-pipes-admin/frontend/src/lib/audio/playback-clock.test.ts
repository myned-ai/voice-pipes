// Tests for PlaybackClock implementations.
//
// Uses a fake `PlaybackSource` plus `createManualTickDriver()` so the entire
// test suite is deterministic — no real timers, no real RAF, no AudioContext.

import { describe, expect, it, vi } from "vitest";
import {
  createManualTickDriver,
  DefaultPlaybackClock,
  type ClockSnapshot,
  type PlaybackSource,
} from "./playback-clock";

class FakeSource implements PlaybackSource {
  nowMs = 0;
  totalMs = 0;
  scheduledAheadMs = 0;
  utteranceId: number | null = null;
  running = false;

  getNowMs() { return this.nowMs; }
  getTotalMs() { return this.totalMs; }
  getUtteranceId() { return this.utteranceId; }
  isRunning() { return this.running; }
  getScheduledAheadMs() { return this.scheduledAheadMs; }
}

describe("DefaultPlaybackClock", () => {
  it("getters delegate to the source", () => {
    const src = new FakeSource();
    src.nowMs = 123;
    src.totalMs = 456;
    src.scheduledAheadMs = 333;
    src.utteranceId = 7;
    src.running = true;

    const { driver } = createManualTickDriver();
    const clock = new DefaultPlaybackClock({ source: src, driver });

    expect(clock.getNowMs()).toBe(123);
    expect(clock.getTotalMs()).toBe(456);
    expect(clock.getUtteranceId()).toBe(7);
    expect(clock.isRunning()).toBe(true);
  });

  it("emits an immediate snapshot when subscribing", () => {
    const src = new FakeSource();
    src.nowMs = 100;
    src.utteranceId = 1;

    const { driver } = createManualTickDriver();
    const clock = new DefaultPlaybackClock({ source: src, driver });

    const sub = vi.fn<(s: ClockSnapshot) => void>();
    clock.subscribe(sub);

    expect(sub).toHaveBeenCalledTimes(1);
    expect(sub.mock.calls[0][0].nowMs).toBe(100);
    expect(sub.mock.calls[0][0].utteranceId).toBe(1);
  });

  it("ticks deliver snapshots to all subscribers", () => {
    const src = new FakeSource();
    const { driver, advance } = createManualTickDriver();
    const clock = new DefaultPlaybackClock({ source: src, driver });

    const a = vi.fn<(s: ClockSnapshot) => void>();
    const b = vi.fn<(s: ClockSnapshot) => void>();
    clock.subscribe(a);
    clock.subscribe(b);
    a.mockClear();
    b.mockClear();

    src.nowMs = 250;
    advance();

    expect(a).toHaveBeenCalledTimes(1);
    expect(b).toHaveBeenCalledTimes(1);
    expect(a.mock.calls[0][0].nowMs).toBe(250);
    expect(b.mock.calls[0][0].nowMs).toBe(250);
  });

  it("starts the driver lazily on first subscriber", () => {
    const src = new FakeSource();
    const startSpy = vi.fn();
    const stopSpy = vi.fn();
    const driver = (cb: () => void) => {
      startSpy(cb);
      return stopSpy;
    };

    const clock = new DefaultPlaybackClock({ source: src, driver });
    expect(startSpy).not.toHaveBeenCalled();

    const unsubA = clock.subscribe(() => {});
    expect(startSpy).toHaveBeenCalledTimes(1);

    const unsubB = clock.subscribe(() => {});
    expect(startSpy).toHaveBeenCalledTimes(1); // still 1 — only first sub starts driver

    unsubA();
    expect(stopSpy).not.toHaveBeenCalled();

    unsubB();
    expect(stopSpy).toHaveBeenCalledTimes(1); // last unsubscribe stops the driver
  });

  it("unsubscribed callbacks are not invoked on subsequent ticks", () => {
    const src = new FakeSource();
    const { driver, advance } = createManualTickDriver();
    const clock = new DefaultPlaybackClock({ source: src, driver });

    const cb = vi.fn<(s: ClockSnapshot) => void>();
    const unsub = clock.subscribe(cb);
    cb.mockClear();

    advance();
    expect(cb).toHaveBeenCalledTimes(1);

    unsub();
    advance();
    expect(cb).toHaveBeenCalledTimes(1);
  });

  it("snapshot reflects the current source state", () => {
    const src = new FakeSource();
    src.nowMs = 10;
    src.totalMs = 50;
    src.scheduledAheadMs = 40;
    src.utteranceId = 9;
    src.running = true;

    const { driver } = createManualTickDriver();
    const clock = new DefaultPlaybackClock({ source: src, driver });

    const snap = clock.snapshot();
    expect(snap).toEqual({
      utteranceId: 9,
      nowMs: 10,
      totalMs: 50,
      isRunning: true,
      scheduledAheadMs: 40,
    });
  });
});

describe("createManualTickDriver", () => {
  it("does nothing when no driver is active", () => {
    const { advance } = createManualTickDriver();
    expect(() => advance()).not.toThrow();
  });

  it("calls the registered callback once per advance()", () => {
    const { driver, advance } = createManualTickDriver();
    const cb = vi.fn();
    driver(cb);

    advance();
    advance();
    advance();
    expect(cb).toHaveBeenCalledTimes(3);
  });

  it("stop fn deregisters the callback", () => {
    const { driver, advance } = createManualTickDriver();
    const cb = vi.fn();
    const stop = driver(cb);

    advance();
    expect(cb).toHaveBeenCalledTimes(1);

    stop();
    advance();
    expect(cb).toHaveBeenCalledTimes(1);
  });
});
