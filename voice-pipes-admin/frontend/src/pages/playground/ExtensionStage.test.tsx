import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { StrictMode } from "react";
import { render, screen, act, fireEvent, cleanup, waitFor } from "@testing-library/react";
import { ThemeContext } from "../../contexts/ThemeContext";
import type { PlaygroundExtension, PlaygroundExtensionHost, PlaygroundExtensionSession } from "../../lib/extensions";

// Loader is mocked so the stage tests never touch fetch / dynamic import().
const h = vi.hoisted(() => ({
  entries: [] as { url: string; options?: Record<string, unknown> }[],
  registry: new Map<string, PlaygroundExtension>(),
}));
vi.mock("../../lib/extensions", async (importActual) => {
  const actual = await importActual<typeof import("../../lib/extensions")>();
  return {
    ...actual,
    discoverExtensionEntries: async () => h.entries,
    loadExtensions: async (entries: { url: string; options?: Record<string, unknown> }[]) => {
      const loaded = [] as { entry: (typeof entries)[number]; extension: PlaygroundExtension; baseUrl: string }[];
      const failed = [] as { entry: (typeof entries)[number]; error: string }[];
      for (const entry of entries) {
        const ext = h.registry.get(entry.url);
        if (ext) loaded.push({ entry, extension: ext, baseUrl: entry.url });
        else failed.push({ entry, error: `cannot load ${entry.url}` });
      }
      return { loaded, failed };
    },
  };
});

import ExtensionStage from "./ExtensionStage";
import { __resetExtensionCache, __resetExtensionReadiness, pendingExtensions } from "./extensionRegistry";

const session = {
  sessionState: "idle",
  pipelineState: null,
  playbackService: null,
  playbackClock: null,
  activeUtteranceId: null,
  on: () => () => {},
  sendClientToolResult: () => {},
} as unknown as PlaygroundExtensionSession;

/** Controllable ResizeObserver stub — tests fire it manually. */
let observers: Array<{ cb: ResizeObserverCallback; el: Element | null }> = [];
let rectSize = { width: 400, height: 300 };

function makeExt(id: string, overrides: Partial<PlaygroundExtension> = {}) {
  const ext: PlaygroundExtension & { hosts: PlaygroundExtensionHost[] } = {
    id,
    name: id.toUpperCase(),
    hosts: [],
    mount: vi.fn(function (this: void, _el: HTMLElement, host: PlaygroundExtensionHost) { ext.hosts.push(host); }),
    unmount: vi.fn(),
    ...overrides,
  };
  return ext;
}

function renderStage(opts: { strict?: boolean; isDark?: boolean; onClose?: () => void } = {}) {
  const ui = (
    <ThemeContext.Provider value={{ theme: opts.isDark === false ? "light" : "dark", isDark: opts.isDark !== false, toggleTheme: () => {} }}>
      <ExtensionStage session={session} onClose={opts.onClose ?? (() => {})} />
    </ThemeContext.Provider>
  );
  return render(opts.strict ? <StrictMode>{ui}</StrictMode> : ui);
}

beforeEach(() => {
  __resetExtensionCache();
  __resetExtensionReadiness();
  h.entries = [];
  h.registry.clear();
  observers = [];
  rectSize = { width: 400, height: 300 };
  vi.stubGlobal(
    "ResizeObserver",
    class {
      cb: ResizeObserverCallback;
      constructor(cb: ResizeObserverCallback) { this.cb = cb; observers.push({ cb, el: null }); }
      observe(el: Element) { const o = observers.find((x) => x.cb === this.cb); if (o) o.el = el; }
      disconnect() { observers = observers.filter((x) => x.cb !== this.cb); }
      unobserve() {}
    },
  );
  Element.prototype.getBoundingClientRect = () =>
    ({ ...rectSize, x: 0, y: 0, top: 0, left: 0, right: rectSize.width, bottom: rectSize.height, toJSON() {} }) as DOMRect;
  localStorage.clear();
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("ExtensionStage", () => {
  it("mounts the active extension exactly once under StrictMode and unmounts once on teardown", async () => {
    const a = makeExt("a");
    h.registry.set("builtin:a", a);
    h.entries = [{ url: "builtin:a", options: { k: 1 } }];
    const r = renderStage({ strict: true });
    await waitFor(() => expect(a.mount).toHaveBeenCalledTimes(1));
    expect(a.hosts[0].options).toEqual({ k: 1 });
    expect(a.hosts[0].baseUrl).toBe("builtin:a");
    expect(a.hosts[0].session).toBe(session);
    r.unmount();
    await waitFor(() => expect(a.unmount).toHaveBeenCalledTimes(1));
  });

  it("switching tabs unmounts the previous extension and mounts the next", async () => {
    const a = makeExt("a");
    const b = makeExt("b");
    h.registry.set("u:a", a);
    h.registry.set("u:b", b);
    h.entries = [{ url: "u:a" }, { url: "u:b" }];
    renderStage();
    await waitFor(() => expect(a.mount).toHaveBeenCalledTimes(1));
    expect(b.mount).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "B" }));
    await waitFor(() => expect(b.mount).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(a.unmount).toHaveBeenCalledTimes(1));
  });

  it("renders a chip for entries the loader failed on, without affecting loaded ones", async () => {
    const a = makeExt("a");
    h.registry.set("u:a", a);
    h.entries = [{ url: "u:a" }, { url: "/ext/broken/mod.js" }];
    renderStage();
    await waitFor(() => expect(a.mount).toHaveBeenCalledTimes(1));
    const chip = screen.getByTestId("extension-failed");
    expect(chip.textContent).toContain("mod.js");
    expect(chip.getAttribute("title")).toContain("cannot load");
  });

  it("a rejecting mount() renders an error chip and does not throw", async () => {
    const a = makeExt("a", { mount: vi.fn(async () => { throw new Error("no webgl"); }) });
    h.registry.set("u:a", a);
    h.entries = [{ url: "u:a" }];
    const err = vi.spyOn(console, "error").mockImplementation(() => {});
    renderStage();
    await waitFor(() => expect(screen.getByTestId("extension-mount-error").textContent).toContain("no webgl"));
    expect(err).toHaveBeenCalled();
    err.mockRestore();
  });

  it("defers mount until the container has size, then mounts when the ResizeObserver fires", async () => {
    const a = makeExt("a");
    h.registry.set("u:a", a);
    h.entries = [{ url: "u:a" }];
    rectSize = { width: 0, height: 0 };
    renderStage();
    // Wait for the loader result to render the mount root (observer registered).
    await waitFor(() => expect(observers.length).toBeGreaterThan(0));
    await act(async () => { await Promise.resolve(); });
    expect(a.mount).not.toHaveBeenCalled();
    rectSize = { width: 500, height: 200 };
    await act(async () => {
      observers.forEach((o) => o.cb([{ contentRect: { width: 500, height: 200 } } as ResizeObserverEntry], {} as ResizeObserver));
      await Promise.resolve();
    });
    await waitFor(() => expect(a.mount).toHaveBeenCalledTimes(1));
  });

  it("theme changes are pushed to host.theme.onChange listeners", async () => {
    const a = makeExt("a");
    h.registry.set("u:a", a);
    h.entries = [{ url: "u:a" }];
    const seen: boolean[] = [];
    const ui = (dark: boolean) => (
      <ThemeContext.Provider value={{ theme: dark ? "dark" : "light", isDark: dark, toggleTheme: () => {} }}>
        <ExtensionStage session={session} onClose={() => {}} />
      </ThemeContext.Provider>
    );
    const r = render(ui(true));
    await waitFor(() => expect(a.mount).toHaveBeenCalledTimes(1));
    expect(a.hosts[0].theme.isDark).toBe(true);
    a.hosts[0].theme.onChange((d) => seen.push(d));
    r.rerender(ui(false));
    await waitFor(() => expect(seen).toEqual([false]));
    expect(a.hosts[0].theme.isDark).toBe(false);
  });

  it("awaits unmount when mount() was still pending at cleanup (never leaves a mounted orphan)", async () => {
    let resolveMount: () => void = () => {};
    const a = makeExt("a", {
      mount: vi.fn(() => new Promise<void>((res) => { resolveMount = res; })),
    });
    h.registry.set("u:a", a);
    h.entries = [{ url: "u:a" }];
    const r = renderStage();
    await waitFor(() => expect(a.mount).toHaveBeenCalledTimes(1));
    r.unmount();
    expect(a.unmount).not.toHaveBeenCalled();
    resolveMount();
    await waitFor(() => expect(a.unmount).toHaveBeenCalledTimes(1));
  });

  it("manifest `disabled` entries are listed but not auto-mounted; clicking the tab mounts them", async () => {
    const a = makeExt("a");
    const b = makeExt("b");
    h.registry.set("u:a", a);
    h.registry.set("u:b", b);
    h.entries = [{ url: "u:a", disabled: true } as { url: string }, { url: "u:b" }];
    renderStage();
    await waitFor(() => expect(b.mount).toHaveBeenCalledTimes(1));
    expect(a.mount).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "A" }));
    await waitFor(() => expect(a.mount).toHaveBeenCalledTimes(1));
  });

  describe("readiness gate", () => {
    it("(a) an extension with ready() resolving later is pending until it resolves", async () => {
      let resolveReady: () => void = () => {};
      const a = makeExt("a", { ready: vi.fn(() => new Promise<void>((res) => { resolveReady = res; })) });
      h.registry.set("u:a", a);
      h.entries = [{ url: "u:a" }];
      renderStage();
      await waitFor(() => expect(a.mount).toHaveBeenCalledTimes(1));
      await waitFor(() => expect(a.ready).toHaveBeenCalledTimes(1));
      expect(pendingExtensions()).toEqual([{ id: "a", name: "A" }]);
      resolveReady();
      await waitFor(() => expect(pendingExtensions()).toEqual([]));
    });

    it("(b) an extension without ready() is never listed as pending", async () => {
      const a = makeExt("a");
      h.registry.set("u:a", a);
      h.entries = [{ url: "u:a" }];
      renderStage();
      await waitFor(() => expect(a.mount).toHaveBeenCalledTimes(1));
      await act(async () => { await Promise.resolve(); });
      expect(pendingExtensions()).toEqual([]);
    });

    it("(c) unmount removes a still-pending extension from the gate", async () => {
      const a = makeExt("a", { ready: () => new Promise<void>(() => {}) });
      const b = makeExt("b");
      h.registry.set("u:a", a);
      h.registry.set("u:b", b);
      h.entries = [{ url: "u:a" }, { url: "u:b" }];
      renderStage();
      await waitFor(() => expect(a.mount).toHaveBeenCalledTimes(1));
      await waitFor(() => expect(pendingExtensions()).toHaveLength(1));
      fireEvent.click(screen.getByRole("button", { name: "B" }));
      await waitFor(() => expect(a.unmount).toHaveBeenCalledTimes(1));
      expect(pendingExtensions()).toEqual([]);
    });

    it("(d) a synchronously throwing ready() is settled, logged as a warning, and shows no error chip", async () => {
      const a = makeExt("a", { ready: () => { throw new Error("splat loader died"); } });
      h.registry.set("u:a", a);
      h.entries = [{ url: "u:a" }];
      const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
      renderStage();
      await waitFor(() => expect(a.mount).toHaveBeenCalledTimes(1));
      await waitFor(() => expect(warn).toHaveBeenCalled());
      expect(String(warn.mock.calls[0][0])).toContain("[ext:a] ready() rejected");
      expect(pendingExtensions()).toEqual([]);
      expect(screen.queryByTestId("extension-mount-error")).toBeNull();
      warn.mockRestore();
    });
  });

  it("hide (onClose) is wired to the close button", async () => {
    const a = makeExt("a");
    h.registry.set("u:a", a);
    h.entries = [{ url: "u:a" }];
    const onClose = vi.fn();
    renderStage({ onClose });
    await waitFor(() => expect(a.mount).toHaveBeenCalledTimes(1));
    fireEvent.click(screen.getByTitle("Hide stage"));
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
