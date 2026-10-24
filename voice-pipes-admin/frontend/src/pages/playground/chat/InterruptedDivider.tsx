import { memo } from "react";
import { Zap } from "lucide-react";
import { useTheme } from "../../../contexts/ThemeContext";

/** Visible "interrupted" divider rendered for entries with `type === "interrupted"`.
 *  Pure: only depends on theme.  Memoized so it never re-renders on transcript
 *  growth or playback-clock ticks. */
function InterruptedDividerImpl() {
  const { isDark } = useTheme();
  return (
    <div className="flex items-center gap-2 self-stretch my-1">
      <div className={`flex-1 h-px ${isDark ? "bg-orange-500/30" : "bg-orange-300/50"}`} />
      <div
        className={`flex items-center gap-1 px-2 py-0.5 rounded-full text-[10px] font-medium ${
          isDark
            ? "bg-orange-950/60 border border-orange-700/40 text-orange-300"
            : "bg-orange-50 border border-orange-200 text-orange-600"
        }`}
      >
        <Zap size={9} />
        interrupted
      </div>
      <div className={`flex-1 h-px ${isDark ? "bg-orange-500/30" : "bg-orange-300/50"}`} />
    </div>
  );
}

export const InterruptedDivider = memo(InterruptedDividerImpl);
