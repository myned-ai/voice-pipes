/* Fleet grouping for the Live Graph overview (P15).
 *
 * The Live Graph shows one worker's topology at a time; this groups the raw
 * /api/v1/workers fleet by pipeline with a fraction-aware health status so an
 * operator can see every pipeline at once and spot which is degraded before
 * drilling into a single worker.  Pure + unit-tested. */

export type HealthState = 'healthy' | 'degraded' | 'unhealthy' | 'not_configured';

export interface FleetWorker {
  id: string;
  status: 'active' | 'draining' | 'dead';
  pipeline_name?: string | null;
  aggregate_health?: HealthState;
}

export interface FleetPipeline {
  name: string;
  workers: FleetWorker[];
  /** Fraction-aware: all-healthy ⇒ healthy, all-down ⇒ unhealthy, mixed ⇒
   *  degraded — so one bad worker of N doesn't read as a whole-pipeline outage. */
  status: HealthState;
  healthy: number;
  total: number;
  dead: number;
}

/** One worker's health: dead ⇒ unhealthy; otherwise the reported aggregate, or
 *  healthy when active with no health info (matches the existing fleet panel). */
export function fleetWorkerHealth(w: FleetWorker): HealthState {
  if (w.status === 'dead') return 'unhealthy';
  return w.aggregate_health ?? 'healthy';
}

const STATUS_RANK: Record<HealthState, number> = {
  unhealthy: 0, degraded: 1, not_configured: 2, healthy: 3,
};

/* ── fleet capacity / concurrency (L0 NOC gauge) ──────────────── */

export interface CapacityWorker {
  status?: string;
  capacity?: { max_concurrent?: number; active_sessions?: number };
}

export interface FleetCapacity {
  workers: number;
  activeWorkers: number;
  deadWorkers: number;
  /** Live concurrent sessions across the (non-dead) fleet. */
  activeSessions: number;
  /** Total serving capacity of the non-dead fleet (dead workers can't serve). */
  maxConcurrent: number;
  /** activeSessions / maxConcurrent as a %, or null when there is no capacity. */
  utilizationPct: number | null;
}

export function fleetCapacity(workers: CapacityWorker[]): FleetCapacity {
  let activeSessions = 0;
  let maxConcurrent = 0;
  let activeWorkers = 0;
  let deadWorkers = 0;
  for (const w of workers) {
    if (w.status === "active") activeWorkers++;
    if (w.status === "dead") { deadWorkers++; continue; }
    maxConcurrent += w.capacity?.max_concurrent ?? 0;
    activeSessions += w.capacity?.active_sessions ?? 0;
  }
  const utilizationPct = maxConcurrent > 0
    ? Math.round((activeSessions / maxConcurrent) * 1000) / 10
    : null;
  return { workers: workers.length, activeWorkers, deadWorkers, activeSessions, maxConcurrent, utilizationPct };
}

export function groupFleetByPipeline(workers: FleetWorker[]): FleetPipeline[] {
  const map = new Map<string, FleetWorker[]>();
  for (const w of workers) {
    const name = w.pipeline_name || 'unknown';
    const arr = map.get(name);
    if (arr) arr.push(w);
    else map.set(name, [w]);
  }

  const groups: FleetPipeline[] = [];
  for (const [name, ws] of map) {
    const states = ws.map(fleetWorkerHealth);
    const total = states.length;
    const healthy = states.filter((s) => s === 'healthy').length;
    const unhealthy = states.filter((s) => s === 'unhealthy').length;
    const dead = ws.filter((w) => w.status === 'dead').length;
    let status: HealthState;
    if (total === 0) status = 'not_configured';
    else if (unhealthy === total) status = 'unhealthy';
    else if (healthy === total) status = 'healthy';
    else status = 'degraded';
    groups.push({ name, workers: ws, status, healthy, total, dead });
  }

  // Worst pipelines first so problems surface at the left of the overview.
  groups.sort((a, b) => STATUS_RANK[a.status] - STATUS_RANK[b.status] || a.name.localeCompare(b.name));
  return groups;
}
