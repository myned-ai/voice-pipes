//! Batch (file-based) commands: TTS, STT, and the full Pipe round-trip.

use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Instant, SystemTime};

use tracing::{info, instrument};

use crate::audit::{AuditSink, AuditTap};
use crate::config::PipelineConfig;
use crate::{audio, audit, config, text};

use super::{AUDIT_FLUSH_DELAY, REQUEST_COUNTER};

// ── TTS: text -> WAV ─────────────────────────────────────────

#[instrument(skip_all, fields(text_len = text.len()))]
pub async fn run_tts(
    text: &str,
    output: &Path,
    cfg: &PipelineConfig,
    audit_sink: Option<&AuditSink>,
) -> Result<(), anyhow::Error> {
    let provider = config::build_tts_provider(cfg, None, None)?;
    info!(provider = provider.name(), "loading TTS provider");

    let start = Instant::now();
    info!(chars = text.chars().count(), "synthesizing");
    let frame = provider.synthesize(text).await?;
    let latency = start.elapsed();

    let duration_s = frame.samples.len() as f32 / frame.sample_rate as f32;
    let rtf = duration_s / latency.as_secs_f32();
    info!(
        latency_ms = latency.as_millis() as u64,
        audio_duration_s = format!("{duration_s:.2}"),
        realtime_factor = format!("{rtf:.2}"),
        "synthesis complete"
    );

    audio::wav_writer::write_wav(&frame, output)?;
    info!(path = %output.display(), "WAV written");

    if let Some(sink) = audit_sink {
        sink.record(audit::AuditRecord {
            timestamp: SystemTime::now(),
            request_id: format!("req-{}", REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed)),
            input_text: text.to_string(),
            audio: frame,
            provider: provider.name().to_string(),
            latency,
        });
        tokio::time::sleep(AUDIT_FLUSH_DELAY).await;
    }

    Ok(())
}

// ── STT: WAV -> text ─────────────────────────────────────────

#[instrument(skip_all, fields(input = %input.display()))]
pub async fn run_stt(input: &Path, cfg: &PipelineConfig) -> Result<(), anyhow::Error> {
    let provider = config::build_stt_provider(cfg, None)?;
    info!(provider = provider.name(), "loading STT provider");

    info!(path = %input.display(), "reading WAV");
    let audio = audio::wav_reader::read_wav(input)?;

    // Resample to the provider's expected rate (e.g. 16kHz for AssemblyAI)
    let audio = audio::resample::resample(audio, provider.expected_sample_rate())?;

    let start = Instant::now();
    info!(
        samples = audio.samples.len(),
        sample_rate = audio.sample_rate,
        "transcribing"
    );
    let result = provider.transcribe(&audio).await?;
    let latency = start.elapsed();

    let audio_duration_s = audio.samples.len() as f32 / audio.sample_rate as f32;
    info!(
        chars = result.text.chars().count(),
        latency_ms = latency.as_millis() as u64,
        audio_duration_s = format!("{audio_duration_s:.2}"),
        "transcription complete"
    );

    println!("{}", result.text);
    Ok(())
}

// ── Pipe: WAV -> STT -> TTS -> WAV ────────────────────────────

#[instrument(skip_all, fields(input = %input.display()))]
pub async fn run_pipe(
    input: &Path,
    output: &Path,
    cfg: &PipelineConfig,
    audit_sink: Option<&AuditSink>,
) -> Result<(), anyhow::Error> {
    // Step 1: Load providers via config registry
    let stt_provider = config::build_stt_provider(cfg, None)?;
    let tts_provider = config::build_tts_provider(cfg, None, None)?;
    let text_processors = config::build_text_processors(cfg, None, None)?;
    info!(
        stt = stt_provider.name(),
        tts = tts_provider.name(),
        text_processors = text_processors.len(),
        "loading providers"
    );

    // Step 2: Read input WAV
    info!(path = %input.display(), "reading WAV");
    let audio = audio::wav_reader::read_wav(input)?;

    // Step 3: STT -- audio -> text (resample input to STT's expected rate)
    let stt_start = Instant::now();
    let audit_tap_side = cfg.tap_side();
    let audit_tap_store: std::sync::Arc<dyn crate::audit::store::AuditStore> = std::sync::Arc::new(
        crate::audit::store::LocalFsAuditStore::new(cfg.tap_dir().to_path_buf()),
    );
    let audio_for_stt = if let Some(side) = audit_tap_side {
        let tap = AuditTap::spawn(std::sync::Arc::clone(&audit_tap_store), "stt_in", side);
        tap.resample(audio, stt_provider.expected_sample_rate())?
    } else {
        audio::resample::resample(audio, stt_provider.expected_sample_rate())?
    };
    let audio_duration_s = audio_for_stt.samples.len() as f32 / audio_for_stt.sample_rate as f32;
    info!(
        samples = audio_for_stt.samples.len(),
        sample_rate = audio_for_stt.sample_rate,
        audio_duration_s = format!("{audio_duration_s:.2}"),
        "transcribing"
    );
    let transcription = stt_provider.transcribe(&audio_for_stt).await?;
    let stt_latency = stt_start.elapsed();
    info!(
        chars = transcription.text.chars().count(),
        stt_latency_ms = stt_latency.as_millis() as u64,
        "transcription complete"
    );

    // Step 3.5: Text processing chain (STT output -> processed text)
    let processed_text = if text_processors.is_empty() {
        transcription.text.clone()
    } else {
        info!(
            chain_len = text_processors.len(),
            "running text processor chain"
        );
        text::run_chain(&text_processors, &transcription.text).await?
    };

    // Step 4: TTS -- text -> audio
    let tts_start = Instant::now();
    info!(chars = processed_text.chars().count(), "synthesizing");
    let raw_frame = tts_provider.synthesize(&processed_text).await?;
    let tts_latency = tts_start.elapsed();

    // Optional resample of TTS output (with audit tap when enabled)
    let target_rate = cfg.output_rate().unwrap_or(raw_frame.sample_rate);
    let frame = if let Some(side) = audit_tap_side {
        let tap = AuditTap::spawn(std::sync::Arc::clone(&audit_tap_store), "tts_out", side);
        tap.resample(raw_frame, target_rate)?
    } else {
        audio::resample::resample(raw_frame, target_rate)?
    };

    let out_duration_s = frame.samples.len() as f32 / frame.sample_rate as f32;
    info!(
        tts_latency_ms = tts_latency.as_millis() as u64,
        output_duration_s = format!("{out_duration_s:.2}"),
        total_latency_ms = (stt_latency + tts_latency).as_millis() as u64,
        "pipeline complete"
    );

    // Step 5: Write output WAV
    audio::wav_writer::write_wav(&frame, output)?;
    info!(path = %output.display(), "WAV written");

    if let Some(sink) = audit_sink {
        sink.record(audit::AuditRecord {
            timestamp: SystemTime::now(),
            request_id: format!("req-{}", REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed)),
            input_text: processed_text,
            audio: frame,
            provider: format!("{}->{}", stt_provider.name(), tts_provider.name()),
            latency: stt_latency + tts_latency,
        });
        tokio::time::sleep(AUDIT_FLUSH_DELAY).await;
    }

    Ok(())
}
