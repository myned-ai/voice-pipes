import { describe, it, expect, vi } from "vitest";
import { createExtensionSession } from "./session-facade";
import type { VoicePipesSessionCore } from "../sdk/VoicePipesSessionCore";

/** Minimal core stand-in: an emitter with the getters the façade delegates to. */
function makeFakeCore() {
  const listeners = new Map<string, Set<(...a: unknown[]) => void>>();
  const core = {
    sessionState: "active",
    pipelineState: "speaking",
    playbackService: { tag: "pb" },
    playbackClock: { tag: "clock" },
    activeUtteranceId: 4,
    on: vi.fn((ev: string, l: (...a: unknown[]) => void) => {
      if (!listeners.has(ev)) listeners.set(ev, new Set());
      listeners.get(ev)!.add(l);
    }),
    off: vi.fn((ev: string, l: (...a: unknown[]) => void) => { listeners.get(ev)?.delete(l); }),
    emit(ev: string, ...args: unknown[]) { listeners.get(ev)?.forEach((l) => l(...args)); },
    sendClientToolResult: vi.fn(),
  };
  return core;
}

describe("createExtensionSession", () => {
  it("getters delegate to the core (live, not snapshotted)", () => {
    const core = makeFakeCore();
    const s = createExtensionSession(core as unknown as VoicePipesSessionCore);
    expect(s.sessionState).toBe("active");
    expect(s.pipelineState).toBe("speaking");
    expect(s.playbackService).toBe(core.playbackService);
    expect(s.playbackClock).toBe(core.playbackClock);
    expect(s.activeUtteranceId).toBe(4);
    core.sessionState = "idle";
    core.activeUtteranceId = null as unknown as number;
    expect(s.sessionState).toBe("idle");
    expect(s.activeUtteranceId).toBeNull();
  });

  it("on() subscribes and returns a working unsubscribe", () => {
    const core = makeFakeCore();
    const s = createExtensionSession(core as unknown as VoicePipesSessionCore);
    const cb = vi.fn();
    const off = s.on("stateChanged", cb);
    core.emit("stateChanged", "listening");
    expect(cb).toHaveBeenCalledWith("listening");
    off();
    core.emit("stateChanged", "speaking");
    expect(cb).toHaveBeenCalledTimes(1);
    expect(core.off).toHaveBeenCalledWith("stateChanged", cb);
  });

  it("forwards playbackMetrics / metricsEmitted with what the core emits, and unsubscribes", () => {
    const core = makeFakeCore();
    const s = createExtensionSession(core as unknown as VoicePipesSessionCore);
    const pm = vi.fn();
    const me = vi.fn();
    const offPm = s.on("playbackMetrics", pm);
    s.on("metricsEmitted", me);
    const metrics = { utteranceId: 3, clientReceiveToPlayMs: 210, clientTtfaMs: 910, clientPlaybackLatencyMs: 260 };
    core.emit("playbackMetrics", metrics);
    expect(pm).toHaveBeenCalledWith(metrics);
    const env = { snapshot: { utterance_id: 3 }, clientTtfaMs: 910 };
    core.emit("metricsEmitted", "3", env);
    expect(me).toHaveBeenCalledWith("3", env);
    offPm();
    core.emit("playbackMetrics", metrics);
    expect(pm).toHaveBeenCalledTimes(1);
  });

  it("forwards audioAnnotation with the core's object, and unsubscribes", () => {
    const core = makeFakeCore();
    const s = createExtensionSession(core as unknown as VoicePipesSessionCore);
    const cb = vi.fn();
    const off = s.on("audioAnnotation", cb);
    const a = { utteranceId: 12, startMs: 160, durationMs: 160, kind: "blendshapes", extras: { fps: 30, frames: [[0.1]] } };
    core.emit("audioAnnotation", a);
    expect(cb).toHaveBeenCalledTimes(1);
    expect(cb.mock.calls[0][0]).toBe(a);
    off();
    core.emit("audioAnnotation", a);
    expect(cb).toHaveBeenCalledTimes(1);
    expect(core.off).toHaveBeenCalledWith("audioAnnotation", cb);
  });

  it("rejects events outside the forwarded allow-list", () => {
    const core = makeFakeCore();
    const s = createExtensionSession(core as unknown as VoicePipesSessionCore);
    expect(() => (s.on as (e: string, l: () => void) => void)("textDelta", () => {})).toThrow(/unknown event/);
    expect(core.on).not.toHaveBeenCalled();
  });

  it("sendClientToolResult forwards all arguments", () => {
    const core = makeFakeCore();
    const s = createExtensionSession(core as unknown as VoicePipesSessionCore);
    s.sendClientToolResult("c1", "completed", { ok: true }, false);
    expect(core.sendClientToolResult).toHaveBeenCalledWith("c1", "completed", { ok: true }, false);
  });
});
