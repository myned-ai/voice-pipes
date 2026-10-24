import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";

// Karaoke reveal completeness: estimated word timings routinely place the
// tail of an utterance PAST the real audio end, while the playback clock is
// clamped to audio that actually played. Without an end-of-playback
// flush the last words stay dim forever — the next turn merely dismisses the
// partial view, which reads as "the rest highlights when I speak again".
// `handlePlaybackEnd` must reveal the remainder; barge-ins must not.

const h = vi.hoisted(() => ({ fake: null as unknown as ReturnType<typeof makeFakeTransport> }));

vi.mock("../../transport", async (importActual) => {
  const actual = await importActual<typeof import("../../transport")>();
  return { ...actual, createTransport: () => h.fake };
});

import { VoicePipesSessionCore } from "../VoicePipesSessionCore";

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
    sendControl() {},
    sendMessage() {},
  };
}

const UTT = 5;

/** Three words; the last STARTS past the real audio duration (1000 ms) —
 *  the shape cumulative estimate drift produces (server 15-chars/s cursor
 *  + client 250 ms/word floor + never-shrink stretching): the reveal
 *  condition is `start_ms <= nowMs`, and the clamped clock can never
 *  reach the drifted tail. `totalDurationMs` also overshoots, so the
 *  near-end shortcut (`totalDurationMs - 50`) is unreachable too. */
function seedOvershootTable(core: VoicePipesSessionCore, uttId = UTT) {
  const table = core["getOrCreateWordTimingTable"](uttId);
  table.append(
    [
      { word: "large", start_ms: 0, end_ms: 400 },
      { word: "language", start_ms: 400, end_ms: 900 },
      { word: "model", start_ms: 1100, end_ms: 1400 },
    ],
    1400,
  );
  return table;
}

describe("VoicePipesSessionCore — karaoke reveal completes at playback end", () => {
  let core: VoicePipesSessionCore;
  let highlights: Array<{ utteranceId: number; wordIndex: number }>;

  beforeEach(async () => {
    h.fake = makeFakeTransport();
    core = new VoicePipesSessionCore();
    highlights = [];
    core.on("wordHighlighted", (w: { utteranceId: number; wordIndex: number }) => {
      highlights.push({ utteranceId: w.utteranceId, wordIndex: w.wordIndex });
    });
    await core.connect("ws://test:8800/ws", "websocket");
  });

  afterEach(() => {
    core.destroy();
  });

  it("reveals the unreachable tail when playback ends naturally", () => {
    seedOvershootTable(core);

    // The clock can only ever reach the real audio end (1000 ms) — the
    // clamp in getPlayedDurationMs freezes it there.
    core["tickHighlight"](1000, UTT);
    expect(highlights.at(-1)?.wordIndex).toBe(1); // "model" (idx 2) unreachable

    core["handlePlaybackEnd"](UTT);
    expect(highlights.at(-1)).toEqual({ utteranceId: UTT, wordIndex: 2 });
  });

  it("emits nothing extra when the reveal already completed via ticks", () => {
    seedOvershootTable(core);
    core["tickHighlight"](1400, UTT); // hypothetical: clock reached the end
    core["tickHighlight"](1400, UTT);
    const count = highlights.length;

    core["handlePlaybackEnd"](UTT);
    expect(highlights.length).toBe(count);
  });

  it("a late playbackEnd never steals the highlight from a newer turn", () => {
    seedOvershootTable(core, UTT);
    seedOvershootTable(core, UTT + 1);
    core["tickHighlight"](500, UTT + 1); // newer turn owns the karaoke view

    const count = highlights.length;
    core["handlePlaybackEnd"](UTT);
    expect(highlights.length).toBe(count);
  });

  it("is a no-op for utterances without a timing table", () => {
    expect(() => core["handlePlaybackEnd"](99)).not.toThrow();
    expect(highlights).toEqual([]);
  });
});
