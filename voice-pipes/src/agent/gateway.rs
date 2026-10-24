//! Agent gateway: a [`TextProcessor`] that fronts a remote agent over
//! an [`AgentLink`].
//!
//! The remote agent is the conversational brain; this processor is the
//! per-session bridge. Turn input goes out as `UserTurn`, the reply
//! streams back as `TurnFragment`s, and everything out-of-turn the
//! agent sends (`Say` / `Message` / `Cancel` / `CompleteTool`) is
//! routed onto the session's [`AgentSessionHandle`] by a pump task
//! spawned at [`bind_session`](TextProcessor::bind_session) time.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::agent::link::{
    AGENT_PROTOCOL_VERSION, AgentLink, AgentToPipeline, OutOfTurnLimiter, PipelineToAgent,
    frame_violation,
};
use crate::agent::{AgentEvent, AgentSessionHandle};
use crate::text::{TextFragment, TextProcessor, TextProcessorError};
use crate::transport::{MessageRole, PipelineMessage};

/// Default per-turn watchdog: how long the gateway waits for the agent
/// to finish a turn before failing it (a hung remote agent must not
/// freeze the turn forever — pre-existing gap made urgent by the
/// network hop).
pub const DEFAULT_TURN_TIMEOUT: Duration = Duration::from_secs(30);

struct ActiveTurn {
    turn_id: u32,
    tx: mpsc::Sender<TextFragment>,
    done_tx: Option<oneshot::Sender<()>>,
}

/// [`TextProcessor`] bridging one session to a remote agent.
pub struct AgentGateway {
    link: Arc<dyn AgentLink>,
    name: String,
    pipeline_name: String,
    turn_timeout: Duration,
    next_turn_id: AtomicU32,
    last_turn_id: AtomicU32,
    active_turn: Arc<Mutex<Option<ActiveTurn>>>,
    /// `SessionStart` is sent from the task spawned in `bind_session`;
    /// this gate keeps every other outbound frame behind it so a fast
    /// first turn can't beat the handshake onto the wire.
    bound: std::sync::atomic::AtomicBool,
    started_tx: tokio::sync::watch::Sender<bool>,
    started_rx: tokio::sync::watch::Receiver<bool>,
}

impl std::fmt::Debug for AgentGateway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentGateway")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl AgentGateway {
    pub fn new(link: Arc<dyn AgentLink>, pipeline_name: impl Into<String>) -> Self {
        let (started_tx, started_rx) = tokio::sync::watch::channel(false);
        Self {
            link,
            name: "agent".to_string(),
            pipeline_name: pipeline_name.into(),
            turn_timeout: DEFAULT_TURN_TIMEOUT,
            next_turn_id: AtomicU32::new(1),
            last_turn_id: AtomicU32::new(0),
            active_turn: Arc::new(Mutex::new(None)),
            bound: std::sync::atomic::AtomicBool::new(false),
            started_tx,
            started_rx,
        }
    }

    /// Wait until the `SessionStart` handshake has gone out (no-op when
    /// the gateway was never bound, e.g. direct unit usage).
    async fn ensure_started(&self) {
        if !self.bound.load(Ordering::Acquire) {
            return;
        }
        let mut rx = self.started_rx.clone();
        let _ = rx.wait_for(|started| *started).await;
    }

    /// Override the per-turn watchdog.
    #[must_use]
    pub fn with_turn_timeout(mut self, timeout: Duration) -> Self {
        self.turn_timeout = timeout;
        self
    }

    /// Run one turn through the link: send the `UserTurn`, then relay
    /// `TurnFragment`s (routed by the pump) until end-of-response,
    /// cancellation, or the watchdog fires.
    async fn run_turn(
        &self,
        text: &str,
        is_context: bool,
        tx: mpsc::Sender<TextFragment>,
        cancel: CancellationToken,
    ) -> Result<(), TextProcessorError> {
        self.ensure_started().await;
        let turn_id = self.next_turn_id.fetch_add(1, Ordering::Relaxed);
        self.last_turn_id.store(turn_id, Ordering::Relaxed);
        let (done_tx, done_rx) = oneshot::channel();
        *self.active_turn.lock() = Some(ActiveTurn {
            turn_id,
            tx,
            done_tx: Some(done_tx),
        });

        let sent = self
            .link
            .send(PipelineToAgent::UserTurn {
                turn_id,
                text: text.to_string(),
                is_context,
            })
            .await;
        if let Err(e) = sent {
            *self.active_turn.lock() = None;
            return Err(TextProcessorError::new(
                self.name.clone(),
                format!("agent link down: {e}"),
            )
            .caused_by(e));
        }

        tokio::select! {
            biased;
            () = cancel.cancelled() => {
                // Barge-in / teardown: stop relaying. The authoritative
                // played-text truncation reaches the agent later via
                // `truncate_history` → `Interrupted`.
                *self.active_turn.lock() = None;
                debug!(turn_id, "agent turn cancelled");
                Ok(())
            }
            done = done_rx => {
                match done {
                    Ok(()) => Ok(()),
                    // Pump dropped the sender without completing the
                    // turn — the link closed mid-turn.
                    Err(_link_closed) => {
                        *self.active_turn.lock() = None;
                        Err(TextProcessorError::new(self.name.clone(), "agent link closed mid-turn"))
                    }
                }
            }
            () = tokio::time::sleep(self.turn_timeout) => {
                *self.active_turn.lock() = None;
                Err(TextProcessorError::new(self.name.clone(), format!(
                        "agent turn watchdog fired after {:?} with no end-of-response",
                        self.turn_timeout
                    )))
            }
        }
    }
}

#[async_trait]
impl TextProcessor for AgentGateway {
    fn name(&self) -> &str {
        &self.name
    }

    async fn process(&self, input: &str) -> Result<String, TextProcessorError> {
        let (tx, mut rx) = mpsc::channel::<TextFragment>(32);
        let collector = tokio::spawn(async move {
            let mut acc = String::new();
            while let Some(frag) = rx.recv().await {
                acc.push_str(&frag.text);
                if frag.is_end_of_response {
                    break;
                }
            }
            acc
        });
        self.run_turn(input, false, tx, CancellationToken::new())
            .await?;
        Ok(collector.await.unwrap_or_default())
    }

    async fn process_stream(
        &self,
        input: &str,
        tx: mpsc::Sender<TextFragment>,
        cancel: CancellationToken,
    ) -> Result<(), TextProcessorError> {
        self.run_turn(input, false, tx, cancel).await
    }

    async fn process_context_stream(
        &self,
        context: &str,
        tx: mpsc::Sender<TextFragment>,
        cancel: CancellationToken,
    ) -> Result<(), TextProcessorError> {
        self.run_turn(context, true, tx, cancel).await
    }

    fn bind_session(&self, session: voice_pipes_sdk::agent::SessionHandle) {
        // The gateway drives the session (speech, messages, cancels), which
        // takes the engine's own handle, not just the processor's view.
        let Some(handle) = AgentSessionHandle::from_session(&session) else {
            warn!("agent gateway bound to a session the engine did not build; it cannot drive it");
            return;
        };
        // The agent phrases idle re-engagement itself.
        handle.register_idle_handler(Arc::new(GatewayIdle {
            link: Arc::clone(&self.link),
            started_rx: self.started_rx.clone(),
        }));
        // Handshake, then pump agent frames for the session's lifetime.
        self.bound.store(true, Ordering::Release);
        let link = Arc::clone(&self.link);
        let active_turn = Arc::clone(&self.active_turn);
        let session_id = handle.session_id().to_string();
        let pipeline_name = self.pipeline_name.clone();
        let started_tx = self.started_tx.clone();
        tokio::spawn(async move {
            let started = link
                .send(PipelineToAgent::SessionStart {
                    protocol_version: AGENT_PROTOCOL_VERSION,
                    session_id: session_id.clone(),
                    pipeline_name,
                    resumed: false,
                })
                .await;
            // Open the gate either way — a dead link fails the queued
            // frames with a proper error instead of wedging them.
            let _ = started_tx.send(true);
            if let Err(e) = started {
                warn!(session_id = %session_id, error = %e, "agent SessionStart failed");
                return;
            }
            pump(link, active_turn, handle, session_id).await;
        });
    }

    fn truncate_history(&self, played_text: &str) -> Option<String> {
        // Ground truth for the agent's own history; the pipeline holds
        // no transcript here, so there is no original text to return.
        let turn_id = self.last_turn_id.load(Ordering::Relaxed);
        if turn_id == 0 {
            return None;
        }
        let link = Arc::clone(&self.link);
        let played = played_text.to_string();
        tokio::spawn(async move {
            let _ = link
                .send(PipelineToAgent::Interrupted {
                    turn_id,
                    played_text: played,
                })
                .await;
        });
        None
    }

    async fn append_assistant_message(&self, text: &str) {
        self.ensure_started().await;
        let _ = self
            .link
            .send(PipelineToAgent::ContextNote {
                text: text.to_string(),
                kind: "assistant_message".to_string(),
            })
            .await;
    }

    async fn inject_context(&self, text: &str) {
        self.ensure_started().await;
        let _ = self
            .link
            .send(PipelineToAgent::ContextNote {
                text: text.to_string(),
                kind: "context".to_string(),
            })
            .await;
    }
}

/// The gateway's answer to an idle user: the agent decides the wording
/// (Retell's `reminder_required` precedent), so the prompt is claimed
/// whenever the link takes it. Registered on bind, so the handshake is
/// already under way; the frame waits for it like every other.
struct GatewayIdle {
    link: Arc<dyn AgentLink>,
    started_rx: tokio::sync::watch::Receiver<bool>,
}

#[async_trait]
impl crate::agent::UserIdleHandler for GatewayIdle {
    async fn on_user_idle(&self, idle_ms: u64, prompt_count: u32) -> bool {
        let mut rx = self.started_rx.clone();
        let _ = rx.wait_for(|started| *started).await;
        self.link
            .send(PipelineToAgent::UserIdle {
                idle_ms,
                prompt_count,
            })
            .await
            .is_ok()
    }
}

impl Drop for AgentGateway {
    fn drop(&mut self) {
        self.link.close("session ended");
    }
}

/// Route agent frames until the link closes: turn fragments into the
/// active turn, everything out-of-turn onto the session handle. Frames
/// past the text bounds or carrying a reserved message kind are
/// dropped; out-of-turn speech is rate limited.
#[expect(
    clippy::too_many_lines,
    reason = "single exhaustive dispatcher over the AgentToPipeline \
              frames; splitting per-variant helpers would scatter the \
              routing policy"
)]
async fn pump(
    link: Arc<dyn AgentLink>,
    active_turn: Arc<Mutex<Option<ActiveTurn>>>,
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
                    "agent session established"
                );
                if protocol_version != AGENT_PROTOCOL_VERSION {
                    warn!(
                        session_id = %session_id,
                        agent_version = protocol_version,
                        pipeline_version = AGENT_PROTOCOL_VERSION,
                        "agent protocol version mismatch — continuing best-effort"
                    );
                }
            }
            AgentToPipeline::TurnFragment {
                turn_id,
                text,
                is_end_of_response,
                first_token_ms,
                token_count,
            } => {
                // Extract what we need under the lock, never await
                // while holding it.
                let target = {
                    let mut active = active_turn.lock();
                    match active.as_mut() {
                        Some(turn) if turn.turn_id == turn_id => {
                            let tx = turn.tx.clone();
                            let done = if is_end_of_response {
                                let done = turn.done_tx.take();
                                *active = None;
                                done
                            } else {
                                None
                            };
                            Some((tx, done))
                        }
                        // Stale fragment for a cancelled/finished turn
                        // (mirror of the stale-playback guards).
                        _ => None,
                    }
                };
                match target {
                    Some((tx, done)) => {
                        let mut tf =
                            TextFragment::new(text).with_is_end_of_response(is_end_of_response);
                        tf.first_token_ms = first_token_ms;
                        tf.token_count = token_count;
                        let _ = tx.send(tf).await;
                        if let Some(done) = done {
                            let _ = done.send(());
                        }
                    }
                    None => {
                        debug!(session_id = %session_id, turn_id, "dropping stale agent turn fragment");
                    }
                }
            }
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
                // The classic pipeline issues no tool calls, so a
                // completion here is agent-initiated speech like `Say`
                // and shares its budget.
                if !speech_limiter.allow() {
                    warn!(
                        session_id = %session_id,
                        call_id = %call_id,
                        "agent CompleteTool rate limit exceeded; dropped"
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
            AgentToPipeline::ToolResult { call_id, .. } => {
                // Tool results are owned by the S2S tool registry's own
                // link session; on a classic gateway they are a protocol
                // misuse.
                warn!(
                    session_id = %session_id,
                    call_id = %call_id,
                    "unexpected ToolResult on classic agent link -- dropped"
                );
            }
        }
    }
    // Link closed: fail any in-flight turn instead of hanging until
    // the watchdog (dropping done_tx wakes run_turn with an error).
    if let Some(turn) = active_turn.lock().take() {
        warn!(
            session_id = %session_id,
            turn_id = turn.turn_id,
            "agent link closed with a turn in flight"
        );
    }
    info!(session_id = %session_id, "agent link closed");
}

// ── Tests ───────────────────────────────────────────────────

#[cfg(test)]
#[path = "tests/gateway.rs"]
mod tests;
