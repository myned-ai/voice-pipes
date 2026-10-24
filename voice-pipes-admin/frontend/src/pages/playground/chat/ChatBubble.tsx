import { memo } from "react";
import { Bot, User } from "lucide-react";
import type { TranscriptEntry } from "../session/types";
import { useTheme } from "../../../contexts/ThemeContext";

interface Props {
  entry: TranscriptEntry;
}

/** Standard user / agent chat bubble for entries that are NOT the currently
 *  speaking agent turn.  Memoized: only re-renders when `entry` reference
 *  changes (i.e. when the reducer mutated this specific entry).  The
 *  playback-clock ticks driving the karaoke highlight do not touch this
 *  component — that is `StreamingAgentBubble`'s job. */
function ChatBubbleImpl({ entry }: Props) {
  const { isDark } = useTheme();
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
            : isDark
              ? "bg-slate-700"
              : "bg-gray-200"
        }`}
      >
        {entry.role === "user" ? (
          <User size={14} className="text-indigo-300" />
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
        {entry.heardText ?? entry.text}
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

export const ChatBubble = memo(ChatBubbleImpl);
