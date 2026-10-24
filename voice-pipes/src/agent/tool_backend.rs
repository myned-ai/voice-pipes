//! S2S tool backend: a [`ToolRegistry`] whose handlers live behind an
//! [`AgentLink`].
//!
//! On S2S sessions the provider model is the conversational brain and
//! the remote agent is the **tool backend**: the pipeline forwards
//! provider `FunctionCall`s over the link as `ToolCall` frames and
//! relays the agent's `ToolResult` back (Done, or a Pending ack whose
//! real result arrives later as `CompleteTool`). Out-of-turn frames
//! (`Say`/`Message`/`Cancel`/`CompleteTool`) route onto the session's
//! [`AgentSessionHandle`] exactly like the classic gateway pump.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::sync::oneshot;
use tracing::{info, warn};

use crate::agent::link::{
    AGENT_PROTOCOL_VERSION, AgentLink, AgentToPipeline, OutOfTurnLimiter, PipelineToAgent,
    frame_violation,
};
use crate::agent::{AgentEvent, AgentSessionHandle};
use crate::realtime::{ToolError, ToolHandler, ToolOutcome, ToolRegistry};
use crate::transport::{MessageRole, PipelineMessage};

/// How long a remote tool invocation may wait for the agent's
/// `ToolResult` (Done or the Pending ack — long-running work is
/// expected to ack fast and deliver later via `CompleteTool`).
pub const DEFAULT_TOOL_ACK_TIMEOUT: Duration = Duration::from_secs(10);

/// How long after a Pending ack the agent's `CompleteTool` is still
/// applied. Later (or never issued) completions are dropped: the model
/// has moved on, and a stale "completed" would contradict what it was
/// told.
pub const DEFAULT_COMPLETION_TTL: Duration = Duration::from_mins(5);

/// Most calls awaiting a `CompleteTool` tracked at once.
const MAX_AWAITING_COMPLETION: usize = 64;

type PendingCalls = Arc<Mutex<HashMap<String, oneshot::Sender<(bool, String)>>>>;

/// Calls the agent acked as Pending, with the deadline by which their
/// `CompleteTool` is still accepted.
type AwaitingCompletion = Arc<Mutex<HashMap<String, Instant>>>;

/// Wires a per-session S2S tool backend: opens the handshake, pumps
/// agent frames, and materialises the agent's tools as a
/// [`ToolRegistry`].
pub struct AgentToolBackend {
    link: Arc<dyn AgentLink>,
    pending: PendingCalls,
    ack_timeout: Duration,
    /// `SessionStart` goes out from the spawned pump task; invocations
    /// wait on this gate so a fast first `ToolCall` cannot beat the
    /// handshake onto the wire (same gate as the classic gateway).
    started_rx: tokio::sync::watch::Receiver<bool>,
}

impl std::fmt::Debug for AgentToolBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentToolBackend")
            .field("ack_timeout", &self.ack_timeout)
            .finish_non_exhaustive()
    }
}

impl AgentToolBackend {
    /// Start the backend: send `SessionStart` and spawn the frame pump.
    pub fn start(link: Arc<dyn AgentLink>, handle: AgentSessionHandle) -> Self {
        Self::start_with_completion_ttl(link, handle, DEFAULT_COMPLETION_TTL)
    }

    /// [`start`](Self::start) with an explicit window for late
    /// `CompleteTool` frames.
    pub fn start_with_completion_ttl(
        link: Arc<dyn AgentLink>,
        handle: AgentSessionHandle,
        completion_ttl: Duration,
    ) -> Self {
        let pending: PendingCalls = Arc::new(Mutex::new(HashMap::new()));
        let awaiting: AwaitingCompletion = Arc::new(Mutex::new(HashMap::new()));
        let (started_tx, started_rx) = tokio::sync::watch::channel(false);
        let pump_link = Arc::clone(&link);
        let pump_pending = Arc::clone(&pending);
        let session_id = handle.session_id().to_string();
        tokio::spawn(async move {
            let started = pump_link
                .send(PipelineToAgent::SessionStart {
                    protocol_version: AGENT_PROTOCOL_VERSION,
                    session_id: session_id.clone(),
                    pipeline_name: "voice-pipes-s2s".to_string(),
                    resumed: false,
                })
                .await;
            // Open the gate either way — a dead link fails queued
            // invocations with a proper error instead of wedging them.
            let _ = started_tx.send(true);
            if let Err(e) = started {
                warn!(session_id = %session_id, error = %e, "agent SessionStart failed (tool backend)");
                return;
            }
            pump(
                pump_link,
                pump_pending,
                awaiting,
                completion_ttl,
                handle,
                session_id,
            )
            .await;
        });
        Self {
            link,
            pending,
            ack_timeout: DEFAULT_TOOL_ACK_TIMEOUT,
            started_rx,
        }
    }

    /// Override the per-invocation ack timeout.
    #[must_use]
    pub fn with_ack_timeout(mut self, timeout: Duration) -> Self {
        self.ack_timeout = timeout;
        self
    }

    /// List the agent's tools and build the registry the realtime
    /// runner hands to the provider.
    pub async fn registry(&self) -> Result<ToolRegistry, anyhow::Error> {
        let specs = self
            .link
            .list_tools()
            .await
            .map_err(|e| anyhow::anyhow!("agent ListTools failed: {e}"))?;
        let mut registry = ToolRegistry::new();
        for spec in specs {
            let schema = serde_json::from_str(&spec.parameters_json_schema)
                .unwrap_or_else(|_bad_schema| serde_json::json!({ "type": "object" }));
            registry.register(Arc::new(RemoteTool {
                name: spec.name,
                description: spec.description,
                schema,
                link: Arc::clone(&self.link),
                pending: Arc::clone(&self.pending),
                ack_timeout: self.ack_timeout,
                started_rx: self.started_rx.clone(),
            }));
        }
        info!(
            tools = registry.tool_specs().len(),
            "agent tool registry built"
        );
        Ok(registry)
    }
}

/// One agent-side tool, invoked over the link.
struct RemoteTool {
    name: String,
    description: String,
    schema: serde_json::Value,
    link: Arc<dyn AgentLink>,
    pending: PendingCalls,
    ack_timeout: Duration,
    started_rx: tokio::sync::watch::Receiver<bool>,
}

#[async_trait]
impl ToolHandler for RemoteTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.schema.clone()
    }

    async fn invoke(&self, arguments: &str) -> Result<ToolOutcome, ToolError> {
        // Hold until the SessionStart handshake has gone out.
        let mut started = self.started_rx.clone();
        let _ = started.wait_for(|s| *s).await;
        // Wire-level call id: correlates ToolCall → ToolResult on the
        // link (distinct from the provider's call_id, which the runner
        // keeps for the function output).
        let call_id = uuid::Uuid::new_v4().to_string();
        let (result_tx, result_rx) = oneshot::channel();
        self.pending.lock().insert(call_id.clone(), result_tx);

        let sent = self
            .link
            .send(PipelineToAgent::ToolCall {
                call_id: call_id.clone(),
                name: self.name.clone(),
                arguments_json: arguments.to_string(),
            })
            .await;
        if let Err(e) = sent {
            self.pending.lock().remove(&call_id);
            return Err(ToolError::new(format!("agent link down: {e}")));
        }

        match tokio::time::timeout(self.ack_timeout, result_rx).await {
            Ok(Ok((pending, output))) => {
                if pending {
                    Ok(ToolOutcome::Pending { ack: output })
                } else {
                    Ok(ToolOutcome::Done(output))
                }
            }
            Ok(Err(_link_closed)) => Err(ToolError::new("agent link closed during tool call")),
            Err(_elapsed) => {
                self.pending.lock().remove(&call_id);
                Err(ToolError::new(format!(
                    "agent tool '{}' did not ack within {:?}",
                    self.name, self.ack_timeout
                )))
            }
        }
    }
}

/// Route agent frames: `ToolResult` to its waiting invocation,
/// everything out-of-turn onto the session handle. Frames past the
/// text bounds or carrying a reserved message kind are dropped, `Say`
/// is rate limited, and `CompleteTool` is applied only for a call the
/// agent acked as Pending within the completion window.
async fn pump(
    link: Arc<dyn AgentLink>,
    pending: PendingCalls,
    awaiting: AwaitingCompletion,
    completion_ttl: Duration,
    handle: AgentSessionHandle,
    session_id: String,
) {
    let mut speech_limiter = OutOfTurnLimiter::new();
    while let Some(frame) = link.recv().await {
        if let Some(violation) = frame_violation(&frame) {
            warn!(
                session_id = %session_id,
                frame = ?std::mem::discriminant(&frame),
                %violation,
                "agent frame refused"
            );
            continue;
        }
        match frame {
            AgentToPipeline::SessionAck {
                protocol_version,
                agent_name,
            } => {
                info!(
                    session_id = %session_id,
                    agent = %agent_name,
                    protocol_version,
                    "agent tool backend established"
                );
            }
            AgentToPipeline::ToolResult {
                call_id,
                pending: is_pending,
                output,
            } => match self_remove(&pending, &call_id) {
                Some(tx) => {
                    if is_pending {
                        expect_completion(&awaiting, call_id, completion_ttl);
                    }
                    let _ = tx.send((is_pending, output));
                }
                None => {
                    warn!(
                        session_id = %session_id,
                        call_id = %call_id,
                        "ToolResult for unknown/expired call -- dropped"
                    );
                }
            },
            AgentToPipeline::Say { text, scheduling } => {
                if !speech_limiter.allow() {
                    warn!(session_id = %session_id, "agent Say rate limit exceeded; dropped");
                    continue;
                }
                let _ = handle.send(AgentEvent::Say { text, scheduling }).await;
            }
            AgentToPipeline::Message {
                kind,
                text,
                payload_json,
                record_in_history,
            } => {
                let payload = payload_json
                    .as_deref()
                    .and_then(|p| serde_json::from_str(p).ok());
                let mut message =
                    PipelineMessage::text(text.unwrap_or_default(), MessageRole::Assistant);
                message.kind = kind;
                message.payload = payload;
                let _ = handle
                    .send(AgentEvent::Message {
                        message,
                        record_in_history,
                    })
                    .await;
            }
            AgentToPipeline::Cancel => {
                let _ = handle.send(AgentEvent::Cancel).await;
            }
            AgentToPipeline::CompleteTool {
                call_id,
                result,
                scheduling,
            } => {
                if !take_awaited(&awaiting, &call_id) {
                    warn!(
                        session_id = %session_id,
                        call_id = %call_id,
                        "CompleteTool for a call that is unknown, already completed or past its window -- dropped"
                    );
                    continue;
                }
                let _ = handle
                    .send(AgentEvent::CompleteTool {
                        call_id,
                        result,
                        scheduling,
                    })
                    .await;
            }
            AgentToPipeline::TurnFragment { turn_id, .. } => {
                warn!(
                    session_id = %session_id,
                    turn_id,
                    "TurnFragment on an S2S tool-backend link -- dropped (the provider is the brain)"
                );
            }
        }
    }
    info!(session_id = %session_id, "agent tool-backend link closed");
}

fn self_remove(pending: &PendingCalls, call_id: &str) -> Option<oneshot::Sender<(bool, String)>> {
    pending.lock().remove(call_id)
}

/// Record that `call_id` was acked as Pending and its `CompleteTool`
/// is accepted until `ttl` from now. Expired entries are swept on the
/// way in, and the oldest deadline gives way past the cap.
fn expect_completion(awaiting: &AwaitingCompletion, call_id: String, ttl: Duration) {
    let now = Instant::now();
    let mut map = awaiting.lock();
    map.retain(|_, deadline| *deadline > now);
    if map.len() >= MAX_AWAITING_COMPLETION
        && let Some(oldest) = map
            .iter()
            .min_by_key(|(_, deadline)| **deadline)
            .map(|(id, _)| id.clone())
    {
        map.remove(&oldest);
    }
    map.insert(call_id, now + ttl);
}

/// Consume the completion slot for `call_id`; false when the call was
/// never acked as Pending, was already completed, or its window passed.
fn take_awaited(awaiting: &AwaitingCompletion, call_id: &str) -> bool {
    awaiting
        .lock()
        .remove(call_id)
        .is_some_and(|deadline| deadline > Instant::now())
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests/tool_backend.rs"]
mod tests;
