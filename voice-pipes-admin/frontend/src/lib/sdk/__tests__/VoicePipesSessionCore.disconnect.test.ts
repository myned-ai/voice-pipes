import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import type { TransportCloseInfo, TransportState } from "../../transport/adapter";
import type { ServerMessage } from "../../transport/types";

// Shared handle so the hoisted vi.mock factory and the tests reference the same
// fake transport instance. `beforeEach` assigns a fake before any test runs, so
// the handle is never observed as null (same pattern as the sibling suites).
const h = vi.hoisted(() => ({ fake: null as unknown as ReturnType<typeof makeFakeTransport> }));

vi.mock("../../transport", async (importActual) => {
  const actual = await importActual<typeof import("../../transport")>();
  return { ...actual, createTransport: () => h.fake };
});

import { VoicePipesSessionCore, describeUnexpectedClose } from "../VoicePipesSessionCore";

/** Minimal controllable TransportAdapter stand-in. Tests drive `onStateChange`
 *  / `onMessage` directly to simulate transport events. */
function makeFakeTransport() {
  return {
    _state: "disconnected" as "disconnected" | "connecting" | "connected",
    onMessage: null as null | ((msg: ServerMessage) => void),
    onStateChange: null as null | ((state: TransportState, info?: TransportCloseInfo) => void),
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

describe("VoicePipesSessionCore — clean vs. unexpected close", () => {
  let core: VoicePipesSessionCore;
  let states: string[];
  let errors: Array<{ code: number; message: string }>;
  let terminations: string[];

  beforeEach(async () => {
    h.fake = makeFakeTransport();
    core = new VoicePipesSessionCore();
    states = [];
    errors = [];
    terminations = [];
    core.on("sessionStateChanged", (s) => states.push(s));
    core.on("errorRaised", (code, message) => errors.push({ code, message }));
    core.on("sessionTerminated", (reason) => terminations.push(reason));
    await core.connect("ws://test:8800/ws", "websocket");
    expect(core.sessionState).toBe("active");
  });

  afterEach(() => {
    core.destroy();
  });

  it("surfaces an error when the socket drops with no `closing` frame (abnormal)", () => {
    h.fake.onStateChange?.("disconnected", { code: 1006, wasClean: false });

    expect(core.sessionState).toBe("error");
    expect(states.at(-1)).toBe("error");
    expect(errors).toHaveLength(1);
    expect(errors[0].message).toMatch(/unexpectedly/i);
    expect(errors[0].message).toContain("1006");
    expect(core.errorMessage).toMatch(/unexpectedly/i);
    // The user is informed the session ended, not left silently idle.
    expect(terminations.at(-1)).toMatch(/unexpectedly/i);
  });

  it("goes idle (no error) after a server `closing` frame", () => {
    const closing: ServerMessage = {
      kind: "control",
      control: { action: "closing", reason: "session_ended" },
    };
    core["handleMessage"](closing);
    // The transport close that follows the closing frame.
    h.fake.onStateChange?.("disconnected", { code: 1000, wasClean: true });

    expect(core.sessionState).toBe("idle");
    expect(states).not.toContain("error");
    expect(errors).toHaveLength(0);
  });

  it("goes idle (no error) on a client-initiated disconnect", () => {
    core.disconnect();

    expect(core.sessionState).toBe("idle");
    expect(states).not.toContain("error");
    expect(errors).toHaveLength(0);
    expect(terminations.at(-1)).toMatch(/clean/i);
  });

  it("does not downgrade an already-surfaced fatal error to idle", () => {
    const fatal: ServerMessage = {
      kind: "control",
      control: { action: "error", code: "500", message: "boom", severity: "fatal" },
    };
    core["handleMessage"](fatal);
    // Fatal error triggers transport.disconnect() internally; simulate the
    // resulting close event arriving afterwards.
    h.fake.onStateChange?.("disconnected", { code: 1011 });

    expect(core.sessionState).toBe("error");
    expect(core.errorMessage).toBe("boom");
  });
});

describe("describeUnexpectedClose", () => {
  it("returns a base message with no info", () => {
    expect(describeUnexpectedClose()).toMatch(/closed unexpectedly/i);
  });

  it("appends the close code and reason when present", () => {
    const msg = describeUnexpectedClose({ code: 1006, reason: "going away" });
    expect(msg).toContain("1006");
    expect(msg).toContain("going away");
  });
});
