import { memo } from "react";
import { AlertCircle, AlertTriangle } from "lucide-react";
import type { TranscriptEntry } from "../session/types";
import { useTheme } from "../../../contexts/ThemeContext";

interface Props {
  entry: TranscriptEntry;
}

/** Inline error bubble (severity = info | warning | fatal).  Pure render of
 *  fields on the entry. */
function ErrorBubbleImpl({ entry }: Props) {
  const { isDark } = useTheme();
  const isFatal = entry.severity === "fatal";
  const isWarning = entry.severity === "warning";

  return (
    <div
      className={`flex items-start gap-2 self-start max-w-[90%] px-3 py-2 rounded-xl text-sm ${
        isFatal
          ? isDark
            ? "bg-red-900/40 border border-red-700/60 text-red-200"
            : "bg-red-50 border border-red-200 text-red-700"
          : isWarning
            ? isDark
              ? "bg-yellow-900/30 border border-yellow-700/50 text-yellow-200"
              : "bg-yellow-50 border border-yellow-200 text-yellow-700"
            : isDark
              ? "bg-red-900/25 border border-red-800/40 text-red-300"
              : "bg-red-50 border border-red-200 text-red-600"
      }`}
    >
      {isFatal ? (
        <AlertCircle size={16} className="shrink-0 mt-0.5" />
      ) : isWarning ? (
        <AlertTriangle size={16} className="shrink-0 mt-0.5" />
      ) : (
        <AlertCircle size={14} className="shrink-0 mt-0.5" />
      )}
      <div className="flex flex-col gap-0.5">
        <span>{entry.text}</span>
        {entry.errorCode && (
          <span className={`text-[10px] ${isDark ? "opacity-50" : "opacity-60"}`}>
            {entry.component ? `${entry.component}: ` : ""}
            {entry.errorCode}
          </span>
        )}
      </div>
    </div>
  );
}

export const ErrorBubble = memo(ErrorBubbleImpl);
