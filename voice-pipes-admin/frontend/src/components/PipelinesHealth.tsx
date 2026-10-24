import { useEffect, useMemo, useRef, useState } from "react";
import { Activity, AlertTriangle, CheckCircle, MinusCircle, XCircle, Server, ChevronDown, ChevronUp, Search } from "lucide-react";
import { useTheme } from "../contexts/ThemeContext";
import { API_BASE } from "../config/api";
import { fleetCapacity, type FleetCapacity } from "../lib/fleet";
import {
  pipelineHealthSummary,
  type ComponentKind,
  type ComponentSnapshot,
  type HealthState,
  type PipelineHealthSummary,
  type PipelineWorker,
  type SystemTime,
  type WorkerComponentsRollup,
  type WorkerListItem,
} from "./pipelinesHealth.logic";

/* Wire types + the pure health aggregation (`workerHealthState`,
   `pipelineHealthSummary`) live in ./pipelinesHealth.logic so this module
   exports only components (Fast Refresh) and the math stays unit-testable. */

/* ── helpers ────────────────────────────────────────────── */

function fmtAge(since: SystemTime | undefined): string {
  if (!since) return "";
  const seconds = Math.max(0, Math.floor(Date.now() / 1000 - since.secs_since_epoch));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h`;
  return `${Math.floor(hours / 24)}d`;
}

function stateColor(state: HealthState, isDark: boolean): string {
  switch (state) {
    case "healthy":
      return isDark ? "bg-emerald-500/20 text-emerald-300 border-emerald-500/40"
                    : "bg-emerald-100 text-emerald-700 border-emerald-300";
    case "degraded":
      return isDark ? "bg-amber-500/20 text-amber-300 border-amber-500/40"
                    : "bg-amber-100 text-amber-700 border-amber-300";
    case "unhealthy":
      return isDark ? "bg-rose-500/20 text-rose-300 border-rose-500/40"
                    : "bg-rose-100 text-rose-700 border-rose-300";
    case "not_configured":
      return isDark ? "bg-slate-700/40 text-slate-400 border-slate-600"
                    : "bg-gray-100 text-gray-500 border-gray-300";
  }
}

function StateIcon({ state, size = 14 }: { state: HealthState; size?: number }) {
  switch (state) {
    case "healthy":       return <CheckCircle size={size} />;
    case "degraded":      return <AlertTriangle size={size} />;
    case "unhealthy":     return <XCircle size={size} />;
    case "not_configured":return <MinusCircle size={size} />;
  }
}

/* ── pill ──────────────────────────────────────────────── */

function ComponentPill({ snap }: { snap: ComponentSnapshot }) {
  const { isDark } = useTheme();
  const color = stateColor(snap.health.state, isDark);
  
  const providerRaw = snap.telemetry.extras?.active_provider || snap.telemetry.extras?.requested_provider;
  const providerStr = typeof providerRaw === "string" ? providerRaw : undefined;
  const isGpu = providerStr && providerStr.toLowerCase().includes("cuda");
  const fallback = providerStr && providerStr.toLowerCase().includes("fallback");
  
  const tooltip = [
    `${snap.kind.toUpperCase()} · ${snap.id}`,
    `state: ${snap.health.state}`,
    snap.health.reason && `reason: ${snap.health.reason}`,
    `since: ${fmtAge(snap.health.since)} ago`,
    `processed: ${snap.telemetry.processed_total}`,
    snap.telemetry.errors_total > 0 && `errors: ${snap.telemetry.errors_total}`,
    snap.telemetry.last_error && `last_error: ${snap.telemetry.last_error}`,
    providerStr && `provider: ${providerStr}`,
  ].filter(Boolean).join("\n");

  return (
    <div
      title={tooltip}
      className={`inline-flex items-center gap-1.5 rounded border px-2 py-1 text-xs font-mono cursor-help ${color}`}
    >
      <StateIcon state={snap.health.state} />
      <span className="font-bold uppercase">{snap.kind}</span>
      <span className="opacity-70">·</span>
      <span className="truncate max-w-[8rem]">{snap.id}</span>
      
      {providerStr && (
        <span className={`ml-1 rounded px-1 text-[10px] uppercase font-semibold border ${
          isGpu && !fallback
            ? "bg-indigo-500/20 text-indigo-400 border-indigo-500/30" 
            : fallback 
            ? "bg-amber-500/20 text-amber-500 border-amber-500/30"
            : "bg-slate-500/20 text-slate-400 border-slate-500/30"
        }`}>
          {isGpu && !fallback ? "GPU" : fallback ? "GPU (FB)" : "CPU"}
        </span>
      )}

      {snap.telemetry.errors_total > 0 && (
        <span className="ml-1 rounded bg-rose-600/30 px-1 text-[10px]">
          {snap.telemetry.errors_total}e
        </span>
      )}
    </div>
  );
}

/* ── pipeline card ─────────────────────────────────────── */

interface PipelineGroup {
  pipeline_name: string;
  workers: PipelineWorker[];
}

/** Thin proportional bar: healthy / degraded / down across the pipeline's
 *  workers, so the *scale* of a problem is glanceable. */
function HealthBar({ summary }: { summary: PipelineHealthSummary }) {
  const { total, healthy, degraded, unhealthy } = summary;
  if (total === 0) return null;
  const pct = (n: number) => `${(n / total) * 100}%`;
  return (
    <span className="inline-flex h-1.5 w-14 rounded-full overflow-hidden bg-slate-600/40 align-middle">
      {healthy > 0 && <span style={{ width: pct(healthy) }} className="bg-emerald-400" />}
      {degraded > 0 && <span style={{ width: pct(degraded) }} className="bg-amber-400" />}
      {unhealthy > 0 && <span style={{ width: pct(unhealthy) }} className="bg-rose-400" />}
    </span>
  );
}

/** L0 — live concurrency vs serving capacity, colored by saturation so an
 *  operator knows to scale before the fleet is full. */
function CapacityBar({ capacity }: { capacity: FleetCapacity }) {
  const { isDark } = useTheme();
  const pct = capacity.utilizationPct ?? 0;
  const tone = pct >= 85 ? "rose" : pct >= 60 ? "amber" : "emerald";
  const barColor = tone === "rose" ? "bg-rose-400" : tone === "amber" ? "bg-amber-400" : "bg-emerald-400";
  const valColor = tone === "rose" ? (isDark ? "text-rose-300" : "text-rose-600")
    : tone === "amber" ? (isDark ? "text-amber-300" : "text-amber-600")
      : (isDark ? "text-emerald-300" : "text-emerald-600");
  return (
    <div className={`rounded-xl border px-4 py-3 flex items-center gap-4 ${isDark ? "bg-slate-800/60 border-slate-700" : "bg-white border-gray-200"}`}>
      <div className="shrink-0">
        <div className={`text-[10px] uppercase tracking-wide ${isDark ? "text-slate-500" : "text-gray-400"}`}>Live concurrency</div>
        <div className={`text-lg font-semibold tabular-nums ${isDark ? "text-white" : "text-gray-900"}`}>
          {capacity.activeSessions}<span className={`text-sm font-normal ${isDark ? "text-slate-500" : "text-gray-400"}`}> / {capacity.maxConcurrent} calls</span>
        </div>
      </div>
      <div className="flex-1 min-w-[80px]">
        <div className={`h-2 rounded-full overflow-hidden ${isDark ? "bg-slate-700" : "bg-gray-200"}`}>
          <div className={`h-full ${barColor} transition-all`} style={{ width: `${Math.min(100, pct)}%` }} />
        </div>
      </div>
      <div className={`shrink-0 text-xs ${isDark ? "text-slate-400" : "text-gray-500"}`}>
        <span className={`font-semibold ${valColor}`}>{pct.toFixed(0)}%</span> used
        <span className="mx-2">·</span>
        {capacity.activeWorkers}/{capacity.workers} workers
        {capacity.deadWorkers > 0 && <span className="text-rose-400"> · {capacity.deadWorkers} dead</span>}
      </div>
    </div>
  );
}

function PipelineCard({ group, highlight, cardRef }: { group: PipelineGroup; highlight?: boolean; cardRef?: (el: HTMLDivElement | null) => void }) {
  const { isDark } = useTheme();

  // Fraction-aware health (P14): replaces worst-of so one bad worker of N
  // doesn't paint the whole pipeline as down — the badge reflects the
  // proportion and the bar shows the scale.
  const summary = pipelineHealthSummary(group.workers);
  const aggregate = summary.status;

  const totalWorkers = group.workers.length;
  const activeWorkers = group.workers.filter((w) => w.worker.status === "active").length;
  const drainingWorkers = group.workers.filter((w) => w.worker.status === "draining").length;
  const deadWorkers = group.workers.filter((w) => w.worker.status === "dead").length;

  return (
    <div ref={cardRef} className={`rounded-xl border p-4 flex flex-col gap-3 transition-shadow ${isDark ? "bg-slate-800/60 border-slate-700" : "bg-white border-gray-200"} ${highlight ? (isDark ? "ring-2 ring-rose-500/70 shadow-lg shadow-rose-900/30" : "ring-2 ring-rose-400 shadow-lg") : ""}`}>
      {/* Header */}
      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <span className={`inline-flex items-center gap-1.5 rounded-full border px-2.5 py-1 text-xs font-semibold uppercase ${stateColor(aggregate, isDark)}`}>
            <StateIcon state={aggregate} size={12} />
            {aggregate.replace("_", " ")}
          </span>
          <h3 className={`text-base font-semibold ${isDark ? "text-white" : "text-gray-900"}`}>
            {group.pipeline_name}
          </h3>
        </div>
        <div className={`flex items-center gap-3 text-xs ${isDark ? "text-slate-400" : "text-gray-500"}`}>
          {summary.total > 0 && (
            <span
              className="inline-flex items-center gap-1.5"
              title={`${summary.healthy} healthy · ${summary.degraded} degraded · ${summary.unhealthy} down`}
            >
              <HealthBar summary={summary} />
              <span className={summary.healthy === summary.total ? "text-emerald-400" : ""}>
                {summary.healthy}/{summary.total} healthy
              </span>
            </span>
          )}
          <span className="inline-flex items-center gap-1">
            <Server size={12} />
            {activeWorkers}/{totalWorkers} active
            {drainingWorkers > 0 && <span className="text-amber-400">· {drainingWorkers} draining</span>}
            {deadWorkers > 0 && <span className="text-rose-400">· {deadWorkers} dead</span>}
          </span>
        </div>
      </div>

      {/* Workers */}
      <div className="flex flex-col gap-2">
        {group.workers.map(({ worker, rollup }) => {
          const components = rollup?.components ?? [];
          return (
            <div
              key={worker.id}
              className={`rounded-lg border px-3 py-2 ${isDark ? "bg-slate-900/40 border-slate-700/50" : "bg-gray-50 border-gray-200"}`}
            >
              <div className="flex items-center justify-between mb-2">
                <div className="flex items-center gap-2">
                  <span className={`text-xs font-mono ${isDark ? "text-slate-400" : "text-gray-600"}`}>
                    {worker.id}
                  </span>
                  <span className={`text-[10px] uppercase font-semibold ${
                    worker.status === "active" ? "text-emerald-400"
                    : worker.status === "draining" ? "text-amber-400"
                    : "text-rose-400"
                  }`}>
                    {worker.status}
                  </span>
                </div>
                {components.length === 0 && (
                  <span className={`text-[10px] italic ${isDark ? "text-slate-500" : "text-gray-400"}`}>
                    no component data yet
                  </span>
                )}
              </div>
              {components.length > 0 && (
                <div className="flex flex-wrap gap-1.5">
                  {components.map((c) => (
                    <ComponentPill key={`${worker.id}-${c.id}`} snap={c} />
                  ))}
                </div>
              )}
            </div>
          );
        })}
      </div>
    </div>
  );
}

/* ── top-level section ─────────────────────────────────── */

interface Alert {
  key: string;
  pipelineName: string;
  workerId: string;
  componentId: string;
  kind: ComponentKind;
  state: HealthState;
  reason: string;
  since: SystemTime;
  errorsTotal: number;
  timeoutsTotal: number;
  lastError?: string;
  /** Severity rank (higher = worse). Drives sort + color. */
  severity: number;
}

function alertSeverity(state: HealthState, errors: number): number {
  // unhealthy beats degraded; errors > 0 always raises above healthy.
  let s = 0;
  if (state === "unhealthy") s = 3;
  else if (state === "degraded") s = 2;
  else if (errors > 0) s = 1;
  return s;
}

export default function PipelinesHealth({ productionMode = false }: { productionMode?: boolean }) {
  const { isDark } = useTheme();
  const [workers, setWorkers] = useState<WorkerListItem[] | null>(null);
  const [components, setComponents] = useState<WorkerComponentsRollup[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [focusedPipeline, setFocusedPipeline] = useState<string | null>(null);
  const [alertsCollapsed, setAlertsCollapsed] = useState(false);
  // P14 — show a manageable number by default but let the operator expand to
  // the full list instead of silently hiding the tail.
  const [showAllAlerts, setShowAllAlerts] = useState(false);
  const ALERT_PREVIEW = 15;
  // P15 — filter the pipeline cards by name so a 20-pipeline fleet isn't a wall
  // of cards.  Alerts stay unfiltered (you always want to see every problem).
  const [pipelineFilter, setPipelineFilter] = useState("");
  const cardRefs = useRef<Map<string, HTMLDivElement | null>>(new Map());

  useEffect(() => {
    // P14 — subscribe to the BFF's shared fleet stream (one orchestrator poll,
    // fanned out to every tab) instead of each client polling /workers +
    // /components every 5s.  The browser auto-reconnects on drop.
    // EventSource cannot send headers; the login cookie carries the token.
    const es = new EventSource(`${API_BASE}/api/v1/stream/fleet`, { withCredentials: true });
    es.onmessage = (ev) => {
      try {
        const snap = JSON.parse(ev.data) as {
          workers?: unknown; components?: unknown; error?: string | null;
        };
        setWorkers(Array.isArray(snap.workers) ? (snap.workers as WorkerListItem[]) : []);
        setComponents(Array.isArray(snap.components) ? (snap.components as WorkerComponentsRollup[]) : []);
        setError(snap.error ? `Orchestrator unreachable: ${snap.error}` : null);
      } catch {
        /* ignore a malformed frame */
      }
    };
    es.onerror = () => {
      setError((prev) => prev ?? "Live stream interrupted — reconnecting…");
    };
    return () => es.close();
  }, []);

  // Group workers by pipeline_name.
  const groups: PipelineGroup[] = useMemo(() => {
    if (!workers) return [];
    const byName = new Map<string, PipelineGroup>();
    const componentsByWorker = new Map(components.map((c) => [c.worker_id, c]));
    for (const worker of workers) {
      const name = worker.pipeline_name || "(unnamed)";
      let group = byName.get(name);
      if (!group) {
        group = { pipeline_name: name, workers: [] };
        byName.set(name, group);
      }
      group.workers.push({
        worker,
        rollup: componentsByWorker.get(worker.id),
      });
    }
    return Array.from(byName.values()).sort((a, b) =>
      a.pipeline_name.localeCompare(b.pipeline_name),
    );
  }, [workers, components]);

  // L0 — live concurrency vs serving capacity across the fleet.
  const capacity = useMemo(() => fleetCapacity(workers ?? []), [workers]);

  // P15 — name filter over the pipeline cards.
  const visibleGroups = useMemo(() => {
    const q = pipelineFilter.trim().toLowerCase();
    if (!q) return groups;
    return groups.filter((g) => g.pipeline_name.toLowerCase().includes(q));
  }, [groups, pipelineFilter]);

  // Derive live alerts from the latest poll.
  const alerts: Alert[] = useMemo(() => {
    const out: Alert[] = [];
    for (const g of groups) {
      for (const { worker, rollup } of g.workers) {
        for (const c of rollup?.components ?? []) {
          const sev = alertSeverity(c.health.state, c.telemetry.errors_total);
          if (sev === 0) continue;
          out.push({
            key: `${worker.id}:${c.id}`,
            pipelineName: g.pipeline_name,
            workerId: worker.id,
            componentId: c.id,
            kind: c.kind,
            state: c.health.state,
            reason: c.health.reason,
            since: c.health.since,
            errorsTotal: c.telemetry.errors_total,
            timeoutsTotal: c.telemetry.timeouts_total,
            lastError: c.telemetry.last_error,
            severity: sev,
          });
        }
      }
    }
    out.sort((a, b) => b.severity - a.severity || b.errorsTotal - a.errorsTotal);
    return out;
  }, [groups]);

  const handleAlertClick = (pipelineName: string) => {
    setFocusedPipeline(pipelineName);
    const el = cardRefs.current.get(pipelineName);
    if (el) {
      el.scrollIntoView({ behavior: "smooth", block: "center" });
    }
    // Auto-clear highlight after a few seconds.
    window.setTimeout(() => {
      setFocusedPipeline((cur) => (cur === pipelineName ? null : cur));
    }, 4000);
  };

  if (workers === null && error === null) {
    return (
      <div className={`rounded-xl border p-6 flex items-center justify-center ${isDark ? "bg-slate-800/50 border-slate-700" : "bg-white border-gray-200"}`}>
        <Activity className="animate-spin text-indigo-500" size={20} />
      </div>
    );
  }

  return (
    <section className="flex flex-col gap-3">
      <div className="flex items-center justify-between">
        <h2 className={`text-lg font-semibold flex items-center gap-2 ${isDark ? "text-white" : "text-gray-900"}`}>
          <Server size={18} className="text-indigo-400" />
          Pipelines &amp; Component Health
        </h2>
        <span className={`inline-flex items-center gap-1.5 text-xs ${isDark ? "text-slate-500" : "text-gray-400"}`}>
          <span className="inline-block w-1.5 h-1.5 rounded-full bg-emerald-400 animate-pulse" />
          live
        </span>
      </div>

      {/* L0 — live concurrency vs serving capacity (scale before saturation). */}
      {productionMode && capacity.maxConcurrent > 0 && <CapacityBar capacity={capacity} />}

      {error && (
        <div className={`rounded-lg border px-4 py-3 text-sm ${isDark ? "bg-rose-900/20 border-rose-800 text-rose-300" : "bg-rose-50 border-rose-200 text-rose-700"}`}>
          <AlertTriangle className="inline mr-2" size={14} />
          {error}
        </div>
      )}

      {alerts.length > 0 && (
        <div className={`rounded-xl border ${isDark ? "bg-rose-950/30 border-rose-800/60" : "bg-rose-50 border-rose-200"}`}>
          <button
            type="button"
            onClick={() => setAlertsCollapsed((v) => !v)}
            className={`w-full flex items-center justify-between px-4 py-2.5 text-sm font-semibold ${isDark ? "text-rose-200" : "text-rose-800"}`}
          >
            <span className="inline-flex items-center gap-2">
              <AlertTriangle size={16} className={isDark ? "text-rose-400" : "text-rose-600"} />
              {alerts.length} active alert{alerts.length === 1 ? "" : "s"}
              <span className={`text-xs font-normal ${isDark ? "text-rose-300/70" : "text-rose-700/70"}`}>
                ({alerts.filter((a) => a.severity === 3).length} unhealthy ·{" "}
                {alerts.filter((a) => a.severity === 2).length} degraded ·{" "}
                {alerts.filter((a) => a.severity === 1).length} errors-only)
              </span>
            </span>
            {alertsCollapsed ? <ChevronDown size={16} /> : <ChevronUp size={16} />}
          </button>
          {!alertsCollapsed && (
            <ul className={`divide-y ${isDark ? "divide-rose-800/40" : "divide-rose-200"}`}>
              {alerts.slice(0, showAllAlerts ? alerts.length : ALERT_PREVIEW).map((a) => {
                const sevColor =
                  a.severity === 3
                    ? (isDark ? "bg-rose-500/30 text-rose-200 border-rose-500/50" : "bg-rose-200 text-rose-800 border-rose-400")
                    : a.severity === 2
                    ? (isDark ? "bg-amber-500/30 text-amber-200 border-amber-500/50" : "bg-amber-200 text-amber-800 border-amber-400")
                    : (isDark ? "bg-slate-700/50 text-slate-200 border-slate-500/40" : "bg-slate-200 text-slate-700 border-slate-300");
                return (
                  <li key={a.key}>
                    <button
                      type="button"
                      onClick={() => handleAlertClick(a.pipelineName)}
                      className={`w-full text-left px-4 py-2 flex items-center gap-3 hover:${isDark ? "bg-rose-900/30" : "bg-rose-100"} transition-colors`}
                    >
                      <span className={`inline-flex items-center gap-1 rounded border px-1.5 py-0.5 text-[10px] font-bold uppercase ${sevColor}`}>
                        <StateIcon state={a.state} size={10} />
                        {a.state.replace("_", " ")}
                      </span>
                      <span className={`text-xs font-mono ${isDark ? "text-slate-300" : "text-gray-700"}`}>
                        {a.kind.toUpperCase()}·{a.componentId}
                      </span>
                      <span className={`text-xs ${isDark ? "text-slate-400" : "text-gray-500"}`}>
                        @ {a.pipelineName} / {a.workerId}
                      </span>
                      <span className={`flex-1 truncate text-xs ${isDark ? "text-slate-400" : "text-gray-600"}`}>
                        {a.lastError || a.reason || (a.errorsTotal > 0 ? `${a.errorsTotal} error(s) recorded` : "—")}
                      </span>
                      {a.errorsTotal > 0 && (
                        <span className="text-[10px] rounded bg-rose-600/40 px-1.5 py-0.5 text-rose-100 font-mono">
                          {a.errorsTotal}e
                        </span>
                      )}
                      {a.timeoutsTotal > 0 && (
                        <span className="text-[10px] rounded bg-amber-600/40 px-1.5 py-0.5 text-amber-100 font-mono">
                          {a.timeoutsTotal}t
                        </span>
                      )}
                      <span className={`text-[10px] tabular-nums ${isDark ? "text-slate-500" : "text-gray-400"}`}>
                        {fmtAge(a.since)}
                      </span>
                    </button>
                  </li>
                );
              })}
              {alerts.length > ALERT_PREVIEW && (
                <li>
                  <button
                    type="button"
                    onClick={() => setShowAllAlerts((v) => !v)}
                    className={`w-full text-left px-4 py-2 text-xs font-medium ${isDark ? "text-rose-300/80 hover:text-rose-200" : "text-rose-700/80 hover:text-rose-800"}`}
                  >
                    {showAllAlerts ? "Show fewer" : `Show all ${alerts.length} alerts`}
                  </button>
                </li>
              )}
            </ul>
          )}
        </div>
      )}

      {groups.length === 0 && !error && (
        <div className={`rounded-xl border p-8 text-center text-sm ${isDark ? "bg-slate-800/30 border-slate-700 text-slate-500" : "bg-white border-gray-200 text-gray-400"}`}>
          No workers registered.
        </div>
      )}

      {groups.length > 3 && (
        <div className="flex items-center gap-2">
          <div className="relative flex-1 max-w-xs">
            <Search size={14} className={`absolute left-2.5 top-1/2 -translate-y-1/2 ${isDark ? "text-slate-500" : "text-gray-400"}`} />
            <input
              type="text"
              value={pipelineFilter}
              onChange={(e) => setPipelineFilter(e.target.value)}
              placeholder="Filter pipelines…"
              className={`w-full rounded-lg border pl-8 pr-3 py-1.5 text-sm outline-none ${isDark ? "bg-slate-800/60 border-slate-700 text-slate-200 placeholder-slate-500" : "bg-white border-gray-200 text-gray-800 placeholder-gray-400"}`}
            />
          </div>
          {pipelineFilter.trim() && (
            <span className={`text-xs ${isDark ? "text-slate-500" : "text-gray-400"}`}>
              {visibleGroups.length} of {groups.length}
            </span>
          )}
        </div>
      )}

      {groups.length > 0 && visibleGroups.length === 0 && (
        <div className={`rounded-xl border p-6 text-center text-sm ${isDark ? "bg-slate-800/30 border-slate-700 text-slate-500" : "bg-white border-gray-200 text-gray-400"}`}>
          No pipelines match “{pipelineFilter.trim()}”.
        </div>
      )}

      <div className="grid gap-4 grid-cols-1 lg:grid-cols-2">
        {visibleGroups.map((g) => (
          <PipelineCard
            key={g.pipeline_name}
            group={g}
            highlight={focusedPipeline === g.pipeline_name}
            cardRef={(el) => {
              if (el) cardRefs.current.set(g.pipeline_name, el);
              else cardRefs.current.delete(g.pipeline_name);
            }}
          />
        ))}
      </div>
    </section>
  );
}
