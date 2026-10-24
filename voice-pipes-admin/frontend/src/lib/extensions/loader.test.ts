import { describe, it, expect } from "vitest";
import { discoverExtensionEntries, loadExtensions, MANIFEST_URL } from "./loader";

function fakeFetch(status: number, contentType: string, body: unknown): typeof fetch {
  return (async (url: string | URL | Request) => {
    expect(String(url)).toBe(MANIFEST_URL);
    return {
      ok: status >= 200 && status < 300,
      status,
      headers: { get: (k: string) => (k.toLowerCase() === "content-type" ? contentType : null) },
      json: async () => body,
    } as unknown as Response;
  }) as typeof fetch;
}

describe("discoverExtensionEntries", () => {
  it("returns manifest entries in order, dropping malformed ones", async () => {
    const entries = await discoverExtensionEntries(
      fakeFetch(200, "application/json", {
        extensions: [{ url: "builtin:blendshape-meter" }, { nope: true }, { url: "/ext/a.js", options: { x: 1 } }],
      }),
    );
    expect(entries).toEqual([{ url: "builtin:blendshape-meter" }, { url: "/ext/a.js", options: { x: 1 } }]);
  });

  it("treats the SPA index.html fallback (200 text/html) and 404 as 'no manifest'", async () => {
    expect(await discoverExtensionEntries(fakeFetch(200, "text/html", "<!doctype html>"))).toEqual([]);
    expect(await discoverExtensionEntries(fakeFetch(404, "text/html", ""))).toEqual([]);
  });

  it("never throws when fetch rejects", async () => {
    const throwing = (async () => { throw new TypeError("network"); }) as unknown as typeof fetch;
    expect(await discoverExtensionEntries(throwing)).toEqual([]);
  });
});

describe("loadExtensions", () => {
  it("resolves builtin: entries and reports unknown builtins as failures", async () => {
    const r = await loadExtensions([{ url: "builtin:blendshape-meter" }, { url: "builtin:does-not-exist" }]);
    expect(r.loaded).toHaveLength(1);
    expect(r.loaded[0].extension.id).toBe("voice-pipes.blendshape-meter");
    expect(r.loaded[0].baseUrl).toBe("builtin:blendshape-meter");
    expect(r.failed).toHaveLength(1);
    expect(r.failed[0].error).toMatch(/unknown builtin/);
  });

  it("rejects duplicate ids", async () => {
    const r = await loadExtensions([{ url: "builtin:blendshape-meter" }, { url: "builtin:blendshape-meter" }]);
    expect(r.loaded).toHaveLength(1);
    expect(r.failed[0].error).toMatch(/duplicate extension id/);
  });

  it("imports an ESM module URL and validates its default export", async () => {
    const good = "data:text/javascript," + encodeURIComponent(
      'export default { id: "t.good", name: "Good", mount() {}, unmount() {} };',
    );
    const factory = "data:text/javascript," + encodeURIComponent(
      'export default () => ({ id: "t.factory", name: "Factory", mount() {}, unmount() {} });',
    );
    const bad = "data:text/javascript," + encodeURIComponent('export default { id: "t.bad" };');
    const r = await loadExtensions([{ url: good }, { url: factory }, { url: bad }]);
    expect(r.loaded.map((l) => l.extension.id)).toEqual(["t.good", "t.factory"]);
    expect(r.loaded[0].baseUrl).toBe(good);
    expect(r.failed).toHaveLength(1);
    expect(r.failed[0].error).toMatch(/name missing/);
  });

  it("validates the optional ready(): a non-function is rejected, an async function loads", async () => {
    const badReady = "data:text/javascript," + encodeURIComponent(
      'export default { id: "t.badready", name: "BadReady", mount() {}, unmount() {}, ready: 42 };',
    );
    const goodReady = "data:text/javascript," + encodeURIComponent(
      'export default { id: "t.ready", name: "Ready", mount() {}, unmount() {}, ready: async () => {} };',
    );
    const r = await loadExtensions([{ url: badReady }, { url: goodReady }]);
    expect(r.loaded.map((l) => l.extension.id)).toEqual(["t.ready"]);
    expect(typeof r.loaded[0].extension.ready).toBe("function");
    expect(r.failed).toHaveLength(1);
    expect(r.failed[0].error).toBe('extension "t.badready": ready must be a function');
  });
});
