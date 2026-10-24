import { useEffect, useRef, useState } from "react";
import { X, ArrowRight, Clock, HelpCircle } from "lucide-react";
import type { StateTransition } from "./usePlaygroundSession";
import type { PipelineState } from "../../lib/transport/types";
import { useTheme } from "../../contexts/ThemeContext";

interface Props {
  timeline: StateTransition[];
  onClose: () => void;
}

/** Per-state badge colors, mirroring the pipeline-state semantics used in the
 *  connection bar (listening = neutral, processing = amber, speaking = indigo,
 *  draining = sky). */
function stateBadge(state: PipelineState | null, isDark: boolean): string {
  switch (state) {
    case "processing":
      return isDark ? "text-amber-400 bg-amber-500/15" : "text-amber-600 bg-amber-100";
    case "speaking":
      return isDark ? "text-indigo-400 bg-indigo-500/15" : "text-indigo-600 bg-indigo-100";
    case "draining":
      return isDark ? "text-sky-400 bg-sky-500/15" : "text-sky-600 bg-sky-100";
    case "listening":
    default:
      return isDark ? "text-emerald-400 bg-emerald-500/15" : "text-emerald-600 bg-emerald-100";
  }
}

/** Format an inter-transition gap for display. */
function formatGap(ms: number | null): string {
  if (ms == null) return "—";
  if (ms < 1000) return `${ms} ms`;
  return `${(ms / 1000).toFixed(2)} s`;
}

/**
 * StateTimeline — T6 state-timeline panel.
 *
 * Records every `state_changed` pipeline transition over the life of a session
 * with the wall-clock time it occurred and the latency since the previous
 * transition. Complements the single current-state badge in the connection bar
 * by exposing the full lifecycle, making stalls (e.g. a long gap between
 * `processing` and `speaking`) visible at a glance.
 */
export default function StateTimeline({ timeline, onClose }: Props) {
  const { isDark } = useTheme();
  const bottomRef = useRef<HTMLDivElement>(null);
  const [showHelp, setShowHelp] = useState(false);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [timeline.length]);

  return (
    <div
      className={`flex flex-col w-72 shrink-0 border-l ${
        isDark ? "bg-slate-900 border-slate-700" : "bg-white border-gray-200"
      }`}
    >
      {/* Header */}
      <div
        className={`flex items-center justify-between px-3 py-2 border-b ${
          isDark ? "border-slate-700" : "border-gray-200"
        }`}
      >
        <div className="flex items-center gap-1.5">
          <Clock size={13} className={isDark ? "text-slate-400" : "text-gray-500"} />
          <span className={`text-xs font-semibold ${isDark ? "text-slate-200" : "text-gray-700"}`}>
            State Timeline
          </span>
          <span className={`text-[10px] ${isDark ? "text-slate-500" : "text-gray-400"}`}>
            ({timeline.length})
          </span>
        </div>
        <div className="flex items-center gap-0.5">
          <button
            onClick={() => setShowHelp((s) => !s)}
            title="What is this?"
            aria-label="Explain state timeline"
            className={`p-1 rounded transition-colors ${
              showHelp
                ? isDark ? "text-sky-400" : "text-sky-600"
                : isDark ? "hover:bg-slate-700 text-slate-400" : "hover:bg-gray-100 text-gray-500"
            }`}
          >
            <HelpCircle size={14} />
          </button>
          <button
            onClick={onClose}
            className={`p-1 rounded transition-colors ${
              isDark ? "hover:bg-slate-700 text-slate-400" : "hover:bg-gray-100 text-gray-500"
            }`}
            aria-label="Close state timeline"
          >
            <X size={14} />
          </button>
        </div>
      </div>

      {/* Help / explanation */}
      {showHelp && (
        <div className={`px-3 py-2.5 border-b text-[11px] leading-relaxed ${
          isDark ? "bg-slate-800/60 border-slate-700 text-slate-300" : "bg-sky-50 border-gray-200 text-gray-600"
        }`}>
          <p className="mb-1.5">
            Records every pipeline <span className="font-semibold">state change</span> over the life of the
            session — <span className="font-mono">listening → processing → speaking → draining</span> — with the
            time it happened and the gap since the previous change.
          </p>
          <p className="mb-1.5">
            Use it to spot latency and stalls: a long gap before <span className="font-mono">speaking</span> means
            slow first audio (LLM/TTS); a long gap before <span className="font-mono">processing</span> means slow
            end-of-speech detection (VAD). If the session never returns to <span className="font-mono">listening</span>,
            it's stuck.
          </p>
          <p className={isDark ? "text-slate-500" : "text-gray-400"}>
            Clears on disconnect or “Clear conversation”.
          </p>
        </div>
      )}

      {/* Transitions */}
      <div className="flex-1 overflow-y-auto">
        {timeline.length === 0 ? (
          <div className={`px-3 py-6 text-[11px] ${isDark ? "text-slate-500" : "text-gray-400"}`}>
            No state transitions yet. Pipeline state changes will appear here once the session is active.
          </div>
        ) : (
          timeline.map((t, i) => (
            <div
              key={i}
              className={`px-3 py-1.5 border-b ${
                isDark ? "border-slate-700/30" : "border-gray-100"
              }`}
            >
              <div className="flex items-center gap-1.5">
                <span className={`text-[10px] font-mono ${isDark ? "text-slate-500" : "text-gray-400"}`}>
                  {new Date(t.timestamp).toLocaleTimeString([], {
                    hour: "2-digit",
                    minute: "2-digit",
                    second: "2-digit",
                    fractionalSecondDigits: 3,
                  } as Intl.DateTimeFormatOptions)}
                </span>
              </div>
              <div className="flex items-center gap-1.5 mt-0.5">
                {t.prevState && (
                  <>
                    <span
                      className={`text-[9px] font-bold uppercase px-1.5 py-0.5 rounded-full ${stateBadge(
                        t.prevState,
                        isDark,
                      )}`}
                    >
                      {t.prevState}
                    </span>
                    <ArrowRight size={10} className={isDark ? "text-slate-600" : "text-gray-300"} />
                  </>
                )}
                <span
                  className={`text-[9px] font-bold uppercase px-1.5 py-0.5 rounded-full ${stateBadge(
                    t.state,
                    isDark,
                  )}`}
                >
                  {t.state ?? "idle"}
                </span>
                <span className={`ml-auto text-[10px] font-mono ${isDark ? "text-slate-400" : "text-gray-500"}`}>
                  +{formatGap(t.sincePrevMs)}
                </span>
              </div>
            </div>
          ))
        )}
        <div ref={bottomRef} />
      </div>
    </div>
  );
}
