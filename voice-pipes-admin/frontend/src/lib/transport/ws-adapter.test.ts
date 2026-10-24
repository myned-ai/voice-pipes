import { describe, it, expect, vi } from "vitest";
import { WebSocketAdapter, TransportParseError, TransportHandlerError } from "./ws-adapter";
import { encodeBinaryFrame, makeAudioHeader, encodePcmI16 } from "./types";
import type { ServerMessage, AudioMeta } from "./types";

// Helpers to reach private members for unit testing.  TypeScript permits
// bracket access to `private` members with a literal key (but not a generic
// one), so each field gets an overload and the result stays typed against
// the adapter's own declarations — no widening to `any`.
function getPrivate(adapter: WebSocketAdapter, property: "pendingAudioMetas"): WebSocketAdapter["pendingAudioMetas"];
function getPrivate(adapter: WebSocketAdapter, property: "lastPoppedFragmentIdx"): WebSocketAdapter["lastPoppedFragmentIdx"];
function getPrivate(adapter: WebSocketAdapter, property: "lastPoppedUtteranceId"): WebSocketAdapter["lastPoppedUtteranceId"];
function getPrivate(
  adapter: WebSocketAdapter,
  property: "pendingAudioMetas" | "lastPoppedFragmentIdx" | "lastPoppedUtteranceId",
): AudioMeta[] | number | null {
  switch (property) {
    case "pendingAudioMetas":
      return adapter["pendingAudioMetas"];
    case "lastPoppedFragmentIdx":
      return adapter["lastPoppedFragmentIdx"];
    case "lastPoppedUtteranceId":
      return adapter["lastPoppedUtteranceId"];
  }
}

function handleText(adapter: WebSocketAdapter, data: string): void {
  adapter["handleText"](data);
}

function handleBinary(adapter: WebSocketAdapter, data: ArrayBuffer): void {
  adapter["handleBinary"](data);
}

describe("WebSocketAdapter protocol mapping", () => {
  it("maps top-level audio_meta snake_case fields to camelCase properties", () => {
    const adapter = new WebSocketAdapter();
    const emittedMessages: ServerMessage[] = [];
    adapter.onMessage = (msg) => emittedMessages.push(msg);

    // Provide top-level audio_meta as spec'd
    const rawMeta = JSON.stringify({
      type: "audio_meta",
      payload: {
        text: "Hello world",
        utterance_id: 42,
        full_text: "Hello world from complete response"
      }
    });

    // using private access to test the internal mapper logic directly
    handleText(adapter,rawMeta);

    // It should be buffered, let's inspect the internal buffer
    const pendingMetas: AudioMeta[] = getPrivate(adapter, "pendingAudioMetas");
    expect(pendingMetas.length).toBe(1);
    expect(pendingMetas[0]).toEqual({
      caption: { text: "Hello world", estimated_duration_ms: undefined, word_timings: undefined },
      utteranceId: 42,
      fullText: "Hello world from complete response"
    });
  });

  it("merges the oldest pending audio_meta payload with the subsequent binary frame and clears the stash", () => {
    const adapter = new WebSocketAdapter();
    const emittedMessages: ServerMessage[] = [];
    adapter.onMessage = (msg) => emittedMessages.push(msg);

    // Pre-seed an audio meta
    handleText(adapter,JSON.stringify({
      type: "audio_meta",
      payload: { text: "Test", utterance_id: 99 }
    }));

    // Pre-seed a second audio_meta to ensure FIFO popping
    handleText(adapter,JSON.stringify({
      type: "audio_meta",
      payload: { text: "Test two", utterance_id: 99 }
    }));

    // Create a dummy binary frame with 10 samples
    const samples = new Int16Array(10);
    const audioBytes = encodePcmI16(samples);
    const header = makeAudioHeader(0, false, false);
    const frameBuffer = encodeBinaryFrame(header, audioBytes);

    // Feed to the parser
    handleBinary(adapter,frameBuffer);

    expect(emittedMessages.length).toBe(1);
    const audioMsg = emittedMessages[0];
    expect(audioMsg.kind).toBe("audio");
    if (audioMsg.kind === "audio") {
      expect(audioMsg.meta.caption?.text).toBe("Test");
      expect(audioMsg.meta.utteranceId).toBe(99);
      // Math: (10 / 48000) * 1000 = 0.20833333333333334
      expect(audioMsg.meta.durationMs).toBeCloseTo((10 / 48000) * 1000, 4);
    }

    // Stash should now only have the second meta
    const pendingMetas: AudioMeta[] = getPrivate(adapter, "pendingAudioMetas");
    expect(pendingMetas.length).toBe(1);
    expect(pendingMetas[0].caption?.text).toBe("Test two");
  });

  it("generates a synthetic text frame if full_text is provided and endOfUtterance is true", () => {
    const adapter = new WebSocketAdapter();
    const emittedMessages: ServerMessage[] = [];
    adapter.onMessage = (msg) => emittedMessages.push(msg);

    // Provide EOU top-level audio_meta
    handleText(adapter,JSON.stringify({
      type: "audio_meta",
      payload: {
        text: "Last word.",
        utterance_id: 11,
        full_text: "Full response string"
      }
    }));

    // Dummy empty frame marked EOU
    const audioBytes = encodePcmI16(new Int16Array(0));
    const header = makeAudioHeader(1, true, false); // EOU = true
    const frameBuffer = encodeBinaryFrame(header, audioBytes);

    handleBinary(adapter,frameBuffer);

    // Expect an audio frame followed by a text frame containing full_text
    expect(emittedMessages.length).toBe(2);
    expect(emittedMessages[0].kind).toBe("audio");
    
    const textMsg = emittedMessages[1];
    expect(textMsg.kind).toBe("text");
    if (textMsg.kind === "text") {
      expect(textMsg.text).toBe("Full response string");
      expect(textMsg.messageId).toBe("llm-11");
    }
  });

  it("ignores legacy tunneled audio_meta under control (strictness check)", () => {
    const adapter = new WebSocketAdapter();
    const emittedMessages: ServerMessage[] = [];
    adapter.onMessage = (msg) => emittedMessages.push(msg);

    const legacyPayload = JSON.stringify({
      type: "control",
      payload: {
        action: "audio_meta",
        text: "Should be dropped"
      }
    });

    handleText(adapter,legacyPayload);
    const pendingMetas: AudioMeta[] = getPrivate(adapter, "pendingAudioMetas");
    expect(pendingMetas.length).toBe(0);
    
    // It should just emit a control event, no pending meta
    expect(emittedMessages.length).toBe(1);
    expect(emittedMessages[0].kind).toBe("control");
  });
});

// ── Regression: parse vs handler error differentiation ─────────────────────
//
// Previously, both malformed server frames (a server/network problem) and
// onMessage handlers throwing (a client bug) were caught by the same generic
// try/catch and surfaced via a single `onError(err)` call.  Logs gave no way
// to tell "the server is sending garbage" from "my reducer crashed".
// New behaviour: parse failures throw `TransportParseError`, handler failures
// throw `TransportHandlerError`.  Both still flow through onError, but the
// concrete subclass identifies the failure layer.

describe("WebSocketAdapter error classification", () => {
  it("throws TransportParseError when JSON is malformed", () => {
    const adapter = new WebSocketAdapter();
    expect(() => handleText(adapter,"{ this is not json")).toThrow(TransportParseError);
  });

  it("throws TransportParseError when the binary frame is too short", () => {
    const adapter = new WebSocketAdapter();
    // A 2-byte buffer cannot contain a valid frame header.
    const tooShort = new ArrayBuffer(2);
    expect(() => handleBinary(adapter,tooShort)).toThrow(TransportParseError);
  });

  it("throws TransportHandlerError when an onMessage handler throws on a text frame", () => {
    const adapter = new WebSocketAdapter();
    adapter.onMessage = () => {
      throw new Error("reducer crashed");
    };
    const frame = JSON.stringify({ type: "control", payload: { action: "noop" } });
    expect(() => handleText(adapter,frame)).toThrow(TransportHandlerError);
  });

  it("throws TransportHandlerError when an onMessage handler throws on a binary frame", () => {
    const adapter = new WebSocketAdapter();
    adapter.onMessage = () => {
      throw new Error("audio dispatch crashed");
    };
    const samples = new Int16Array(10);
    const frameBuffer = encodeBinaryFrame(makeAudioHeader(0, false, false), encodePcmI16(samples));
    expect(() => handleBinary(adapter,frameBuffer)).toThrow(TransportHandlerError);
  });

  it("preserves the original error as `cause` on TransportParseError", () => {
    const adapter = new WebSocketAdapter();
    let caught: unknown;
    try {
      handleText(adapter,"not json");
    } catch (err) {
      caught = err;
    }
    expect(caught).toBeInstanceOf(TransportParseError);
    expect((caught as TransportParseError).cause).toBeInstanceOf(SyntaxError);
  });

  it("preserves the original error as `cause` on TransportHandlerError", () => {
    const adapter = new WebSocketAdapter();
    const original = new Error("boom");
    adapter.onMessage = () => {
      throw original;
    };
    let caught: unknown;
    try {
      handleText(adapter,JSON.stringify({ type: "control", payload: {} }));
    } catch (err) {
      caught = err;
    }
    expect(caught).toBeInstanceOf(TransportHandlerError);
    expect((caught as TransportHandlerError).cause).toBe(original);
  });
});

describe("WebSocketAdapter desync defenses", () => {
  function pushMeta(adapter: WebSocketAdapter, payload: Record<string, unknown>) {
    handleText(adapter,
      JSON.stringify({ type: "audio_meta", payload }),
    );
  }

  function pushBinary(adapter: WebSocketAdapter, samples = 10) {
    const audioBytes = encodePcmI16(new Int16Array(samples));
    const frame = encodeBinaryFrame(makeAudioHeader(0, false, false), audioBytes);
    handleBinary(adapter,frame);
  }

  it("disconnect() clears pendingAudioMetas so a reused adapter does not start with stale entries", () => {
    const adapter = new WebSocketAdapter();
    adapter.onMessage = () => {};

    pushMeta(adapter, { text: "stale", utterance_id: 1 });
    pushMeta(adapter, { text: "stale-2", utterance_id: 1 });
    expect(getPrivate(adapter, "pendingAudioMetas").length).toBe(2);

    adapter.disconnect();

    const queue = getPrivate(adapter, "pendingAudioMetas");
    expect(queue.length).toBe(0);
    expect(getPrivate(adapter, "lastPoppedFragmentIdx")).toBeNull();
    expect(getPrivate(adapter, "lastPoppedUtteranceId")).toBeNull();
  });

  it("warns and bounds memory when pendingAudioMetas exceeds the soft limit", () => {
    const adapter = new WebSocketAdapter();
    adapter.onMessage = () => {};
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});

    // Soft limit is 64 — push 70 metas without consuming any binary frame.
    for (let i = 0; i < 70; i++) {
      pushMeta(adapter, { text: `frag-${i}`, utterance_id: 1, fragment_idx: i });
    }

    // Queue should be capped at the soft limit (oldest entries dropped).
    const queue = getPrivate(adapter, "pendingAudioMetas");
    expect(queue.length).toBe(64);
    // Newest entries are preserved; oldest dropped.
    expect(queue[queue.length - 1].caption?.text).toBe("frag-69");
    // Warning logged exactly once per overflow burst.
    expect(warnSpy).toHaveBeenCalledTimes(1);
    expect(warnSpy.mock.calls[0][0]).toMatch(/exceeded soft limit/);

    warnSpy.mockRestore();
  });

  it("warns when a binary frame arrives with an empty pendingAudioMetas queue", () => {
    const adapter = new WebSocketAdapter();
    adapter.onMessage = () => {};
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});

    pushBinary(adapter);

    expect(warnSpy).toHaveBeenCalledTimes(1);
    expect(warnSpy.mock.calls[0][0]).toMatch(/empty pendingAudioMetas queue/);

    warnSpy.mockRestore();
  });

  it("warns on non-monotonic fragment_idx within the same utterance (queue desync signal)", () => {
    const adapter = new WebSocketAdapter();
    adapter.onMessage = () => {};
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});

    // Push 3 metas in order, then re-order one of them by hand to simulate desync.
    pushMeta(adapter, { text: "a", utterance_id: 7, fragment_idx: 0 });
    pushMeta(adapter, { text: "b", utterance_id: 7, fragment_idx: 1 });
    pushMeta(adapter, { text: "c", utterance_id: 7, fragment_idx: 2 });
    // Tamper: swap last two entries → simulates a meta reorder bug.
    const queue = getPrivate(adapter, "pendingAudioMetas");
    [queue[1], queue[2]] = [queue[2], queue[1]];

    pushBinary(adapter); // pops fragment_idx=0 → ok
    pushBinary(adapter); // pops fragment_idx=2 → ok (becomes new highWatermark)
    pushBinary(adapter); // pops fragment_idx=1 → 1 < 2 → warns

    const nonMonotonicWarn = warnSpy.mock.calls.find((c) =>
      String(c[0]).includes("non-monotonic"),
    );
    expect(nonMonotonicWarn).toBeDefined();
    warnSpy.mockRestore();
  });

  it("does NOT warn on fragment_idx reset across utterance boundaries", () => {
    const adapter = new WebSocketAdapter();
    adapter.onMessage = () => {};
    const warnSpy = vi.spyOn(console, "warn").mockImplementation(() => {});

    pushMeta(adapter, { text: "u1-a", utterance_id: 1, fragment_idx: 0 });
    pushMeta(adapter, { text: "u1-b", utterance_id: 1, fragment_idx: 1 });
    pushMeta(adapter, { text: "u2-a", utterance_id: 2, fragment_idx: 0 });

    pushBinary(adapter);
    pushBinary(adapter);
    pushBinary(adapter);

    const nonMonotonicWarn = warnSpy.mock.calls.find((c) =>
      String(c[0]).includes("non-monotonic"),
    );
    expect(nonMonotonicWarn).toBeUndefined();
    warnSpy.mockRestore();
  });

  it("passes audio_meta.extras and server_timestamp_ms through to the paired binary frame", () => {
    const adapter = new WebSocketAdapter();
    const out: ServerMessage[] = [];
    adapter.onMessage = (m) => out.push(m);
    adapter["outputSampleRate"] = 16000;
    handleText(adapter,JSON.stringify({ type: "audio_meta", payload: {
      utterance_id: 7, fragment_idx: 0, server_timestamp_ms: 1700000000000,
      extras: { blendshapes: { fps: 30, frames: [[0.1, 0.2]] } },
    }}));
    handleBinary(adapter,encodeBinaryFrame(makeAudioHeader(1), encodePcmI16(new Int16Array(640))));
    const audio = out.find((m) => m.kind === "audio") as Extract<ServerMessage, { kind: "audio" }>;
    expect(audio.meta.extras).toEqual({ blendshapes: { fps: 30, frames: [[0.1, 0.2]] } });
    expect(audio.meta.serverTimestampMs).toBe(1700000000000);
  });

  it("does not inherit extras onto continuation chunks (sticky meta strips extras like caption)", () => {
    const adapter = new WebSocketAdapter();
    const out: ServerMessage[] = [];
    adapter.onMessage = (m) => out.push(m);
    handleText(adapter,JSON.stringify({ type: "audio_meta", payload: {
      utterance_id: 7, fragment_idx: 0, extras: { blendshapes: { a: 1 } },
    }}));
    handleBinary(adapter,encodeBinaryFrame(makeAudioHeader(1), encodePcmI16(new Int16Array(10))));
    handleBinary(adapter,encodeBinaryFrame(makeAudioHeader(2), encodePcmI16(new Int16Array(10))));
    const audios = out.filter((m) => m.kind === "audio") as Extract<ServerMessage, { kind: "audio" }>[];
    expect(audios.length).toBe(2);
    expect(audios[0].meta.extras).toEqual({ blendshapes: { a: 1 } });
    const second = audios[1];
    expect(second.meta.utteranceId).toBe(7);
    expect(second.meta.extras).toBeUndefined();
  });

  it("ignores non-object extras", () => {
    const adapter = new WebSocketAdapter();
    const out: ServerMessage[] = [];
    adapter.onMessage = (m) => out.push(m);
    handleText(adapter,JSON.stringify({ type: "audio_meta", payload: { utterance_id: 1, extras: "oops" } }));
    handleBinary(adapter,encodeBinaryFrame(makeAudioHeader(1), encodePcmI16(new Int16Array(10))));
    const audio = out.find((m) => m.kind === "audio") as Extract<ServerMessage, { kind: "audio" }>;
    expect(audio.meta.extras).toBeUndefined();
  });
});

// `audio_annotation` is its own message (not paired with a binary frame)
// mapped snake→camel like `audio_meta`.
describe("WebSocketAdapter audio_annotation mapping", () => {
  it("maps a valid audio_annotation frame to { kind: 'annotation' } with camelCase fields and extras", () => {
    const adapter = new WebSocketAdapter();
    const out: ServerMessage[] = [];
    adapter.onMessage = (m) => out.push(m);
    handleText(adapter, JSON.stringify({
      type: "audio_annotation",
      payload: {
        utterance_id: 12, start_ms: 160, duration_ms: 160, kind: "blendshapes",
        extras: { fps: 30, names: ["jawOpen"], frames: [[0.1], [0.2]] },
        server_timestamp_ms: 1724700000000,
      },
    }));
    expect(out).toHaveLength(1);
    expect(out[0]).toEqual({
      kind: "annotation",
      annotation: {
        utteranceId: 12, startMs: 160, durationMs: 160, kind: "blendshapes",
        extras: { fps: 30, names: ["jawOpen"], frames: [[0.1], [0.2]] },
        serverTimestampMs: 1724700000000,
      },
    });
    // No pending audio_meta side effect: annotations never pair with binary frames.
    expect(getPrivate(adapter, "pendingAudioMetas")).toHaveLength(0);

    // Absent / non-object extras default to {} and server_timestamp_ms stays optional.
    handleText(adapter, JSON.stringify({
      type: "audio_annotation",
      payload: { utterance_id: 13, start_ms: 0, duration_ms: 80, kind: "visemes", extras: "nope" },
    }));
    const second = out[1] as Extract<ServerMessage, { kind: "annotation" }>;
    expect(second.annotation.extras).toEqual({});
    expect("serverTimestampMs" in second.annotation).toBe(false);
  });

  it("drops a malformed audio_annotation frame with a console.warn instead of emitting", () => {
    const adapter = new WebSocketAdapter();
    const out: ServerMessage[] = [];
    adapter.onMessage = (m) => out.push(m);
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const bad = [
      { utterance_id: "12", start_ms: 160, duration_ms: 160, kind: "blendshapes" }, // id not a number
      { utterance_id: 12, duration_ms: 160, kind: "blendshapes" },                  // start_ms missing
      { utterance_id: 12, start_ms: 160, duration_ms: null, kind: "blendshapes" },  // duration not a number
      { utterance_id: 12, start_ms: 160, duration_ms: 160, kind: 7 },               // kind not a string
    ];
    for (const payload of bad) {
      expect(() => handleText(adapter, JSON.stringify({ type: "audio_annotation", payload }))).not.toThrow();
    }
    expect(out).toHaveLength(0);
    expect(warn).toHaveBeenCalledTimes(bad.length);
    expect(String(warn.mock.calls[0][0])).toMatch(/malformed audio_annotation/);
    warn.mockRestore();
  });
});

// The `playback_quality` control frame carries the arrival span, the
// interrupted flag and the annotation-lateness
// counters next to the underrun gap-list fields.
describe("WebSocketAdapter.sendPlaybackQuality wire mapping", () => {
  it("maps every PlaybackQuality field to its snake_case wire name", () => {
    const adapter = new WebSocketAdapter();
    const sendControl = vi.spyOn(adapter, "sendControl").mockImplementation(() => {});
    adapter.sendPlaybackQuality({
      utteranceId: 4,
      underrunCount: 2,
      totalSilenceMs: 550,
      audioMs: 36285,
      heardMs: 36000,
      wallMs: 36835,
      arrivalWallMs: 60000,
      interrupted: true,
      annotationLateCount: 3,
      annotationLateMsMax: 420,
      gaps: [{ atAudioMs: 100, silenceMs: 300 }, { atAudioMs: 200, silenceMs: 250 }],
    });
    expect(sendControl).toHaveBeenCalledTimes(1);
    expect(sendControl.mock.calls[0][0]).toEqual({
      action: "playback_quality",
      utterance_id: 4,
      underrun_count: 2,
      total_silence_ms: 550,
      audio_ms: 36285,
      heard_ms: 36000,
      wall_ms: 36835,
      arrival_wall_ms: 60000,
      interrupted: true,
      annotation_late_count: 3,
      annotation_late_ms_max: 420,
      gaps: [{ at_audio_ms: 100, silence_ms: 300 }, { at_audio_ms: 200, silence_ms: 250 }],
    });
  });
});
