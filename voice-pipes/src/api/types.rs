//! Shared types for the HTTP API layer.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use voice_pipes_sdk::TenantId;

use crate::audit::store::{AuditStore, SessionStore};
use crate::config::PipelineConfig;

// ── Response envelope ───────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct ApiResponse<T: Serialize> {
    pub data: Option<T>,
    pub error: Option<ApiError>,
}

#[derive(Debug, Serialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

impl<T: Serialize> ApiResponse<T> {
    /// Wrap `data` in a successful API response.
    pub fn ok(data: T) -> Self {
        Self {
            data: Some(data),
            error: None,
        }
    }
}

impl ApiResponse<()> {
    /// Build a 404 Not Found error response.
    pub fn not_found(msg: impl Into<String>) -> ApiResponse<()> {
        ApiResponse {
            data: None,
            error: Some(ApiError {
                code: "NOT_FOUND".to_string(),
                message: msg.into(),
            }),
        }
    }

    /// Build a 400 Bad Request error response.
    pub fn bad_request(msg: impl Into<String>) -> ApiResponse<()> {
        ApiResponse {
            data: None,
            error: Some(ApiError {
                code: "BAD_REQUEST".to_string(),
                message: msg.into(),
            }),
        }
    }

    /// Build an error response with an arbitrary code (for statuses
    /// without a dedicated constructor, e.g. 409 CONFLICT).
    pub fn error(code: impl Into<String>, msg: impl Into<String>) -> ApiResponse<()> {
        ApiResponse {
            data: None,
            error: Some(ApiError {
                code: code.into(),
                message: msg.into(),
            }),
        }
    }

    /// Build a 500 Internal Server Error response.
    pub fn internal_error(msg: impl Into<String>) -> ApiResponse<()> {
        ApiResponse {
            data: None,
            error: Some(ApiError {
                code: "INTERNAL_ERROR".to_string(),
                message: msg.into(),
            }),
        }
    }
}

// ── Worker status ───────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerStatus {
    Active,
    Draining,
    Dead,
}

// ── Worker record (orchestrator-side) ───────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct WorkerRecord {
    pub id: String,
    pub address: String,
    pub registered_at: DateTime<Utc>,
    pub last_heartbeat: DateTime<Utc>,
    pub status: WorkerStatus,
    pub capacity: WorkerCapacity,
    pub pipeline_config_hash: String,
    pub pipeline_name: Option<String>,
    pub version: String,
    pub sessions_processed: u64,
    pub errors_total: u64,
    #[serde(default)]
    pub tags: Vec<String>,
    pub transport_address: Option<String>,
    /// Browser-reachable transport (see `WorkerRegistration`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_transport_address: Option<String>,
    #[serde(skip_serializing)]
    pub pipeline_config: Option<serde_json::Value>,
    /// Pipeline-config TOML files the worker advertised at registration.
    /// Used by the orchestrator's `/api/v1/benchmark/configs` endpoint
    /// to surface only configs the worker can actually load.
    #[serde(default)]
    pub available_configs: Vec<crate::config::discover::ConfigEntry>,
    /// Most recent per-component snapshot reported by the worker via
    /// heartbeat. Empty until the first heartbeat carrying components arrives.
    #[serde(default)]
    pub components: Vec<ComponentSnapshot>,
    /// Aggregate health derived from `components` (worst-of). `None` until
    /// the first heartbeat carrying components arrives.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregate_health: Option<crate::component::HealthState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerCapacity {
    pub max_concurrent: u32,
    pub active_sessions: u32,
}

// ── Session record ──────────────────────────────────────────

/// A client connection lifecycle: connect -> 1...N turns -> disconnect.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRecord {
    /// UUID v4, generated at WebSocket/gRPC connect time.
    pub id: String,
    /// Tenant owning this session, if running multi-tenant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<TenantId>,
    /// Worker that owns this session.
    pub worker_id: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub status: SessionStatus,
    pub pipeline_name: Option<String>,
    pub pipeline_config_hash: Option<String>,
    pub stt_provider: Option<String>,
    pub tts_provider: Option<String>,
    pub input_sample_rate: Option<u32>,
    pub output_sample_rate: Option<u32>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Number of turns completed in this session.
    pub turn_count: u32,
}

// ── Turn record (one utterance pass through the pipeline) ───

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnRecord {
    pub id: String,
    /// FK to `SessionRecord.id`.
    #[serde(default)]
    pub session_id: String,
    pub worker_id: String,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub status: SessionStatus,
    /// End-to-end turn duration (ms). Canonical name, matches
    /// `TurnMetricsSnapshot.total_ms`.
    pub total_ms: Option<u64>,
    pub stt_provider: Option<String>,
    pub tts_provider: Option<String>,
    pub text_preview: Option<String>,
    pub otel_trace_id: Option<String>,
    pub error: Option<String>,
    pub detail: Option<SessionDetail>,
    pub pipeline_name: Option<String>,
    pub pipeline_config_hash: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub events: Vec<crate::audit::TraceEvent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    InProgress,
    Completed,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionDetail {
    pub pipeline_name: Option<String>,
    pub input: Option<SessionInput>,
    pub stt: Option<SessionStt>,
    pub tts: Option<SessionTts>,
    /// Latency from end-of-speech to first LLM token (TTFT), if applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_latency_ms: Option<u64>,
    pub realtime_factor: Option<f64>,
    /// Full per-component latency breakdown (mirrors the wire
    /// `ServerControl::TurnMetrics` shape).  Persisted so that historical
    /// per-component performance trends can be reconstructed from the
    /// audit log without going through OTEL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_breakdown: Option<crate::pipeline::TurnMetricsSnapshot>,
    /// Client-reported playback timing events captured during this turn,
    /// in arrival order.  Populated from `ClientControl::TimingReport`
    /// frames received between turn start and turn end.  Empty when the
    /// client did not send any timing reports (e.g. text-only sessions
    /// or older clients).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub client_timings: Vec<ClientTimingEntry>,
    /// Client-reported playback health for this turn (underruns / inserted
    /// silence), from the `playback_quality` control frame. `None` for
    /// older clients or turns whose playback never completed. Rides the
    /// sqlite `data` JSON blob — no schema migration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playback_health: Option<PlaybackHealth>,
}

/// Per-turn client playback health.
///
/// Mirrors `ClientControl::PlaybackQuality` minus the utterance id (implied
/// by the owning turn record).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackHealth {
    /// Number of playback breaks.
    pub underrun_count: u32,
    /// Total inserted silence, ms.
    pub total_silence_ms: u32,
    /// Ideal audio delivered, ms.
    pub audio_ms: u32,
    /// Audio actually heard (enqueue-bounded), ms.
    pub heard_ms: u32,
    /// Client wall-clock playback span, ms.
    pub wall_ms: u32,
    /// Underruns in ideal-audio coordinates.
    pub gaps: Vec<crate::transport::PlaybackGap>,
    /// First → last audio frame *received* on the client, ms
    /// (`arrival_rtf = arrival_wall_ms / audio_ms`; absent on old captures).
    #[serde(default)]
    pub arrival_wall_ms: u32,
    /// Sent from the flush path (barge-in / cancel) rather than at natural
    /// playback completion.
    #[serde(default)]
    pub interrupted: bool,
    /// Annotations that arrived after their audio had started playing.
    #[serde(default)]
    pub annotation_late_count: u32,
    /// Max lateness of those annotations, ms.
    #[serde(default)]
    pub annotation_late_ms_max: u32,
}

/// One client-side timing event captured for a turn.
///
/// Mirrors the payload of `ClientControl::TimingReport`.  Persisted on
/// `SessionDetail.client_timings` so post-hoc analysis can reconstruct
/// the full client-perceived playback timeline (`audio_play_started`,
/// `audio_play_finished`, `first_caption_shown`, ...) alongside the
/// server-measured per-turn metrics snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClientTimingEntry {
    /// Client-reported event name (e.g. `"audio_play_started"`).
    pub event: String,
    /// Client wall-clock timestamp (epoch ms) when the event occurred.
    pub client_timestamp_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInput {
    pub source: String,
    pub path: Option<String>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u16>,
    pub duration_s: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStt {
    pub provider: String,
    pub input_sample_rate: u32,
    pub resampled_from: Option<u32>,
    /// `None` when the pipeline could not measure STT latency for this turn
    /// (e.g. the turn was cancelled before STT produced a final transcript).
    /// Avoid emitting `0` placeholders so percentiles stay accurate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionTts {
    pub provider: String,
    /// TTS time-to-first-byte. `None` if the pipeline aborted before TTS
    /// emitted any audio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    pub native_sample_rate: u32,
    pub output_sample_rate: u32,
    pub resampled: bool,
    pub audio_duration_s: f64,
}

// ── Internal API types (worker -> orchestrator) ──────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkerRegistration {
    pub worker_id: String,
    pub address: String,
    pub capacity: WorkerCapacity,
    pub pipeline_config_hash: String,
    pub pipeline_name: Option<String>,
    pub version: String,
    #[serde(default)]
    pub tags: Vec<String>,
    /// Client transport as reachable from the *orchestrator* (bind:port;
    /// the orchestrator swaps a wildcard host for the worker's service
    /// name). Used by orchestrator-side consumers such as the benchmark
    /// runner.
    pub transport_address: Option<String>,
    /// Client transport as reachable from *browsers*, when it differs
    /// (containerised workers publishing distinct host ports). Handed out
    /// by `GET /api/v1/dispatch`; falls back to `transport_address`.
    #[serde(default)]
    pub public_transport_address: Option<String>,
    pub pipeline_config: Option<serde_json::Value>,
    /// Pipeline-config TOML files discovered under the worker's CWD
    /// `examples/` directory at startup. The orchestrator aggregates
    /// these across workers to populate the benchmark variant-config
    /// dropdown so the UI only offers configs the worker can actually
    /// load. Older workers omit this field.
    #[serde(default)]
    pub available_configs: Vec<crate::config::discover::ConfigEntry>,
}

/// A worker leaving the fleet on purpose (a graceful shutdown).
#[derive(Debug, Serialize, Deserialize)]
pub struct WorkerDeregistration {
    pub worker_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkerHeartbeat {
    pub worker_id: String,
    pub capacity: WorkerCapacity,
    /// Optional snapshot of the worker's component registry. Populated since
    /// the Component-trait rollout so the orchestrator can surface per-stage
    /// health/telemetry to the admin UI without scraping `/debug/components`
    /// on each worker. Older workers omit this field.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<ComponentSnapshot>,
    /// Aggregate health rolled up from `components` (worst-of). `None` when
    /// the worker did not include component data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregate_health: Option<crate::component::HealthState>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SessionCompleteEvent {
    pub worker_id: String,
    pub session: TurnRecord,
}

// ── Health response types ───────────────────────────────────

#[derive(Debug, Serialize)]
pub struct OrchestratorHealthResponse {
    pub status: String,
    pub uptime_s: u64,
    pub version: String,
    pub workers: WorkerFleetSummary,
    pub sessions: SessionSummary,
}

#[derive(Debug, Serialize)]
pub struct WorkerFleetSummary {
    pub total: usize,
    pub active: usize,
    pub draining: usize,
    pub dead: usize,
}

#[derive(Debug, Serialize)]
pub struct SessionSummary {
    pub total_processed: u64,
    pub active: u64,
    pub errors_total: u64,
}

#[derive(Debug, Serialize)]
pub struct WorkerHealthResponse {
    pub status: String,
    pub worker_id: String,
    pub uptime_s: u64,
    pub components: WorkerComponents,
}

#[derive(Debug, Serialize)]
pub struct WorkerComponents {
    pub stt: ComponentStatus,
    pub tts: ComponentStatus,
    pub vad: ComponentStatus,
    pub audit: ComponentStatus,
    pub otel: ComponentStatus,
}

#[derive(Debug, Serialize)]
pub struct ComponentStatus {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub records_written: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub records_dropped: Option<u64>,
}

/// Snapshot of one entry in the worker's [`crate::component::ComponentRegistry`].
/// Returned by `GET /debug/components` and carried over the worker->orchestrator
/// heartbeat so the orchestrator can expose per-component health/telemetry to
/// the admin UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentSnapshot {
    pub id: String,
    pub kind: crate::component::ComponentKind,
    pub health: crate::component::HealthStatus,
    pub telemetry: crate::component::ComponentTelemetry,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub param_schema: Option<Vec<crate::api::param_schema::ParamDescriptor>>,
}

/// Response for `GET /debug/components`.
#[derive(Debug, Serialize)]
pub struct ComponentsResponse {
    pub aggregate_health: crate::component::HealthState,
    pub components: Vec<ComponentSnapshot>,
}

// ── Metrics response ────────────────────────────────────────

/// Per-worker component snapshots, shipped inside the rolled-up
/// [`MetricsResponse`] so a single dashboard call can render the full
/// fleet's component health without N follow-up requests.
#[derive(Debug, Clone, Serialize)]
pub struct WorkerComponentsRollup {
    pub worker_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aggregate_health: Option<crate::component::HealthState>,
    pub components: Vec<ComponentSnapshot>,
}

#[derive(Debug, Serialize)]
pub struct MetricsResponse {
    pub timestamp: DateTime<Utc>,
    pub window: String,
    pub workers_active: usize,
    pub pipeline: PipelineMetrics,
    pub stt: ComponentMetrics,
    /// LLM / text-processor latency (TTFT -- end-of-speech to first token).
    pub text: ComponentMetrics,
    pub tts: ComponentMetrics,
    pub audit: AuditMetrics,
    /// Per-worker component snapshots harvested from the most recent
    /// heartbeats. Empty when no worker has reported component data yet.
    /// This is the shape the admin UI consumes; the flat
    /// `stt`/`text`/`tts` fields above remain for clients that still
    /// read them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<WorkerComponentsRollup>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PipelineMetrics {
    pub requests_total: u64,
    pub requests_active: u64,
    pub errors_total: u64,
    pub latency_ms: HistogramSummary,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ComponentMetrics {
    pub latency_ms: HistogramSummary,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct AuditMetrics {
    pub records_total: u64,
    pub records_dropped: u64,
}

/// Rolling latency percentiles, the same type components report their
/// telemetry in.
pub use voice_pipes_sdk::component::LatencyStats as HistogramSummary;

// ── Workers list response ───────────────────────────────────

#[derive(Debug, Serialize)]
pub struct WorkersListResponse {
    pub total: usize,
    pub items: Vec<WorkerListItem>,
}

#[derive(Debug, Serialize)]
pub struct WorkerListItem {
    pub id: String,
    pub address: String,
    pub registered_at: DateTime<Utc>,
    pub last_heartbeat: DateTime<Utc>,
    pub status: WorkerStatus,
    pub capacity: WorkerCapacity,
    pub pipeline_config_hash: String,
    pub pipeline_name: Option<String>,
    pub version: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct WorkerDetailResponse {
    pub id: String,
    pub address: String,
    pub registered_at: DateTime<Utc>,
    pub last_heartbeat: DateTime<Utc>,
    pub status: WorkerStatus,
    pub capacity: WorkerCapacity,
    pub pipeline_config_hash: String,
    pub pipeline_name: Option<String>,
    pub pipeline_config: Option<serde_json::Value>,
    pub version: String,
    pub sessions_processed: u64,
    pub errors_total: u64,
    pub tags: Vec<String>,
    pub transport_address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_transport_address: Option<String>,
    /// Most recent component snapshots reported via heartbeat. Empty when the
    /// worker has not yet sent component data.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<ComponentSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregate_health: Option<crate::component::HealthState>,
}

#[derive(Debug, Serialize)]
pub struct DrainResponse {
    pub worker_id: String,
    pub status: WorkerStatus,
    pub active_sessions_remaining: u32,
}

// ── Sessions list response ──────────────────────────────────

#[derive(Debug, Serialize)]
pub struct PaginatedResponse<T: Serialize> {
    pub total: usize,
    pub page: usize,
    pub per_page: usize,
    pub items: Vec<T>,
}

/// A session record with its embedded turns (for session detail endpoint).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionWithTurns {
    #[serde(flatten)]
    pub session: SessionRecord,
    pub turns: Vec<TurnRecord>,
}

#[derive(Debug, Deserialize)]
pub struct SessionsQuery {
    pub page: Option<usize>,
    pub per_page: Option<usize>,
    pub tenant_id: Option<String>,
    pub status: Option<String>,
    pub since: Option<String>,
    pub until: Option<String>,
    pub search: Option<String>,
    pub worker_id: Option<String>,
    pub pipeline_name: Option<String>,
    pub pipeline_hash: Option<String>,
    pub tag: Option<String>,
    pub exclude_tag: Option<String>,
    /// Only sessions with at least this many turns (e.g. 1 hides
    /// empty sessions server-side instead of post-filtering a page).
    pub min_turns: Option<u32>,
}

// ── Audit list response ─────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct AuditFileItem {
    pub filename: String,
    pub size_bytes: u64,
    #[serde(rename = "type")]
    pub file_type: String,
    pub session_id: Option<String>,
    pub worker_id: Option<String>,
    pub created_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
pub struct AuditFilesQuery {
    pub page: Option<usize>,
    pub per_page: Option<usize>,
    pub session_id: Option<String>,
    #[serde(rename = "type")]
    pub file_type: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AuditTapItem {
    pub filename: String,
    pub size_bytes: u64,
    pub label: String,
    pub tag: String,
    pub sample_rate: u32,
    pub sequence: u64,
    pub worker_id: Option<String>,
    pub created_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
pub struct AuditTapsQuery {
    pub page: Option<usize>,
    pub per_page: Option<usize>,
    pub label: Option<String>,
    pub tag: Option<String>,
}

// ── Pipeline graph response ─────────────────────────────────

#[derive(Debug, Serialize)]
pub struct PipelineGraphResponse {
    pub name: String,
    pub nodes: Vec<PipelineNode>,
    pub edges: Vec<PipelineEdge>,
    pub audit_tap: PipelineAuditTap,
}

#[derive(Debug, Serialize)]
pub struct PipelineNode {
    pub id: String,
    #[serde(rename = "type")]
    pub node_type: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_sample_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_sample_rate: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub algorithm: Option<String>,
    pub metadata: serde_json::Value,
    #[serde(default)]
    pub param_schema: Vec<crate::api::param_schema::ParamDescriptor>,
}

#[derive(Debug, Serialize)]
pub struct PipelineEdge {
    pub from: String,
    pub to: String,
    pub data_type: String,
    pub sample_rate: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct PipelineAuditTap {
    pub enabled: bool,
    pub side: Option<String>,
    pub tapped_nodes: Vec<String>,
}

// ── Metrics query ───────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct MetricsQuery {
    pub window: Option<String>,
    pub worker_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PipelineQuery {
    pub worker_id: Option<String>,
}

// ── Orchestrator state ──────────────────────────────────────

#[derive(Debug, Clone)]
pub struct OrchestratorState {
    pub config: OrchestratorConfig,
    pub workers: Arc<DashMap<String, WorkerRecord>>,
    pub sessions: Arc<dyn SessionStore>,
    pub audit_store: Arc<dyn AuditStore>,
    pub tap_store: Option<Arc<dyn AuditStore>>,
    pub start_time: Instant,
}

// ── Orchestrator config ─────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OrchestratorConfig {
    pub port: u16,
    pub bind: String,
    pub cors_origins: Vec<String>,
    pub workers: OrchestratorWorkersConfig,
    pub sessions: OrchestratorSessionsConfig,
    pub dispatch: DispatchConfig,
    pub audit: OrchestratorAuditConfig,
    pub benchmarks_dir: PathBuf,
    /// Canonical path to the captures directory written by workers.
    /// The admin BFF discovers this via `GET /api/v1/config` so it
    /// doesn't need its own copy of the path.
    pub captures_dir: PathBuf,
    /// Runtime auth: admin + internal tokens resolve from the
    /// env vars named here; unset env vars leave the surface open.
    pub auth: crate::api::auth::AuthSettings,
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            port: 9090,
            bind: "127.0.0.1".to_string(),
            cors_origins: vec!["http://localhost:3000".to_string()],
            workers: OrchestratorWorkersConfig::default(),
            sessions: OrchestratorSessionsConfig::default(),
            dispatch: DispatchConfig::default(),
            audit: OrchestratorAuditConfig::default(),
            benchmarks_dir: PathBuf::from("benchmarks/"),
            captures_dir: PathBuf::from("captures/"),
            auth: crate::api::auth::AuthSettings::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OrchestratorWorkersConfig {
    pub heartbeat_timeout_s: u64,
    pub reap_interval_s: u64,
    /// How long a worker marked dead stays in the registry before the
    /// reaper removes it. Long enough for a restarting worker to be seen
    /// coming back under the same id; short enough that a fleet of
    /// workers without fixed ids does not fill the registry with ghosts.
    pub dead_grace_s: u64,
    /// Registry cap: registrations beyond this many *live* worker ids
    /// are rejected, bounding memory against spoofed registrations.
    /// Re-registration of a known id always succeeds.
    pub max_registered: usize,
    /// Optional allowlist of hosts workers may advertise.
    /// Empty = any syntactically valid `host:port`. Entries match the
    /// host part of both `address` and `transport_address`.
    pub allowed_hosts: Vec<String>,
    /// Bound on one proxied request to a worker's debug API.
    pub proxy_timeout_s: u64,
}

impl Default for OrchestratorWorkersConfig {
    fn default() -> Self {
        Self {
            heartbeat_timeout_s: 30,
            reap_interval_s: 10,
            dead_grace_s: 600,
            max_registered: 1_000,
            allowed_hosts: Vec::new(),
            proxy_timeout_s: 10,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OrchestratorSessionsConfig {
    pub max_entries: usize,
    #[serde(default)]
    pub store: SessionPersistenceConfig,
}

impl Default for OrchestratorSessionsConfig {
    fn default() -> Self {
        Self {
            max_entries: 50_000,
            store: SessionPersistenceConfig::default(),
        }
    }
}

/// Configuration for the session/turn store backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionPersistenceConfig {
    /// Backend for the session/turn index:
    /// - `"memory"` (default) — bounded in-RAM ring buffer, lost on restart.
    /// - `"fs"` — one JSON file per session/turn under `path`.
    /// - `"sqlite"` — indexed `SQLite` database at `path`. Recommended for
    ///   production: durable, fast filtered queries, unbounded history.
    pub provider: String,
    /// For `"fs"`: directory holding `sessions/` and `turns/` subdirs.
    /// For `"sqlite"`: path to the database file (e.g. `sessions/sessions.db`).
    pub path: Option<String>,
}

impl Default for SessionPersistenceConfig {
    fn default() -> Self {
        Self {
            provider: "memory".into(),
            path: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DispatchConfig {
    /// Routing policy: "least-connections" (default), "round-robin", "random".
    pub policy: String,
    /// When true, reject dispatch requests that don't specify a pipeline filter.
    pub require_pipeline_match: bool,
}

impl Default for DispatchConfig {
    fn default() -> Self {
        Self {
            policy: "least-connections".to_string(),
            require_pipeline_match: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct OrchestratorAuditConfig {
    pub shared_dir: PathBuf,
}

impl Default for OrchestratorAuditConfig {
    fn default() -> Self {
        Self {
            shared_dir: PathBuf::from("audit/"),
        }
    }
}

// ── Worker config ───────────────────────────────────────────

/// Unknown keys are an error: a table this struct does not read (a
/// pipeline setting placed in the wrong file) must fail at load rather
/// than be silently ignored.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkerConfig {
    pub id: Option<String>,
    pub max_concurrent: u32,
    pub debug_port: u16,
    pub debug_bind: String,
    /// Address the worker *advertises* to the orchestrator for its debug
    /// API, when the bind address is not reachable from the orchestrator —
    /// e.g. separate Docker containers, where a registered `0.0.0.0` falls
    /// back to the orchestrator's own loopback and every proxied live edit
    /// fails with 502. Use the compose service name (`"worker-custom:8085"`);
    /// a bare host defaults to the actual debug port. Unset: registers
    /// `{debug_bind}:{debug_port}` as before.
    pub advertise_address: Option<String>,
    /// *Browser-reachable* address of the client transport — what the
    /// orchestrator's `GET /api/v1/dispatch` hands out as `ws_url`. Unset:
    /// dispatch derives it from the bind address, which the orchestrator
    /// rewrites from `0.0.0.0` to `127.0.0.1` — right for one worker on the
    /// host, wrong as soon as two containerised workers publish different
    /// host ports (every dispatch then points at `127.0.0.1:8800`). Set it
    /// to what the browser can reach, e.g. `"localhost:8801"`; a bare host
    /// defaults to the actual transport port. The orchestrator itself (the
    /// benchmark runner) keeps using the internal `{bind}:{port}` address,
    /// so this never has to be reachable from inside the fleet.
    pub transport_advertise_address: Option<String>,
    pub orchestrator: WorkerOrchestratorConfig,
    /// Arbitrary tags for dispatch filtering (e.g. "gpu:a100", "region:eu-west").
    #[serde(default)]
    pub tags: Vec<String>,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            id: None,
            max_concurrent: 10,
            debug_port: 8081,
            debug_bind: "127.0.0.1".to_string(),
            advertise_address: None,
            transport_advertise_address: None,
            orchestrator: WorkerOrchestratorConfig::default(),
            tags: Vec::new(),
        }
    }
}

/// A misspelt key here (`endpont`) would otherwise parse and leave the
/// worker silently unregistered.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkerOrchestratorConfig {
    pub endpoint: Option<String>,
    pub heartbeat_interval_s: u64,
    pub register_timeout_s: u64,
}

impl Default for WorkerOrchestratorConfig {
    fn default() -> Self {
        Self {
            endpoint: None,
            heartbeat_interval_s: 5,
            register_timeout_s: 10,
        }
    }
}

// ── Worker application state ────────────────────────────────

/// What the served STT and TTS providers report about themselves.
#[derive(Debug, Clone, Default)]
pub struct ProviderFacts {
    /// STT provider capabilities.
    pub stt_capabilities: Vec<String>,
    /// STT provider's expected sample rate.
    pub stt_sample_rate: Option<u32>,
    /// TTS provider's native sample rate.
    pub tts_sample_rate: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct WorkerState {
    pub worker_id: String,
    pub config: PipelineConfig,
    pub worker_config: WorkerConfig,
    pub audit_store: Option<Arc<dyn AuditStore>>,
    pub tap_store: Arc<dyn AuditStore>,
    pub start_time: Instant,
    /// Completed-turn history, served by `GET /debug/sessions`. History only --
    /// never a capacity gauge; see `live_sessions`.
    pub local_sessions: Arc<RwLock<VecDeque<TurnRecord>>>,
    /// Live session count, shared with
    /// [`TransportState`](crate::transport::serve::TransportState) so the
    /// heartbeat and `/health` cannot disagree. Zero when transport is
    /// disabled (nothing can open a session).
    pub live_sessions: Arc<std::sync::atomic::AtomicUsize>,
    /// Resolved VAD backend name (e.g. "silero", "energy").
    pub vad_backend: String,
    /// Resolved VAD config values.
    pub vad_info: VadInfo,
    /// What the served providers reported about themselves, filled once
    /// when they are built.
    ///
    /// They are taken from the served providers rather than from providers
    /// built just to be asked, which would be a second full load of every
    /// model (a gigabyte for a Parakeet pool of four) for two numbers and a
    /// list. The served providers are built after this state exists, so the
    /// values arrive late; until they do, `/debug/providers` falls back to
    /// the config.
    pub provider_facts: std::sync::OnceLock<ProviderFacts>,
    /// Generic registry of mutable runtime parameters keyed by `PipelineNode.id`.
    /// Layer 2 components register here at construction; the
    /// `PATCH /debug/params` handler dispatches through this single map.
    pub param_registry: Arc<crate::api::param_store::ParamRegistry>,
    /// Process-wide registry of every live [`crate::component::Component`].
    /// Populated at worker startup; powers `/debug/components` and the
    /// aggregate health on `/debug/health`.
    pub component_registry: Arc<crate::component::ComponentRegistry>,
}

// ── Provider info DTOs ──────────────────────────────────────

/// Resolved VAD runtime information.
#[derive(Debug, Clone, Serialize)]
pub struct VadInfo {
    pub mode: String,
    pub backend: String,
    pub model: Option<String>,
    pub threshold: f32,
    pub neg_threshold: f32,
    pub min_speech_ms: u32,
    pub min_silence_ms: u32,
}

/// STT provider runtime information.
#[derive(Debug, Clone, Serialize)]
pub struct SttProviderInfo {
    pub name: String,
    pub expected_sample_rate: u32,
    pub capabilities: Vec<String>,
}

/// Response for /debug/providers endpoint.
#[derive(Debug, Serialize)]
pub struct ProvidersResponse {
    pub stt: Option<SttProviderInfo>,
    pub tts: Option<TtsProviderInfo>,
    pub vad: VadInfo,
}

/// TTS provider runtime information.
#[derive(Debug, Clone, Serialize)]
pub struct TtsProviderInfo {
    pub name: String,
    pub native_sample_rate: u32,
}

// ── Dispatch types ──────────────────────────────────────────

/// Query parameters for `GET /api/v1/dispatch`.
#[derive(Debug, Deserialize)]
pub struct DispatchQuery {
    /// Filter workers by pipeline name.
    pub pipeline_name: Option<String>,
    /// Filter workers by config hash (exact or prefix).
    pub pipeline_hash: Option<String>,
    /// Comma-separated tags -- all must match.
    pub tags: Option<String>,
    /// Override the default dispatch policy.
    pub policy: Option<String>,
}

/// Dispatch response with the selected worker's connection info.
#[derive(Debug, Serialize)]
pub struct DispatchResponse {
    pub worker_id: String,
    pub ws_url: String,
    pub pipeline_name: Option<String>,
    pub pipeline_config_hash: String,
}

// ── Pipeline group types ────────────────────────────────────

/// Response for `GET /api/v1/pipelines` -- workers grouped by config hash.
#[derive(Debug, Serialize)]
pub struct PipelineGroupsResponse {
    pub groups: Vec<PipelineGroup>,
}

/// A group of workers sharing the same pipeline configuration.
#[derive(Debug, Serialize)]
pub struct PipelineGroup {
    pub pipeline_name: Option<String>,
    pub pipeline_config_hash: String,
    pub workers: Vec<PipelineGroupWorker>,
    pub total_capacity: u32,
    pub total_active: u32,
    pub sessions_processed: u64,
}

/// Minimal worker info within a pipeline group.
#[derive(Debug, Serialize)]
pub struct PipelineGroupWorker {
    pub id: String,
    pub status: WorkerStatus,
    pub active_sessions: u32,
    pub max_concurrent: u32,
}

// ── Pipeline diff types ─────────────────────────────────────

/// Query parameters for `GET /api/v1/pipelines/diff`.
#[derive(Debug, Deserialize)]
pub struct DiffQuery {
    pub hash_a: String,
    pub hash_b: String,
}

/// Diff response showing structural differences between two pipeline configs.
#[derive(Debug, Serialize)]
pub struct DiffResponse {
    pub hash_a: String,
    pub hash_b: String,
    pub diffs: Vec<DiffEntry>,
}

/// A single field difference between two configs.
#[derive(Debug, Serialize)]
pub struct DiffEntry {
    pub path: String,
    pub a: Option<serde_json::Value>,
    pub b: Option<serde_json::Value>,
}
