use std::sync::Arc;

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::text::{self, TextFragment, TextProcessor};

use super::types::Utterance;

/// LLM task: receives transcribed utterances, runs the text processor
/// chain in streaming mode, and forwards sentence fragments.
///
/// Checks `cancel` before each utterance and passes it into the
/// processing chain so token generation can be interrupted.
pub async fn llm_task(
    mut text_rx: mpsc::Receiver<Utterance>,
    frag_tx: mpsc::Sender<TextFragment>,
    processors: Arc<Vec<Box<dyn TextProcessor>>>,
    cancel: CancellationToken,
) {
    loop {
        let utterance = tokio::select! {
            biased;
            () = cancel.cancelled() => {
                tracing::info!("LLM task cancelled");
                return;
            }
            utt = text_rx.recv() => match utt {
                Some(u) => u,
                None => return,
            },
        };

        tracing::info!(
            utterance_id = utterance.utterance_id,
            chars = utterance.text.chars().count(),
            "processing utterance"
        );

        if let Err(e) = text::run_chain_stream(
            &processors,
            &utterance.text,
            frag_tx.clone(),
            cancel.clone(),
        )
        .await
        {
            tracing::error!(error = %e, "text processing failed");
        }
    }
}
