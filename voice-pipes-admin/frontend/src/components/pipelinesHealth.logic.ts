// pipelinesHealth.logic — wire types + pure health aggregation for the
// PipelinesHealth section.  Kept out of the component module so Fast Refresh
// sees a components-only file there, and so the aggregation is unit-testable
// without rendering (see PipelinesHealth.test.ts).

/* ── wire types (mirror voice-pipes/src/api/types.rs) ───────── */

export type HealthState = "healthy" | "degraded" | "unhealthy" | "not_configured";
export type ComponentKind =
  | "stt" | "tts" | "text" | "vad" | "audio" | "transport" | "other";

/** SystemTime serializes as { secs_since_epoch, nanos_since_epoch } in serde. */
export interface SystemTime {
  secs_since_epoch: number;
  nanos_since_epoch: number;
}

export interface HealthStatus {
  state: HealthState;
  reason: string;
  since: SystemTime;
}

export interface ComponentTelemetry {
  processed_total: number;
  errors_total: number;
  timeouts_total: number;
  last_error?: string;
  extras?: Record<string, unknown>;
}

export interface ComponentSnapshot {
  id: string;
  kind: ComponentKind;
  health: HealthStatus;
  telemetry: ComponentTelemetry;
}

export interface WorkerComponentsRollup {
  worker_id: string;
  aggregate_health?: HealthState;
  components: ComponentSnapshot[];
}

export interface WorkerListItem {
  id: string;
  status: "active" | "draining" | "dead";
  pipeline_name?: string | null;
  pipeline_config_hash?: string;
  last_heartbeat?: string;
  capacity?: { max_concurrent?: number; active_sessions?: number };
}

/* ── health aggregation ─────────────────────────────────── */

/** Worst-of aggregation matching backend `HealthState::worst`. */
function worstOf(states: HealthState[]): HealthState {
  const rank: Record<HealthState, number> = {
    not_configured: 0, healthy: 1, degraded: 2, unhealthy: 3,
  };
  let acc: HealthState = "not_configured";
  for (const s of states) if (rank[s] > rank[acc]) acc = s;
  return acc;
}

export type PipelineWorker = { worker: WorkerListItem; rollup?: WorkerComponentsRollup };

/** One worker's overall health: a dead worker is unhealthy regardless of its
 *  last-known components; otherwise prefer the server's aggregate, falling back
 *  to worst-of its component states. */
export function workerHealthState(w: PipelineWorker): HealthState {
  if (w.worker.status === "dead") return "unhealthy";
  if (w.rollup?.aggregate_health) return w.rollup.aggregate_health;
  const comps = w.rollup?.components ?? [];
  if (comps.length === 0) return "not_configured";
  return worstOf(comps.map((c) => c.health.state));
}

export interface PipelineHealthSummary {
  /** Fraction-aware status: all-healthy ⇒ healthy, all-down ⇒ unhealthy,
   *  anything in between ⇒ degraded.  This is the fix for worst-of making
   *  "1 bad worker of 50" look like a total pipeline outage (P14). */
  status: HealthState;
  total: number;
  healthy: number;
  degraded: number;
  unhealthy: number;
  notConfigured: number;
}

export function pipelineHealthSummary(workers: PipelineWorker[]): PipelineHealthSummary {
  const states = workers.map(workerHealthState);
  const total = states.length;
  const healthy = states.filter((s) => s === "healthy").length;
  const degraded = states.filter((s) => s === "degraded").length;
  const unhealthy = states.filter((s) => s === "unhealthy").length;
  const notConfigured = states.filter((s) => s === "not_configured").length;
  let status: HealthState;
  if (healthy + degraded + unhealthy === 0) status = "not_configured";
  else if (unhealthy === total) status = "unhealthy";
  else if (healthy === total) status = "healthy";
  else status = "degraded";
  return { status, total, healthy, degraded, unhealthy, notConfigured };
}
