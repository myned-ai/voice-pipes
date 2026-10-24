
import { Bot } from "lucide-react";
import type { TranscriptEntry, HighlightInfo } from "../session/types";
import { useTheme } from "../../../contexts/ThemeContext";

interface Props {
  entry: TranscriptEntry;
  /** Index in the parent's transcript array \u2014 used purely as a key for the
   *  shrink-detection debug log so consecutive turns don't share state. */
  entryIndex: number;
  /** Authoritative highlight info from the playback clock + WordTimingTable.
   *  When `null`, no progressive reveal data is available yet \u2014 fall back to
   *  animated typing dots so the user sees something is happening. */
  highlightInfo: HighlightInfo | null;
  /** Whether this entry is the turn currently being spoken. While speaking
   *  and before the first `wordHighlighted` tick, the accumulated text is
   *  rendered fully DIMMED (no word heard yet) instead of full-bright \u2014
   *  otherwise providers whose transcript races ahead of audio (OpenAI
   *  realtime streams the full text in ~1s) flash the whole reply, which
   *  then visibly "un-reveals" when the karaoke clock starts. */
  speaking: boolean;
}

function AgentBubbleText({
  entry,
  highlightInfo,
}: {
  entry: TranscriptEntry;
  highlightInfo: HighlightInfo;
}) {
  const tokens = entry.text.split(/(\s+)/);
  let wordCounter = 0;
  
  const nodes: React.ReactNode[] = [];
  for (let i = 0; i < tokens.length; i++) {
    const token = tokens[i];
    const isWord = /\S/.test(token);
    
    let isHeard = false;
    if (isWord) {
      if (wordCounter <= highlightInfo.wordIndex) {
        isHeard = true;
      }
      wordCounter++;
    } else {
      // The space immediately following the currently spoken word is also considered "heard"
      if (wordCounter <= highlightInfo.wordIndex + 1) {
        isHeard = true;
      }
    }
    
    nodes.push(
      <span 
        key={i} 
        className={isHeard ? "" : "opacity-40 transition-opacity duration-150"}
      >
        {token}
      </span>
    );
  }

  return <>{nodes}</>;
}

function TypingDots() {
  const { isDark } = useTheme();
  return (
    <span className="inline-flex items-center gap-1">
      <span className={`w-1.5 h-1.5 rounded-full animate-pulse ${isDark ? "bg-slate-400" : "bg-gray-400"}`} />
      <span className={`w-1.5 h-1.5 rounded-full animate-pulse ${isDark ? "bg-slate-400" : "bg-gray-400"}`} style={{ animationDelay: "0.15s" }} />
      <span className={`w-1.5 h-1.5 rounded-full animate-pulse ${isDark ? "bg-slate-400" : "bg-gray-400"}`} style={{ animationDelay: "0.3s" }} />
    </span>
  );
}

/** The ONLY transcript bubble that subscribes (transitively, via its
 *  `highlightInfo` prop) to the playback clock.  Re-renders on every
 *  reveal-pipeline tick.  Renders the active agent turn's progressively
 *  revealed text \u2014 or animated typing dots until words are heard. */
export function StreamingAgentBubble({ entry, entryIndex, highlightInfo, speaking }: Props) {
  const { isDark } = useTheme();
  const showHighlight =
    highlightInfo !== null && highlightInfo.entryIndex === entryIndex;
  // Speaking but no highlight for THIS entry yet: playback hasn't reached
  // word 0 (or is still on the previous turn). Render everything dimmed so
  // the reveal starts from silence instead of flashing the full reply.
  const preReveal = !showHighlight && speaking;

  return (
    <div className="flex gap-2.5 max-w-[85%] self-start">
      <div
        className={`w-7 h-7 rounded-full flex items-center justify-center shrink-0 ${isDark ? "bg-slate-700" : "bg-gray-200"}`}
      >
        <Bot size={14} className={isDark ? "text-slate-300" : "text-gray-500"} />
      </div>
      <div
        className={`rounded-2xl px-3.5 py-2 text-sm leading-relaxed ${
          isDark
            ? "bg-slate-800 text-slate-200 border border-slate-700 rounded-bl-md"
            : "bg-gray-100 text-gray-800 border border-gray-200 rounded-bl-md"
        }`}
      >
        {showHighlight ? (
          <AgentBubbleText
            entry={entry}
            highlightInfo={highlightInfo!}
          />
        ) : entry.text && entry.text.trim().length > 0 ? (
          preReveal ? (
            <AgentBubbleText
              entry={entry}
              highlightInfo={{ entryIndex, wordIndex: -1 }}
            />
          ) : (
            <span>{entry.heardText ?? entry.text}</span>
          )
        ) : (
          <TypingDots />
        )}
        <div className={`text-[10px] mt-1 ${isDark ? "text-slate-500" : "text-gray-400"}`}>
          {new Date(entry.timestamp).toLocaleTimeString()}
        </div>
      </div>
    </div>
  );
}
