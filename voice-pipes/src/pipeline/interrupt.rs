use tokio::sync::mpsc;

use super::types::AudioChunk;

/// Forwards audio chunks from a single receiver to two senders.
///
/// Used to split mic audio to both the STT task and the interrupt monitor.
/// Exits when the input closes or both outputs are dropped.
pub async fn mic_audio_splitter(
    mut input: mpsc::Receiver<AudioChunk>,
    stt_tx: mpsc::Sender<AudioChunk>,
    monitor_tx: mpsc::Sender<AudioChunk>,
) {
    while let Some(chunk) = input.recv().await {
        let stt_ok = stt_tx.send(chunk.clone()).await.is_ok();
        let mon_ok = monitor_tx.send(chunk).await.is_ok();
        if !stt_ok && !mon_ok {
            return;
        }
    }
}
