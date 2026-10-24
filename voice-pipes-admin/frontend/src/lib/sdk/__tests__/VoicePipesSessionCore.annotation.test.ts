import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";

// The core turns `{ kind: "annotation" }` transport messages into the
// `audioAnnotation` event (+ an Event Log row) and drops
// annotations for an utterance that has been flushed, mirroring the audio
// path (`shouldDropAudio`).

const h = vi.hoisted(() => ({ fake: null as unknown as ReturnType<typeof makeFakeTransport> }));

vi.mock("../../transport", async (importActual) => {
  const actual = await importActual<typeof import("../../transport")>();
  return { ...actual, createTransport: () => h.fake };
});

import { VoicePipesSessionCore } from "../VoicePipesSessionCore";
import type { AudioAnnotation, ServerMessage } from "../../transport/types";

function makeFakeTransport() {
  return {
    _state: "disconnected" as "disconnected" | "connecting" | "connected",
    onMessage: null as null | ((msg: ServerMessage) => void),
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

function annotation(utteranceId: number, startMs = 160): AudioAnnotation {
  return {
    utteranceId,
    startMs,
    durationMs: 160,
    kind: "blendshapes",
    extras: { fps: 30, frames: [[0.1], [0.2]] },
    serverTimestampMs: 1724700000000,
  };
}

describe("VoicePipesSessionCore — audioAnnotation", () => {
  let core: VoicePipesSessionCore;
  let emitted: AudioAnnotation[];
  let events: { dir: string; type: string; summary: string; detail?: unknown }[];

  beforeEach(async () => {
    h.fake = makeFakeTransport();
    core = new VoicePipesSessionCore();
    await core.connect("ws://test:8800/ws", "websocket");
    emitted = [];
    events = [];
    core.on("audioAnnotation", (a) => emitted.push(a));
    core.on("eventsEmitted", (dir, type, summary, detail) => events.push({ dir, type, summary, detail }));
  });

  afterEach(() => {
    core.destroy();
  });

  it("emits audioAnnotation (same object) and an `annotation` Event Log row for a live utterance", () => {
    const a = annotation(12);
    h.fake.onMessage?.({ kind: "annotation", annotation: a });
    expect(emitted).toHaveLength(1);
    expect(emitted[0]).toBe(a);
    const row = events.find((e) => e.type === "annotation");
    expect(row).toBeDefined();
    expect(row!.dir).toBe("in");
    expect(row!.summary).toBe("annotation kind=blendshapes utt=12 start=160ms dur=160ms");
    expect(row!.detail).toBe(a);
  });

  it("drops annotations for a flushed utterance (mirrors shouldDropAudio) but keeps later utterances", () => {
    // Put utterance 12 into the flushed state the way clear_audio / cancel do.
    core["dispatchInterrupt"]({ type: "SERVER_CLEAR_AUDIO", utteranceId: 12, reason: "user_speech" });

    h.fake.onMessage?.({ kind: "annotation", annotation: annotation(12) });
    h.fake.onMessage?.({ kind: "annotation", annotation: annotation(12, 320) });
    expect(emitted).toHaveLength(0);
    expect(events.filter((e) => e.type === "annotation")).toHaveLength(0);

    // The next utterance is unaffected.
    const next = annotation(13, 0);
    h.fake.onMessage?.({ kind: "annotation", annotation: next });
    expect(emitted).toEqual([next]);
    expect(events.filter((e) => e.type === "annotation")).toHaveLength(1);
  });

  // Per annotation, `late = playbackClock.nowMs − startMs` when the clock
  // is on the annotation's utterance; count / max of
  // `late > 0` ride the turn's `playback_quality` frame.
  it("accumulates lateness (count / max) only for annotations behind the playhead of the utterance being played", () => {
    core["playbackClock_"] = {
      getUtteranceId: () => 12,
      getNowMs: () => 500,
      getTotalMs: () => 1000,
      isRunning: () => true,
      subscribe: () => () => {},
    };
    h.fake.onMessage?.({ kind: "annotation", annotation: annotation(12, 300) }); // 200 ms late
    h.fake.onMessage?.({ kind: "annotation", annotation: annotation(12, 450) }); // 50 ms late
    h.fake.onMessage?.({ kind: "annotation", annotation: annotation(12, 500) }); // on time
    h.fake.onMessage?.({ kind: "annotation", annotation: annotation(12, 800) }); // ahead
    h.fake.onMessage?.({ kind: "annotation", annotation: annotation(13, 0) });   // clock not on 13
    expect(core["annotationLate"]).toEqual({ utteranceId: 12, count: 2, maxMs: 200 });
    // Late annotations are still delivered — lateness is telemetry, not a filter.
    expect(emitted).toHaveLength(5);
  });

  it("never records lateness without a playback clock or for a flushed utterance", () => {
    core["playbackClock_"] = null;
    h.fake.onMessage?.({ kind: "annotation", annotation: annotation(12, 0) });
    expect(core["annotationLate"]).toBeNull();

    core["playbackClock_"] = { getUtteranceId: () => 12, getNowMs: () => 900, getTotalMs: () => 900, isRunning: () => true, subscribe: () => () => {} };
    core["dispatchInterrupt"]({ type: "SERVER_CLEAR_AUDIO", utteranceId: 12, reason: "user_speech" });
    h.fake.onMessage?.({ kind: "annotation", annotation: annotation(12, 0) });
    expect(core["annotationLate"]).toBeNull();
  });
});
