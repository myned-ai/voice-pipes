import { useEffect, useState } from "react";
import axios from "axios";
import { Clock, Loader2, Users, XCircle } from "lucide-react";
import { API_BASE } from "../../config/api";
import { useTheme } from "../../contexts/ThemeContext";

export interface QueueStatusEvent {
  type: "queue_status" | "queue_granted";
  ticket_id?: string;
  position?: number;
  ahead?: number;
  queue_depth?: number;
  active_sessions?: number;
  capacity_sessions?: number;
  estimated_wait_s?: number;
}

export interface CapacitySummary {
  admission_enabled: boolean;
  active_sessions: number;
  leased_sessions: number;
  capacity_sessions: number;
  effective_limit: number;
  queue_depth: number;
  estimated_wait_s: number;
}

export const PLAYGROUND_QUEUE_EVENT = "vp-playground-queue";

/** Live fleet capacity pill + FIFO waiting room banner for the Playground. */
export default function QueueWaitingBanner({
  sessionState,
  onLeaveQueue,
}: {
  sessionState: string;
  onLeaveQueue: () => void;
}) {
  const { isDark } = useTheme();
  const [queueInfo, setQueueInfo] = useState<QueueStatusEvent | null>(null);
  const [capacity, setCapacity] = useState<CapacitySummary | null>(null);

  useEffect(() => {
    const onQueueEvent = (e: Event) => {
      const detail = (e as CustomEvent<QueueStatusEvent>).detail;
      if (!detail) return;
      if (detail.type === "queue_status") {
        setQueueInfo(detail);
      } else if (detail.type === "queue_granted") {
        setQueueInfo(null);
      }
    };
    window.addEventListener(PLAYGROUND_QUEUE_EVENT, onQueueEvent);
    return () => window.removeEventListener(PLAYGROUND_QUEUE_EVENT, onQueueEvent);
  }, []);

  useEffect(() => {
    let alive = true;
    const pollCapacity = async () => {
      try {
        const res = await axios.get<{ data?: CapacitySummary }>(`${API_BASE}/api/v1/capacity`, {
          withCredentials: true,
        });
        if (alive && res.data?.data) {
          setCapacity(res.data.data);
        }
      } catch {
        /* capacity badge is optional when orchestrator is unreachable */
      }
    };
    void pollCapacity();
    const timer = window.setInterval(() => void pollCapacity(), 10_000);
    return () => {
      alive = false;
      window.clearInterval(timer);
    };
  }, [sessionState]);

  const activeQueue = sessionState === "idle" || sessionState === "error" ? null : queueInfo;
  const showCapacityStrip =
    capacity?.admission_enabled && capacity.effective_limit > 0 && !activeQueue;
  const inUse = capacity ? capacity.active_sessions + capacity.leased_sessions : 0;
  const limit = capacity?.effective_limit ?? 0;
  const isFull = limit > 0 && inUse >= limit;

  if (!activeQueue && !showCapacityStrip) return null;

  return (
    <div
      className={`border-b px-4 py-2 text-xs transition-colors ${
        activeQueue
          ? isDark
            ? "bg-amber-950/60 border-amber-700/50 text-amber-200"
            : "bg-amber-50 border-amber-200 text-amber-900"
          : isDark
            ? "bg-slate-900/80 border-slate-800 text-slate-300"
            : "bg-gray-100 border-gray-200 text-gray-700"
      }`}
    >
      {activeQueue ? (
        <div className="flex flex-wrap items-center justify-between gap-3" role="status" aria-live="polite">
          <div className="flex items-center gap-2.5">
            <Loader2 size={15} className="animate-spin text-amber-400 shrink-0" />
            <span className="font-semibold">
              Waiting in Queue — Position #{activeQueue.position ?? 1}
            </span>
            <span className="opacity-80">
              ({activeQueue.active_sessions ?? limit}/{activeQueue.capacity_sessions ?? limit} voice slots active
              {activeQueue.estimated_wait_s ? ` · est. wait ~${activeQueue.estimated_wait_s}s` : ""})
            </span>
          </div>
          <button
            type="button"
            onClick={() => {
              setQueueInfo(null);
              onLeaveQueue();
            }}
            className="inline-flex items-center gap-1 rounded-md border border-amber-500/40 bg-amber-500/10 px-2.5 py-1 text-xs font-medium hover:bg-amber-500/20"
          >
            <XCircle size={13} />
            Leave Queue
          </button>
        </div>
      ) : (
        <div className="flex items-center justify-between gap-3">
          <div className="flex items-center gap-2">
            <Users size={13} className={isFull ? "text-amber-400" : "text-emerald-400"} />
            <span>
              Live Capacity:{" "}
              <strong>
                {Math.min(inUse, limit)}/{limit}
              </strong>{" "}
              slots in use
              {capacity && capacity.queue_depth > 0 ? ` · ${capacity.queue_depth} in queue` : ""}
            </span>
          </div>
          {isFull && capacity && capacity.estimated_wait_s > 0 && (
            <span className="inline-flex items-center gap-1 text-amber-400">
              <Clock size={12} />
              Est. queue wait ~{capacity.estimated_wait_s}s
            </span>
          )}
        </div>
      )}
    </div>
  );
}
