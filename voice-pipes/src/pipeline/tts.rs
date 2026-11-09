use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::audio::{self, AudioFrame};
use crate::config::OutputChunkingConfig;
use crate::text::TextFragment;
use crate::transport::{ErrorCode, ErrorComponent, ErrorSeverity, ServerControl, ServerMessage};
use crate::tts::{SynthesisRequest, TtsProvider, WordBoundary};

use super::types::AudioChunk;

/// Helper to determine the number of audio samples per fixed-size chunk,
/// based on the target sample rate and the `OutputChunkingConfig` settings.
/// Returns `None` if output chunking is disabled or not configured.
fn fixed_chunk_samples(
    output_chunking: Option<&OutputChunkingConfig>,
    sample_rate: u32,
) -> Option<usize> {
    let cfg = output_chunking.filter(|cfg| cfg.enabled)?;
    let samples = (sample_rate as u64 * cfg.frame_ms as u64) / 1000;
    Some(samples.max(1) as usize)
}

/// Calculates the Real-Time Factor (RTF).
/// RTF < 1.0 means synthesis generated audio faster than real-time playback length.
///
/// **Convention note:** this returns the *inverse* RTF (`wall / audio`,
/// lower = faster) for backward compatibility with historical capture
/// data and the frontend. New code should prefer [`compute_rtfx`] which
/// follows the Open ASR Leaderboard / paper convention
/// (`audio / wall`, higher = faster) and is also added to emitted audit
/// events as the `rtfx` sibling field.
pub fn compute_rtf(wall_duration_ms: f64, audio_duration_s: f64) -> f64 {
    if audio_duration_s > 0.0 {
        let synth_s = wall_duration_ms / 1000.0;
        (synth_s / audio_duration_s * 100.0).round() / 100.0
    } else {
        0.0
    }
}

/// Calculates `RTFx` -- `audio_duration` / `wall_duration` (higher = faster).
///
/// This is the convention used by the Microsoft on-device streaming ASR
/// paper (arXiv:2604.14493v2  3.2) and the Open ASR Leaderboard. Emit
/// this alongside [`compute_rtf`] so future consumers have a single
/// canonical "speed" number across STT and TTS.
pub fn compute_rtfx(wall_duration_ms: f64, audio_duration_s: f64) -> f64 {
    let wall_s = wall_duration_ms / 1000.0;
    if wall_s > 0.0 {
        (audio_duration_s / wall_s * 100.0).round() / 100.0
    } else {
        0.0
    }
}

/// Emits the `tts_timing` trace event containing RTF and byte/duration counts.
fn emit_tts_timing_trace(
    audit: Option<&crate::audit::StreamingAuditSink>,
    tts_name: &str,
    total_chars: usize,
    wall_ms: f64,
    total_audio_samples: usize,
    target_rate: u32,
) {
    if let Some(sink) = audit
        && total_chars > 0
    {
        let audio_dur_s = total_audio_samples as f64 / target_rate as f64;
        let rtf = compute_rtf(wall_ms, audio_dur_s);
        let rtfx = compute_rtfx(wall_ms, audio_dur_s);

        sink.record(crate::audit::AuditEvent::Trace(
            crate::audit::TraceEvent::new(
                format!("tts:{tts_name}"),
                "tts_timing",
                serde_json::json!({
                    "elapsed_ms": (wall_ms * 10.0).round() / 10.0,
                    "chars": total_chars,
                    "audio_duration_s": (audio_dur_s * 100.0).round() / 100.0,
                    "sample_rate": target_rate,
                    "rtf": rtf,
                    "rtfx": rtfx,
                    "provider": tts_name,
                }),
            ),
        ));
    }
}

/// Helper to configure and spawn the TTS synthesis asynchronous task for a given text fragment.
///
/// When `max_retries == 0` (the default for providers without
/// `ErrorPolicy::RetryWithBackoff`), the task is a single shot of
/// [`TtsProvider::synthesize_stream`], with the word-timing channel in the
/// request when the provider reports words. On `Err` the receiver simply
/// closes and the caller surfaces the error to the client.
///
/// When `max_retries > 0`, the task wraps the call in a retry loop with
/// exponential backoff. Retries fire **only if no audio frame has been
/// forwarded to the caller yet** -- this is the safety gate that prevents
/// the client from ever hearing the same audio twice. Once a single audio
/// frame has been delivered, any later error propagates as-is. Word
/// boundaries before the first audio frame are also discarded on retry so
/// the timeline restarts cleanly.
///
/// Backoff sequence: 100ms, 200ms, 400ms, ... capped at 1s.
#[expect(
    clippy::type_complexity,
    reason = "handle/receiver bundle is destructured immediately at both call sites"
)]
fn spawn_synthesis_task(
    tts: Arc<dyn TtsProvider>,
    tts_session: Arc<tokio::sync::Mutex<Option<Box<dyn crate::tts::TtsSession>>>>,
    // Whether a stateful TTS session is in play. Passed in (computed once at
    // task start) rather than probed here with `tts_session.lock().await`:
    // locking inside this function is what deadlocked prefetch -- the select
    // loop would park on the lock (held by the in-flight synthesis) and stop
    // draining its frame channel, which the in-flight synthesis is blocked on.
    has_session: bool,
    text: String,
    hints: Option<crate::tts::SynthesisHints>,
    max_retries: u8,
    cancel: CancellationToken,
) -> (
    tokio::task::JoinHandle<Result<(), crate::tts::TtsError>>,
    mpsc::Receiver<AudioFrame>,
    Option<mpsc::Receiver<WordBoundary>>,
) {
    let (ftx, frx) = mpsc::channel::<AudioFrame>(8);
    let use_word_ts = tts.supports_word_timestamps();
    let (word_tx_outer, word_rx) = if use_word_ts {
        let (tx, rx) = mpsc::channel::<WordBoundary>(64);
        (Some(tx), Some(rx))
    } else {
        (None, None)
    };

    if has_session {
        let cancel_session = cancel.clone();
        let tts_session_inner = Arc::clone(&tts_session);
        let text_inner = text.clone();
        let hints_inner = hints.clone();
        let ftx_inner = ftx.clone();

        let synth_handle = tokio::spawn(async move {
            let mut session_opt = tts_session_inner.lock().await;
            if let Some(s) = session_opt.as_mut() {
                // Stateful path
                let mut res = Ok(());
                let mut cancelled = false;
                tokio::select! {
                    biased;
                    () = cancel_session.cancelled() => {
                        cancelled = true;
                    }
                    r = s.synthesize_turn(&text_inner, hints_inner.as_ref(), ftx_inner) => {
                        res = r;
                    }
                }
                if cancelled {
                    let _ = s.cancel().await;
                }
                res
            } else {
                Ok(())
            }
        });
        return (synth_handle, frx, word_rx);
    }

    if max_retries == 0 {
        // Fast path: behave exactly as before -- single attempt, the
        // provider writes directly into the caller-visible channels.
        let synth_handle = tokio::spawn(async move {
            let req = SynthesisRequest::new(&text)
                .with_hints(hints.as_ref())
                .with_word_timing(word_tx_outer);
            tts.synthesize_stream(req, ftx, cancel).await
        });
        return (synth_handle, frx, word_rx);
    }

    // Retry path: relay frames/words from per-attempt inner channels into
    // the caller-visible channels. The atomic flips the first time a frame
    // crosses to the outer side; once it does, retries are disabled.
    let cancel_outer = cancel.clone();
    let synth_handle = tokio::spawn(async move {
        let mut attempts_remaining = max_retries;
        loop {
            if cancel_outer.is_cancelled() {
                return Ok(());
            }
            let (inner_audio_tx, mut inner_audio_rx) = mpsc::channel::<AudioFrame>(8);
            let (inner_word_tx, mut inner_word_rx_opt) = if use_word_ts {
                let (tx, rx) = mpsc::channel::<WordBoundary>(64);
                (Some(tx), Some(rx))
            } else {
                (None, None)
            };

            let tts_attempt = Arc::clone(&tts);
            let text_attempt = text.clone();
            let hints_attempt = hints.clone();
            let cancel_attempt = cancel_outer.clone();
            let mut synth_call = tokio::spawn(async move {
                let req = SynthesisRequest::new(&text_attempt)
                    .with_hints(hints_attempt.as_ref())
                    .with_word_timing(inner_word_tx);
                tts_attempt
                    .synthesize_stream(req, inner_audio_tx, cancel_attempt)
                    .await
            });

            let mut frame_seen = false;
            let synth_result: Result<(), crate::tts::TtsError>;

            // Relay loop: forward frames + words until the synth call ends
            // or both channels close.
            loop {
                tokio::select! {
                    biased;
                    () = cancel_outer.cancelled() => {
                        synth_call.abort();
                        return Ok(());
                    }
                    // Forward audio frames first so we never reorder.
                    audio = inner_audio_rx.recv() => {
                        if let Some(f) = audio {
                            frame_seen = true;
                            if ftx.send(f).await.is_err() {
                                // Outer consumer dropped -- abort the
                                // synth and exit cleanly.
                                synth_call.abort();
                                return Ok(());
                            }
                        } else {
                            // Inner audio channel closed -- drain words
                            // then await the synth result.
                            if let (Some(outer_wtx), Some(ref mut inner_wrx)) =
                                (word_tx_outer.as_ref(), inner_word_rx_opt.as_mut())
                            {
                                while let Ok(wb) = inner_wrx.try_recv() {
                                    let _ = outer_wtx.send(wb).await;
                                }
                            }
                            let res = (&mut synth_call).await;
                            synth_result = match res {
                                Ok(r) => r,
                                Err(je) if je.is_panic() => {
                                    Err(crate::tts::TtsError::InferenceFailed(format!(
                                        "TTS task panicked: {je}"
                                    )))
                                }
                                Err(je) => Err(crate::tts::TtsError::InferenceFailed(
                                    je.to_string(),
                                )),
                            };
                            break;
                        }
                    }
                    word = async {
                        match inner_word_rx_opt.as_mut() {
                            Some(rx) => rx.recv().await,
                            None => std::future::pending().await,
                        }
                    } => {
                        // A closed word channel answers `None` at once on
                        // every poll; drop it, or this arm spins until the
                        // audio ends.
                        if word.is_none() {
                            inner_word_rx_opt = None;
                        }
                        if let (Some(wb), Some(outer_wtx)) = (word, word_tx_outer.as_ref()) {
                            // Words alone don't disqualify retry -- only
                            // forward them once we know we're committed.
                            // Keep the simple path: forward immediately.
                            // Retry resets the timeline by spawning a fresh
                            // attempt; downstream tolerates duplicate words
                            // just like it tolerates none.
                            let _ = outer_wtx.send(wb).await;
                        }
                    }
                }
            }

            match synth_result {
                Ok(()) => return Ok(()),
                Err(e) => {
                    // A rejected request (bad key, unknown voice, bad
                    // config) fails identically on every attempt; only an
                    // error that may pass is worth the backoff.
                    if !e.is_transient() {
                        return Err(e);
                    }
                    if !frame_seen && attempts_remaining > 0 {
                        let attempt_idx = (max_retries - attempts_remaining) as u32;
                        let backoff_ms =
                            (RETRY_BASE_BACKOFF_MS << attempt_idx).min(RETRY_MAX_BACKOFF_MS);
                        tracing::warn!(
                            error = %e,
                            attempt = max_retries - attempts_remaining + 1,
                            of = max_retries,
                            backoff_ms,
                            "TTS synthesis failed before any audio; retrying after backoff"
                        );
                        tokio::select! {
                            () = cancel_outer.cancelled() => {
                                return Ok(());
                            }
                            () = tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)) => {}
                        }
                        attempts_remaining -= 1;
                        continue;
                    }
                    if frame_seen && attempts_remaining > 0 {
                        tracing::warn!(
                            error = %e,
                            "TTS synthesis failed after audio was already forwarded; retry skipped to avoid duplicate audio"
                        );
                    }
                    return Err(e);
                }
            }
        }
    });

    (synth_handle, frx, word_rx)
}

/// Base backoff for the first TTS retry attempt, in milliseconds.
const RETRY_BASE_BACKOFF_MS: u64 = 100;
/// Cap applied to exponential backoff so no single retry waits longer than this.
const RETRY_MAX_BACKOFF_MS: u64 = 1_000;

/// Drains chunks of exactly `target_samples` from the provided `buffer` and sends them
/// downstream via the `audio_tx` channel. Any remaining samples are left in the buffer.
/// Also attaches the `pending_caption` to the first chunk sent.
/// Returns `false` if the `audio_tx` channel has closed.
async fn send_fixed_chunks(
    audio_tx: &mpsc::Sender<AudioChunk>,
    buffer: &mut Vec<i16>,
    target_samples: usize,
    sample_rate: u32,
    channels: u16,
    pending_caption: &mut Option<super::types::CaptionMeta>,
    utterance_id: u32,
) -> bool {
    while buffer.len() >= target_samples {
        let remainder = buffer.split_off(target_samples);
        let samples = std::mem::replace(buffer, remainder);
        let chunk = AudioChunk {
            samples,
            sample_rate,
            channels,
            is_end_of_utterance: false,
            caption: pending_caption.take(),
            utterance_id,
            full_text: None,
            extras: None,
            last_speech_at: None,
            synth_duration: None,
        };
        if audio_tx.send(chunk).await.is_err() {
            return false;
        }
    }
    true
}

/// Flushes any remaining samples in the `buffer` as a single downstream `AudioChunk`.
/// Ensures any pending captions are delivered if the buffer is non-empty.
/// Returns `false` if the `audio_tx` channel has closed.
async fn flush_fragment_buffer(
    audio_tx: &mpsc::Sender<AudioChunk>,
    buffer: &mut Vec<i16>,
    sample_rate: u32,
    channels: u16,
    pending_caption: &mut Option<super::types::CaptionMeta>,
    utterance_id: u32,
) -> bool {
    if buffer.is_empty() {
        return true;
    }
    let chunk = AudioChunk {
        samples: std::mem::take(buffer),
        sample_rate,
        channels,
        is_end_of_utterance: false,
        caption: pending_caption.take(),
        utterance_id,
        full_text: None,
        extras: None,
        last_speech_at: None,
        synth_duration: None,
    };
    audio_tx.send(chunk).await.is_ok()
}

struct PrefetchedFragment {
    fragment: TextFragment,
    spoken_text: String,
    directives: Vec<voice_pipes_sdk::TtsDirectiveRecord>,
    stripped: Vec<voice_pipes_sdk::StrippedDirectiveRecord>,
    t0: std::time::Instant,
    synth_handle: tokio::task::JoinHandle<Result<(), crate::tts::TtsError>>,
    frame_rx: mpsc::Receiver<AudioFrame>,
    word_rx: Option<mpsc::Receiver<WordBoundary>>,
}

/// TTS task: receives text fragments, synthesizes each into audio
/// via the streaming interface, and sends audio chunks downstream.
/// When `output_rate` is set and differs from the TTS native rate,
/// audio is resampled chunk-by-chunk using `StreamingResampler`.
///
/// Checks `cancel` before each fragment; exits early when cancelled.
pub async fn tts_task(
    mut frag_rx: mpsc::Receiver<TextFragment>,
    audio_tx: mpsc::Sender<AudioChunk>,
    tts: Arc<dyn TtsProvider>,
    tts_session: Arc<tokio::sync::Mutex<Option<Box<dyn crate::tts::TtsSession>>>>,
    output_rate: Option<u32>,
    output_chunking: Option<OutputChunkingConfig>,
    cancel: CancellationToken,
    utterance_id: u32,
    audit: Option<crate::audit::StreamingAuditSink>,
    event_tx: mpsc::Sender<ServerMessage>,
    error_reported: Arc<AtomicBool>,
    sanitize_cfg: Option<voice_pipes_sdk::SanitizeConfig>,
) {
    let tts_rate = tts.sample_rate();
    let tts_name = tts.name().to_string();
    let target_rate = output_rate.unwrap_or(tts_rate);
    let sanitize_cfg = sanitize_cfg.unwrap_or_else(|| {
        voice_pipes_sdk::SanitizeConfig::from_tts(None, &tts_name, tts.supported_inline_tags())
    });
    let needs_resample = tts_rate != target_rate;
    // A rate the resampler refuses (a provider reporting zero) could not be
    // converted fragment by fragment either: fail the task up front.
    if needs_resample
        && let Err(e) = audio::resample::StreamingResampler::new(tts_rate, target_rate)
    {
        tracing::error!(error = %e, tts = %tts_name, "TTS output rate cannot be converted");
        if !error_reported.swap(true, Ordering::SeqCst) {
            let _ = event_tx
                .send(ServerMessage::Control(ServerControl::Error {
                    code: ErrorCode::TtsSynthesisFailed,
                    message: format!("TTS output cannot be converted: {e}"),
                    severity: ErrorSeverity::Error,
                    turn_id: None,
                    component: Some(ErrorComponent::Tts),
                }))
                .await;
        }
        return;
    }
    let fixed_chunk_samples = fixed_chunk_samples(output_chunking.as_ref(), target_rate);
    let mut fragment_idx: u32 = 0;

    // Resolve the TTS provider's error policy once at task entry.
    // Honoured: Propagate (default), LogAndContinue, and RetryWithBackoff
    // (TTS-only -- replays the synthesize call before any audio reaches
    // the client). Fallback still falls through to Propagate.
    let tts_component = tts.as_component();
    let tts_error_action = crate::component::classify_streaming_error(
        tts_component
            .as_deref()
            .map(|c| c as &dyn crate::component::Component),
        "tts",
    );
    let synth_max_retries = match tts_error_action {
        crate::component::ErrorAction::Retry { max_attempts } => max_attempts,
        _ => 0,
    };

    if let Some(samples) = fixed_chunk_samples {
        tracing::info!(
            frame_ms = output_chunking
                .as_ref()
                .map(|cfg| cfg.frame_ms)
                .unwrap_or_default(),
            frame_samples = samples,
            sample_rate = target_rate,
            "fixed-size TTS output chunking enabled"
        );
    }

    // Whether a stateful TTS session is in use. The `tts_session` Option is set
    // once at session open and never toggles, so probe it a single time here
    // (before any synthesis holds the lock) and thread the answer through --
    // never lock the session mutex from the select loop below.
    let has_stateful_session = tts_session.lock().await.is_some();

    // Accumulate per-utterance TTS timing metrics.
    let mut total_synth_ms: f64 = 0.0;
    let mut total_chars: usize = 0;
    let mut total_audio_samples: usize = 0;

    // ── Prefetch state ───────────────────────────────────────
    // While forwarding audio for the current fragment we also listen on
    // `frag_rx`.  When the next fragment arrives we kick off its synthesis
    // immediately, overlapping the API round-trip with audio delivery --
    // critical for non-streaming TTS providers (e.g. Gemini).
    let mut prefetched: Option<PrefetchedFragment> = None;
    // Fragment received during prefetch that we could not synthesize
    // inline (e.g. empty / end-sentinel).  Consumed next iteration.
    let mut deferred_frag: Option<TextFragment> = None;
    // Once `frag_rx` reports closed, stop polling it (see the
    // prefetch arm) — a closed receiver is ready forever.
    let mut frag_rx_closed = false;
    // Wall-clock start of the first synthesis (for accurate aggregate timing).
    let mut tts_wall_t0: Option<std::time::Instant> = None;

    loop {
        // ── Step 1: obtain the next fragment + its synthesis state ──
        let (
            fragment,
            frag_spoken_text,
            frag_directives,
            frag_stripped,
            frag_t0,
            synth_handle,
            mut frame_rx,
        );
        let mut word_rx: Option<mpsc::Receiver<WordBoundary>>;

        if let Some(pf) = prefetched.take() {
            tracing::debug!(
                chars = pf.fragment.text.chars().count(),
                "TTS: using prefetched synthesis"
            );
            fragment = pf.fragment;
            frag_spoken_text = pf.spoken_text;
            frag_directives = pf.directives;
            frag_stripped = pf.stripped;
            frag_t0 = pf.t0;
            synth_handle = pf.synth_handle;
            frame_rx = pf.frame_rx;
            word_rx = pf.word_rx;
        } else {
            // Get the raw fragment -- from the deferred slot or the channel.
            let mut raw = if let Some(df) = deferred_frag.take() {
                df
            } else {
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => {
                        tracing::info!("TTS task cancelled");
                        while frag_rx.try_recv().is_ok() {}
                        return;
                    }
                    frag = frag_rx.recv() => match frag {
                        Some(f) => f,
                        None => return,
                    },
                }
            };

            let is_end = raw.is_end_of_response;

            // Dual-track sanitization: separate display text from spoken text,
            // evaluate inline directives, and apply pronunciation lexicon.
            let dual = if let Some(ref spoken) = raw.spoken_text {
                voice_pipes_sdk::DualTrackText {
                    display: raw.text.clone(),
                    spoken: spoken.clone(),
                    directives: Vec::new(),
                    stripped: Vec::new(),
                }
            } else {
                voice_pipes_sdk::sanitize_dual_track(&raw.text, &sanitize_cfg)
            };
            raw.text = dual.display;
            let spoken_text = dual.spoken;
            let tts_directives = dual.directives;
            let stripped_directives = dual.stripped;

            if spoken_text.trim().is_empty() {
                // Still send the end-of-utterance sentinel for empty final fragments
                // so the runner can collect pipeline metrics and emit TurnMetrics.
                if is_end {
                    // Emit per-utterance tts_timing trace event for the accumulated metrics.
                    let wall_ms =
                        tts_wall_t0.map_or(total_synth_ms, |t| t.elapsed().as_secs_f64() * 1000.0);
                    emit_tts_timing_trace(
                        audit.as_ref(),
                        &tts_name,
                        total_chars,
                        wall_ms,
                        total_audio_samples,
                        target_rate,
                    );

                    let sentinel = AudioChunk {
                        samples: vec![],
                        sample_rate: target_rate,
                        channels: 1,
                        is_end_of_utterance: true,
                        caption: None,
                        utterance_id,
                        full_text: None,
                        extras: None,
                        last_speech_at: None,
                        synth_duration: Some(std::time::Duration::ZERO),
                    };
                    if audio_tx.send(sentinel).await.is_err() {
                        return;
                    }
                }
                continue;
            }

            // Start synthesis for this fragment.
            let (handle, frx, wrx) = spawn_synthesis_task(
                Arc::clone(&tts),
                Arc::clone(&tts_session),
                has_stateful_session,
                spoken_text.clone(),
                raw.hints.clone(),
                synth_max_retries,
                cancel.clone(),
            );
            synth_handle = handle;
            frame_rx = frx;
            word_rx = wrx;
            frag_t0 = std::time::Instant::now();
            fragment = raw;
            frag_spoken_text = spoken_text;
            frag_directives = tts_directives;
            frag_stripped = stripped_directives;
        }

        let is_end = fragment.is_end_of_response;
        let frag_char_count = fragment.text.chars().count();
        if tts_wall_t0.is_none() {
            tts_wall_t0 = Some(frag_t0);
        }

        // If word timestamp channel was created for this fragment, drain it
        // into a Vec as synthesis progresses.  We collect boundaries both
        // during the frame loop and after it finishes -- batch TTS providers
        // send all boundaries before the first audio frame, while streaming
        // providers interleave them.
        let mut collected_word_timings: Vec<WordBoundary> = Vec::new();

        // Create a per-fragment resampler to keep state across streamed frames.
        // The rates were checked at task entry.
        let mut resampler = needs_resample
            .then(|| audio::resample::StreamingResampler::new(tts_rate, target_rate).ok())
            .flatten();

        // ── Step 2: stream audio frames immediately ──
        let mut fragment_samples: Vec<i16> = Vec::new();
        let mut frag_channels: u16 = 1;
        let mut cancelled_mid_synthesis = false;

        let mut pending_caption = Some(super::types::CaptionMeta {
            text: fragment.text.clone(),
            fragment_idx,
            is_final: is_end,
            total_fragment_duration_ms: None,
            word_timings: None,
            spoken_text: Some(frag_spoken_text.clone()),
            tts_directives: frag_directives.clone(),
            stripped_directives: frag_stripped.clone(),
        });

        let mut rechunk_buffer: Vec<i16> = Vec::new();
        let mut words_open = word_rx.is_some();

        loop {
            tokio::select! {
                biased;
                () = cancel.cancelled() => {
                    tracing::info!("TTS cancelled mid-synthesis");
                    cancelled_mid_synthesis = true;
                    break;
                }
                // Word boundaries are taken as they come, not only when a
                // frame arrives: a provider that times every word before
                // its first audio would otherwise fill the bounded channel
                // and wait on it, while this loop waits on the audio.
                word = async {
                    match word_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                }, if words_open => match word {
                    Some(wb) => collected_word_timings.push(wb),
                    None => words_open = false,
                },
                frame = frame_rx.recv() => match frame {
                    Some(f) => {
                        frag_channels = f.channels;
                        let samples = if let Some(ref mut r) = resampler {
                            r.process_chunk(&f.samples)
                        } else {
                            f.samples
                        };
                        fragment_samples.extend_from_slice(&samples);

                        // Drain any word boundaries that arrived so far.
                        if let Some(ref mut wrx) = word_rx {
                            while let Ok(wb) = wrx.try_recv() {
                                collected_word_timings.push(wb);
                            }
                        }

                        if let Some(ref mut cap) = pending_caption
                            && !collected_word_timings.is_empty() {
                                cap.word_timings = Some(
                                    collected_word_timings
                                        .iter()
                                        .map(|wb| super::types::WordTiming {
                                            word: wb.word.clone(),
                                            offset_ms: wb.start_ms,
                                            duration_ms: wb.duration_ms,
                                        })
                                        .collect(),
                                );
                            }

                        // Stream immediately
                        if let Some(target_samples) = fixed_chunk_samples {
                            rechunk_buffer.extend_from_slice(&samples);
                            if !send_fixed_chunks(
                                &audio_tx,
                                &mut rechunk_buffer,
                                target_samples,
                                target_rate,
                                frag_channels,
                                &mut pending_caption,
                                utterance_id,
                            )
                            .await
                            {
                                return;
                            }
                        } else {
                            let chunk = AudioChunk {
                                samples: samples.clone(),
                                sample_rate: target_rate,
                                channels: frag_channels,
                                is_end_of_utterance: false,
                                caption: pending_caption.take(),
                                utterance_id,
                                full_text: None,
                                extras: None,
                                last_speech_at: None,
                                synth_duration: None,
                            };
                            if audio_tx.send(chunk).await.is_err() {
                                return;
                            }
                        }
                    }
                    None => break, // synthesis done
                },
                // ── Prefetch: start next fragment's synthesis while we
                //    wait for (or forward) the current fragment's audio.
                //    Skipped for stateful sessions: a second `synthesize_turn`
                //    would just serialize behind the in-flight one on the
                //    session lock (no overlap to win) and prefetching there was
                //    the source of a deadlock. The next fragment is
                //    picked up normally on the following loop iteration.
                next = frag_rx.recv(), if !has_stateful_session && prefetched.is_none() && deferred_frag.is_none() && !is_end && !frag_rx_closed => {
                    // `None` means the channel closed -- no more fragments
                    // coming. : remember it, or a closed channel is
                    // `Ready(None)` on every poll and this arm spins a core
                    // for the rest of the in-flight fragment.
                    if next.is_none() {
                        frag_rx_closed = true;
                    }
                    if let Some(mut nf) = next {
                        let dual = if let Some(ref spoken) = nf.spoken_text {
                            voice_pipes_sdk::DualTrackText {
                                display: nf.text.clone(),
                                spoken: spoken.clone(),
                                directives: Vec::new(),
                                stripped: Vec::new(),
                            }
                        } else {
                            voice_pipes_sdk::sanitize_dual_track(&nf.text, &sanitize_cfg)
                        };
                        nf.text = dual.display;
                        let n_spoken = dual.spoken;
                        if n_spoken.trim().is_empty() {
                            // Empty fragment (possibly end-sentinel) -- defer.
                            deferred_frag = Some(nf);
                        } else {
                            let (nhandle, nrx, nwrx) = spawn_synthesis_task(
                                Arc::clone(&tts),
                                Arc::clone(&tts_session),
                                has_stateful_session,
                                n_spoken.clone(),
                                nf.hints.clone(),
                                synth_max_retries,
                                cancel.clone(),
                            );
                            prefetched = Some(PrefetchedFragment {
                                fragment: nf,
                                spoken_text: n_spoken,
                                directives: dual.directives,
                                stripped: dual.stripped,
                                t0: std::time::Instant::now(),
                                synth_handle: nhandle,
                                frame_rx: nrx,
                                word_rx: nwrx,
                            });
                            tracing::debug!("TTS: prefetch started for next fragment");
                        }
                    }
                }
            }
        }

        // Drain any trailing word boundaries
        if let Some(ref mut wrx) = word_rx {
            while let Ok(wb) = wrx.try_recv() {
                collected_word_timings.push(wb);
            }
        }

        // Flush resampler tail.
        let tail_samples = resampler
            .as_mut()
            .map(super::super::audio::resample::StreamingResampler::flush)
            .unwrap_or_default();
        if !tail_samples.is_empty() {
            fragment_samples.extend_from_slice(&tail_samples);
            if let Some(target_samples) = fixed_chunk_samples {
                rechunk_buffer.extend_from_slice(&tail_samples);
                if !send_fixed_chunks(
                    &audio_tx,
                    &mut rechunk_buffer,
                    target_samples,
                    target_rate,
                    frag_channels,
                    &mut pending_caption,
                    utterance_id,
                )
                .await
                {
                    return;
                }
            } else {
                let chunk = AudioChunk {
                    samples: tail_samples,
                    sample_rate: target_rate,
                    channels: frag_channels,
                    is_end_of_utterance: false,
                    caption: pending_caption.take(),
                    utterance_id,
                    full_text: None,
                    extras: None,
                    last_speech_at: None,
                    synth_duration: None,
                };
                if audio_tx.send(chunk).await.is_err() {
                    return;
                }
            }
        }

        // Flush remaining chunks from the rechunk buffer.
        if fixed_chunk_samples.is_some()
            && !flush_fragment_buffer(
                &audio_tx,
                &mut rechunk_buffer,
                target_rate,
                frag_channels,
                &mut pending_caption,
                utterance_id,
            )
            .await
        {
            return;
        }

        let audio_dur_ms =
            (fragment_samples.len() as f64 / (target_rate as f64 * frag_channels as f64) * 1000.0)
                .round() as u32;
        let mut total_fragment_duration_ms = audio_dur_ms;
        if !collected_word_timings.is_empty()
            && let Some(last_word) = collected_word_timings.last()
        {
            let last_end = last_word.start_ms + last_word.duration_ms;
            if last_end > total_fragment_duration_ms {
                total_fragment_duration_ms = last_end;
            }
        }

        let wt = if collected_word_timings.is_empty() {
            None
        } else {
            Some(
                collected_word_timings
                    .iter()
                    .map(|wb| super::types::WordTiming {
                        word: wb.word.clone(),
                        offset_ms: wb.start_ms,
                        duration_ms: wb.duration_ms,
                    })
                    .collect(),
            )
        };

        // Emit final metadata chunk if there was no error
        if !cancelled_mid_synthesis {
            let final_caption = super::types::CaptionMeta {
                text: fragment.text.clone(),
                fragment_idx,
                is_final: is_end,
                total_fragment_duration_ms: Some(total_fragment_duration_ms),
                word_timings: wt,
                spoken_text: Some(frag_spoken_text.clone()),
                tts_directives: frag_directives.clone(),
                stripped_directives: frag_stripped.clone(),
            };

            // If we generated no audio (or if pending_caption is somehow still present)
            // we will send it as a 0-sample chunk. If we already sent it, this serves
            // as the metadata update chunk that the frontend will recognize by 0-samples.
            let chunk = AudioChunk {
                samples: vec![],
                sample_rate: target_rate,
                channels: frag_channels,
                is_end_of_utterance: false,
                caption: pending_caption.take().or(Some(final_caption)),
                utterance_id,
                full_text: None,
                extras: None,
                last_speech_at: None,
                synth_duration: None,
            };
            if audio_tx.send(chunk).await.is_err() {
                return;
            }
        }

        // Emit TTS fragment audit event.
        let frag_sample_count = fragment_samples.len();
        if let Some(ref sink) = audit {
            sink.record(crate::audit::AuditEvent::TtsFragment {
                utterance_id,
                fragment_idx,
                text: fragment.text.clone(),
                spoken_text: Some(frag_spoken_text.clone()),
                audio: crate::audio::AudioFrame {
                    samples: fragment_samples,
                    sample_rate: target_rate,
                    channels: frag_channels,
                },
            });
        }
        // Advance the fragment counter regardless of whether audit is enabled:
        // CaptionMeta and the audio ledger key on fragment_idx in every deployment.
        fragment_idx += 1;

        // Accumulate per-utterance TTS timing.
        let frag_elapsed = frag_t0.elapsed().as_secs_f64() * 1000.0;
        total_synth_ms += frag_elapsed;
        total_chars += frag_char_count;
        total_audio_samples += frag_sample_count;

        if cancelled_mid_synthesis {
            // For a stateful session the synth task is the one
            // delivering `TtsSession::cancel()` to the provider (it observed
            // the same token); aborting it here could cut that call short
            // and leave the remote generating the interrupted turn. Let it
            // finish on its own; stateless synthesis is safe to abort.
            if !has_stateful_session {
                synth_handle.abort();
            }
            if let Some(pf) = prefetched.take() {
                pf.synth_handle.abort(); // also abort prefetched synthesis
            }
            while frag_rx.try_recv().is_ok() {}
            return;
        }

        // Flush resampler tail + signal end-of-response
        if is_end {
            // Emit per-utterance tts_timing trace event using active synthesis time.
            emit_tts_timing_trace(
                audit.as_ref(),
                &tts_name,
                total_chars,
                total_synth_ms,
                total_audio_samples,
                target_rate,
            );

            let sentinel = AudioChunk {
                samples: vec![],
                sample_rate: target_rate,
                channels: frag_channels,
                is_end_of_utterance: true,
                caption: None,
                utterance_id,
                full_text: None,
                extras: None,
                last_speech_at: None,
                synth_duration: Some(std::time::Duration::from_secs_f64(total_synth_ms / 1000.0)),
            };
            if audio_tx.send(sentinel).await.is_err() {
                return;
            }
        }

        // Surface any synthesis error.
        match synth_handle.await {
            Ok(Err(e)) => {
                tracing::error!(error = %e, "TTS synthesis failed");
                match tts_error_action {
                    crate::component::ErrorAction::Suppress => {
                        // LogAndContinue: do not surface to client; the
                        // pipeline continues with whatever frames already
                        // streamed.
                    }
                    // Retry semantics are handled inside spawn_synthesis_task;
                    // an error reaching here means retries were exhausted
                    // (or audio had already been forwarded), so it propagates.
                    crate::component::ErrorAction::Retry { .. }
                    | crate::component::ErrorAction::Propagate => {
                        if !error_reported.swap(true, Ordering::SeqCst) {
                            let _ = event_tx
                                .send(ServerMessage::Control(ServerControl::Error {
                                    code: ErrorCode::TtsSynthesisFailed,
                                    message: crate::transport::error::with_backend_hint(
                                        format!("TTS synthesis failed: {e}"),
                                        "TTS",
                                    ),
                                    severity: ErrorSeverity::Error,
                                    turn_id: None,
                                    component: Some(ErrorComponent::Tts),
                                }))
                                .await;
                        }
                    }
                }
            }
            Err(e) if !e.is_cancelled() => {
                tracing::error!(error = %e, "TTS task panicked");
                match tts_error_action {
                    crate::component::ErrorAction::Suppress => {
                        // LogAndContinue: swallow panics too.
                    }
                    crate::component::ErrorAction::Retry { .. }
                    | crate::component::ErrorAction::Propagate => {
                        if !error_reported.swap(true, Ordering::SeqCst) {
                            let _ = event_tx
                                .send(ServerMessage::Control(ServerControl::Error {
                                    code: ErrorCode::TtsSynthesisFailed,
                                    message: format!("TTS task panicked: {e}"),
                                    severity: ErrorSeverity::Error,
                                    turn_id: None,
                                    component: Some(ErrorComponent::Tts),
                                }))
                                .await;
                        }
                    }
                }
            }
            _ => {}
        }

        if is_end {
            return;
        }
    }
}

#[cfg(test)]
#[path = "tests/tts.rs"]
mod tests;
