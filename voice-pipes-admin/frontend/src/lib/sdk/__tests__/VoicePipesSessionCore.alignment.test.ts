import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";

// When a realtime provider supplies server-authoritative caption word
// timings (ElevenLabs alignment), the
// estimate-based paths (rt text-delta seeds, final-text fallback) must
// not mix into — or duplicate — the exact WordTimingTable, and caption
// text must not re-enter a transcript already fed by text deltas.

const h = vi.hoisted(() => ({ fake: null as unknown as ReturnType<typeof makeFakeTransport> }));

vi.mock("../../transport", async (importActual) => {
  const actual = await importActual<typeof import("../../transport")>();
  return { ...actual, createTransport: () => h.fake };
});

import { VoicePipesSessionCore } from "../VoicePipesSessionCore";
import type { ServerMessage } from "../../transport/types";

/** Drive the core's private message router with a hand-built frame. */
function deliver(core: VoicePipesSessionCore, msg: unknown) {
  core["handleMessage"](msg as ServerMessage);
}

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

const UTT = 3;

function rtTextDelta(core: VoicePipesSessionCore, text: string) {
  deliver(core, {
    kind: "message",
    message: {
      kind: "text",
      role: "assistant",
      text,
      payload: { audio_ms: 0 },
      turn_id: `rt-${UTT}`,
      message_id: `rt-asst-${UTT}-0`,
    },
  });
}

function alignedAudioChunk(core: VoicePipesSessionCore) {
  deliver(core, {
    kind: "audio",
    samples: new Int16Array(1600),
    seqNum: 1,
    endOfUtterance: false,
    meta: {
      utteranceId: UTT,
      total_fragment_duration_ms: 100,
      caption: {
        text: "Alpha beta",
        word_timings: [
          { word: "Alpha", start_ms: 0, end_ms: 40 },
          { word: "beta", start_ms: 50, end_ms: 80 },
        ],
      },
    },
  });
}

describe("VoicePipesSessionCore — authoritative caption word timings", () => {
  let core: VoicePipesSessionCore;
  let assistantDeltas: string[];

  beforeEach(async () => {
    h.fake = makeFakeTransport();
    core = new VoicePipesSessionCore();
    assistantDeltas = [];
    core.on("textDelta", (d: { role: string; text: string }) => {
      if (d.role === "assistant") assistantDeltas.push(d.text);
    });
    await core.connect("ws://test:8800/ws", "websocket");
  });

  afterEach(() => {
    core.destroy();
  });

  it("first authoritative caption evicts the estimate-seeded table", () => {
    rtTextDelta(core, "Alpha beta gamma");
    // The rt delta seeded an estimate fragment (3 words).
    expect(core["tables"].get(UTT)?.wordCount).toBe(3);

    alignedAudioChunk(core);
    const table = core["tables"].get(UTT);
    expect(table?.wordCount).toBe(2);
    expect(table?.timings.map((t) => t.word)).toEqual(["Alpha", "beta"]);
  });

  it("later estimate paths do not pollute an authoritative table", () => {
    rtTextDelta(core, "Alpha beta gamma");
    alignedAudioChunk(core);

    // A second rt delta for the same utterance must be ignored for
    // timing purposes...
    rtTextDelta(core, "Alpha beta gamma");
    expect(core["tables"].get(UTT)?.wordCount).toBe(2);

    // ...and so must the final-text fallback.
    core["recordFinalTextFallback"](UTT, "Alpha beta gamma", 5000);
    expect(core["tables"].get(UTT)?.wordCount).toBe(2);
  });

  it("caption text is not re-emitted into a text-delta-fed transcript", () => {
    rtTextDelta(core, "Alpha beta gamma");
    const before = assistantDeltas.length;
    alignedAudioChunk(core);
    // No new assistant text from the caption — timing only.
    expect(assistantDeltas.length).toBe(before);
  });

  it("caption arriving BEFORE the first text delta never feeds the transcript", () => {
    // ElevenLabs ordering: aligned audio chunks stream before the
    // agent_response text arrives. The first caption must not land in
    // the transcript as a phantom prefix ("Hello there! How canHello
    // there! How can I help you today?" — live-observed).
    // Live sessions have the FSM speaking id set by the time captions
    // arrive; the unit harness has no playback pipeline, so set it
    // directly — without it the emission path under test is inert.
    core["_speakingUtteranceId"] = UTT;
    alignedAudioChunk(core);
    expect(assistantDeltas).toEqual([]);

    rtTextDelta(core, "Alpha beta gamma");
    expect(assistantDeltas).toEqual(["Alpha beta gamma"]);

    // The caption still fed the timing table (words, not estimates),
    // and the late text delta didn't pollute it.
    expect(core["tables"].get(UTT)?.wordCount).toBe(2);
  });

  it("estimate paths still work for providers without alignment", () => {
    rtTextDelta(core, "Alpha beta gamma");
    expect(core["tables"].get(UTT)?.wordCount).toBe(3);
    core["recordFinalTextFallback"](77, "one two", 1000);
    expect(core["tables"].get(77)?.wordCount).toBe(2);
  });
});
