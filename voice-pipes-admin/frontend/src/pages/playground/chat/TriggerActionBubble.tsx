import { memo, useState } from "react";
import { TerminalSquare } from "lucide-react";
import type { TranscriptEntry } from "../session/types";
import type { ClientToolPhase } from "../../../lib/transport/types";
import { useTheme } from "../../../contexts/ThemeContext";
import { DEFAULT_COMPLETED_RESULT, resultTextFor } from "./triggerActionResult";

interface Props {
  entry: TranscriptEntry;
  /** Sends a `client_tool_result` control for this card's `call_id`.
   *  Absent when the session is not connected (buttons disabled). */
  onResult?: (
    callId: string,
    phase: ClientToolPhase,
    result?: unknown,
    speak?: boolean,
  ) => void;
}

type CardStatus = "pending" | "started" | "progress" | "completed" | "failed";

const STATUS_STYLE: Record<CardStatus, string> = {
  pending: "bg-amber-500/15 text-amber-400 border-amber-500/40",
  started: "bg-sky-500/15 text-sky-400 border-sky-500/40",
  progress: "bg-sky-500/15 text-sky-400 border-sky-500/40",
  completed: "bg-emerald-500/15 text-emerald-400 border-emerald-500/40",
  failed: "bg-red-500/15 text-red-400 border-red-500/40",
};

/** Renders a `trigger_action` PipelineMessage — a client-side tool
 *  command issued by the server. The
 *  playground doubles as the reference client-tool executor: the card
 *  exposes Started / Progress / Complete / Fail controls (with an
 *  editable JSON result) so a tester can step through the full loop by
 *  hand — including duplicate/late replies (press Complete twice to
 *  test server-side idempotency).
 *
 *  The per-result "speak" request checkbox is hidden for now (the wire
 *  affordance — `speak: true` on the client_tool_result control — still
 *  exists server-side); re-add the control here once its UX is settled. */
function TriggerActionBubbleImpl({ entry, onResult }: Props) {
  const { isDark } = useTheme();
  const [localStatus, setLocalStatus] = useState<CardStatus>("pending");
  const [resultJson, setResultJson] = useState(DEFAULT_COMPLETED_RESULT);
  // Untouched, the box stands for "the default outcome": Fail then sends a
  // failure payload instead of the "ok" shown for Complete.
  const [resultEdited, setResultEdited] = useState(false);
  const [jsonError, setJsonError] = useState<string | null>(null);

  const pm = entry.pipelineMessage;
  if (!pm) return null;
  const payload = (pm.payload ?? {}) as {
    call_id?: string;
    name?: string;
    arguments?: unknown;
    speak_on_result?: boolean;
  };
  const callId = payload.call_id ?? "";

  // The transcript entry is the durable record of what was sent (it
  // survives re-renders and remounts); local state only bridges the
  // instant between click and reducer update.
  const outcome = entry.clientToolOutcome;
  const status: CardStatus = outcome?.phase ?? localStatus;

  const parseResult = (phase: "completed" | "failed"): { ok: boolean; value?: unknown } => {
    const raw = resultTextFor(phase, resultJson, resultEdited).trim();
    if (!raw) return { ok: true, value: undefined };
    try {
      const value = JSON.parse(raw);
      setJsonError(null);
      return { ok: true, value };
    } catch (e) {
      setJsonError(e instanceof Error ? e.message : String(e));
      return { ok: false };
    }
  };

  const send = (phase: ClientToolPhase) => {
    if (!onResult || !callId) return;
    if (phase === "completed" || phase === "failed") {
      const parsed = parseResult(phase);
      if (!parsed.ok) return;
      onResult(callId, phase, parsed.value);
      setLocalStatus(phase);
    } else {
      onResult(callId, phase);
      setLocalStatus(phase as CardStatus);
    }
  };

  const btn = (label: string, phase: ClientToolPhase, emphasis?: "ok" | "bad") => (
    <button
      onClick={() => send(phase)}
      disabled={!onResult || !callId}
      className={`rounded px-2 py-0.5 text-[11px] font-medium border transition-colors disabled:opacity-40 disabled:cursor-not-allowed ${
        emphasis === "ok"
          ? "border-emerald-500/50 text-emerald-400 hover:bg-emerald-500/15"
          : emphasis === "bad"
            ? "border-red-500/50 text-red-400 hover:bg-red-500/15"
            : isDark
              ? "border-slate-600 text-slate-300 hover:bg-slate-700"
              : "border-gray-300 text-gray-600 hover:bg-gray-100"
      }`}
    >
      {label}
    </button>
  );

  return (
    <div
      className={`self-start max-w-[90%] min-w-[280px] rounded-xl px-3.5 py-2.5 text-sm border ${
        isDark
          ? "bg-indigo-950/40 border-indigo-700/50 text-slate-200"
          : "bg-indigo-50 border-indigo-200 text-gray-800"
      }`}
    >
      <div className="flex items-center gap-2 mb-1.5">
        <TerminalSquare size={14} className={isDark ? "text-indigo-400" : "text-indigo-500"} />
        <span
          className={`text-[10px] font-bold uppercase tracking-widest ${
            isDark ? "text-indigo-400" : "text-indigo-500"
          }`}
        >
          Client Command
        </span>
        {payload.speak_on_result && (
          <span
            title="This tool is declared with speak_on_result = true: the server speaks a generated confirmation after Complete."
            className={`rounded-full border px-2 py-px text-[10px] font-medium cursor-help ${
              isDark
                ? "border-indigo-500/50 text-indigo-300 bg-indigo-500/15"
                : "border-indigo-300 text-indigo-600 bg-indigo-100"
            }`}
          >
            🔊 speaks on result
          </span>
        )}
        <span
          className={`ml-auto rounded-full border px-2 py-px text-[10px] font-medium ${STATUS_STYLE[status]}`}
        >
          {status}
        </span>
      </div>

      <div className="font-mono text-xs mb-1">
        <span className={isDark ? "text-indigo-300" : "text-indigo-600"}>
          {payload.name ?? "?"}
        </span>
        <span className={isDark ? "text-slate-400" : "text-gray-500"}>
          ({JSON.stringify(payload.arguments ?? {})})
        </span>
      </div>
      <div className={`text-[10px] font-mono mb-2 ${isDark ? "text-slate-500" : "text-gray-400"}`}>
        call_id: {callId || "(missing)"}
      </div>

      <textarea
        value={resultJson}
        onChange={(e) => {
          setResultJson(e.target.value);
          setResultEdited(true);
        }}
        title="Sent with Complete or Fail. Left as is, Fail sends an error result instead."
        rows={2}
        spellCheck={false}
        placeholder='result JSON, e.g. {"status": "ok"}'
        className={`w-full rounded-lg px-2 py-1 text-[11px] font-mono resize-y focus:outline-none focus:border-indigo-500 ${
          isDark
            ? "bg-slate-900/80 border border-slate-700 text-slate-200 placeholder-slate-600"
            : "bg-white border border-gray-200 text-gray-800 placeholder-gray-300"
        }`}
      />
      {jsonError && (
        <div className="text-[10px] text-red-400 mt-0.5">invalid JSON: {jsonError}</div>
      )}

      <div className="flex items-center gap-1.5 mt-1.5">
        {btn("Started", "started")}
        {btn("Progress", "progress")}
        {btn("Complete", "completed", "ok")}
        {btn("Fail", "failed", "bad")}
      </div>

      {outcome && (
        <div
          className={`mt-1.5 rounded-lg px-2 py-1 text-[10px] font-mono ${
            isDark ? "bg-slate-900/60 text-slate-400" : "bg-white/70 text-gray-500"
          }`}
        >
          sent {outcome.phase}
          {outcome.speak ? " · speak requested" : ""}
          {outcome.result !== undefined
            ? ` · result ${JSON.stringify(outcome.result)}`
            : ""}
          {" · "}
          {new Date(outcome.timestamp).toLocaleTimeString()}
        </div>
      )}

      <div className={`text-[10px] mt-1.5 ${isDark ? "text-slate-500" : "text-gray-400"}`}>
        {new Date(entry.timestamp).toLocaleTimeString()}
      </div>
    </div>
  );
}

export const TriggerActionBubble = memo(TriggerActionBubbleImpl);
