//! Admission control, per-tenant concurrency quota, FIFO waiting queue, and
//! production autoscaling telemetry for the orchestrator.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use tokio::sync::watch;

use crate::api::types::{
    AcquireDispatchRequest, AcquireDispatchResponse, ApiResponse, CapacitySnapshot,
    OrchestratorState, TicketPollQuery, WorkerCapacity, WorkerStatus,
};

use super::handlers::connectable_addr;

/// Look up the [`AdmissionController`] associated with `state`.
pub(crate) fn for_state(state: &OrchestratorState) -> &AdmissionController {
    &state.admission
}

#[derive(Debug, Clone)]
struct LeaseEntry {
    worker_id: String,
    tenant_id: Option<String>,
    expires_at: Instant,
}

#[derive(Debug)]
struct QueuedTicket {
    ticket_id: String,
    request: AcquireDispatchRequest,
    enqueued_at: Instant,
    expires_at: Instant,
    tx: watch::Sender<AcquireDispatchResponse>,
    rx: watch::Receiver<AcquireDispatchResponse>,
}

#[derive(Debug)]
struct PeakTracker {
    epoch: Instant,
    /// `(minute_bucket, max_concurrent_sessions)` within the last 24h (max 1440 buckets).
    buckets: VecDeque<(u64, u32)>,
}

impl Default for PeakTracker {
    fn default() -> Self {
        Self {
            epoch: Instant::now(),
            buckets: VecDeque::new(),
        }
    }
}

impl PeakTracker {
    fn bucket_of(&self, now: Instant) -> u64 {
        now.saturating_duration_since(self.epoch).as_secs() / 60
    }

    fn prune(&mut self, current_bucket: u64) {
        while let Some(&(b, _)) = self.buckets.front() {
            if current_bucket.saturating_sub(b) >= 1440 {
                self.buckets.pop_front();
            } else {
                break;
            }
        }
    }

    fn record(&mut self, now: Instant, current: u32) {
        let bucket = self.bucket_of(now);
        self.prune(bucket);
        if let Some((b, val)) = self.buckets.back_mut()
            && *b == bucket
        {
            *val = (*val).max(current);
            return;
        }
        self.buckets.push_back((bucket, current));
    }

    fn peaks(&mut self, now: Instant, current: u32) -> (u32, u32) {
        let bucket = self.bucket_of(now);
        self.prune(bucket);
        let mut p1h = current;
        let mut p24h = current;
        for &(b, val) in &self.buckets {
            if val > p24h {
                p24h = val;
            }
            if bucket.saturating_sub(b) < 60 && val > p1h {
                p1h = val;
            }
        }
        (p1h, p24h)
    }
}

#[derive(Debug, Default)]
struct RejectCounters {
    queue_full: AtomicU64,
    tenant_limit: AtomicU64,
    no_workers: AtomicU64,
    timeout: AtomicU64,
    wait_count: AtomicU64,
    wait_ms_sum: AtomicU64,
}

#[derive(Debug)]
struct InnerState {
    leases: HashMap<String, LeaseEntry>,
    active_sessions: HashMap<String, (String, Option<String>)>,
    queue: VecDeque<QueuedTicket>,
    resolved_tickets: HashMap<String, (Instant, AcquireDispatchResponse)>,
    peaks: PeakTracker,
}

/// Orchestrator-wide admission controller, lease tracker, and FIFO waiting queue.
#[derive(Debug)]
pub struct AdmissionController {
    inner: Mutex<InnerState>,
    counters: RejectCounters,
    rr_counter: AtomicU64,
}

impl Default for AdmissionController {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
struct CandidateWorker {
    id: String,
    effective_active: u32,
    max_concurrent: u32,
    pipeline_name: Option<String>,
    pipeline_config_hash: String,
    ws_url: String,
}

impl AdmissionController {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(InnerState {
                leases: HashMap::new(),
                active_sessions: HashMap::new(),
                queue: VecDeque::new(),
                resolved_tickets: HashMap::new(),
                peaks: PeakTracker::default(),
            }),
            counters: RejectCounters::default(),
            rr_counter: AtomicU64::new(0),
        }
    }

    fn next_id(prefix: &str) -> String {
        format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
    }
}

fn clean_expired(
    inner: &mut InnerState,
    state: &OrchestratorState,
    counters: &RejectCounters,
    now: Instant,
) {
    let mut expired_workers: Vec<String> = Vec::new();
    inner.leases.retain(|_, lease| {
        if lease.expires_at <= now {
            expired_workers.push(lease.worker_id.clone());
            false
        } else {
            true
        }
    });
    for wid in expired_workers {
        if let Some(mut w) = state.workers.get_mut(&wid) {
            w.capacity.active_sessions = w.capacity.active_sessions.saturating_sub(1);
        }
    }
    let resolved_cutoff = now.checked_sub(Duration::from_secs(60)).unwrap_or(now);
    inner
        .resolved_tickets
        .retain(|_, (ts, _)| *ts > resolved_cutoff);

    let drained: Vec<QueuedTicket> = inner.queue.drain(..).collect();
    for t in drained {
        if t.expires_at <= now {
            counters.timeout.fetch_add(1, Ordering::Relaxed);
            let msg = AcquireDispatchResponse::Expired {
                ticket_id: t.ticket_id.clone(),
                reason: "queue_timeout".to_string(),
            };
            let _ = t.tx.send(msg.clone());
            inner.resolved_tickets.insert(t.ticket_id, (now, msg));
        } else {
            inner.queue.push_back(t);
        }
    }
}

fn tenant_usage(inner: &InnerState, tenant: &str) -> u32 {
    let active = inner
        .active_sessions
        .values()
        .filter(|(_, t)| t.as_deref() == Some(tenant))
        .count();
    let leased = inner
        .leases
        .values()
        .filter(|l| l.tenant_id.as_deref() == Some(tenant))
        .count();
    let queued = inner
        .queue
        .iter()
        .filter(|q| q.request.tenant_id.as_deref() == Some(tenant))
        .count();
    u32::try_from(active.saturating_add(leased).saturating_add(queued)).unwrap_or(u32::MAX)
}

fn fleet_totals(inner: &InnerState, state: &OrchestratorState) -> (u32, u32, u32) {
    let mut worker_active: u32 = 0;
    let mut worker_cap: u32 = 0;
    for entry in state.workers.iter() {
        let w = entry.value();
        if w.status == WorkerStatus::Active {
            worker_active = worker_active.saturating_add(w.capacity.active_sessions);
            worker_cap = worker_cap.saturating_add(w.capacity.max_concurrent);
        }
    }
    let tracked_active = u32::try_from(inner.active_sessions.len()).unwrap_or(u32::MAX);
    let leased = u32::try_from(inner.leases.len()).unwrap_or(u32::MAX);
    // `worker_active` already includes optimistic increments for leases; take max with tracked.
    let active = worker_active.saturating_sub(leased).max(tracked_active);
    let cfg_max = state.config.dispatch.admission.max_active_sessions;
    let effective_limit = if state.config.dispatch.admission.enabled && cfg_max > 0 {
        if worker_cap > 0 {
            cfg_max.min(worker_cap)
        } else {
            cfg_max
        }
    } else {
        worker_cap
    };
    (active, leased, effective_limit)
}

fn matching_candidates(
    state: &OrchestratorState,
    req: &AcquireDispatchRequest,
) -> Vec<CandidateWorker> {
    let required_tags: Vec<String> = req
        .tags
        .as_deref()
        .map(|t| {
            t.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();

    let mut out = Vec::new();
    for entry in state.workers.iter() {
        let w = entry.value();
        if w.status != WorkerStatus::Active {
            continue;
        }
        if let Some(ref wid) = req.worker_id
            && &w.id != wid
        {
            continue;
        }
        if let Some(ref name) = req.pipeline_name
            && w.pipeline_name.as_deref() != Some(name.as_str())
        {
            continue;
        }
        if let Some(ref hash) = req.pipeline_hash
            && !w.pipeline_config_hash.starts_with(hash.as_str())
        {
            continue;
        }
        if !required_tags.is_empty() && !required_tags.iter().all(|t| w.tags.contains(t)) {
            continue;
        }
        let raw_addr = w
            .public_transport_address
            .as_deref()
            .or(w.transport_address.as_deref())
            .unwrap_or(&w.address);
        out.push(CandidateWorker {
            id: w.id.clone(),
            effective_active: w.capacity.active_sessions,
            max_concurrent: w.capacity.max_concurrent,
            pipeline_name: w.pipeline_name.clone(),
            pipeline_config_hash: w.pipeline_config_hash.clone(),
            ws_url: format!("ws://{}/ws", connectable_addr(raw_addr)),
        });
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    out
}

fn pick_available_worker<'a>(
    candidates: &'a [CandidateWorker],
    policy: &str,
    rr: &AtomicU64,
) -> Option<&'a CandidateWorker> {
    let available: Vec<&CandidateWorker> = candidates
        .iter()
        .filter(|c| c.effective_active < c.max_concurrent)
        .collect();
    if available.is_empty() {
        return None;
    }
    let idx = match policy {
        "round-robin" => {
            usize::try_from(rr.fetch_add(1, Ordering::Relaxed)).unwrap_or(0) % available.len()
        }
        _ => available
            .iter()
            .enumerate()
            .min_by_key(|(_, c)| c.effective_active)
            .map_or(0, |(i, _)| i),
    };
    available.get(idx).copied()
}

fn broadcast_queue_positions(inner: &InnerState, active: u32, limit: u32, hint_s: u64) {
    let depth = inner.queue.len();
    let slots = u64::from(limit.max(1));
    for (idx, ticket) in inner.queue.iter().enumerate() {
        let pos = idx + 1;
        let eta = ((idx as u64 + 1).saturating_mul(hint_s)) / slots;
        let _ = ticket.tx.send(AcquireDispatchResponse::Queued {
            ticket_id: ticket.ticket_id.clone(),
            position: pos,
            ahead: idx,
            queue_depth: depth,
            active_sessions: active,
            capacity_sessions: limit,
            estimated_wait_s: eta.max(5),
        });
    }
}

fn grant_lease(
    inner: &mut InnerState,
    state: &OrchestratorState,
    chosen: &CandidateWorker,
    tenant_id: Option<String>,
    now: Instant,
) -> AcquireDispatchResponse {
    let ttl_s = state.config.dispatch.admission.lease_ttl_s.max(3);
    let lease_id = AdmissionController::next_id("vp_lease");
    if let Some(mut entry) = state.workers.get_mut(&chosen.id) {
        let cap = &mut entry.value_mut().capacity;
        cap.active_sessions = cap.active_sessions.saturating_add(1);
    }
    inner.leases.insert(
        lease_id.clone(),
        LeaseEntry {
            worker_id: chosen.id.clone(),
            tenant_id,
            expires_at: now + Duration::from_secs(ttl_s),
        },
    );
    let (active, leased, _) = fleet_totals(inner, state);
    inner.peaks.record(now, active.saturating_add(leased));
    AcquireDispatchResponse::Granted {
        worker_id: chosen.id.clone(),
        ws_url: chosen.ws_url.clone(),
        pipeline_name: chosen.pipeline_name.clone(),
        pipeline_config_hash: chosen.pipeline_config_hash.clone(),
        lease_id,
        expires_in_s: ttl_s,
    }
}

/// Promote waiting FIFO tickets whenever a session ends or worker capacity frees up.
pub(crate) fn promote_queued_tickets(state: &OrchestratorState) {
    let ctrl = for_state(state);
    let Ok(mut inner) = ctrl.inner.lock() else {
        return;
    };
    let now = Instant::now();
    clean_expired(&mut inner, state, &ctrl.counters, now);

    let policy_default = state.config.dispatch.policy.clone();
    let mut promoted_any = false;
    let mut idx = 0;
    while idx < inner.queue.len() {
        let (active, leased, limit) = fleet_totals(&inner, state);
        if limit == 0 || active.saturating_add(leased) >= limit {
            break;
        }
        let Some(ticket_ref) = inner.queue.get(idx) else {
            break;
        };
        let candidates = matching_candidates(state, &ticket_ref.request);
        let policy = ticket_ref
            .request
            .policy
            .as_deref()
            .unwrap_or(&policy_default);
        let Some(chosen) = pick_available_worker(&candidates, policy, &ctrl.rr_counter).cloned()
        else {
            idx += 1;
            continue;
        };
        let Some(ticket) = inner.queue.remove(idx) else {
            break;
        };
        let wait_ms =
            u64::try_from(now.duration_since(ticket.enqueued_at).as_millis()).unwrap_or(0);
        ctrl.counters.wait_count.fetch_add(1, Ordering::Relaxed);
        ctrl.counters
            .wait_ms_sum
            .fetch_add(wait_ms, Ordering::Relaxed);
        let granted = grant_lease(&mut inner, state, &chosen, ticket.request.tenant_id, now);
        let _ = ticket.tx.send(granted.clone());
        inner
            .resolved_tickets
            .insert(ticket.ticket_id, (now, granted));
        promoted_any = true;
    }

    if promoted_any {
        let (active, _, limit) = fleet_totals(&inner, state);
        let hint_s = state.config.dispatch.admission.avg_session_duration_hint_s;
        broadcast_queue_positions(&inner, active, limit, hint_s);
    }
}

/// Hook invoked when a worker reports a session start event (`ended_at.is_none()`).
pub(crate) fn on_session_start(
    state: &OrchestratorState,
    session_id: &str,
    worker_id: &str,
    tenant_id: Option<&str>,
) {
    let ctrl = for_state(state);
    let Ok(mut inner) = ctrl.inner.lock() else {
        return;
    };
    let now = Instant::now();
    // Redeem the oldest matching lease for (worker_id, tenant_id) so the lease
    // transitions cleanly into an active session without double-counting.
    let matching_lease = inner
        .leases
        .iter()
        .find(|(_, l)| {
            l.worker_id == worker_id && (tenant_id.is_none() || l.tenant_id.as_deref() == tenant_id)
        })
        .map(|(k, _)| k.clone());
    if let Some(lid) = matching_lease {
        inner.leases.remove(&lid);
    }
    inner.active_sessions.insert(
        session_id.to_string(),
        (worker_id.to_string(), tenant_id.map(str::to_string)),
    );
    let (active, leased, _) = fleet_totals(&inner, state);
    inner.peaks.record(now, active.saturating_add(leased));
}

/// Merge a worker's reported heartbeat capacity with any unredeemed admission leases
/// currently held for that worker so optimistic lease counts are not overwritten.
pub(crate) fn on_worker_heartbeat(
    state: &OrchestratorState,
    worker_id: &str,
    mut cap: WorkerCapacity,
) -> WorkerCapacity {
    let ctrl = for_state(state);
    let Ok(mut inner) = ctrl.inner.lock() else {
        return cap;
    };
    let now = Instant::now();
    clean_expired(&mut inner, state, &ctrl.counters, now);
    let tracked_on_worker = u32::try_from(
        inner
            .active_sessions
            .values()
            .filter(|(wid, _)| wid == worker_id)
            .count(),
    )
    .unwrap_or(u32::MAX);
    let pending_leases = u32::try_from(
        inner
            .leases
            .values()
            .filter(|l| l.worker_id == worker_id)
            .count(),
    )
    .unwrap_or(u32::MAX);
    let untracked_live = cap.active_sessions.saturating_sub(tracked_on_worker);
    let unconnected_leases = pending_leases.saturating_sub(untracked_live);
    cap.active_sessions = cap.active_sessions.saturating_add(unconnected_leases);
    cap
}

/// Hook invoked when a worker reports a session end event (`ended_at.is_some()`).
pub(crate) fn on_session_end(state: &OrchestratorState, session_id: &str, worker_id: &str) {
    let ctrl = for_state(state);
    if let Ok(mut inner) = ctrl.inner.lock() {
        let removed = inner.active_sessions.remove(session_id);
        let wid = removed
            .as_ref()
            .map_or(worker_id, |(recorded_wid, _)| recorded_wid.as_str());
        if !wid.is_empty() {
            let remaining_tracked = u32::try_from(
                inner
                    .active_sessions
                    .values()
                    .filter(|(w, _)| w == wid)
                    .count(),
            )
            .unwrap_or(u32::MAX);
            let pending_leases =
                u32::try_from(inner.leases.values().filter(|l| l.worker_id == wid).count())
                    .unwrap_or(u32::MAX);
            let min_expected = remaining_tracked.saturating_add(pending_leases);
            if let Some(mut w) = state.workers.get_mut(wid) {
                w.capacity.active_sessions = w
                    .capacity
                    .active_sessions
                    .saturating_sub(1)
                    .max(min_expected);
            }
        }
    }
    promote_queued_tickets(state);
}

fn extract_tenant_from_req(headers: &HeaderMap, body_tenant: Option<&str>) -> Option<String> {
    if let Some(t) = body_tenant.map(str::trim).filter(|s| !s.is_empty()) {
        return Some(t.to_string());
    }
    headers
        .get("x-tenant-id")
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// `POST /api/v1/dispatch/acquire` — Acquire a worker lease or enter the FIFO waiting queue.
pub async fn acquire_dispatch(
    State(state): State<Arc<OrchestratorState>>,
    headers: HeaderMap,
    Json(mut req): Json<AcquireDispatchRequest>,
) -> Response {
    req.tenant_id = extract_tenant_from_req(&headers, req.tenant_id.as_deref());
    let candidates = matching_candidates(&state, &req);
    let ctrl = for_state(&state);
    if candidates.is_empty() {
        ctrl.counters.no_workers.fetch_add(1, Ordering::Relaxed);
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ApiResponse::<()>::internal_error(
                "no available workers matching criteria",
            )),
        )
            .into_response();
    }

    let Ok(mut inner) = ctrl.inner.lock() else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ApiResponse::<()>::internal_error("admission lock poisoned")),
        )
            .into_response();
    };
    let now = Instant::now();
    clean_expired(&mut inner, &state, &ctrl.counters, now);

    let adm = &state.config.dispatch.admission;
    if adm.enabled
        && adm.max_per_tenant > 0
        && let Some(ref tid) = req.tenant_id
        && tenant_usage(&inner, tid) >= adm.max_per_tenant
    {
        ctrl.counters.tenant_limit.fetch_add(1, Ordering::Relaxed);
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(ApiResponse::<()>::error(
                "TENANT_LIMIT_REACHED",
                format!(
                    "per-user concurrency limit ({}) reached; close your existing session first",
                    adm.max_per_tenant
                ),
            )),
        )
            .into_response();
    }

    let (active, leased, limit) = fleet_totals(&inner, &state);
    let policy = req
        .policy
        .as_deref()
        .unwrap_or(&state.config.dispatch.policy);
    let global_has_room = !adm.enabled || (limit > 0 && active.saturating_add(leased) < limit);

    if inner.queue.is_empty()
        && global_has_room
        && let Some(chosen) = pick_available_worker(&candidates, policy, &ctrl.rr_counter)
    {
        let granted = grant_lease(&mut inner, &state, chosen, req.tenant_id, now);
        return (StatusCode::OK, Json(ApiResponse::ok(granted))).into_response();
    }

    if !adm.enabled || adm.max_queue_size == 0 {
        ctrl.counters.queue_full.fetch_add(1, Ordering::Relaxed);
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(ApiResponse::<()>::error(
                "CAPACITY_FULL",
                "all voice worker slots are currently busy",
            )),
        )
            .into_response();
    }

    if inner.queue.len() >= adm.max_queue_size {
        ctrl.counters.queue_full.fetch_add(1, Ordering::Relaxed);
        return (
            StatusCode::TOO_MANY_REQUESTS,
            Json(ApiResponse::<()>::error(
                "QUEUE_FULL",
                format!(
                    "waiting queue is full ({} waiting); try again shortly",
                    adm.max_queue_size
                ),
            )),
        )
            .into_response();
    }

    let initial = enqueue_ticket(&mut inner, req, adm, active, leased, limit, now);
    (StatusCode::ACCEPTED, Json(ApiResponse::ok(initial))).into_response()
}

fn enqueue_ticket(
    inner: &mut InnerState,
    req: AcquireDispatchRequest,
    adm: &crate::api::types::AdmissionConfig,
    active: u32,
    leased: u32,
    limit: u32,
    now: Instant,
) -> AcquireDispatchResponse {
    let ticket_id = AdmissionController::next_id("vp_ticket");
    let pos = inner.queue.len() + 1;
    let ahead = inner.queue.len();
    let slots = u64::from(limit.max(1));
    let eta = ((pos as u64).saturating_mul(adm.avg_session_duration_hint_s)) / slots;
    let initial = AcquireDispatchResponse::Queued {
        ticket_id: ticket_id.clone(),
        position: pos,
        ahead,
        queue_depth: pos,
        active_sessions: active.saturating_add(leased),
        capacity_sessions: limit,
        estimated_wait_s: eta.max(5),
    };
    let (tx, rx) = watch::channel(initial.clone());
    inner.queue.push_back(QueuedTicket {
        ticket_id,
        request: req,
        enqueued_at: now,
        expires_at: now + Duration::from_secs(adm.queue_timeout_s.max(5)),
        tx,
        rx,
    });
    initial
}

/// `GET /api/v1/dispatch/tickets/{id}` — Poll or long-poll a waiting queue ticket.
pub async fn get_ticket_status(
    State(state): State<Arc<OrchestratorState>>,
    headers: HeaderMap,
    Path(ticket_id): Path<String>,
    Query(q): Query<TicketPollQuery>,
) -> Response {
    let ctrl = for_state(&state);
    let wait_ms = q.wait_ms.unwrap_or(0).min(25_000);
    let caller_tenant = extract_tenant_from_req(&headers, None);

    let mut rx_opt = None;
    {
        let Ok(mut inner) = ctrl.inner.lock() else {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<()>::internal_error("admission lock poisoned")),
            )
                .into_response();
        };
        let now = Instant::now();
        clean_expired(&mut inner, &state, &ctrl.counters, now);
        if let Some((_, resolved)) = inner.resolved_tickets.get(&ticket_id) {
            return Json(ApiResponse::ok(resolved.clone())).into_response();
        }
        if let Some(t) = inner.queue.iter().find(|t| t.ticket_id == ticket_id) {
            if let Some(ref tid) = caller_tenant
                && t.request.tenant_id.as_deref() != Some(tid.as_str())
            {
                return (
                    StatusCode::NOT_FOUND,
                    Json(ApiResponse::<()>::not_found(format!(
                        "Ticket {ticket_id} not found"
                    ))),
                )
                    .into_response();
            }
            if wait_ms == 0 {
                return Json(ApiResponse::ok(t.rx.borrow().clone())).into_response();
            }
            rx_opt = Some(t.rx.clone());
        }
    }

    let Some(mut rx) = rx_opt else {
        return (
            StatusCode::NOT_FOUND,
            Json(ApiResponse::<()>::not_found(format!(
                "Ticket {ticket_id} not found"
            ))),
        )
            .into_response();
    };

    let _ = tokio::time::timeout(Duration::from_millis(wait_ms), rx.changed()).await;
    let current = rx.borrow().clone();
    Json(ApiResponse::ok(current)).into_response()
}

/// `DELETE /api/v1/dispatch/tickets/{id}` — Cancel a waiting queue ticket or release an unused lease.
pub async fn cancel_ticket(
    State(state): State<Arc<OrchestratorState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let ctrl = for_state(&state);
    let caller_tenant = extract_tenant_from_req(&headers, None);
    let mut promote = false;
    {
        let Ok(mut inner) = ctrl.inner.lock() else {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiResponse::<()>::internal_error("admission lock poisoned")),
            )
                .into_response();
        };
        let now = Instant::now();
        if let Some(lease) = inner.leases.get(&id)
            && caller_tenant
                .as_deref()
                .is_none_or(|tid| lease.tenant_id.as_deref() == Some(tid))
            && let Some(lease) = inner.leases.remove(&id)
        {
            if let Some(mut w) = state.workers.get_mut(&lease.worker_id) {
                w.capacity.active_sessions = w.capacity.active_sessions.saturating_sub(1);
            }
            promote = true;
        } else if let Some(idx) = inner.queue.iter().position(|t| {
            t.ticket_id == id
                && caller_tenant
                    .as_deref()
                    .is_none_or(|tid| t.request.tenant_id.as_deref() == Some(tid))
        }) && let Some(t) = inner.queue.remove(idx)
        {
            let msg = AcquireDispatchResponse::Cancelled {
                ticket_id: id.clone(),
            };
            let _ = t.tx.send(msg.clone());
            inner.resolved_tickets.insert(id.clone(), (now, msg));
            let (active, _, limit) = fleet_totals(&inner, &state);
            let hint_s = state.config.dispatch.admission.avg_session_duration_hint_s;
            broadcast_queue_positions(&inner, active, limit, hint_s);
            promote = true;
        }
    }
    if promote {
        promote_queued_tickets(&state);
    }
    Json(ApiResponse::ok(serde_json::json!({
        "id": id,
        "cancelled": promote,
    })))
    .into_response()
}

/// Build the real-time [`CapacitySnapshot`] for `state`.
pub(crate) fn build_capacity_snapshot(state: &OrchestratorState) -> CapacitySnapshot {
    let ctrl = for_state(state);
    let adm = &state.config.dispatch.admission;
    let Ok(mut inner) = ctrl.inner.lock() else {
        return CapacitySnapshot {
            admission_enabled: adm.enabled,
            active_sessions: 0,
            leased_sessions: 0,
            capacity_sessions: 0,
            effective_limit: 0,
            utilization_ratio: 0.0,
            queue_depth: 0,
            max_queue_size: adm.max_queue_size,
            max_per_tenant: adm.max_per_tenant,
            estimated_wait_s: 0,
            peak_concurrent_1h: 0,
            peak_concurrent_24h: 0,
            active_tenants: 0,
            per_tenant_active: None,
        };
    };
    let now = Instant::now();
    clean_expired(&mut inner, state, &ctrl.counters, now);

    let (active, leased, effective_limit) = fleet_totals(&inner, state);
    let mut worker_cap: u32 = 0;
    for entry in state.workers.iter() {
        if entry.value().status == WorkerStatus::Active {
            worker_cap = worker_cap.saturating_add(entry.value().capacity.max_concurrent);
        }
    }
    let in_use = active.saturating_add(leased);
    let util = if effective_limit > 0 {
        (f64::from(in_use) / f64::from(effective_limit)).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let (p1h, p24h) = inner.peaks.peaks(now, in_use);
    let mut per_tenant: BTreeMap<String, u32> = BTreeMap::new();
    for (_, tid) in inner.active_sessions.values() {
        if let Some(t) = tid {
            *per_tenant.entry(t.clone()).or_insert(0) += 1;
        }
    }
    for l in inner.leases.values() {
        if let Some(ref t) = l.tenant_id {
            *per_tenant.entry(t.clone()).or_insert(0) += 1;
        }
    }
    let q_depth = inner.queue.len();
    let slots = u64::from(effective_limit.max(1));
    let eta = if q_depth == 0 && in_use < effective_limit {
        0
    } else {
        (((q_depth as u64) + 1).saturating_mul(adm.avg_session_duration_hint_s)) / slots
    };

    CapacitySnapshot {
        admission_enabled: adm.enabled,
        active_sessions: active,
        leased_sessions: leased,
        capacity_sessions: worker_cap,
        effective_limit,
        utilization_ratio: (util * 1000.0).round() / 1000.0,
        queue_depth: q_depth,
        max_queue_size: adm.max_queue_size,
        max_per_tenant: adm.max_per_tenant,
        estimated_wait_s: eta,
        peak_concurrent_1h: p1h,
        peak_concurrent_24h: p24h,
        active_tenants: per_tenant.len(),
        per_tenant_active: Some(per_tenant),
    }
}

/// `GET /api/v1/capacity` — Real-time capacity, queue depth, and peak concurrency.
pub async fn get_capacity(
    State(state): State<Arc<OrchestratorState>>,
) -> Json<ApiResponse<CapacitySnapshot>> {
    Json(ApiResponse::ok(build_capacity_snapshot(&state)))
}

/// `GET /metrics` — Prometheus `OpenMetrics` exposition for KEDA / HPA autoscalers.
pub async fn prometheus_metrics(State(state): State<Arc<OrchestratorState>>) -> Response {
    let snap = build_capacity_snapshot(&state);
    let ctrl = for_state(&state);
    let q_full = ctrl.counters.queue_full.load(Ordering::Relaxed);
    let t_limit = ctrl.counters.tenant_limit.load(Ordering::Relaxed);
    let no_wrk = ctrl.counters.no_workers.load(Ordering::Relaxed);
    let t_out = ctrl.counters.timeout.load(Ordering::Relaxed);
    let w_cnt = ctrl.counters.wait_count.load(Ordering::Relaxed);
    let w_sum_s = ctrl.counters.wait_ms_sum.load(Ordering::Relaxed) as f64 / 1000.0;

    let body = format!(
        "# HELP vp_fleet_active_sessions Currently active voice sessions across healthy workers.\n\
         # TYPE vp_fleet_active_sessions gauge\n\
         vp_fleet_active_sessions {}\n\
         # HELP vp_fleet_leased_sessions Granted dispatch leases awaiting WebSocket connect.\n\
         # TYPE vp_fleet_leased_sessions gauge\n\
         vp_fleet_leased_sessions {}\n\
         # HELP vp_fleet_capacity_sessions Total max_concurrent capacity across healthy workers.\n\
         # TYPE vp_fleet_capacity_sessions gauge\n\
         vp_fleet_capacity_sessions {}\n\
         # HELP vp_fleet_effective_limit_sessions Effective admission session limit.\n\
         # TYPE vp_fleet_effective_limit_sessions gauge\n\
         vp_fleet_effective_limit_sessions {}\n\
         # HELP vp_fleet_utilization_ratio Ratio of (active + leased) to effective capacity (0.0..1.0).\n\
         # TYPE vp_fleet_utilization_ratio gauge\n\
         vp_fleet_utilization_ratio {:.4}\n\
         # HELP vp_dispatch_queue_depth Number of callers waiting in the FIFO admission queue.\n\
         # TYPE vp_dispatch_queue_depth gauge\n\
         vp_dispatch_queue_depth {}\n\
         # HELP vp_active_tenants_total Number of distinct tenants with active or leased sessions.\n\
         # TYPE vp_active_tenants_total gauge\n\
         vp_active_tenants_total {}\n\
         # HELP vp_peak_concurrent_sessions Peak concurrent sessions observed over rolling windows.\n\
         # TYPE vp_peak_concurrent_sessions gauge\n\
         vp_peak_concurrent_sessions{{window=\"1h\"}} {}\n\
         vp_peak_concurrent_sessions{{window=\"24h\"}} {}\n\
         # HELP vp_dispatch_rejections_total Total rejected dispatch requests by reason.\n\
         # TYPE vp_dispatch_rejections_total counter\n\
         vp_dispatch_rejections_total{{reason=\"queue_full\"}} {q_full}\n\
         vp_dispatch_rejections_total{{reason=\"tenant_limit\"}} {t_limit}\n\
         vp_dispatch_rejections_total{{reason=\"no_workers\"}} {no_wrk}\n\
         vp_dispatch_rejections_total{{reason=\"timeout\"}} {t_out}\n\
         # HELP vp_dispatch_queue_wait_seconds Total time spent waiting in the FIFO queue.\n\
         # TYPE vp_dispatch_queue_wait_seconds summary\n\
         vp_dispatch_queue_wait_seconds_sum {w_sum_s:.3}\n\
         vp_dispatch_queue_wait_seconds_count {w_cnt}\n",
        snap.active_sessions,
        snap.leased_sessions,
        snap.capacity_sessions,
        snap.effective_limit,
        snap.utilization_ratio,
        snap.queue_depth,
        snap.active_tenants,
        snap.peak_concurrent_1h,
        snap.peak_concurrent_24h,
    );

    (
        StatusCode::OK,
        [(
            header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        body,
    )
        .into_response()
}

#[cfg(test)]
#[path = "tests/admission.rs"]
mod tests;
