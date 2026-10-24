// Extension Stage — the Playground's host panel for playground extensions.
// Discovers + imports extension modules once
// per page load, renders a tab strip when there is more than one, owns one
// container per extension, mounts only after the container has layout size,
// unmounts on tab switch / hide / route leave. StrictMode-safe.

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { X, AlertTriangle } from "lucide-react";
import { useTheme } from "../../contexts/ThemeContext";
import {
  PLAYGROUND_EXTENSION_API_VERSION,
  type ExtensionLoadResult,
  type LoadedExtension,
  type PlaygroundExtensionHost,
  type PlaygroundExtensionSession,
} from "../../lib/extensions";
import { loadPlaygroundExtensionsOnce, setExtensionReadiness } from "./extensionRegistry";

const STORAGE_KEY = "vp.playground.stageHeight";
const DEFAULT_HEIGHT = 360;
const MIN_HEIGHT = 160;
const MAX_HEIGHT = 900;

interface Props {
  session: PlaygroundExtensionSession;
  onClose: () => void;
  /** Called with the loaded count so callers can hide the toggle when 0. */
  onLoaded?: (count: number) => void;
}

function shortUrl(url: string): string {
  const clean = url.split("?")[0].replace(/\/+$/, "");
  const seg = clean.split("/").pop();
  return seg && seg.length > 0 ? seg : url;
}

export default function ExtensionStage({ session, onClose, onLoaded }: Props) {
  const { isDark } = useTheme();
  const [result, setResult] = useState<ExtensionLoadResult | null>(null);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [height, setHeight] = useState(() => {
    const stored = Number(localStorage.getItem(STORAGE_KEY));
    return Number.isFinite(stored) && stored >= MIN_HEIGHT && stored <= MAX_HEIGHT ? stored : DEFAULT_HEIGHT;
  });

  useEffect(() => {
    let alive = true;
    loadPlaygroundExtensionsOnce().then((r) => {
      if (!alive) return;
      setResult(r);
      // `disabled: true` entries are listed as tabs but not auto-mounted; the
      // user opts in by clicking the tab.
      setActiveId((cur) => cur ?? r.loaded.find((l) => !l.entry.disabled)?.extension.id ?? null);
      onLoaded?.(r.loaded.length);
    });
    return () => { alive = false; };
  }, [onLoaded]);

  useEffect(() => { localStorage.setItem(STORAGE_KEY, String(height)); }, [height]);

  const onResize = useCallback((dy: number) => {
    setHeight((h) => Math.min(MAX_HEIGHT, Math.max(MIN_HEIGHT, h + dy)));
  }, []);

  const active = result?.loaded.find((l) => l.extension.id === activeId) ?? null;

  return (
    <div
      data-testid="extension-stage"
      className={`relative border-b shrink-0 ${isDark ? "bg-slate-950 border-slate-700" : "bg-white border-gray-200"}`}
      style={{ height }}
    >
      {/* header: tabs + failures + close */}
      <div
        className={`absolute top-0 inset-x-0 z-10 flex items-center gap-1 px-2 h-7 text-xs ${
          isDark ? "bg-slate-900/80 text-slate-300" : "bg-gray-100/80 text-gray-700"
        }`}
      >
        {(result?.loaded ?? []).map((l) => (
          <button
            key={l.extension.id}
            type="button"
            onClick={() => setActiveId(l.extension.id)}
            className={`px-2 py-0.5 rounded ${
              l.extension.id === activeId ? "bg-indigo-600/30 text-indigo-300" : "opacity-70 hover:opacity-100"
            }`}
            title={`${l.extension.id}${l.extension.version ? ` v${l.extension.version}` : ""}${l.entry.disabled ? " (disabled in manifest — click to enable)" : ""}`}
          >
            {l.extension.name}
          </button>
        ))}
        {(result?.failed ?? []).map((f) => (
          <span
            key={f.entry.url}
            title={f.error}
            data-testid="extension-failed"
            className="inline-flex items-center gap-1 px-2 text-amber-400"
          >
            <AlertTriangle size={12} /> {shortUrl(f.entry.url)}
          </span>
        ))}
        {result && result.loaded.length === 0 && result.failed.length === 0 && (
          <span className="opacity-60">No extensions configured</span>
        )}
        <span className="flex-1" />
        <button type="button" onClick={onClose} className="p-0.5 opacity-70 hover:opacity-100" title="Hide stage">
          <X size={13} />
        </button>
      </div>

      {/* one mount root for the ACTIVE extension only; `key` forces unmount/mount on tab switch */}
      {active && <MountRoot key={active.extension.id} loaded={active} session={session} isDark={isDark} />}

      <ResizeHandle onResize={onResize} isDark={isDark} />
    </div>
  );
}

function MountRoot({
  loaded,
  session,
  isDark,
}: {
  loaded: LoadedExtension;
  session: PlaygroundExtensionSession;
  isDark: boolean;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const [error, setError] = useState<string | null>(null);
  const isDarkRef = useRef(isDark);
  const themeListeners = useRef(new Set<(d: boolean) => void>());
  useEffect(() => {
    isDarkRef.current = isDark;
    themeListeners.current.forEach((cb) => cb(isDark));
  }, [isDark]);

  const host = useMemo<PlaygroundExtensionHost>(
    () => ({
      apiVersion: PLAYGROUND_EXTENSION_API_VERSION,
      session,
      theme: {
        get isDark() { return isDarkRef.current; },
        onChange: (cb) => {
          themeListeners.current.add(cb);
          return () => { themeListeners.current.delete(cb); };
        },
      },
      options: Object.freeze({ ...(loaded.entry.options ?? {}) }),
      baseUrl: loaded.baseUrl,
      log: (level, message, detail) => {
        const fn = level === "debug" ? console.debug : level === "info" ? console.info : level === "warn" ? console.warn : console.error;
        fn(`[ext:${loaded.extension.id}] ${message}`, detail ?? "");
      },
    }),
    [session, loaded],
  );

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    // StrictMode: effect runs, cleans up, runs again → the first mount must be abandoned cleanly.
    let cancelled = false;
    let mounted = false;
    let mounting = false;
    const ext = loaded.extension;

    // Mount only once the container has layout size (WebGL renderers read the rect at construction).
    const tryMount = async () => {
      if (mounting || mounted || cancelled) return;
      const { width, height } = el.getBoundingClientRect();
      if (width < 1 || height < 1) return;
      mounting = true;
      try {
        await ext.mount(el, host);
        if (cancelled) {
          // Unmounted while mount() was pending.
          await ext.unmount();
          return;
        }
        mounted = true;
        // Readiness gate: only MOUNTED extensions register, so a hidden stage
        // or an inactive tab never holds Connect. A throwing/rejecting
        // ready() is treated as ready — the operator is never blocked.
        if (ext.ready) {
          const p = Promise.resolve()
            .then(() => ext.ready!())
            .catch((e) => host.log("warn", "ready() rejected — treating as ready", e));
          setExtensionReadiness(ext.id, ext.name, p);
        }
      } catch (e) {
        if (!cancelled) setError(e instanceof Error ? e.message : String(e));
        host.log("error", "mount failed", e);
      } finally {
        mounting = false;
      }
    };
    const ro = typeof ResizeObserver === "function"
      ? new ResizeObserver((entries) => {
          if (!mounted) { void tryMount(); return; }
          const r = entries[0]?.contentRect;
          if (r) ext.resize?.(r.width, r.height);
        })
      : null;
    ro?.observe(el);
    // Deferred one microtask so StrictMode's synchronous mount→cleanup→mount
    // cycle cancels the first pass BEFORE the extension is ever mounted.
    queueMicrotask(() => { void tryMount(); });

    return () => {
      cancelled = true;
      ro?.disconnect();
      if (mounted) {
        mounted = false;
        void Promise.resolve()
          .then(() => ext.unmount())
          .catch((e) => host.log("error", "unmount failed", e));
      }
      // Unmounted (tab switch / hide / route leave) → nothing to wait for,
      // even if ready() was still pending.
      setExtensionReadiness(ext.id, ext.name, null);
      // Extension owns the subtree; make sure nothing leaks into the next mount.
      el.replaceChildren();
    };
  }, [loaded, host]);

  return (
    <div className="absolute inset-0 pt-7">
      <div ref={ref} className="w-full h-full overflow-hidden" data-extension-id={loaded.extension.id} />
      {error && (
        <div className="absolute inset-x-0 bottom-0 m-2 rounded bg-red-900/70 px-2 py-1 text-xs text-red-200" data-testid="extension-mount-error">
          {loaded.extension.name}: {error}
        </div>
      )}
    </div>
  );
}

function ResizeHandle({ onResize, isDark }: { onResize: (dy: number) => void; isDark: boolean }) {
  const lastY = useRef<number | null>(null);
  return (
    <div
      role="separator"
      aria-orientation="horizontal"
      title="Drag to resize"
      className={`absolute bottom-0 inset-x-0 h-1 cursor-row-resize ${isDark ? "hover:bg-indigo-500/50" : "hover:bg-indigo-400/50"}`}
      onPointerDown={(e) => {
        lastY.current = e.clientY;
        e.currentTarget.setPointerCapture(e.pointerId);
      }}
      onPointerMove={(e) => {
        if (lastY.current == null) return;
        const dy = e.clientY - lastY.current;
        lastY.current = e.clientY;
        if (dy !== 0) onResize(dy);
      }}
      onPointerUp={(e) => {
        lastY.current = null;
        e.currentTarget.releasePointerCapture(e.pointerId);
      }}
      onPointerCancel={() => { lastY.current = null; }}
    />
  );
}
