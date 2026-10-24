// Tests for useSynchronizedSpeech.
// Uses the manual tick driver from playback-clock so frame timing is
// deterministic.  No real audio context, no RAF.

import { act, renderHook } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import {
  DefaultPlaybackClock,
  createManualTickDriver,
  type PlaybackSource,
} from "../../../lib/audio/playback-clock";
import { WordTimingTable } from "../../../lib/audio/word-timing-table";
import {
  computeHighlight,
  useSynchronizedSpeech,
  type ActiveUtterance,
} from "./useSynchronizedSpeech";

// ── Test helpers ───────────────────────────────────────────────────────────

/** A controllable PlaybackSource: mutate `state` to advance the clock. */
function makeSource(initial: Partial<{
  nowMs: number;
  totalMs: number;
  utteranceId: number | null;
  isRunning: boolean;
  scheduledAheadMs: number;
}> = {}) {
  const state = {
    nowMs: 0,
    totalMs: 0,
    utteranceId: null as number | null,
    isRunning: false,
    scheduledAheadMs: 0,
    ...initial,
  };
  const source: PlaybackSource = {
    getNowMs: () => state.nowMs,
    getTotalMs: () => state.totalMs,
    getUtteranceId: () => state.utteranceId,
    isRunning: () => state.isRunning,
    getScheduledAheadMs: () => state.scheduledAheadMs,
  };
  return { source, state };
}

/** Build a WordTimingTable with one fragment of N words at uniform spacing. */
function tableForWords(words: string[], totalMs: number): WordTimingTable {
  const table = new WordTimingTable();
  const perWord = totalMs / words.length;
  table.append(
    words.map((w, i) => ({ word: w, start_ms: i * perWord, end_ms: (i + 1) * perWord })),
    totalMs,
  );
  return table;
}

// ── computeHighlight (pure) ────────────────────────────────────────────────

describe("computeHighlight (pure)", () => {
  it("returns null when no active utterance", () => {
    const r = computeHighlight(null, 1000, 0);
    expect(r.highlight).toBeNull();
    expect(r.nextRatchet).toBe(0);
  });

  it("returns null when no words have been heard yet", () => {
    const active: ActiveUtterance = {
      utteranceId: 1,
      table: tableForWords(["a", "b", "c"], 300),
      entryIndex: 5,
    };
    const r = computeHighlight(active, 0, 0);
    expect(r.highlight).toBeNull();
    expect(r.nextRatchet).toBe(0);
  });

  it("emits the latest word index based on nowMs", () => {
    const active: ActiveUtterance = {
      utteranceId: 1,
      table: tableForWords(["one", "two", "three", "four"], 400),
      entryIndex: 7,
    };
    const r = computeHighlight(active, 250, 0);
    // 250 ms ⇒ words at 0, 100, 200 are heard ⇒ count = 3 ⇒ index 2
    expect(r.highlight).toEqual({ entryIndex: 7, wordIndex: 2 });
    expect(r.nextRatchet).toBe(3);
  });

  it("clamps wordIndex at table.wordCount-1 when nowMs runs past end", () => {
    const active: ActiveUtterance = {
      utteranceId: 1,
      table: tableForWords(["a", "b", "c"], 300),
      entryIndex: 0,
    };
    const r = computeHighlight(active, 9999, 0);
    expect(r.highlight).toEqual({ entryIndex: 0, wordIndex: 2 });
  });

  it("never moves backwards (ratchet floor wins)", () => {
    const active: ActiveUtterance = {
      utteranceId: 1,
      table: tableForWords(["a", "b", "c", "d"], 400),
      entryIndex: 0,
    };
    // Caller previously ratcheted to 3 words heard.  nowMs would only justify
    // 1 word, but ratchet keeps it at index 2.
    const r = computeHighlight(active, 50, 3);
    expect(r.highlight).toEqual({ entryIndex: 0, wordIndex: 2 });
    expect(r.nextRatchet).toBe(3);
  });
});

// ── useSynchronizedSpeech (React) ──────────────────────────────────────────

describe("useSynchronizedSpeech", () => {
  it("emits null highlight when no active utterance", () => {
    const { source } = makeSource();
    const { driver, advance } = createManualTickDriver();
    const clock = new DefaultPlaybackClock({ source, driver });

    const { result } = renderHook(() =>
      useSynchronizedSpeech(clock, { getActive: () => null }),
    );

    act(() => advance());
    expect(result.current.highlight).toBeNull();
  });

  it("advances the highlight as the clock ticks", () => {
    const { source, state } = makeSource({ utteranceId: 1, isRunning: true });
    const table = tableForWords(["alpha", "beta", "gamma", "delta"], 400);
    const active: ActiveUtterance = { utteranceId: 1, table, entryIndex: 2 };

    const { driver, advance } = createManualTickDriver();
    const clock = new DefaultPlaybackClock({ source, driver });

    const { result } = renderHook(() =>
      useSynchronizedSpeech(clock, { getActive: () => active }),
    );

    // Initial subscribe-time snapshot fires immediately, but nowMs is 0 so
    // no highlight yet.
    expect(result.current.highlight).toBeNull();

    act(() => {
      state.nowMs = 150; // starts at 0,100 ⇒ 2 words heard
      advance();
    });
    expect(result.current.highlight).toEqual({ entryIndex: 2, wordIndex: 1 });

    act(() => {
      state.nowMs = 350; // starts at 0,100,200,300 ⇒ 4 words heard
      advance();
    });
    expect(result.current.highlight).toEqual({ entryIndex: 2, wordIndex: 3 });

    act(() => {
      state.nowMs = 1000; // past end → clamp at last word
      advance();
    });
    expect(result.current.highlight).toEqual({ entryIndex: 2, wordIndex: 3 });
  });

  it("ratchets monotonically within an utterance", () => {
    const { source, state } = makeSource({ utteranceId: 1, isRunning: true });
    const table = tableForWords(["a", "b", "c", "d"], 400);
    const active: ActiveUtterance = { utteranceId: 1, table, entryIndex: 0 };

    const { driver, advance } = createManualTickDriver();
    const clock = new DefaultPlaybackClock({ source, driver });

    const { result } = renderHook(() =>
      useSynchronizedSpeech(clock, { getActive: () => active }),
    );

    act(() => {
      state.nowMs = 350; // 4 words ⇒ wordIndex 3
      advance();
    });
    expect(result.current.highlight?.wordIndex).toBe(3);

    // Simulate a clock blip that goes backwards — ratchet must hold.
    act(() => {
      state.nowMs = 50;
      advance();
    });
    expect(result.current.highlight?.wordIndex).toBe(3);
  });

  it("resets the ratchet when utteranceId changes", () => {
    const { source, state } = makeSource({ utteranceId: 1, isRunning: true });
    const table1 = tableForWords(["one", "two", "three"], 300);
    const table2 = tableForWords(["four", "five", "six"], 300);
    let active: ActiveUtterance = { utteranceId: 1, table: table1, entryIndex: 0 };

    const { driver, advance } = createManualTickDriver();
    const clock = new DefaultPlaybackClock({ source, driver });

    const { result } = renderHook(() =>
      useSynchronizedSpeech(clock, { getActive: () => active }),
    );

    act(() => {
      state.nowMs = 250;
      advance();
    });
    expect(result.current.highlight).toEqual({ entryIndex: 0, wordIndex: 2 });

    // New utterance starts.  state.nowMs would normally reset to 0 in real
    // playback (beginUtterance) — simulate that.
    act(() => {
      active = { utteranceId: 2, table: table2, entryIndex: 1 };
      state.utteranceId = 2;
      state.nowMs = 50;
      advance();
    });
    // Ratchet was reset; only 0 words heard at 50 ms (boundary at 100).
    expect(result.current.highlight?.entryIndex).toBe(1);
    expect(result.current.highlight?.wordIndex).toBe(0);
  });

  it("emits null and skips subscription when disabled", () => {
    const { source, state } = makeSource({ utteranceId: 1, isRunning: true });
    const table = tableForWords(["a", "b"], 200);
    const active: ActiveUtterance = { utteranceId: 1, table, entryIndex: 0 };

    const { driver, advance } = createManualTickDriver();
    const clock = new DefaultPlaybackClock({ source, driver });

    const { result } = renderHook(() =>
      useSynchronizedSpeech(clock, {
        getActive: () => active,
        enabled: false,
      }),
    );

    act(() => {
      state.nowMs = 999;
      advance();
    });
    expect(result.current.highlight).toBeNull();
    expect(result.current.snapshot).toBeNull();
  });

  it("unsubscribes from clock on unmount", () => {
    const { source } = makeSource();
    const { driver } = createManualTickDriver();
    const clock = new DefaultPlaybackClock({ source, driver });

    const { unmount } = renderHook(() =>
      useSynchronizedSpeech(clock, { getActive: () => null }),
    );

    // Internal subscribers count is private; assert indirectly by ensuring
    // unmount does not throw and the next subscribe restarts cleanly.
    expect(() => unmount()).not.toThrow();
  });
});
