import type { Node, Edge } from 'reactflow';
import type { FleetWorker, HealthState } from '../../lib/fleet';
import type { ParamDescriptor } from '../../lib/orchestrator';

// ── Wire types (see lib/orchestrator.ts for the shared pipeline-graph shapes) ──

/** `GET /api/v1/health` → `OrchestratorHealthResponse` (voice-pipes/src/api/types.rs).
 *  `status` is `healthy` | `degraded` from the orchestrator; this page
 *  synthesises `{ status: 'offline' }` when the BFF can't reach it. */
export interface HealthSnapshot {
  status: string;
  uptime_s?: number;
  version?: string;
  workers?: { total: number; active: number; draining: number; dead: number };
  sessions?: { total_processed: number; active: number; errors_total: number };
}

/** `HistogramSummary` — every field is a u64 on the wire. */
export interface HistogramSummary {
  p50?: number;
  p90?: number;
  p99?: number;
  mean?: number;
  min?: number;
  max?: number;
}

/** `GET /api/v1/metrics` → `MetricsResponse`; only the fields this page reads.
 *  All optional because the BFF returns `data: null` when the orchestrator is
 *  unreachable and older orchestrators may omit a component block. */
export interface LiveMetrics {
  workers_active?: number;
  pipeline?: { requests_total?: number; requests_active?: number; errors_total?: number; latency_ms?: HistogramSummary };
  stt?: { latency_ms?: HistogramSummary };
  /** LLM / text-processor TTFT. */
  text?: { latency_ms?: HistogramSummary };
  tts?: { latency_ms?: HistogramSummary };
}

/** Rows of the "Pipeline Metrics" overlay; keys render as labels. */
export interface OverallMetrics {
  active_workers: number | undefined;
  active_requests: number | undefined;
  total_requests: number;
  p50_latency: string;
  p90_latency: string;
  errors: number | undefined;
}

/** `PATCH /api/v1/workers/{id}/params` → worker `PatchParamsResp`
 *  (voice-pipes/src/api/worker_debug.rs); `value` is what the worker stored. */
export interface PatchParamsResp {
  applied: boolean;
  component: string;
  key: string;
  value: unknown;
}

/** One `ComponentSnapshot` as carried on a worker (health only — the panel
 *  renders a badge per component). */
export interface LiveWorkerComponent {
  id: string;
  kind: string;
  health?: { state?: HealthState; reason?: string };
}

/** One row of the fleet SSE `workers` array: the orchestrator's
 *  `WorkerListItem` (`GET /api/v1/workers`), plus the optional detail-only
 *  fields (`WorkerDetailResponse`) the panel shows when present. Extends
 *  `FleetWorker` so the fleet-overview grouping needs no cast. */
export interface LiveWorker extends FleetWorker {
  address?: string;
  registered_at?: string;
  last_heartbeat?: string;
  capacity?: { max_concurrent?: number; active_sessions?: number };
  pipeline_config_hash?: string;
  version?: string;
  tags?: string[];
  transport_address?: string | null;
  components?: LiveWorkerComponent[];
  /** Flat legacy fallbacks the panel still reads when `capacity` is absent. */
  active_sessions?: number;
  max_concurrent?: number;
}

/** React Flow `data` for one pipeline node (built in `fetchPipeline`,
 *  decorated with the live worker + PATCH callback by the layout memo). */
export interface PipelineNodeData {
  label: string;
  componentType: string;
  provider?: string;
  input_sample_rate: number | null;
  output_sample_rate: number | null;
  metadata: Record<string, unknown>;
  param_schema: ParamDescriptor[];
  nodeId: string;
  showMetrics: boolean;
  metrics: Record<string, string>;
  workerId?: string;
  onParamUpdate?: (nodeId: string, key: string, value: unknown) => Promise<string | null>;
}

export type PipelineFlowNode = Node<PipelineNodeData>;

/** Node + edge layout derived from the raw graph for the current view. */
export interface GraphLayout {
  nodes: PipelineFlowNode[];
  edges: Edge[];
}
