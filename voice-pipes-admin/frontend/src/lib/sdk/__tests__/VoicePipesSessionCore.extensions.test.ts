import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";

// Playground Extensions: the core exposes `audioChunk` / `playbackChanged`
// events and a `playbackClock`
// getter so external renderers can bind to output audio without touching
// the core internals.

const h = vi.hoisted(() => ({ fake: null as unknown as ReturnType<typeof makeFakeTransport> }));

vi.mock("../../transport", async (importActual) => {
  const actual = await importActual<typeof import("../../transport")>();
  return { ...actual, createTransport: () => h.fake };
});

import { VoicePipesSessionCore } from "../VoicePipesSessionCore";
import type { AudioPlaybackService, PlaybackChunk } from "../../audio/playback";

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

function makeChunk(overrides: Partial<PlaybackChunk> = {}): PlaybackChunk {
  return {
    utteranceId: 1,
    samples: new Float32Array(160),
    startTime: 1.0,
    endTime: 1.5,
    meta: {},
    ...overrides,
  } as PlaybackChunk;
}

describe("VoicePipesSessionCore — extension events", () => {
  let core: VoicePipesSessionCore;

  beforeEach(() => {
    h.fake = makeFakeTransport();
    core = new VoicePipesSessionCore();
  });

  afterEach(() => {
    core.destroy();
  });

  it("createPlaybackService emits playbackChanged(pb) with playbackService/playbackClock readable", () => {
    const seen: Array<{ pb: AudioPlaybackService | null; svc: AudioPlaybackService | null; clock: unknown }> = [];
    core.on("playbackChanged", (pb) => {
      seen.push({ pb, svc: core.playbackService, clock: core.playbackClock });
    });
    const pb = core["createPlaybackService"](16000) as AudioPlaybackService;
    expect(seen).toHaveLength(1);
    expect(seen[0].pb).toBe(pb);
    expect(seen[0].svc).toBe(pb);
    expect(seen[0].clock).not.toBeNull();
    expect(core.playbackClock).not.toBeNull();
  });

  it("pb.onChunk emits audioChunk with the same object and still extends captionSync", () => {
    const pb = core["createPlaybackService"](16000) as AudioPlaybackService;
    const extend = vi.spyOn(core["captionSync"]!, "extendLastSentence");
    const got: PlaybackChunk[] = [];
    core.on("audioChunk", (c) => got.push(c));
    const chunk = makeChunk();
    pb.onChunk?.(chunk);
    expect(got).toHaveLength(1);
    expect(got[0]).toBe(chunk);
    expect(extend).toHaveBeenCalledWith(0.5);
  });

  it("stopAudioPipeline emits playbackChanged(null) exactly once, and not when there was no playback", () => {
    const nulls: unknown[] = [];
    core.on("playbackChanged", (pb) => { if (pb === null) nulls.push(pb); });
    core["stopAudioPipeline"]();
    expect(nulls).toHaveLength(0);
    core["createPlaybackService"](16000);
    core["stopAudioPipeline"]();
    expect(nulls).toHaveLength(1);
    expect(core.playbackService).toBeNull();
    expect(core.playbackClock).toBeNull();
    core["stopAudioPipeline"]();
    expect(nulls).toHaveLength(1);
  });

  it("a throwing audioChunk listener does not break other listeners or the caption path", () => {
    const pb = core["createPlaybackService"](16000) as AudioPlaybackService;
    const extend = vi.spyOn(core["captionSync"]!, "extendLastSentence");
    const err = vi.spyOn(console, "error").mockImplementation(() => {});
    const second = vi.fn();
    core.on("audioChunk", () => { throw new Error("boom"); });
    core.on("audioChunk", second);
    expect(() => pb.onChunk?.(makeChunk())).not.toThrow();
    expect(err).toHaveBeenCalled();
    expect(second).toHaveBeenCalledTimes(1);
    expect(extend).toHaveBeenCalledTimes(1);
    err.mockRestore();
  });
});
