//! Audit sink -- writes completed pipeline records via an [`AuditStore`].

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::audio::AudioFrame;
use crate::audio::wav_writer;

use super::store::AuditStore;

/// A record of one TTS invocation for audit purposes.
#[derive(Debug)]
pub struct AuditRecord {
    pub timestamp: SystemTime,
    pub request_id: String,
    pub input_text: String,
    pub audio: AudioFrame,
    pub provider: String,
    pub latency: Duration,
}

/// Non-blocking audit sink. Sends records to a background writer task.
/// If the channel is full, records are dropped with a warning -- never blocks the pipeline.
#[derive(Debug, Clone)]
pub struct AuditSink {
    tx: mpsc::Sender<AuditRecord>,
}

impl AuditSink {
    /// Spawn the audit background writer. Returns the sink handle.
    pub fn spawn(store: Arc<dyn AuditStore>, buffer_size: usize) -> Self {
        let (tx, rx) = mpsc::channel(buffer_size);

        tokio::spawn(audit_writer_task(rx, store));

        Self { tx }
    }

    /// Try to send an audit record. Never blocks -- drops if channel is full.
    pub fn record(&self, record: AuditRecord) {
        if self.tx.try_send(record).is_err() {
            warn!("audit channel full, dropping record");
        }
    }
}

/// Background task that drains the audit channel and writes via the
/// store. Gives up after repeated consecutive failures and discards the
/// rest instead of logging every record.
async fn audit_writer_task(mut rx: mpsc::Receiver<AuditRecord>, store: Arc<dyn AuditStore>) {
    let mut health = super::streaming::WriterHealth::default();
    while let Some(record) = rx.recv().await {
        if health.disabled() {
            continue;
        }
        let result = write_audit_record(&record, &*store).await;
        health.observe("audit write", &result);
    }

    info!("audit writer shutting down");
}

/// Write one audit record (text file + WAV file) via the store.
async fn write_audit_record(
    record: &AuditRecord,
    store: &dyn AuditStore,
) -> Result<(), anyhow::Error> {
    let ts = record
        .timestamp
        .duration_since(SystemTime::UNIX_EPOCH)?
        .as_millis();
    let prefix = format!("{ts}_{}", record.request_id);

    // Write input text
    let text = format!(
        "provider: {}\nlatency_ms: {}\ntext: {}",
        record.provider,
        record.latency.as_millis(),
        record.input_text
    );
    store.put(&format!("{prefix}.txt"), text.as_bytes()).await?;

    // Write output audio
    let wav_bytes = wav_writer::encode_wav(&record.audio)?;
    store.put(&format!("{prefix}.wav"), &wav_bytes).await?;

    Ok(())
}

#[cfg(test)]
#[path = "tests/sink.rs"]
mod tests;
