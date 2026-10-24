// Integration regression tests: full word-reveal pipeline.
//
// These tests drive the chain:
//
//   AudioPlaybackService (mock AudioContext)
//     → createPlaybackSource (adapter)
//       → DefaultPlaybackClock (with manual tick driver)
//         → computeHighlight (pure)
//           → highlightInfo / wordIndex
//
// Goal: guarantee that after the `beginUtterance`-before-`enqueue` fix the
// last word of a single-fragment utterance is eventually revealed, and that
// re-introducing the old broken ordering regresses visibly.
//
// See: REGRESSION-NOTES.md for the full post-mortem.

import { describe, expect, it, vi, beforeEach } from "vitest";
import { AudioPlaybackService } from "./playback";
import { createPlaybackSource } from "./playback-source";
import {
  createManualTickDriver,
  DefaultPlaybackClock,
} from "./playback-clock";
import { WordTimingTable } from "./word-timing-table";
import { computeHighlight, type ActiveUtterance } from "../../pages/playground/session/useSynchronizedSpeech";

// ── Shared AudioContext mock (mirrors playback.test.ts) ───────────────────

class MockAudioBufferSourceNode {
  buffer: unknown = null;
  onended: (() => void) | null = null;
  private _stopped = false;
  connect() {}
  start() {}
  stop() {
    if (this._stopped) throw new DOMException("already stopped");
    this._stopped = true;
  }
  _fireEnded() { this.onended?.(); }
}

class MockAudioBuffer {
  readonly sampleRate: number;
  readonly duration: number;
  readonly length: number;
  private data: Float32Array;
  constructor(_ch: number, length: number, sr: number) {
    this.sampleRate = sr;
    this.length = length;
    this.duration = length / sr;
    this.data = new Float32Array(length);
  }
  getChannelData() { return this.data; }
}

let mockCurrentTime = 0;

class MockAudioContext {
  readonly sampleRate: number;
  readonly destination = {};
  constructor(opts?: { sampleRate?: number }) {
    this.sampleRate = opts?.sampleRate ?? 16000;
  }
  get currentTime() { return mockCurrentTime; }
  createBuffer(ch: number, len: number, sr: number) { return new MockAudioBuffer(ch, len, sr); }
  createBufferSource() { return new MockAudioBufferSourceNode() as unknown as AudioBufferSourceNode; }
  resume() { return Promise.resolve(); }
  close() { return Promise.resolve(); }
}

vi.stubGlobal("AudioContext", MockAudioContext);

// ── Helpers ───────────────────────────────────────────────────────────────

/** PCM samples: `n` samples at 16 kHz → n/16 ms of audio. */
function samples(ms: number, sampleRate = 16000): Int16Array {
  return new Int16Array(Math.round(ms * sampleRate / 1000)).fill(1000);
}

/** WordTimingTable with N words spread evenly over `totalMs`. */
function tableForWords(words: string[], totalMs: number): WordTimingTable {
  const table = new WordTimingTable();
  const perWord = totalMs / words.length;
  table.append(
    words.map((w, i) => ({ word: w, start_ms: i * perWord, end_ms: (i + 1) * perWord })),
    totalMs,
  );
  return table;
}

// ── Tests ─────────────────────────────────────────────────────────────────

describe("word-reveal pipeline: PlaybackSource → PlaybackClock → computeHighlight", () => {
  let svc: AudioPlaybackService;

  beforeEach(() => {
    // Start with mockCurrentTime at 0 — the fix allows utteranceStartTime = 0
    // to be a valid value (AudioContext at t=0 when the first chunk is scheduled).
    // The sentinel is now -1, so the guard `utteranceStartTime < 0` no longer fires
    // when ctx.currentTime happens to be 0 at scheduling time.
    mockCurrentTime = 0.001; // 1 ms: mirrors a running AudioContext clock
    svc = new AudioPlaybackService(16000);
    svc.start();
  });

  // ── Correct ordering ───────────────────────────────────────────────────

  describe("correct ordering: beginUtterance BEFORE enqueue", () => {
    it("reveals the first word of a single-fragment utterance after playback starts", () => {
      // mockCurrentTime = 0.001 (set in beforeEach) so startAt ≈ 0.001 s.
      svc.beginUtterance(1);               // ← correct: before enqueue
      svc.enqueue(samples(400));           // 400 ms utterance

      const src = createPlaybackSource({ current: svc });
      // Just after scheduling, no real time has elapsed yet.
      expect(src.getNowMs()).toBe(0);

      // Advance 100 ms past the utteranceStartTime.
      mockCurrentTime = 0.001 + 0.1;      // 100 ms into utterance
      expect(src.getNowMs()).toBeGreaterThan(0);

      const table = tableForWords(["Hello", "world"], 400);
      const active: ActiveUtterance = { utteranceId: 1, table, entryIndex: 0 };

      const { highlight } = computeHighlight(active, src.getNowMs(), 0);
      expect(highlight).not.toBeNull();
      expect(highlight!.wordIndex).toBeGreaterThanOrEqual(0);
    });

    it("reveals the LAST word of a single-fragment utterance when audio finishes", () => {
      // mockCurrentTime = 0.001 in beforeEach, so utteranceStartTime ≈ 0.001 s.
      svc.beginUtterance(1);
      svc.enqueue(samples(400));

      // Move clock to past the end of the 400 ms utterance.
      mockCurrentTime = 0.001 + 0.45; // 450 ms past utteranceStartTime

      const src = createPlaybackSource({ current: svc });
      const table = tableForWords(["Hello", "world"], 400);
      const active: ActiveUtterance = { utteranceId: 1, table, entryIndex: 0 };

      const { highlight } = computeHighlight(active, src.getNowMs(), 0);
      // Both words must be revealed (wordIndex = 1 = last word index)
      expect(highlight).not.toBeNull();
      expect(highlight!.wordIndex).toBe(1); // "world" — the last word
    });

    it("reveals all words of a three-word single-fragment utterance", () => {
      const WORDS = ["one", "two", "three"];
      const DURATION_MS = 300;

      // utteranceStartTime ≈ 0.001 s (set in beforeEach).
      svc.beginUtterance(1);
      svc.enqueue(samples(DURATION_MS));

      const src = createPlaybackSource({ current: svc });
      const table = tableForWords(WORDS, DURATION_MS);
      const active: ActiveUtterance = { utteranceId: 1, table, entryIndex: 0 };

      // Simulate clock advancing word by word (each word = 100 ms).
      // Offset by utteranceStartTime (≈ 0.001 s) so elapsed ms is correct.
      const baseTime = svc.utteranceStartTime;
      for (let w = 0; w < WORDS.length; w++) {
        const elapsedSec = (w * 100 + 50) / 1000; // midpoint of each word's window
        mockCurrentTime = baseTime + elapsedSec;

        const { highlight } = computeHighlight(active, src.getNowMs(), w);
        expect(highlight).not.toBeNull();
        expect(highlight!.wordIndex).toBe(w);
      }
    });

    it("PlaybackClock emits nowMs > 0 after first chunk is scheduled", () => {
      // utteranceStartTime ≈ 0.001 s (set in beforeEach).
      svc.beginUtterance(1);
      svc.enqueue(samples(200));

      // Advance 50 ms past utteranceStartTime.
      mockCurrentTime = svc.utteranceStartTime + 0.05;
      const src = createPlaybackSource({ current: svc });
      const { driver, advance } = createManualTickDriver();
      const clock = new DefaultPlaybackClock({ source: src, driver });

      const snaps: number[] = [];
      clock.subscribe((snap) => { snaps.push(snap.nowMs); });

      advance(); // trigger a tick
      // At least one snapshot must have nowMs > 0.
      expect(snaps.some((ms) => ms > 0)).toBe(true);
    });
  });

  // ── Wrong ordering — documents the bug ────────────────────────────────
  //
  // These tests intentionally demonstrate the broken state so that any
  // future refactor that re-introduces the old call order will fail loudly.

  describe("BROKEN ordering: beginUtterance AFTER enqueue (documents the bug)", () => {
    it("utteranceStartTime is -1 after the wrong call order", () => {
      mockCurrentTime = 1.0; // non-zero to make the error visible

      svc.enqueue(samples(400));           // ← WRONG: enqueue first
      // enqueue sets utteranceStartTime = 1.0 (correct at this point)…
      expect(svc.utteranceStartTime).toBeGreaterThan(0);

      svc.beginUtterance(1);              // ← WRONG: beginUtterance second
      // …but beginUtterance resets utteranceStartTime to -1 (the sentinel).
      expect(svc.utteranceStartTime).toBe(-1); // the bug
    });

    it("getPlayedDurationMs returns 0 with the wrong call order", () => {
      mockCurrentTime = 1.0;

      svc.enqueue(samples(400));           // wrong order
      svc.beginUtterance(1);               // wrong order — resets timing

      mockCurrentTime = 1.2; // 200 ms later
      // Despite audio playing, played duration is stuck at 0.
      expect(svc.getPlayedDurationMs()).toBe(0); // ← the bug manifesting
    });

    it("PlaybackSource.getNowMs is stuck at 0 with the wrong call order", () => {
      mockCurrentTime = 1.0;

      svc.enqueue(samples(400));
      svc.beginUtterance(1);               // wrong order

      mockCurrentTime = 1.3;
      const src = createPlaybackSource({ current: svc });
      expect(src.getNowMs()).toBe(0); // clock never advances
    });

    it("computeHighlight returns null when nowMs is stuck at 0", () => {
      mockCurrentTime = 1.0;

      svc.enqueue(samples(400));
      svc.beginUtterance(1);               // wrong order

      mockCurrentTime = 1.3;
      const src = createPlaybackSource({ current: svc });
      const table = tableForWords(["Hello", "world"], 400);
      const active: ActiveUtterance = { utteranceId: 1, table, entryIndex: 0 };

      const { highlight } = computeHighlight(active, src.getNowMs(), 0);
      // With the bug, highlight is null → UI renders <TypingDots> forever.
      expect(highlight).toBeNull(); // documents what the user sees with the bug
    });
  });
});
