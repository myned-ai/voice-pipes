import { useEffect, useRef } from "react";
import { Headphones, Wifi } from "lucide-react";
import type { TranscriptEntry, HighlightInfo } from "./usePlaygroundSession";
import type { PipelineState } from "../../lib/transport/types";
import { useTheme } from "../../contexts/ThemeContext";
import { InterruptedDivider } from "./chat/InterruptedDivider";
import { ErrorBubble } from "./chat/ErrorBubble";
import { PipelineMessageBubble } from "./chat/PipelineMessageBubble";
import { TriggerActionBubble } from "./chat/TriggerActionBubble";
import { ChatBubble } from "./chat/ChatBubble";
import { StreamingAgentBubble } from "./chat/StreamingAgentBubble";
import type { ClientToolPhase } from "../../lib/transport/types";

interface Props {
  transcript: TranscriptEntry[];
  pipelineState: PipelineState | null;
  connected?: boolean;
  highlightInfo?: HighlightInfo | null;
  /** Index of the transcript entry currently being spoken (progressive reveal). */
  speakingEntryIndex?: number;
  /** WebSocket URL displayed in the "Not Connected" hint.  Provided by the
   *  parent so the displayed URL matches what `connect()` will actually use
   *  (driven by `VITE_WS_URL` env var via `config/api.ts`). */
  wsUrl?: string;
  /** Sends a `client_tool_result` control — makes `trigger_action` cards
   *  actionable so the playground doubles as the reference client-tool
   *  executor. */
  onClientToolResult?: (
    callId: string,
    phase: ClientToolPhase,
    result?: unknown,
    speak?: boolean,
  ) => void;
}

const stateLabel: Record<PipelineState, string> = {
  listening: "Listening...",
  processing: "Processing...",
  speaking: "Speaking...",
  draining: "Draining...",
};

const stateDot: Record<PipelineState, string> = {
  listening: "bg-emerald-400",
  processing: "bg-yellow-400 animate-pulse",
  speaking: "bg-indigo-400 animate-pulse",
  draining: "bg-purple-400 animate-pulse",
};

/** Pure router: dispatches each `TranscriptEntry` to the appropriate
 *  memoized bubble component, plus a single `<StreamingAgentBubble>` for
 *  the active agent turn (which is the only branch that subscribes to
 *  `highlightInfo` ticks). */
export default function ChatTranscript({
  transcript,
  pipelineState,
  connected,
  highlightInfo,
  speakingEntryIndex = -1,
  wsUrl,
  onClientToolResult,
}: Props) {
  const { isDark } = useTheme();
  const bottomRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [transcript.length, pipelineState]);

  return (
    <div className="flex-1 overflow-y-auto px-4 py-4 flex flex-col gap-3">
      {transcript.length === 0 && !connected && (
        <div className="flex-1 flex flex-col items-center justify-center gap-3">
          <div className={`w-16 h-16 rounded-2xl flex items-center justify-center ${isDark ? "bg-slate-800" : "bg-gray-100"}`}>
            <Headphones size={28} className={isDark ? "text-slate-500" : "text-gray-400"} />
          </div>
          <span className={`text-base font-semibold mt-1 ${isDark ? "text-slate-300" : "text-gray-600"}`}>
            Not Connected
          </span>
          <span className={`text-sm text-center max-w-[220px] leading-relaxed ${isDark ? "text-slate-500" : "text-gray-400"}`}>
            Connect to a worker, then speak or type a message
          </span>
          <div className={`flex items-center gap-1.5 mt-2 text-[11px] px-3 py-1 rounded-full ${isDark ? "bg-slate-800 text-slate-500" : "bg-gray-100 text-gray-400"}`}>
            <Wifi size={11} />
            {wsUrl ?? ""}
          </div>
        </div>
      )}

      {transcript.map((entry, i) => {
        if (entry.type === "interrupted") {
          return <InterruptedDivider key={i} />;
        }
        if (entry.type === "error") {
          return <ErrorBubble key={i} entry={entry} />;
        }
        if (entry.type === "message" && entry.pipelineMessage) {
          // Client command cards get lifecycle controls (Complete / Fail /
          // Started / Progress) — the playground is the reference
          // client-tool executor.
          if (entry.pipelineMessage.kind === "trigger_action") {
            // Stable key: entries before this one can be removed by the
            // placeholder cleanup, and an index key would remount the
            // card and drop its local state (result JSON draft, etc.).
            return (
              <TriggerActionBubble
                key={entry.pipelineMessage.message_id ?? i}
                entry={entry}
                onResult={connected ? onClientToolResult : undefined}
              />
            );
          }
          return <PipelineMessageBubble key={i} entry={entry} />;
        }

        // Standard chat entry: agent OR user.
        // The agent's currently-speaking turn is the only entry that needs
        // to subscribe to playback-clock ticks \u2014 route it to the
        // non-memoized `<StreamingAgentBubble>`.  All other entries go to
        // the memoized `<ChatBubble>` so a clock tick re-renders only one
        // bubble in the entire transcript.
        const isSpeakingEntry =
          entry.role === "agent" &&
          (speakingEntryIndex === i ||
            (speakingEntryIndex < 0 &&
              pipelineState === "speaking" &&
              i === transcript.length - 1));
        const isActiveAgentTurn =
          isSpeakingEntry ||
          (entry.role === "agent" &&
            highlightInfo !== null &&
            highlightInfo!.entryIndex === i);

        if (isActiveAgentTurn) {
          return (
            <StreamingAgentBubble
              key={i}
              entry={entry}
              entryIndex={i}
              highlightInfo={highlightInfo ?? null}
              speaking={isSpeakingEntry}
            />
          );
        }

        return <ChatBubble key={i} entry={entry} />;
      })}

      {pipelineState && (
        <div className={`flex items-center gap-2 self-start px-3 py-1.5 rounded-full ${isDark ? "bg-slate-800/50 border border-slate-700/50" : "bg-gray-100 border border-gray-200"}`}>
          <div className={`w-2 h-2 rounded-full ${stateDot[pipelineState]}`} />
          <span className={`text-xs ${isDark ? "text-slate-400" : "text-gray-500"}`}>{stateLabel[pipelineState]}</span>
        </div>
      )}

      <div ref={bottomRef} />
    </div>
  );
}
