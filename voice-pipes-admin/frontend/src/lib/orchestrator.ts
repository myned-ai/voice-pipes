/* Orchestrator wire types shared by the live-graph and sessions pages.
 *
 * Every shape here mirrors a `#[derive(Serialize)]` struct in
 * `voice-pipes/src/api/types.rs` (the BFF proxies these endpoints verbatim —
 * see `voice-pipes-admin/backend/app/routers/{health,sessions}.py`). Keep the
 * field names snake_case to match serde output. */

/** `ApiResponse<T>`: every orchestrator endpoint wraps its payload in this. */
export interface ApiEnvelope<T> {
  data: T | null;
  error?: { code: string; message: string } | null;
}

/** `PaginatedResponse<T>` (sessions / audit listings). */
export interface PaginatedResponse<T> {
  total: number;
  page: number;
  per_page: number;
  items: T[];
}

// ── Parameter schema (mirror of Rust api::param_schema) ─────────────────────

export interface OnlyWhen {
  field: string;
  values: string[];
}

export interface ParamDescriptor {
  key: string;
  label: string;
  description: string;
  only_when?: OnlyWhen;
  mutable: boolean;
  value_type: 'float' | 'int' | 'bool' | 'str' | 'path' | 'enum';
  constraints?: { min?: number; max?: number; options?: string[] };
}

// ── GET /api/v1/pipeline (`PipelineGraphResponse`) ──────────────────────────

/** `PipelineNode` — `metadata` is a free-form `serde_json::Value` object. */
export interface PipelineNode {
  id: string;
  type: string;
  label: string;
  provider?: string;
  input_sample_rate?: number;
  output_sample_rate?: number;
  algorithm?: string;
  metadata: Record<string, unknown> | null;
  param_schema?: ParamDescriptor[];
}

/** `PipelineEdge`. */
export interface PipelineEdge {
  from: string;
  to: string;
  data_type: string;
  sample_rate?: number | null;
}

/** `PipelineGraphResponse` (the `data` of `GET /api/v1/pipeline`). */
export interface PipelineGraph {
  name: string;
  nodes: PipelineNode[];
  edges: PipelineEdge[];
  /** Absent from the BFF's offline mock pipeline. */
  audit_tap?: { enabled: boolean; side?: string | null; tapped_nodes: string[] };
  raw_toml?: string | null;
}

// ── GET /api/v1/pipelines (`PipelineGroupsResponse`) ────────────────────────

/** `PipelineGroupWorker`. */
export interface PipelineGroupWorker {
  id: string;
  status: 'active' | 'draining' | 'dead';
  active_sessions: number;
  max_concurrent: number;
}

/** `PipelineGroup` — workers sharing one pipeline config hash. */
export interface PipelineGroup {
  pipeline_name: string | null;
  pipeline_config_hash: string;
  workers: PipelineGroupWorker[];
  total_capacity: number;
  total_active: number;
  sessions_processed: number;
}

// ── GET /api/v1/sessions (`PaginatedResponse<SessionRecord>`) ───────────────

/** `SessionRecord` — one row of the sessions list. */
export interface SessionListItem {
  id: string;
  worker_id: string;
  started_at: string;
  ended_at: string | null;
  status: 'in_progress' | 'completed' | 'error' | string;
  pipeline_name: string | null;
  pipeline_config_hash: string | null;
  stt_provider: string | null;
  tts_provider: string | null;
  input_sample_rate: number | null;
  output_sample_rate: number | null;
  tags: string[];
  turn_count: number;
}
