import { memo } from "react";
import { Bot, Info, User } from "lucide-react";
import type { TranscriptEntry } from "../session/types";
import { useTheme } from "../../../contexts/ThemeContext";

interface Props {
  entry: TranscriptEntry;
}

/** Renders entries with `type === "message"` (PipelineMessage envelope).
 *  Two visual variants: plain "text" kind reuses chat-bubble styling;
 *  structured kinds (cards / tool / data) render a labelled panel with a
 *  collapsible payload. */
function PipelineMessageBubbleImpl({ entry }: Props) {
  const { isDark } = useTheme();
  const pm = entry.pipelineMessage;
  if (!pm) return null;

  if (pm.kind === "text" || !pm.kind) {
    return (
      <div
        className={`flex gap-2.5 max-w-[85%] ${
          entry.role === "user" ? "self-end flex-row-reverse" : "self-start"
        }`}
      >
        <div
          className={`w-7 h-7 rounded-full flex items-center justify-center shrink-0 ${
            entry.role === "user"
              ? "bg-indigo-600/30"
              : entry.role === "system"
                ? isDark
                  ? "bg-slate-600"
                  : "bg-gray-300"
                : isDark
                  ? "bg-slate-700"
                  : "bg-gray-200"
          }`}
        >
          {entry.role === "user" ? (
            <User size={14} className="text-indigo-300" />
          ) : entry.role === "system" ? (
            <Info size={14} className={isDark ? "text-slate-300" : "text-gray-500"} />
          ) : (
            <Bot size={14} className={isDark ? "text-slate-300" : "text-gray-500"} />
          )}
        </div>
        <div
          className={`rounded-2xl px-3.5 py-2 text-sm leading-relaxed ${
            entry.role === "user"
              ? "bg-indigo-600 text-white rounded-br-md"
              : isDark
                ? "bg-slate-800 text-slate-200 border border-slate-700 rounded-bl-md"
                : "bg-gray-100 text-gray-800 border border-gray-200 rounded-bl-md"
          }`}
        >
          {entry.text}
          <div
            className={`text-[10px] mt-1 ${
              entry.role === "user"
                ? "text-indigo-200/60"
                : isDark
                  ? "text-slate-500"
                  : "text-gray-400"
            }`}
          >
            {new Date(entry.timestamp).toLocaleTimeString()}
          </div>
        </div>
      </div>
    );
  }

  return (
    <div
      className={`self-start max-w-[90%] rounded-xl px-3.5 py-2.5 text-sm ${
        isDark
          ? "bg-slate-800/70 border border-slate-700 text-slate-300"
          : "bg-gray-50 border border-gray-200 text-gray-700"
      }`}
    >
      <div
        className={`text-[10px] font-medium uppercase tracking-wider mb-1 ${
          isDark ? "text-slate-500" : "text-gray-400"
        }`}
      >
        {pm.kind}
      </div>
      {pm.text && <div className="mb-1">{pm.text}</div>}
      {!!pm.payload && (
        <details className={`text-xs ${isDark ? "text-slate-400" : "text-gray-500"}`}>
          <summary className="cursor-pointer select-none">payload</summary>
          <pre className="mt-1 overflow-x-auto whitespace-pre-wrap">
            {JSON.stringify(pm.payload, null, 2)}
          </pre>
        </details>
      )}
      <div
        className={`text-[10px] mt-1 ${isDark ? "text-slate-500" : "text-gray-400"}`}
      >
        {new Date(entry.timestamp).toLocaleTimeString()}
      </div>
    </div>
  );
}

export const PipelineMessageBubble = memo(PipelineMessageBubbleImpl);
