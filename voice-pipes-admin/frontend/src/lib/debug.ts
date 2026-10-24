// Diagnostic logging helper — gated so production users with DevTools open
// don't see ~30 lines of [DIAG]/[barge-in]/[playback]/[turn_metrics] noise
// per turn.  Errors should NOT use these helpers — keep `console.error` for
// real failures so they always reach the user / monitoring tools.
//
// Enable in dev by either:
//   • Setting VITE_DEBUG_PLAYGROUND=true in `.env` (build-time)
//   • localStorage.setItem("playground-debug", "true") then reloading (runtime)

function readFlag(): boolean {
  try {
    if (typeof localStorage !== "undefined" && localStorage.getItem("playground-debug") === "true") {
      return true;
    }
  } catch {
    // localStorage may throw in sandboxed contexts; ignore.
  }
  try {
    // Vite injects import.meta.env at build time.
    return import.meta.env?.VITE_DEBUG_PLAYGROUND === "true";
  } catch {
    return false;
  }
}

/** Whether debug output is currently enabled.  Cached at module load \u2014 a
 *  full reload is required to pick up a localStorage flip. */
export const debugEnabled = readFlag();

/** Diagnostic log.  No-op in production unless the user opts in. */
export function debug(...args: unknown[]): void {
  if (debugEnabled) {
    console.log(...args);
  }
}

/** Diagnostic warning.  No-op in production unless the user opts in.
 *  Use `console.error` directly for real failures that should always log. */
export function debugWarn(...args: unknown[]): void {
  if (debugEnabled) {
    console.warn(...args);
  }
}
