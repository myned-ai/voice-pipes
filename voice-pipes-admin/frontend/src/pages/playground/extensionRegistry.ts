// Page-lifetime cache for Playground Extension discovery + import, plus the
// readiness registry the Connect/Dispatch gate consults. Kept out of
// ExtensionStage.tsx so that file only exports components (react-refresh rule)
// and so Playground.tsx can query the count / readiness without rendering the
// stage (which may unmount/remount while the Playground needs a stable answer
// to "is anything still loading?").
import { discoverExtensionEntries, loadExtensions, type ExtensionLoadResult } from "../../lib/extensions";

let loadPromise: Promise<ExtensionLoadResult> | null = null;

/** Discover + import once per page load; shared by all mounts (navigating
 *  away/back must not re-import). */
export function loadPlaygroundExtensionsOnce(): Promise<ExtensionLoadResult> {
  return (loadPromise ??= discoverExtensionEntries().then(loadExtensions));
}

/** Test hook. */
export function __resetExtensionCache(): void { loadPromise = null; }

// ── Readiness gate ──

/** Upper bound the Playground waits for mounted extensions before connecting. */
export const EXTENSION_READY_TIMEOUT_MS = 10_000;

interface ReadinessEntry { name: string; promise: Promise<void>; settled: boolean }
const readiness = new Map<string, ReadinessEntry>();

/** Called by the Stage after `mount()`: register the extension's `ready()`
 *  promise (or `null` on unmount). A rejecting/throwing `ready()` counts as
 *  settled — the gate must never block on a broken extension. Only MOUNTED
 *  extensions register: a hidden stage or an inactive tab never gates. */
export function setExtensionReadiness(id: string, name: string, promise: Promise<void> | null): void {
  if (!promise) { readiness.delete(id); return; }
  const entry: ReadinessEntry = { name, promise: Promise.resolve(), settled: false };
  entry.promise = promise.then(() => { entry.settled = true; }, () => { entry.settled = true; });
  readiness.set(id, entry);
}

/** Mounted extensions whose `ready()` has not settled yet. */
export function pendingExtensions(): { id: string; name: string }[] {
  return [...readiness].filter(([, e]) => !e.settled).map(([id, e]) => ({ id, name: e.name }));
}

/** Wait (bounded) for every currently pending extension. Never rejects. */
export async function waitForExtensionsReady(
  timeoutMs = EXTENSION_READY_TIMEOUT_MS,
): Promise<{ ready: string[]; pending: string[]; waitedMs: number }> {
  const snapshot = [...readiness].filter(([, e]) => !e.settled);
  const t0 = performance.now();
  if (snapshot.length === 0) return { ready: [], pending: [], waitedMs: 0 };
  let timer: ReturnType<typeof setTimeout> | undefined;
  const timeout = new Promise<void>((resolve) => { timer = setTimeout(resolve, timeoutMs); });
  await Promise.race([Promise.all(snapshot.map(([, e]) => e.promise)), timeout]);
  clearTimeout(timer);
  const pending = snapshot.filter(([, e]) => !e.settled).map(([id]) => id);
  const ready = snapshot.filter(([, e]) => e.settled).map(([id]) => id);
  return { ready, pending, waitedMs: Math.round(performance.now() - t0) };
}

/** Test hook. */
export function __resetExtensionReadiness(): void { readiness.clear(); }
