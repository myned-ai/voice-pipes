use tokio::sync::mpsc;

use super::types::AudioChunk;

/// VAD filter task: receives raw audio from the transport layer,
/// runs Voice Activity Detection to segment continuous audio into
/// utterances, and forwards only speech chunks (with `is_end_of_utterance`
/// sentinels) to the downstream STT task.
///
/// Without this filter, the STT model receives an unbounded stream
/// with no utterance boundaries and quickly starts hallucinating.
///
/// Mirrors the VAD + onset-confirmation logic from `audio::io::run_mic_stream_loop`
/// but as an async tokio task suitable for the WebSocket transport path.
///
/// `speech_probe`, when supplied, is held `true` from confirmed speech
/// onset until the end-of-utterance sentinel. It lets observers (the
/// idle re-engagement countdown) see in-progress user speech *before*
/// the utterance commits as a turn, so the agent doesn't talk over the
/// user's first words.
/// `pre_roll_ms` is how much audio from before the onset is held back and
/// flushed once speech is confirmed. See `VadSettings::pre_roll_ms`: too
/// little of it removes the first syllable of every utterance.
fn vad_chunk(
    samples: Vec<i16>,
    sample_rate: u32,
    is_end_of_utterance: bool,
    last_speech_at: Option<std::time::Instant>,
) -> AudioChunk {
    AudioChunk {
        samples,
        sample_rate,
        channels: 1,
        is_end_of_utterance,
        caption: None,
        utterance_id: 0,
        full_text: None,
        extras: None,
        last_speech_at,
        synth_duration: None,
    }
}

pub async fn vad_filter_task(
    mut raw_rx: mpsc::Receiver<AudioChunk>,
    filtered_tx: mpsc::Sender<AudioChunk>,
    mut vad: Box<dyn crate::vad::segmenter::VadBackend>,
    silence_ms: u64,
    pre_roll_ms: u32,
    speech_probe: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
) {
    use std::collections::VecDeque;
    use std::sync::atomic::Ordering;

    const CHUNK_MS: u32 = 20;
    const MIN_ONSET_CHUNKS: usize = 3;

    // At least one chunk: a zero would drop the audio the onset confirmation
    // has not yet accounted for.
    let pre_roll_chunks = ((pre_roll_ms as usize).div_ceil(CHUNK_MS as usize)).max(1);

    let set_probe = |active: bool| {
        if let Some(p) = &speech_probe {
            p.store(active, Ordering::Release);
        }
    };

    let mut sample_rate: u32 = 0;
    let mut chunk_samples: usize = 0;
    let mut silence_limit: usize = 0;

    let mut accumulator: Vec<i16> = Vec::with_capacity(4096);
    let mut speech_active = false;
    let mut silent_samples: usize = 0;
    let mut pre_roll: VecDeque<Vec<i16>> = VecDeque::with_capacity(pre_roll_chunks + 1);
    let mut onset_buf: VecDeque<Vec<i16>> = VecDeque::new();
    let mut onset_speech_count: usize = 0;
    let mut utterance_seq: u64 = 0;
    let mut forwarded_speech_chunks: u64 = 0;
    let mut forwarded_non_speech_chunks: u64 = 0;
    let mut last_speech_at: Option<std::time::Instant> = None;

    while let Some(chunk) = raw_rx.recv().await {
        // Initialise rate-dependent constants from the first chunk, and
        // re-initialise if the rate changes mid-session: a device switch or
        // an adaptive-quality renegotiation would otherwise leave the 20 ms
        // framing, the silence budget and the VAD's own resampler on the old
        // rate, and stamp the old rate onto chunks bound for STT.
        if chunk.sample_rate > 0 && chunk.sample_rate != sample_rate {
            if sample_rate != 0 {
                tracing::warn!(
                    old_rate = sample_rate,
                    new_rate = chunk.sample_rate,
                    "VAD filter: input sample rate changed mid-session; re-initialising"
                );
                accumulator.clear();
                pre_roll.clear();
                onset_buf.clear();
                onset_speech_count = 0;
                vad.reset();
            }
            sample_rate = chunk.sample_rate;
            chunk_samples = (sample_rate as u64 * CHUNK_MS as u64 / 1000) as usize;
            silence_limit = (silence_ms as f64 / 1000.0 * sample_rate as f64) as usize;
            tracing::info!(
                sample_rate,
                chunk_samples,
                silence_limit,
                vad = vad.name(),
                "VAD filter initialised"
            );
        }

        let is_end_chunk = chunk.is_end_of_utterance;

        if !chunk.samples.is_empty() {
            accumulator.extend(&chunk.samples);
        }

        while chunk_samples > 0 && accumulator.len() >= chunk_samples {
            let chunk_data: Vec<i16> = accumulator.drain(..chunk_samples).collect();

            let is_speech = match vad.process_chunk(&chunk_data, sample_rate) {
                Ok(r) => r.is_speech,
                Err(e) => {
                    tracing::error!(error = %e, "VAD filter error");
                    false
                }
            };

            // ── Onset confirmation ──────────────────────────
            if !speech_active {
                if is_speech {
                    last_speech_at = Some(std::time::Instant::now());
                    onset_buf.push_back(chunk_data.clone());
                    onset_speech_count += 1;

                    if onset_speech_count >= MIN_ONSET_CHUNKS {
                        speech_active = true;
                        set_probe(true);
                        utterance_seq += 1;
                        forwarded_speech_chunks = 0;
                        forwarded_non_speech_chunks = 0;
                        tracing::debug!(
                            utterance_seq,
                            onset_chunks = onset_speech_count,
                            "VAD filter: speech onset confirmed"
                        );

                        // Flush pre-roll + onset buffers
                        for pre in pre_roll.drain(..) {
                            let c = vad_chunk(pre, sample_rate, false, last_speech_at);
                            if filtered_tx.send(c).await.is_err() {
                                return;
                            }
                            forwarded_speech_chunks += 1;
                        }
                        for buf in onset_buf.drain(..) {
                            let c = vad_chunk(buf, sample_rate, false, last_speech_at);
                            if filtered_tx.send(c).await.is_err() {
                                return;
                            }
                            forwarded_speech_chunks += 1;
                        }
                        onset_speech_count = 0;
                    }
                } else {
                    if onset_speech_count > 0 {
                        onset_buf.clear();
                        onset_speech_count = 0;
                    }
                    if pre_roll.len() >= pre_roll_chunks {
                        pre_roll.pop_front();
                    }
                    pre_roll.push_back(chunk_data);
                }
                continue;
            }

            // ── Active speech tracking ──────────────────────
            if is_speech {
                silent_samples = 0;
                last_speech_at = Some(std::time::Instant::now());
            } else {
                silent_samples += chunk_data.len();
            }

            let is_end = silent_samples >= silence_limit;

            // Once speech is active, forward contiguous audio. Dropping chunks that
            // momentarily fall below VAD threshold can delete low-energy phonemes
            // (especially in fast speech) and degrade STT word accuracy.
            if speech_active {
                if !is_speech {
                    forwarded_non_speech_chunks += 1;
                }
                let c = vad_chunk(chunk_data, sample_rate, false, last_speech_at);
                if filtered_tx.send(c).await.is_err() {
                    return;
                }
                forwarded_speech_chunks += 1;
            }

            // End-of-utterance sentinel when enough silence after speech.
            if is_end {
                let sentinel = vad_chunk(Vec::new(), sample_rate, true, last_speech_at);
                if filtered_tx.send(sentinel).await.is_err() {
                    return;
                }
                tracing::info!(
                    utterance_seq,
                    forwarded_speech_chunks,
                    forwarded_non_speech_chunks,
                    silence_samples = silent_samples,
                    "VAD filter: emitted end-of-utterance sentinel"
                );
                speech_active = false;
                set_probe(false);
                silent_samples = 0;
                last_speech_at = None;
                vad.reset();
            }
        }

        if is_end_chunk {
            if speech_active && sample_rate > 0 {
                let sentinel = vad_chunk(Vec::new(), sample_rate, true, last_speech_at);
                if filtered_tx.send(sentinel).await.is_err() {
                    return;
                }
            }
            speech_active = false;
            set_probe(false);
            silent_samples = 0;
            last_speech_at = None;
            onset_buf.clear();
            onset_speech_count = 0;
            pre_roll.clear();
            accumulator.clear();
            vad.reset();
        }
    }

    // Channel closed -- if speech was active, send a final sentinel.
    set_probe(false);
    if speech_active && sample_rate > 0 {
        let _ = filtered_tx
            .send(vad_chunk(Vec::new(), sample_rate, true, last_speech_at))
            .await;
    }
}
