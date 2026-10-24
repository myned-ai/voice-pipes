use super::{MAX_RAPID_PROVIDER_CLOSES, MAX_SESSION_RECONNECTS, reconnect_backoff};
use std::time::Duration;

// The lifetime cap sits above the rapid-close cap: a fast storm ends the
// session first, a slow drip of closes ends it eventually.
const _: () = assert!(MAX_SESSION_RECONNECTS > MAX_RAPID_PROVIDER_CLOSES);

/// The wait before a reconnect doubles with each close in the current
/// rapid streak and stops growing at 16 s, so a recovering provider is
/// retried within seconds while a flapping one is not hammered.
#[test]
fn reconnect_backoff_doubles_and_caps() {
    assert_eq!(reconnect_backoff(0), Duration::from_secs(1));
    assert_eq!(reconnect_backoff(1), Duration::from_secs(1));
    assert_eq!(reconnect_backoff(2), Duration::from_secs(2));
    assert_eq!(reconnect_backoff(3), Duration::from_secs(4));
    assert_eq!(reconnect_backoff(5), Duration::from_secs(16));
    assert_eq!(reconnect_backoff(40), Duration::from_secs(16));
}

mod while_reconnecting {
    use super::super::while_serving_client;
    use crate::transport::{ClientControl, ClientMessage, ServerControl, ServerMessage};
    use std::time::Duration;
    use tokio::sync::{mpsc, oneshot};
    use tokio_util::sync::CancellationToken;

    fn mic_chunk() -> ClientMessage {
        ClientMessage::Audio(crate::pipeline::AudioChunk {
            samples: vec![0; 320],
            sample_rate: 16_000,
            channels: 1,
            is_end_of_utterance: false,
            caption: None,
            utterance_id: 0,
            full_text: None,
            extras: None,
            last_speech_at: None,
            synth_duration: None,
        })
    }

    /// Pings are answered and audio is taken off the channel while the
    /// reconnect runs; its result is returned when it ends.
    #[tokio::test]
    async fn pings_are_answered_while_the_work_runs() {
        let (in_tx, mut in_rx) = mpsc::channel(8);
        let (out_tx, mut out_rx) = mpsc::channel(8);
        let (done_tx, done_rx) = oneshot::channel::<u32>();
        let shutdown = CancellationToken::new();
        let serving = tokio::spawn(async move {
            while_serving_client(&mut in_rx, &out_tx, &shutdown, async {
                done_rx.await.unwrap()
            })
            .await
        });

        for _ in 0..20 {
            in_tx.send(mic_chunk()).await.unwrap();
        }
        in_tx
            .send(ClientMessage::Control(ClientControl::Ping {
                timestamp_ms: 7,
            }))
            .await
            .unwrap();
        let pong = tokio::time::timeout(Duration::from_secs(2), out_rx.recv())
            .await
            .expect("the ping waited for the reconnect")
            .expect("a frame");
        assert!(
            matches!(
                pong,
                ServerMessage::Control(ServerControl::Pong {
                    client_timestamp_ms: 7,
                    ..
                })
            ),
            "{pong:?}"
        );

        done_tx.send(5).unwrap();
        assert_eq!(serving.await.unwrap(), Some(5));
    }

    /// A client that leaves, or a worker that stops, ends the wait.
    #[tokio::test]
    async fn the_wait_ends_when_the_client_leaves_or_the_worker_stops() {
        let (in_tx, mut in_rx) = mpsc::channel::<ClientMessage>(8);
        let (out_tx, _out_rx) = mpsc::channel(8);
        drop(in_tx);
        let out = while_serving_client(
            &mut in_rx,
            &out_tx,
            &CancellationToken::new(),
            std::future::pending::<()>(),
        )
        .await;
        assert_eq!(out, None);

        let (_in_tx, mut in_rx) = mpsc::channel::<ClientMessage>(8);
        let shutdown = CancellationToken::new();
        shutdown.cancel();
        let out =
            while_serving_client(&mut in_rx, &out_tx, &shutdown, std::future::pending::<()>())
                .await;
        assert_eq!(out, None);
    }
}
