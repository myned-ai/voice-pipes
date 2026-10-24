import { useState, useRef, useEffect } from "react";
import { ArrowDown, ArrowUp, X, Filter, ChevronRight, ChevronDown, AlertTriangle, Zap, MessageSquare, Mic, Volume2, Radio, Download } from "lucide-react";
import type { WireEvent } from "./usePlaygroundSession";
import { useTheme } from "../../contexts/ThemeContext";

interface Props {
  events: WireEvent[];
  onClose: () => void;
}

type EventFilter = "all" | "audio" | "text" | "control" | "error" | "message" | "mic";

/** A run of consecutive events sharing the same type + direction. */
interface EventGroup {
  /** Key for the group based on its position in the grouped array. */
  key: number;
  type: string;
  direction: "in" | "out";
  events: WireEvent[];
}

/** Event types that are noisy frame-level events — these get collapsed into groups.
 *  All other types (control, text, message, error) are always shown individually. */
const GROUPABLE_TYPES = new Set(["audio", "mic", "annotation"]);

/** Collapse consecutive events with the same type + direction into groups.
 *  Only audio and mic events are grouped; everything else is always ungrouped. */
function groupConsecutive(events: WireEvent[]): EventGroup[] {
  const groups: EventGroup[] = [];
  for (const evt of events) {
    const last = groups[groups.length - 1];
    if (
      last &&
      last.type === evt.type &&
      last.direction === evt.direction &&
      GROUPABLE_TYPES.has(evt.type)
    ) {
      last.events.push(evt);
    } else {
      groups.push({ key: groups.length, type: evt.type, direction: evt.direction, events: [evt] });
    }
  }
  return groups;
}

/** Type-specific badge colors and icons. */
function typeBadge(type: string, isDark: boolean): { className: string; icon: React.ReactNode } {
  switch (type) {
    case "error":
      return {
        className: isDark ? "text-red-400 bg-red-500/15" : "text-red-600 bg-red-100",
        icon: <AlertTriangle size={9} />,
      };
    case "control":
      return {
        className: isDark ? "text-amber-400 bg-amber-500/15" : "text-amber-600 bg-amber-100",
        icon: <Zap size={9} />,
      };
    case "text":
      return {
        className: isDark ? "text-sky-400 bg-sky-500/15" : "text-sky-600 bg-sky-100",
        icon: <MessageSquare size={9} />,
      };
    case "message":
      return {
        className: isDark ? "text-violet-400 bg-violet-500/15" : "text-violet-600 bg-violet-100",
        icon: <MessageSquare size={9} />,
      };
    case "audio":
      return {
        className: isDark ? "text-emerald-400 bg-emerald-500/15" : "text-emerald-600 bg-emerald-100",
        icon: <Volume2 size={9} />,
      };
    case "mic":
      return {
        className: isDark ? "text-teal-400 bg-teal-500/15" : "text-teal-600 bg-teal-100",
        icon: <Mic size={9} />,
      };
    case "data":
      return {
        className: isDark ? "text-orange-400 bg-orange-500/15" : "text-orange-600 bg-orange-100",
        icon: <Radio size={9} />,
      };
    default:
      return {
        className: isDark ? "text-slate-400 bg-slate-500/15" : "text-gray-500 bg-gray-100",
        icon: null,
      };
  }
}

export default function EventLog({ events, onClose }: Props) {
  const { isDark } = useTheme();
  const [filter, setFilter] = useState<EventFilter>("all");
  const [showDetails, setShowDetails] = useState<boolean>(false);
  const [expandedEvents, setExpandedEvents] = useState<Set<string>>(() => new Set());
  const [expandedGroups, setExpandedGroups] = useState<Set<number>>(() => new Set());
  const bottomRef = useRef<HTMLDivElement>(null);

  const filtered = events.filter((e) => {
    if (!showDetails) {
      if (e.type === "control" && typeof e.summary === "string") {
        if (e.summary.startsWith("ping") || e.summary.startsWith("pong") || e.summary.startsWith("metrics")) return false;
      }
    }
    if (filter === "all") return true;
    return e.type === filter;
  });

  const exportToCsv = () => {
    const csvContent = "Timestamp,Direction,Type,Summary,Detail\n" + filtered.map(e => {
        const timestamp = new Date(e.timestamp).toISOString();
        const direction = e.direction;
        const type = e.type;
        const summary = typeof e.summary === "string" ? `"${e.summary.replace(/"/g, '""')}"` : '""';
        const detail = e.detail ? `"${JSON.stringify(e.detail).replace(/"/g, '""')}"` : '""';
        return `${timestamp},${direction},${type},${summary},${detail}`;
    }).join("\n");
    
    const blob = new Blob([csvContent], { type: 'text/csv;charset=utf-8;' });
    const url = URL.createObjectURL(blob);
    const link = document.createElement("a");
    link.setAttribute("href", url);
    link.setAttribute("download", "event_log_export.csv");
    document.body.appendChild(link);
    link.click();
    document.body.removeChild(link);
  };

  const groups = groupConsecutive(filtered);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [filtered.length]);

  const toggleGroup = (key: number) => {
    setExpandedGroups((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  };

  const dirIcon = (dir: "in" | "out") =>
    dir === "in" ? (
      <ArrowDown size={10} className="text-emerald-400" />
    ) : (
      <ArrowUp size={10} className="text-indigo-400" />
    );

  const filters: { key: EventFilter; label: string }[] = [
    { key: "all", label: "All" },
    { key: "text", label: "Text" },
    { key: "message", label: "Msg" },
    { key: "control", label: "Ctrl" },
    { key: "error", label: "Err" },
    { key: "audio", label: "Audio" },
    { key: "mic", label: "Mic" },
  ];

  /** Count of each filter category for counts in the filter bar. */
  const counts: Record<EventFilter, number> = {
    all: events.length,
    text: 0, message: 0, control: 0, error: 0, audio: 0, mic: 0,
  };
  for (const e of events) {
    if (e.type in counts) counts[e.type as EventFilter]++;
  }

  const renderEvent = (evt: WireEvent, evtKey: string) => {
    const badge = typeBadge(evt.type, isDark);
    const isError = evt.type === "error";
    const isExpanded = expandedEvents.has(evtKey);
    return (
      <div
        key={evtKey}
        onClick={() => setExpandedEvents(prev => {
          const next = new Set(prev);
          if (next.has(evtKey)) next.delete(evtKey);
          else next.add(evtKey);
          return next;
        })}
        className={`px-3 py-1.5 cursor-pointer transition-colors ${
          isError
            ? isDark ? 'border-b border-red-900/30 hover:bg-red-900/20' : 'border-b border-red-100 hover:bg-red-50'
            : isDark ? 'border-b border-slate-700/30 hover:bg-slate-750' : 'border-b border-gray-100 hover:bg-gray-50'
        }`}
      >
        <div className="flex items-center gap-1.5">
          {dirIcon(evt.direction)}
          <span className={`text-[10px] font-mono ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
            {new Date(evt.timestamp).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit", fractionalSecondDigits: 3 } as Intl.DateTimeFormatOptions)}
          </span>
          <span className={`inline-flex items-center gap-0.5 text-[9px] font-bold uppercase px-1.5 py-0.5 rounded-full ${badge.className}`}>
            {badge.icon}
            {evt.type}
          </span>
        </div>
        <div className={`text-[11px] mt-0.5 ${isError ? (isDark ? 'text-red-400' : 'text-red-600') : isDark ? 'text-slate-400' : 'text-gray-500'} ${isExpanded ? '' : 'truncate'}`}>
          {evt.summary}
        </div>
        {isExpanded && evt.detail != null && (
          <pre className={`text-[10px] mt-1 whitespace-pre-wrap break-all max-h-48 overflow-y-auto rounded p-1.5 ${isDark ? 'text-slate-500 bg-slate-900/50' : 'text-gray-500 bg-gray-100'}`}>
            {String(typeof evt.detail === 'object' ? JSON.stringify(evt.detail, null, 2) : evt.detail)}
          </pre>
        )}
      </div>
    );
  };

  return (
    <div className={`w-80 border-l flex flex-col shrink-0 ${isDark ? 'bg-slate-800 border-slate-700' : 'bg-white border-gray-200'}`}>
      {/* Header */}
      <div className={`flex items-center justify-between px-3 py-2.5 border-b ${isDark ? 'border-slate-700' : 'border-gray-200'}`}>
        <div className="flex items-center gap-2">
          <Filter size={14} className={isDark ? 'text-slate-400' : 'text-gray-400'} />
          <h3 className={`text-xs font-semibold uppercase tracking-wide ${isDark ? 'text-slate-300' : 'text-gray-700'}`}>Events</h3>
          <span className={`text-[10px] ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>{filtered.length}</span>
          {counts.error > 0 && (
            <span className="inline-flex items-center gap-0.5 text-[9px] font-bold text-red-400 bg-red-500/15 px-1.5 py-0.5 rounded-full">
              <AlertTriangle size={9} />
              {counts.error}
            </span>
          )}
        </div>
        <button onClick={onClose} className={`transition-colors ${isDark ? 'text-slate-400 hover:text-white' : 'text-gray-400 hover:text-gray-900'}`}>
          <X size={14} />
        </button>
      </div>

      {/* Filter bar */}
      <div className={`flex gap-1 px-3 py-1.5 border-b flex-wrap items-center justify-between ${isDark ? 'border-slate-700/50' : 'border-gray-200'}`}>
        <div className="flex gap-1 flex-wrap">
          {filters.map((f) => {
            const c = counts[f.key];
            // Hide filter buttons with 0 events (except "all")
            if (f.key !== "all" && c === 0) return null;
            return (
              <button
                key={f.key}
                onClick={() => setFilter(f.key)}
                className={`px-2 py-0.5 rounded text-[10px] font-medium transition-colors ${
                  filter === f.key
                    ? f.key === "error" ? "bg-red-600/30 text-red-300" : "bg-indigo-600/30 text-indigo-300"
                    : isDark ? "text-slate-500 hover:text-slate-300" : "text-gray-400 hover:text-gray-600"
                }`}
              >
                {f.label}
                {f.key !== "all" && c > 0 && (
                  <span className="ml-1 opacity-60">{c}</span>
                )}
              </button>
            );
          })}
        </div>
        <div className="flex items-center gap-2 pr-1">
          <label className={`flex items-center gap-1.5 cursor-pointer text-[10px] ${isDark ? 'text-slate-400 hover:text-slate-300' : 'text-gray-500 hover:text-gray-700'}`}>
            <input 
              type="checkbox" 
              checked={showDetails} 
              onChange={(e) => setShowDetails(e.target.checked)} 
              className={`rounded opacity-70 ${isDark ? 'accent-indigo-500 bg-slate-700 border-slate-600' : 'accent-indigo-600 bg-gray-100 border-gray-300'}`}
            />
            Details
          </label>
          <button onClick={exportToCsv} className={`p-1 rounded cursor-pointer transition-colors ${isDark ? 'text-slate-400 hover:text-slate-200' : 'text-gray-500 hover:text-gray-700'}`} title="Export to CSV">
            <Download size={14} />
          </button>
        </div>
      </div>

      {/* Event list */}
      <div className="flex-1 overflow-y-auto">
        {groups.map((group) => {
          if (group.events.length === 1) {
            return renderEvent(group.events[0], `single-${group.key}`);
          }

          const isOpen = expandedGroups.has(group.key);
          const first = group.events[0];
          const last = group.events[group.events.length - 1];
          const badge = typeBadge(group.type, isDark);

          return (
            <div key={`group-${group.key}`}>
              {/* Group header */}
              <div
                onClick={() => toggleGroup(group.key)}
                className={`px-3 py-1.5 cursor-pointer transition-colors ${isDark ? 'border-b border-slate-700/30 hover:bg-slate-750 bg-slate-800/80' : 'border-b border-gray-100 hover:bg-gray-50 bg-gray-50/80'}`}
              >
                <div className="flex items-center gap-1.5">
                  {isOpen ? (
                    <ChevronDown size={10} className={isDark ? 'text-slate-500' : 'text-gray-400'} />
                  ) : (
                    <ChevronRight size={10} className={isDark ? 'text-slate-500' : 'text-gray-400'} />
                  )}
                  {dirIcon(first.direction)}
                  <span className={`text-[10px] font-mono ${isDark ? 'text-slate-500' : 'text-gray-400'}`}>
                    {new Date(first.timestamp).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" })}
                    {" \u2013 "}
                    {new Date(last.timestamp).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" })}
                  </span>
                  <span className={`inline-flex items-center gap-0.5 text-[9px] font-bold uppercase px-1.5 py-0.5 rounded-full ${badge.className}`}>
                    {badge.icon}
                    {group.type}
                  </span>
                  <span className={`ml-auto text-[10px] font-medium px-1.5 rounded ${isDark ? 'text-slate-500 bg-slate-700/50' : 'text-gray-400 bg-gray-200'}`}>
                    \u00d7{group.events.length}
                  </span>
                </div>
              </div>

              {/* Expanded children */}
              {isOpen &&
                group.events.map((evt, j) =>
                  renderEvent(evt, `group-${group.key}-${j}`)
                )}
            </div>
          );
        })}
        <div ref={bottomRef} />
      </div>
    </div>
  );
}
