import type {
  PlaygroundExtension,
  PlaygroundExtensionManifest,
  PlaygroundExtensionManifestEntry,
  PlaygroundExtensionModule,
} from "./types";
import { BUILTIN_EXTENSIONS } from "./builtin";

export const MANIFEST_URL = "/playground-extensions.json";
/** Comma-separated module URLs (no options) — build-time fallback / dev convenience. */
export const ENV_EXTENSIONS = "VITE_PLAYGROUND_EXTENSIONS";

export interface LoadedExtension {
  entry: PlaygroundExtensionManifestEntry;
  extension: PlaygroundExtension;
  /** Absolute module URL (or "builtin:<name>"). */
  baseUrl: string;
}
export interface FailedExtension { entry: PlaygroundExtensionManifestEntry; error: string; }
export interface ExtensionLoadResult { loaded: LoadedExtension[]; failed: FailedExtension[]; }

/** Manifest first; env var appended (dedup by url). Never throws. */
export async function discoverExtensionEntries(
  fetchImpl: typeof fetch = fetch,
): Promise<PlaygroundExtensionManifestEntry[]> {
  const entries: PlaygroundExtensionManifestEntry[] = [];
  try {
    const res = await fetchImpl(MANIFEST_URL, { cache: "no-cache" });
    if (res.ok && (res.headers.get("content-type") ?? "").includes("json")) {
      const manifest = (await res.json()) as PlaygroundExtensionManifest;
      if (Array.isArray(manifest?.extensions)) entries.push(...manifest.extensions.filter(isValidEntry));
    }
    // 404 / HTML (SPA fallback serves index.html for unknown paths!) → no manifest. The content-type
    // check is what makes the nginx `try_files … /index.html` fallback harmless.
  } catch { /* offline / CORS → no manifest */ }
  const env = (import.meta as { env?: Record<string, string | undefined> }).env?.[ENV_EXTENSIONS];
  for (const url of (env ?? "").split(",").map((s) => s.trim()).filter(Boolean)) {
    if (!entries.some((e) => e.url === url)) entries.push({ url });
  }
  return entries;
}

export async function loadExtensions(entries: PlaygroundExtensionManifestEntry[]): Promise<ExtensionLoadResult> {
  const result: ExtensionLoadResult = { loaded: [], failed: [] };
  const seen = new Set<string>();
  await Promise.all(entries.map(async (entry) => {
    try {
      const { extension, baseUrl } = await loadOne(entry);
      if (seen.has(extension.id)) throw new Error(`duplicate extension id "${extension.id}"`);
      seen.add(extension.id);
      result.loaded.push({ entry, extension, baseUrl });
    } catch (err) {
      result.failed.push({ entry, error: err instanceof Error ? err.message : String(err) });
    }
  }));
  // Keep manifest order (Promise.all resolves out of order).
  result.loaded.sort((a, b) => entries.indexOf(a.entry) - entries.indexOf(b.entry));
  result.failed.sort((a, b) => entries.indexOf(a.entry) - entries.indexOf(b.entry));
  return result;
}

async function loadOne(
  entry: PlaygroundExtensionManifestEntry,
): Promise<{ extension: PlaygroundExtension; baseUrl: string }> {
  if (entry.url.startsWith("builtin:")) {
    const name = entry.url.slice("builtin:".length);
    const factory = BUILTIN_EXTENSIONS[name];
    if (!factory) throw new Error(`unknown builtin extension "${name}"`);
    return { extension: validate(factory()), baseUrl: entry.url };
  }
  const abs = new URL(entry.url, window.location.href).href;
  const mod = (await import(/* @vite-ignore */ abs)) as Partial<PlaygroundExtensionModule>;
  const def = typeof mod.default === "function" ? (mod.default as () => PlaygroundExtension)() : mod.default;
  return { extension: validate(def), baseUrl: abs };
}

function validate(ext: unknown): PlaygroundExtension {
  const e = ext as Partial<PlaygroundExtension> | undefined;
  if (!e || typeof e !== "object") throw new Error("module default export is not an extension object");
  if (typeof e.id !== "string" || !e.id) throw new Error("extension.id missing");
  if (typeof e.name !== "string" || !e.name) throw new Error(`extension "${e.id}": name missing`);
  if (typeof e.mount !== "function" || typeof e.unmount !== "function") {
    throw new Error(`extension "${e.id}": mount/unmount missing`);
  }
  if (e.slot != null && e.slot !== "stage") throw new Error(`extension "${e.id}": unsupported slot "${String(e.slot)}"`);
  if (e.ready != null && typeof e.ready !== "function") {
    throw new Error(`extension "${e.id}": ready must be a function`);
  }
  return e as PlaygroundExtension;
}

function isValidEntry(e: unknown): e is PlaygroundExtensionManifestEntry {
  return !!e && typeof e === "object" && typeof (e as { url?: unknown }).url === "string";
}
