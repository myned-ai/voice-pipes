import { useState, useRef, useEffect } from "react";
import { Play, Square, Loader2, AlertCircle, ChevronDown, Trash2, Settings2 } from "lucide-react";
import type { SessionState } from "./usePlaygroundSession";
import type { PipelineState, SessionMode, QualityTier } from "../../lib/transport/types";
import { useTheme } from "../../contexts/ThemeContext";
import { describeTarget, type ConnectSource, type ConnectedTarget } from "./connectTarget";
import ClientToolsChip from "./ClientToolsChip";
import type { ClientToolInfo } from "./clientTools";

interface Props {
  sessionState: SessionState;
  pipelineState: PipelineState | null;
  errorMessage: string | null;
  url: string;
  onUrlChange: (url: string) => void;
  /** Where Start goes: a worker the orchestrator picks for the selected
   *  pipeline, or the URL in the box. Only the input that belongs to the
   *  source in force is shown, so nothing on the bar is silently ignored. */
  source: ConnectSource;
  onSourceChange: (source: ConnectSource) => void;
  /** Why "Pipeline" cannot be chosen (orchestrator down, nothing registered);
   *  null when it can. */
  pipelineUnavailable?: string | null;
  onStart: () => void;
  onDisconnect: () => void;
  /** The orchestrator is being asked for a worker. */
  findingWorker?: boolean;
  /** Where the live session landed; shown in place of the inputs. */
  target?: ConnectedTarget | null;
  pipelines: { name: string; hash: string; workerCount: number }[];
  selectedPipeline: string;
  onPipelineChange: (name: string) => void;
  /** Opens the settings panel — jitter buffer, output chunking, mic checks.
   *  Was commented out, which left `prebufferMs` and the scheduling caps
   *  unreachable at runtime and only changeable by rebuilding. */
  onToggleSettings: () => void;
  showSettings: boolean;
  onToggleEvents: () => void;
  showEvents: boolean;
  onToggleTimeline: () => void;
  showTimeline: boolean;
  /** Playground extensions: the "Stage"
   *  toggle renders only when at least one extension is configured. Optional
   *  so callers without extension support need no changes. */
  hasExtensions?: boolean;
  showStage?: boolean;
  onToggleStage?: () => void;
  /** Names of mounted extensions whose `ready()` is still pending while a
   *  Start is held (readiness gate). Null/empty = not waiting. */
  waitingFor?: string[] | null;
  /** Fidelity tier the server last reported (adaptive quality). Rendered only
   *  when degraded — a badge that is always on is a badge nobody reads. */
  qualityTier?: QualityTier;
  /** Client tools of the worker the session landed on; the chip shows only
   *  while connected and only when there are some. */
  clientTools?: ClientToolInfo[];
  sessionMode: SessionMode;
  onSessionModeChange: (mode: SessionMode) => void;
  onClearConversation: () => void;
  hasConversation: boolean;
}

/** Only degraded tiers get a badge: `full` is the normal state and showing it
 *  would train people to ignore the indicator. */
const tierBadges: Record<QualityTier, { label: string; className: string; title: string } | null> = {
  full: null,
  reduced: {
    label: "Reduced",
    className: "bg-yellow-500/20 text-yellow-300 border-yellow-500/40",
    title: "The link is struggling; the server is shedding some fidelity and the jitter buffer has been deepened.",
  },
  minimal: {
    label: "Minimal",
    className: "bg-orange-500/20 text-orange-300 border-orange-500/40",
    title: "The link cannot carry a full-fidelity session; audio and metadata are being reduced.",
  },
  critical: {
    label: "Critical",
    className: "bg-red-500/20 text-red-300 border-red-500/40",
    title: "The link is barely usable; only intelligible speech is being protected.",
  },
};

const stateColors: Record<SessionState, string> = {
  idle: "bg-slate-500",
  connecting: "bg-yellow-400 animate-pulse",
  active: "bg-emerald-400",
  error: "bg-red-500",
};

const pipelineLabels: Record<PipelineState, string> = {
  listening: "Listening",
  processing: "Processing",
  speaking: "Speaking",
  draining: "Draining",
};

const MODES: { value: SessionMode; label: string }[] = [
  { value: "voice",  label: "Voice" },
  { value: "text",   label: "Text" },
  { value: "hybrid", label: "Hybrid" },
];

function ModeDropdown({ value, onChange, isDark }: { value: SessionMode; onChange: (v: SessionMode) => void; isDark: boolean }) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const handler = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [open]);

  const selected = MODES.find((m) => m.value === value) ?? MODES[0];

  return (
    <div ref={ref} className="relative">
      <button
        onClick={() => setOpen((o) => !o)}
        title="Session mode: speak, type, or both"
        aria-label="Session mode"
        className={`flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-sm font-medium border transition-colors focus:outline-none ${
          open
            ? isDark ? "bg-slate-700 border-indigo-500 text-white" : "bg-gray-100 border-indigo-400 text-gray-900"
            : isDark ? "bg-slate-800 border-slate-600 text-slate-200 hover:border-slate-500 hover:bg-slate-700" : "bg-white border-gray-300 text-gray-700 hover:border-gray-400 hover:bg-gray-50"
        }`}
      >
        {selected.label}
        <ChevronDown size={13} className={`transition-transform duration-150 ${open ? "rotate-180" : ""}`} />
      </button>
      {open && (
        <div className={`absolute left-0 top-full mt-1 w-32 rounded-lg border shadow-xl z-50 overflow-hidden ${
          isDark ? "bg-slate-800 border-slate-600" : "bg-white border-gray-200"
        }`}>
          {MODES.map((m) => (
            <button
              key={m.value}
              onClick={() => { onChange(m.value); setOpen(false); }}
              className={`w-full text-left px-3 py-2 text-sm transition-colors ${
                m.value === value
                  ? isDark ? "bg-indigo-600/30 text-indigo-300 font-medium" : "bg-indigo-50 text-indigo-600 font-medium"
                  : isDark ? "text-slate-300 hover:bg-slate-700" : "text-gray-700 hover:bg-gray-50"
              }`}
            >
              {m.label}
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

export default function ConnectionBar({
  sessionState,
  pipelineState,
  errorMessage,
  url,
  onUrlChange,
  source,
  onSourceChange,
  pipelineUnavailable = null,
  onStart,
  onDisconnect,
  findingWorker = false,
  target = null,
  pipelines,
  selectedPipeline,
  onPipelineChange,
  onToggleEvents,
  showEvents,
  onToggleTimeline,
  showTimeline,
  hasExtensions = false,
  showStage = false,
  onToggleStage,
  waitingFor = null,
  onToggleSettings,
  showSettings,
  qualityTier,
  clientTools = [],
  sessionMode,
  onSessionModeChange,
  onClearConversation,
  hasConversation,
}: Props) {
  const { isDark } = useTheme();
  const connected = sessionState === "active";
  const connecting = sessionState === "connecting";
  const waiting = !!waitingFor && waitingFor.length > 0;

  return (
    <div className="flex flex-col shrink-0">
    <div className={`flex items-center gap-3 px-4 py-2.5 border-b ${isDark ? 'bg-slate-800 border-slate-700' : 'bg-white border-gray-200'}`}>
      {/* Status dot */}
      <div className={`w-2.5 h-2.5 rounded-full ${stateColors[sessionState]}`} title={sessionState} />

      {/* Where the session goes. Hidden once it is going: the choice is made,
          and what matters then is where it landed. */}
      {!connected && !connecting && (
        <div className={`flex flex-1 min-w-0 rounded-lg border overflow-hidden ${isDark ? 'border-slate-600' : 'border-gray-300'}`}>
          <select
            value={source}
            onChange={(e) => onSourceChange(e.target.value as ConnectSource)}
            aria-label="Connect to"
            title={pipelineUnavailable
              ? `${pipelineUnavailable}, so only a direct URL can be used`
              : "Pipeline: the orchestrator picks a worker for you. Direct URL: connect to one worker you name."}
            className={`shrink-0 px-2 py-1.5 text-sm font-medium border-r focus:outline-none ${isDark ? 'bg-slate-700 border-slate-600 text-slate-200' : 'bg-gray-100 border-gray-300 text-gray-700'}`}
          >
            <option value="pipeline" disabled={!!pipelineUnavailable}>
              {pipelineUnavailable ? `Pipeline (${pipelineUnavailable.toLowerCase()})` : "Pipeline"}
            </option>
            <option value="url">Direct URL</option>
          </select>
          {source === "pipeline" ? (
            <select
              value={selectedPipeline}
              onChange={(e) => onPipelineChange(e.target.value)}
              aria-label="Pipeline"
              title="The orchestrator picks the least busy worker serving this pipeline"
              className={`flex-1 min-w-0 px-3 py-1.5 text-sm focus:outline-none ${isDark ? 'bg-slate-900 text-slate-200' : 'bg-gray-50 text-gray-800'}`}
            >
              <option value="">Any pipeline</option>
              {pipelines.map((p) => (
                <option key={p.hash} value={p.name}>
                  {p.name} · {p.workerCount} worker{p.workerCount !== 1 ? 's' : ''}
                </option>
              ))}
            </select>
          ) : (
            <input
              type="text"
              value={url}
              onChange={(e) => onUrlChange(e.target.value)}
              aria-label="Worker URL"
              placeholder="ws://localhost:8800/ws"
              className={`flex-1 min-w-0 px-3 py-1.5 text-sm focus:outline-none ${isDark ? 'bg-slate-900 text-slate-200 placeholder-slate-500' : 'bg-gray-50 text-gray-800 placeholder-gray-400'}`}
            />
          )}
        </div>
      )}

      {/* Where the session landed */}
      {(connected || connecting) && (
        <span
          className={`flex-1 min-w-0 truncate text-sm ${isDark ? 'text-slate-300' : 'text-gray-700'}`}
          title={target ? target.url : url}
        >
          {target ? describeTarget(target) : url}
        </span>
      )}

      {/* Pipeline state badge */}
      {connected && pipelineState && (
        <span className={`px-2.5 py-1 rounded-full text-xs font-medium ${isDark ? 'bg-slate-700 text-slate-300' : 'bg-gray-200 text-gray-600'}`}>
          {pipelineLabels[pipelineState]}
        </span>
      )}

      {connected && <ClientToolsChip tools={clientTools} />}

      {/* Adaptive-quality tier — only when degraded, and only while connected:
          the tier is a property of a live link. */}
      {connected && qualityTier && tierBadges[qualityTier] && (
        <span
          title={tierBadges[qualityTier].title}
          className={`px-2.5 py-1 rounded-full text-xs font-medium border ${tierBadges[qualityTier].className}`}
        >
          {tierBadges[qualityTier].label}
        </span>
      )}

      {/* Session mode selector */}
      {!connected && !connecting && (
        <ModeDropdown value={sessionMode} onChange={onSessionModeChange} isDark={isDark} />
      )}

      {/* The one action: start, or end what was started */}
      {connected || connecting ? (
        <button
          onClick={onDisconnect}
          className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-sm font-medium bg-red-600/20 text-red-400 hover:bg-red-600/30 transition-colors"
        >
          {connecting ? <Loader2 size={14} className="animate-spin" /> : <Square size={13} />}
          {connecting ? "Cancel" : "End session"}
        </button>
      ) : (
        <button
          onClick={onStart}
          disabled={findingWorker || waiting}
          title={waiting ? `Waiting for ${waitingFor!.join(", ")} to finish loading` : undefined}
          className="flex items-center gap-1.5 px-3.5 py-1.5 rounded-lg text-sm font-semibold bg-emerald-600 text-white hover:bg-emerald-500 transition-colors disabled:opacity-50"
        >
          {findingWorker || waiting ? <Loader2 size={14} className="animate-spin" /> : <Play size={13} />}
          {waiting
            ? `Waiting for ${waitingFor![0]}${waitingFor!.length > 1 ? "…" : ""}`
            : findingWorker ? "Finding a worker…" : "Start session"}
        </button>
      )}

      {/* Settings toggle */}
      <button
        onClick={onToggleSettings}
        className={`p-1.5 rounded-md transition-colors ${showSettings ? "bg-indigo-600/30 text-indigo-300" : (isDark ? 'text-slate-400 hover:text-white hover:bg-slate-700' : 'text-gray-500 hover:text-gray-900 hover:bg-gray-100')}`}
        title="Settings — playback, output chunking, microphone and component checks"
      >
        <Settings2 size={16} />
      </button>

      {/* Events toggle */}
      <button
        onClick={onToggleEvents}
        className={`px-2.5 py-1 rounded-md text-xs font-medium transition-colors ${showEvents ? "bg-indigo-600/30 text-indigo-300" : (isDark ? "text-slate-400 hover:text-white hover:bg-slate-700" : "text-gray-500 hover:text-gray-900 hover:bg-gray-100")}`}
        title="Toggle event log"
      >
        Events
      </button>

      {/* State timeline toggle (T6) */}
      <button
        onClick={onToggleTimeline}
        className={`px-2.5 py-1 rounded-md text-xs font-medium transition-colors ${showTimeline ? "bg-indigo-600/30 text-indigo-300" : (isDark ? "text-slate-400 hover:text-white hover:bg-slate-700" : "text-gray-500 hover:text-gray-900 hover:bg-gray-100")}`}
        title="Toggle state timeline"
      >
        Timeline
      </button>

      {/* Extension stage toggle — only when a manifest/env configured extensions */}
      {hasExtensions && (
        <button
          onClick={onToggleStage}
          className={`px-2.5 py-1 rounded-md text-xs font-medium transition-colors ${showStage ? "bg-indigo-600/30 text-indigo-300" : (isDark ? "text-slate-400 hover:text-white hover:bg-slate-700" : "text-gray-500 hover:text-gray-900 hover:bg-gray-100")}`}
          title="Toggle extension stage"
        >
          Stage
        </button>
      )}

      {/* Clear conversation */}
      {hasConversation && (
        <button
          onClick={onClearConversation}
          className={`p-1.5 rounded-md transition-colors ${isDark ? 'text-slate-500 hover:text-red-400 hover:bg-slate-700' : 'text-gray-400 hover:text-red-500 hover:bg-gray-100'}`}
          title="Clear conversation"
        >
          <Trash2 size={15} />
        </button>
      )}
    </div>

    {/* Error banner */}
    {errorMessage && (
      <div className={`flex items-center gap-2 px-4 py-2 text-sm border-b ${
        isDark
          ? 'bg-red-900/30 border-red-800/50 text-red-300'
          : 'bg-red-50 border-red-200 text-red-700'
      }`}>
        <AlertCircle size={14} className="shrink-0" />
        <span>{errorMessage}</span>
      </div>
    )}
    </div>
  );
}
