import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import {
  EXTENSION_READY_TIMEOUT_MS,
  __resetExtensionReadiness,
  pendingExtensions,
  setExtensionReadiness,
  waitForExtensionsReady,
} from "./extensionRegistry";

/** Let the registry's `.then` settlement hooks run (they are microtasks). */
const flush = () => vi.advanceTimersByTimeAsync(0);

describe("extension readiness registry", () => {
  beforeEach(() => {
    vi.useFakeTimers();
    __resetExtensionReadiness();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it("(a) resolves immediately with empty lists when nothing is registered", async () => {
    const r = await waitForExtensionsReady();
    expect(r).toEqual({ ready: [], pending: [], waitedMs: 0 });
    expect(pendingExtensions()).toEqual([]);
  });

  it("(b) waits for a pending entry that resolves before the timeout", async () => {
    setExtensionReadiness("a", "Avatar", new Promise<void>((res) => setTimeout(res, 200)));
    expect(pendingExtensions()).toEqual([{ id: "a", name: "Avatar" }]);
    const p = waitForExtensionsReady();
    await vi.advanceTimersByTimeAsync(200);
    const r = await p;
    expect(r.ready).toEqual(["a"]);
    expect(r.pending).toEqual([]);
    expect(pendingExtensions()).toEqual([]);
  });

  it("(c) an entry that never resolves is reported pending after timeoutMs", async () => {
    setExtensionReadiness("a", "Avatar", new Promise<void>(() => {}));
    const p = waitForExtensionsReady(500);
    await vi.advanceTimersByTimeAsync(499);
    // still waiting
    let settled = false;
    void p.then(() => { settled = true; });
    await vi.advanceTimersByTimeAsync(0);
    expect(settled).toBe(false);
    await vi.advanceTimersByTimeAsync(1);
    const r = await p;
    expect(r.pending).toEqual(["a"]);
    expect(r.ready).toEqual([]);
    expect(pendingExtensions()).toEqual([{ id: "a", name: "Avatar" }]);
    expect(EXTENSION_READY_TIMEOUT_MS).toBe(10_000);
  });

  it("(d) a rejecting ready() counts as settled", async () => {
    setExtensionReadiness("a", "Avatar", Promise.reject(new Error("no webgl")));
    await flush();
    expect(pendingExtensions()).toEqual([]);
    const r = await waitForExtensionsReady();
    expect(r).toEqual({ ready: [], pending: [], waitedMs: 0 });
  });

  it("(e) setExtensionReadiness(id, name, null) removes the entry so a later wait ignores it", async () => {
    setExtensionReadiness("a", "Avatar", new Promise<void>(() => {}));
    expect(pendingExtensions()).toHaveLength(1);
    setExtensionReadiness("a", "Avatar", null);
    expect(pendingExtensions()).toEqual([]);
    const r = await waitForExtensionsReady(100);
    expect(r).toEqual({ ready: [], pending: [], waitedMs: 0 });
  });

  it("(f) pendingExtensions() reflects settlement per entry; wait reports ready vs pending separately", async () => {
    setExtensionReadiness("fast", "Fast", new Promise<void>((res) => setTimeout(res, 50)));
    setExtensionReadiness("slow", "Slow", new Promise<void>(() => {}));
    expect(pendingExtensions().map((p) => p.id)).toEqual(["fast", "slow"]);
    const p = waitForExtensionsReady(300);
    await vi.advanceTimersByTimeAsync(50);
    expect(pendingExtensions().map((p) => p.id)).toEqual(["slow"]);
    await vi.advanceTimersByTimeAsync(250);
    const r = await p;
    expect(r.ready).toEqual(["fast"]);
    expect(r.pending).toEqual(["slow"]);
  });
});
