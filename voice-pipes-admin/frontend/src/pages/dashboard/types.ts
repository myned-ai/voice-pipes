/* ── types ──────────────────────────────────────────────── */

export interface Utterance {
  session_id: string;
  utterance_id: number;
  timestamp: string;
  total_ms: number | null;
  ttfb_ms: number | null;
  ttft_ms: number | null;
  endpointing_ms: number | null;
  /** S2S provider endpointing lag
   *  (`provider_endpointing_ms`). Optional — cascade turns and older rollups
   *  lack it. */
  provider_endpointing_ms?: number | null;
  stt_provider: string;
  tts_provider: string;
  pipeline_name: string;
  tokens_per_sec: number | null;
  first_token_ms: number | null;
  prompt_tokens: number | null;
  output_tokens: number | null;
  generate_ms: number | null;
  model: string | null;
  stt_audio_duration_s: number | null;
  stt_session_duration_s: number | null;
  stt_speech_model: string | null;
  tts_chars: number | null;
  tts_audio_duration_s: number | null;
  tts_voice: string | null;
  /** Per-(provider, voice) breakdown — same value on every row of the
   *  session.  Iterating this is the correct way to attribute usage when a
   *  session switches providers/voices mid-flight. */
  tts_usage_by_voice?: Array<{ provider: string; voice: string; chars: number; audio_s: number }> | null;
  stt_usage_by_model?: Array<{ provider: string; model: string; audio_s: number; session_s: number }> | null;
  client_ttfa_ms: number | null;
  /** Client jitter buffer + scheduling: `audio_receive` → `playback_start`
   *  timing reports (ms). Optional — older rollups predate the field. */
  client_receive_to_play_ms?: number | null;
  /** Output throughput: the server's
   *  streaming-phase realtime factor (`output.emit_rtf`, > 1.0 = emitted
   *  slower than realtime), the provider-only factor, the post-processor
   *  chain's wall overhead and the client-measured arrival factor. Optional —
   *  older rollups and captures without an `output` component lack them. */
  emit_rtf?: number | null;
  source_rtf?: number | null;
  chain_overhead_ms?: number | null;
  arrival_rtf?: number | null;
  audio_underruns: number | null;
  barge_in_rtt_ms: number | null;
  had_error?: boolean;
  error_components?: string[];
  /** USD cost of this LLM call (null = model not in price book). */
  llm_cost_usd?: number | null;
}

export interface FailedSession {
  session_id: string;
  timestamp: string;
  pipeline_name: string;
  stt_provider: string;
  tts_provider: string;
  errors_by_component: Record<string, number>;
  errors_total: number;
  turn_count: number;
  failed_turn_count: number;
  last_error: string | null;
  reason: 'no_turns' | 'errors_recorded' | string;
}

export interface ProviderChange {
  timestamp: string;
  field: string;
  from_value: string;
  to_value: string;
  session_id: string;
}

export interface KPI {
  total_sessions: number;
  total_utterances: number;
  avg_latency_ms: number | null;
  p95_latency_ms: number | null;
  avg_tokens_per_sec: number | null;
  failed_sessions?: number;
  failed_turns?: number;
  error_rate_pct?: number | null;
  errors_by_component?: Record<string, number>;
  // Orchestrator overlay (rolling-window in-memory metrics).
  orchestrator_errors_total?: number;
  orchestrator_requests_total?: number;
  workers_active?: number;
  combined_failed_sessions?: number;
  combined_total_sessions?: number;
  combined_error_rate_pct?: number | null;
  // Window-scoped USD spend (P2 #10/#11).  ``spend_by_provider`` is keyed by
  // the vendor root ("openai", "elevenlabs"), each bucket carries llm/stt/tts
  // subtotals, the optional monthly cap and current cap utilisation.
  spend_total_usd?: number;
  spend_by_provider?: Record<string, ProviderSpend>;
  // P3 #17 — Anomaly counts across the window.
  anomalies_total?: number;
  anomalies_by_kind?: Record<string, number>;
  // Window rollup of `output.emit_rtf`:
  // turns emitted slower than realtime, mean and max factor. Optional until
  // the analytics API ships them; the card falls back to per-utterance data.
  turns_over_realtime?: number;
  avg_emit_rtf?: number | null;
  max_emit_rtf?: number | null;
  // Window rollup of the S2S provider
  // endpointing lag. Optional until the analytics API ships it.
  avg_provider_endpointing_ms?: number | null;
  // Worker resource efficiency (CPU & RAM)
  avg_peak_rss_bytes?: number | null;
  p95_peak_rss_bytes?: number | null;
  max_container_peak_bytes?: number | null;
  avg_turn_cpu_ms?: number | null;
  // Business outcomes / CSAT (present only when captures carry tags).
  contained?: number;
  escalated?: number;
  abandoned?: number;
  outcome_known?: number;
  containment_rate_pct?: number | null;
  escalation_rate_pct?: number | null;
  avg_csat?: number | null;
  csat_samples?: number;
}

export interface ProviderSpend {
  llm: number;
  stt: number;
  tts: number;
  total: number;
  monthly_cap_usd: number | null;
  cap_pct: number | null;
}

export interface OrchestratorMetrics {
  window?: string;
  timestamp?: string;
  pipeline?: { requests_total?: number; requests_active?: number; errors_total?: number };
  workers_active?: number;
}

export interface AnalyticsData {
  kpi: KPI;
  utterances: Utterance[];
  provider_changes: ProviderChange[];
  failed_sessions?: FailedSession[];
  /** P3 #17 — top-N sessions by anomaly count for the bottom-of-page feed. */
  anomaly_sessions?: AnomalySession[];
  /** 1b — every pipeline present in the window, independent of any active
   * pipeline filter, so the picker stays fully populated when filtering
   * server-side. */
  available_pipelines?: string[];
  orchestrator_metrics?: OrchestratorMetrics | null;
}

export interface AnomalySession {
  session_id: string;
  pipeline_name: string;
  timestamp: string;
  anomalies_total: number;
  kind_counts: Record<string, number>;
}

export interface IssueRow {
  id: number;
  kind: string;
  severity: string;
  ts: number;
  session_id?: string | null;
  pipeline_name?: string | null;
  summary?: string | null;
  payload?: Record<string, unknown> | null;
  created_at: number;
}

/** One row of the per-pipeline comparison matrix. */
export interface PipelineRow {
  name: string;
  sessions: number;
  turns: number;
  p50: number | null;
  p95: number | null;
  avgTtft: number | null;
  avgEp: number | null;
  avgTps: number | null;
  failedSessions: number;
  errorRatePct: number | null;
}

/** Activity-feed tab. */
export type ActivityTab = 'anomalies' | 'issues';

/** Bucket size and aggregation picked for the bucketed latency chart. */
export type Bucket = 'auto' | '1m' | '5m' | '1h' | '1d';
export type Agg = 'avg' | 'p50' | 'p95' | 'p99' | 'max';
