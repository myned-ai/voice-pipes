//! Control-message handlers shared verbatim between the classic and
//! realtime session runners.
//!
//! Centralizing them removes drift risk: a fix or protocol change applies
//! once instead of twice. Only the genuinely identical logic lives here;
//! runner-specific side effects (e.g. the classic tracker recording its own
//! client-timing history) stay at the call site.

use tokio::sync::{broadcast, mpsc};

use crate::audit::{AuditEvent, StreamingAuditSink, TraceEvent};
use crate::pipeline::events::PipelineEvent;
use crate::transport::{ServerControl, ServerMessage};

/// Reply to a client `Ping` with a `Pong` carrying the echoed client
/// timestamp and the current server wall-clock time (unix epoch millis).
pub(crate) async fn handle_ping(out_tx: &mpsc::Sender<ServerMessage>, client_timestamp_ms: u64) {
    let server_timestamp_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let _ = out_tx
        .send(ServerMessage::Control(ServerControl::Pong {
            client_timestamp_ms,
            server_timestamp_ms,
        }))
        .await;
}

/// Emit the common observability for a client timing report: a debug log,
/// the [`PipelineEvent::ClientTiming`] broadcast, and the streaming-audit
/// trace.
///
/// This is the portion that is identical across both runners. The classic
/// runner additionally records the timing into its `TurnTracker`; that
/// runner-specific step remains at the call site.
pub(crate) fn emit_client_timing(
    session_id: &str,
    pe_tx: &broadcast::Sender<PipelineEvent>,
    streaming_audit: &Option<StreamingAuditSink>,
    utterance_id: u32,
    event_name: &str,
    client_timestamp_ms: u64,
) {
    tracing::debug!(
        session_id = %session_id,
        utterance_id,
        event = %event_name,
        client_ts = client_timestamp_ms,
        "received timing report from client"
    );
    let _ = pe_tx.send(PipelineEvent::ClientTiming {
        utterance_id,
        timing_event: event_name.to_string(),
        client_timestamp_ms,
    });
    if let Some(audit) = streaming_audit {
        audit.record(AuditEvent::Trace(TraceEvent::new(
            "client",
            "client_timing",
            serde_json::json!({
                "utterance_id": utterance_id,
                "event": event_name,
                "client_timestamp_ms": client_timestamp_ms,
            }),
        )));
    }
}
