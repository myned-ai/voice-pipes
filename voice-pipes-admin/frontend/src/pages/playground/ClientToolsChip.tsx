import { useEffect, useRef, useState } from "react";
import { Wrench } from "lucide-react";
import { useTheme } from "../../contexts/ThemeContext";
import type { ClientToolInfo } from "./clientTools";

/** "N tools" chip for the connected worker's client tools, opening a list of
 *  what the agent can ask this client to do. Renders nothing without tools. */
export default function ClientToolsChip({ tools }: { tools: ClientToolInfo[] }) {
  const { isDark } = useTheme();
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const onMouseDown = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onMouseDown);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onMouseDown);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  if (tools.length === 0) return null;

  return (
    <div ref={ref} className="relative">
      <button
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
        title="Client tools this pipeline can ask the Playground to run"
        className={`flex items-center gap-1 px-2.5 py-1 rounded-full text-xs font-medium border transition-colors ${
          open
            ? isDark ? "bg-indigo-600/30 border-indigo-500 text-indigo-200" : "bg-indigo-50 border-indigo-400 text-indigo-700"
            : isDark ? "bg-slate-700 border-slate-600 text-slate-300 hover:border-slate-500" : "bg-gray-100 border-gray-300 text-gray-600 hover:border-gray-400"
        }`}
      >
        <Wrench size={11} />
        {tools.length} {tools.length === 1 ? "tool" : "tools"}
      </button>
      {open && (
        <div
          role="dialog"
          aria-label="Client tools"
          className={`absolute left-0 top-full mt-1 w-80 max-h-96 overflow-y-auto rounded-lg border shadow-xl z-50 py-1 ${
            isDark ? "bg-slate-800 border-slate-600" : "bg-white border-gray-200"
          }`}
        >
          {tools.map((t) => (
            <div key={t.name} className="px-3 py-2">
              <div className="flex items-baseline justify-between gap-2">
                <code className={`text-xs font-semibold ${isDark ? "text-slate-100" : "text-gray-900"}`}>
                  {t.name}({t.params.map((p) => (t.required.includes(p) ? p : `${p}?`)).join(", ")})
                </code>
                <span
                  className={`shrink-0 text-[10px] uppercase tracking-wide ${isDark ? "text-slate-400" : "text-gray-500"}`}
                  title={t.speakOnResult
                    ? "The agent speaks a confirmation once the client reports the result"
                    : "The agent stays silent after the result"}
                >
                  {t.speakOnResult ? "spoken" : "silent"}
                </span>
              </div>
              {t.description && (
                <p className={`mt-0.5 text-xs ${isDark ? "text-slate-400" : "text-gray-600"}`}>{t.description}</p>
              )}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
