import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";

// Speech the server dropped (no words recognised, too short, or a recogniser
// that never answered) arrives as the `utterance_dropped` control. The core
// turns it into a system line for the transcript, so the Playground shows
// why nothing followed the user's speech instead of showing nothing at all.

const h = vi.hoisted(() => ({ fake: null as unknown as ReturnType<typeof makeFakeTransport> }));

vi.mock("../../transport", async (importActual) => {
  const actual = await importActual<typeof import("../../transport")>();
  return { ...actual, createTransport: () => h.fake };
});

import { VoicePipesSessionCore, describeDroppedUtterance } from "../VoicePipesSessionCore";
import type { ServerMessage } from "../../transport/types";

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

describe("VoicePipesSessionCore — utterance_dropped", () => {
  let core: VoicePipesSessionCore;
  let deltas: { text: string; role: string; isFinal: boolean; messageId?: string }[];
  let events: { dir: string; type: string; summary: string; detail?: unknown }[];

  beforeEach(async () => {
    h.fake = makeFakeTransport();
    core = new VoicePipesSessionCore();
    await core.connect("ws://test:8800/ws", "websocket");
    deltas = [];
    events = [];
    core.on("textDelta", (d) => deltas.push({ text: d.text, role: d.role, isFinal: d.isFinal, messageId: d.messageId }));
    core.on("eventsEmitted", (dir, type, summary, detail) => events.push({ dir, type, summary, detail }));
  });

  afterEach(() => {
    core.destroy();
  });

  it("puts a system line in the transcript and a row in the Event Log", () => {
    const control = { action: "utterance_dropped", utterance_id: 12, reason: "empty_transcript", audio_ms: 1254 } as const;
    h.fake.onMessage?.({ kind: "control", control });

    expect(deltas).toEqual([
      {
        text: "Heard 1.3 s of speech but recognised no words",
        role: "system",
        isFinal: true,
        messageId: "dropped-12",
      },
    ]);
    const row = events.find((e) => e.type === "control" && e.summary.startsWith("utterance_dropped"));
    expect(row).toBeDefined();
    expect(row!.dir).toBe("in");
    expect(row!.summary).toBe("utterance_dropped utterance=12 reason=empty_transcript audio_ms=1254");
    expect(row!.detail).toBe(control);
  });

  it("words each reason the server gives", () => {
    expect(describeDroppedUtterance("too_short", 400)).toBe("Heard 0.4 s of speech, too short to be a turn");
    expect(describeDroppedUtterance("stt_timeout", 15_200)).toBe(
      "The recogniser did not answer 15.2 s of speech and was restarted",
    );
    expect(describeDroppedUtterance("something_new", 900)).toBe("Heard 0.9 s of speech; no turn was made (something_new)");
  });
});
