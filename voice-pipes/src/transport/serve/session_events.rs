//! A session's own pipeline-event channel, and the task that writes what is
//! sent on it into that session's capture.
//!
//! Events carry an utterance id and no session id, so a recorder fed from one
//! broadcast channel shared by the whole worker would fail in two silent ways:
//!
//!  - **Sessions would record each other.** A capture would hold other
//!    sessions' `turn_metrics_ready` rows under its own utterance ids. The
//!    report keeps the richest snapshot per id and breaks ties on the larger
//!    `total_ms`, so a turn's numbers could be replaced by a slower turn from
//!    a different session.
//!  - **The task would never end.** It loops until the channel closes, and a
//!    worker-wide channel closes only when the worker exits. So it would
//!    outlive its session and go on appending every later session's events to
//!    a finished capture -- a stored session's report would change as others
//!    ran.
//!
//! A channel per session removes both at the root: nothing else can arrive on
//! it, and it closes when the session's last sender is dropped, which ends the
//! task.

use tokio::sync::broadcast;

use crate::audit::{AuditEvent, StreamingAuditSink};
use crate::pipeline::events::PipelineEvent;

/// Events buffered for a slow reader before the oldest are dropped. A turn
/// produces a handful, so this is many turns of slack.
const CAPACITY: usize = 256;

/// Create this session's event channel, with the task that records what is
/// sent on it and passes a copy up to the worker's observers.
///
/// The returned sender is the session's only source of pipeline events:
/// everything the session emits goes through it and is recorded exactly once.
/// Do not also record an event by hand where it is sent -- it would then be
/// written twice.
///
/// `observers` is the worker-wide channel. It is written to and never read
/// from here: it exists for something watching the worker as a whole, and
/// because events carry no session id, anything that *recorded* from it would
/// be recording every session at once.
pub(super) fn session_event_channel(
    audit: Option<&StreamingAuditSink>,
    observers: &broadcast::Sender<PipelineEvent>,
) -> broadcast::Sender<PipelineEvent> {
    let (tx, rx) = broadcast::channel(CAPACITY);
    tokio::spawn(record_session_events(rx, audit.cloned(), observers.clone()));
    tx
}

/// Record and relay each event until the session's senders are gone.
async fn record_session_events(
    mut rx: broadcast::Receiver<PipelineEvent>,
    audit: Option<StreamingAuditSink>,
    observers: broadcast::Sender<PipelineEvent>,
) {
    loop {
        match rx.recv().await {
            Ok(event) => {
                // Stamped here, as close to the event as this task gets; the
                // line is written later, by the sink's own task.
                let at = std::time::SystemTime::now();
                // An error here only means nobody is observing.
                let _ = observers.send(event.clone());
                if let Some(audit) = &audit {
                    audit.record(AuditEvent::Pipeline(event, at));
                }
            }
            // Fell behind and lost some. Say so and carry on: stopping here --
            // which a plain `while let Ok(..)` does -- would silently drop
            // every event for the rest of the session over one slow moment.
            Err(broadcast::error::RecvError::Lagged(missed)) => {
                tracing::warn!(
                    missed,
                    "session event recorder fell behind; events were dropped"
                );
            }
            // Every sender is gone: the session is over.
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn interrupted(utterance_id: u32) -> PipelineEvent {
        PipelineEvent::Interrupted {
            utterance_id,
            trigger: crate::pipeline::events::InterruptionTrigger::ServerVad,
            played_ms: None,
        }
    }

    /// The fault this module exists for: events carry no session id, so on a
    /// shared channel one session's recorder saw another's turns.
    #[tokio::test]
    async fn one_session_never_sees_another_sessions_events() {
        let observers = broadcast::channel(CAPACITY).0;
        let a = session_event_channel(None, &observers);
        let b = session_event_channel(None, &observers);
        let mut a_rx = a.subscribe();
        let mut b_rx = b.subscribe();

        a.send(interrupted(1)).unwrap();
        b.send(interrupted(2)).unwrap();

        assert!(matches!(
            a_rx.try_recv(),
            Ok(PipelineEvent::Interrupted {
                utterance_id: 1,
                ..
            })
        ));
        assert!(
            a_rx.try_recv().is_err(),
            "session A received an event that session B sent"
        );
        assert!(matches!(
            b_rx.try_recv(),
            Ok(PipelineEvent::Interrupted {
                utterance_id: 2,
                ..
            })
        ));
        assert!(b_rx.try_recv().is_err());
    }

    /// The recorder must end with its session. On the shared channel it never
    /// did, and kept appending later sessions' events to a finished capture.
    #[tokio::test]
    async fn the_recorder_stops_when_the_session_does() {
        let dir = std::env::temp_dir().join(format!("vp-sess-ev-{}", uuid::Uuid::new_v4()));
        let store: std::sync::Arc<dyn crate::audit::store::AuditStore> =
            std::sync::Arc::new(crate::audit::store::LocalFsAuditStore::new(dir.clone()));
        let sink = StreamingAuditSink::spawn_with_budget(store, "s".into(), 64, 0);

        let (tx, rx) = broadcast::channel(CAPACITY);
        let observers = broadcast::channel(CAPACITY).0;
        let task = tokio::spawn(record_session_events(rx, Some(sink), observers));
        tx.send(interrupted(1)).unwrap();
        drop(tx); // the session ends: its last sender goes away

        tokio::time::timeout(std::time::Duration::from_secs(5), task)
            .await
            .expect("the recorder outlived its session")
            .expect("recorder task panicked");
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Something watching the whole worker still sees every session, which is
    /// what the worker-wide channel is for -- it is only recording from it
    /// that was wrong.
    #[tokio::test]
    async fn observers_of_the_worker_still_see_each_session() {
        let observers = broadcast::channel(CAPACITY).0;
        let mut watching = observers.subscribe();
        let a = session_event_channel(None, &observers);
        let b = session_event_channel(None, &observers);
        a.send(interrupted(1)).unwrap();
        b.send(interrupted(2)).unwrap();

        let mut seen = Vec::new();
        for _ in 0..2 {
            let ev = tokio::time::timeout(std::time::Duration::from_secs(5), watching.recv())
                .await
                .expect("an event was not relayed")
                .unwrap();
            if let PipelineEvent::Interrupted { utterance_id, .. } = ev {
                seen.push(utterance_id);
            }
        }
        seen.sort_unstable();
        assert_eq!(seen, vec![1, 2]);
    }
}
