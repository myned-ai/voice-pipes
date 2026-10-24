use std::sync::Arc;

use tokio::sync::mpsc;

use crate::stt::SttProvider;
use crate::stt::turn::TurnDetector;

use super::types::{AudioChunk, Utterance};

/// STT task: receives audio chunks, buffers until end-of-utterance,
/// resamples if needed, transcribes, and sends the text downstream.
///
/// The `turn_detector` evaluates each STT result together with the
/// elapsed silence to decide when a conversational turn is truly
/// finished -- replacing the previous hard-coded `is_final` check.
///
/// When `audit` is provided, emits `MicRaw`, `MicResampled`, and `SttResult` events.
///
/// `events` is the session's pipeline-event channel. Speech that makes no
/// turn is announced on it as `UtteranceDropped`, which is how the client is
/// told; the session's recorder writes it to the capture. Without one (the
/// `live` command) the event goes straight to `audit`.
pub async fn stt_task(
    mut audio_rx: mpsc::Receiver<AudioChunk>,
    text_tx: mpsc::Sender<Utterance>,
    speculative_tx: Option<mpsc::Sender<Utterance>>,
    stt: Arc<dyn SttProvider>,
    turn_detector: Arc<dyn TurnDetector>,
    audit: Option<crate::audit::StreamingAuditSink>,
    events: Option<tokio::sync::broadcast::Sender<super::events::PipelineEvent>>,
    min_utterance_chars: usize,
    vad_hook_tx: Option<mpsc::Sender<crate::stt::TranscriptionResult>>,
    language: Option<String>,
) {
    // Cancellation root for streaming STT workers, held as a drop-guard:
    // when this task is aborted at session teardown, the guard drops and the
    // workers observe cancellation (even mid-reconnect-backoff) so hosted
    // providers can terminate their sessions instead of leaking them.
    let worker_cancel = tokio_util::sync::CancellationToken::new();
    let _worker_cancel_guard = worker_cancel.clone().drop_guard();
    let mut worker = Worker::spawn(Arc::clone(&stt), language, worker_cancel);

    let sink = TurnSink {
        text_tx: &text_tx,
        audit: audit.as_ref(),
        events: events.as_ref(),
        turn_detector: turn_detector.as_ref(),
        stt_name: stt.name(),
        min_utterance_chars,
    };
    let mut task = SttLoop::new(stt.as_ref());

    // Once the mic channel closes we only stay in the loop to drain
    // the worker's last results. A worker that never finishes (wedged on a
    // provider socket) must not keep this task — and the session teardown
    // behind it — alive indefinitely.
    let mut audio_input_closed = false;
    let mut drain_deadline = tokio::time::Instant::now() + WORKER_JOIN_TIMEOUT;

    loop {
        tokio::select! {
            chunk_opt = audio_rx.recv(), if worker.chunk_tx.is_some() => {
                if let Some(chunk) = chunk_opt {
                    task.on_chunk(chunk, &sink, &mut worker).await;
                } else {
                    // Mic channel closed: dropping the worker's input tells
                    // the provider the stream is over.
                    audio_input_closed = true;
                    worker.chunk_tx = None;
                    drain_deadline = tokio::time::Instant::now() + WORKER_JOIN_TIMEOUT;
                }
            }
            () = tokio::time::sleep_until(drain_deadline), if audio_input_closed => {
                tracing::warn!(
                    provider = stt.name(),
                    timeout_ms = WORKER_JOIN_TIMEOUT.as_millis() as u64,
                    "stt_task: worker did not finish after input closed; tearing down"
                );
                break;
            }
            result_opt = worker.result_rx.recv() => match result_opt {
                Some(result) => {
                    // A live result stream means the worker is healthy.
                    worker.consecutive_failures = 0;
                    let downstream_open = task
                        .on_result(result, &sink, speculative_tx.as_ref(), vad_hook_tx.as_ref())
                        .await;
                    if !downstream_open {
                        break;
                    }
                }
                None if audio_input_closed => break,
                None => {
                    // The worker's result channel closed while the session
                    // is still running: the worker returned early. A
                    // provider that follows the streaming contract keeps
                    // serving across utterances, so this is recovery, not
                    // the normal path. A real failure has already been
                    // logged as an `error!` from inside `transcribe_stream`.
                    tracing::debug!(
                        utterance_id = task.turn.utterance_id,
                        "stt_task: streaming STT worker exited; respawning"
                    );
                    worker.respawn().await;
                }
            },
            // After audio stops flowing (VAD end-of-utterance), the turn
            // detector is never re-evaluated because no new STT results
            // arrive. This timer polls while a turn has text, giving the
            // detector a chance to fire once enough real silence has passed.
            () = tokio::time::sleep(std::time::Duration::from_millis(100)),
                if !task.turn.text.trim().is_empty() && task.turn.last_final_result.is_some() =>
            {
                if !task.on_silence_tick(&sink).await {
                    break;
                }
            }
            () = tokio::time::sleep_until(task.watchdog_deadline),
                if task.final_due_since.is_some() && !task.final_overdue_warned =>
            {
                task.warn_final_overdue(stt.name());
            }
            () = tokio::time::sleep_until(task.give_up_deadline),
                if task.final_due_since.is_some() =>
            {
                task.give_up_on_final(&sink, &mut worker).await;
            }
            else => break,
        }
    }

    worker.shutdown().await;
}

/// Fold one final STT result into the text accumulated for the current
/// turn.
///
/// A final normally covers speech the earlier finals did not (a new
/// segment after a pause), so it is appended with a single space. It
/// supersedes the accumulated text instead when it is a re-send of the
/// same segment: either the provider declares cumulative finals and the
/// `segment_index` did not change, or the new text literally extends what
/// was accumulated so far (a provider that grows its final without
/// declaring the capability).
pub(super) fn merge_final_text(
    accumulated: &mut String,
    previous_segment: Option<u32>,
    result: &crate::stt::TranscriptionResult,
    cumulative_finals: bool,
) {
    let incoming = result.text.trim();
    if incoming.is_empty() {
        return;
    }
    let current = accumulated.trim();
    let same_segment = previous_segment == Some(result.segment_index);
    let supersedes =
        current.is_empty() || (cumulative_finals && same_segment) || incoming.starts_with(current);
    if supersedes {
        *accumulated = incoming.to_string();
    } else {
        let joined = format!("{current} {incoming}");
        *accumulated = joined;
    }
}

/// How long after an end-of-utterance the recogniser may take to finalise
/// before the wait is logged as stuck.
const STT_FINAL_WATCHDOG: std::time::Duration = std::time::Duration::from_secs(4);

/// How long after an end-of-utterance the recogniser may stay silent
/// before the turn is given up: its audio is kept in the capture under
/// `stt_timeout` and the recogniser is restarted, since one that answers
/// nothing for this long will not answer the next utterance either.
const STT_FINAL_GIVE_UP: std::time::Duration = std::time::Duration::from_secs(15);

/// The STT task's state between events: the turn being heard, the audio
/// conversion, and whether a final is overdue.
struct SttLoop {
    turn: Turn,
    input: RecogniserInput,
    /// A provider can deliver several finals before the detector completes
    /// (one per speech segment, or a growing re-send of the same segment);
    /// `merge_final_text` decides whether each one appends or supersedes.
    cumulative_finals: bool,
    last_speech_instant: std::time::Instant,
    last_audio_sr: u32,
    /// Set when an end-of-utterance is forwarded, cleared by the next
    /// final: tells "stuck in listening" with audio endpointed but no final
    /// from "VAD never ended".
    final_due_since: Option<std::time::Instant>,
    final_overdue_warned: bool,
    watchdog_deadline: tokio::time::Instant,
    /// When the overdue final is given up on; see [`STT_FINAL_GIVE_UP`].
    give_up_deadline: tokio::time::Instant,
}

impl SttLoop {
    fn new(stt: &dyn SttProvider) -> Self {
        let expected_sr = stt.expected_sample_rate();
        Self {
            turn: Turn::new(crate::transport::FIRST_UTTERANCE_ID),
            // Providers opt out via `SttProvider::needs_flush_silence`.
            // Nemotron (already receives contiguous VAD trailing silence) and
            // Parakeet (TDT decoder mangles the final token on a digital-zero
            // cliff) both return false here.
            input: RecogniserInput::new(expected_sr, stt.needs_flush_silence()),
            cumulative_finals: stt.supports(&crate::stt::SttCapability::CumulativeFinals),
            last_speech_instant: std::time::Instant::now(),
            last_audio_sr: expected_sr,
            final_due_since: None,
            final_overdue_warned: false,
            watchdog_deadline: tokio::time::Instant::now() + STT_FINAL_WATCHDOG,
            give_up_deadline: tokio::time::Instant::now() + STT_FINAL_GIVE_UP,
        }
    }

    /// One chunk of microphone audio: kept for the capture, timed, converted
    /// to the recogniser's rate, shown to the turn detector and forwarded.
    async fn on_chunk(&mut self, chunk: AudioChunk, sink: &TurnSink<'_>, worker: &mut Worker) {
        let is_end = chunk.is_end_of_utterance;
        let chunk_sr = chunk.sample_rate;
        if sink.audit.is_some() {
            self.turn.hold_audio(&chunk.samples, is_end, chunk_sr);
        }
        // Any non-silent chunk resets the silence clock (unless `vad_filter_task`
        // supplied the true last-speech timestamp `chunk.last_speech_at`) and,
        // for the first one of an utterance, starts its timer. An end-of-utterance
        // alone starts nothing: there is nothing to time, and a timer started then
        // would charge the gap to the next real utterance.
        if let Some(speech_at) = chunk.last_speech_at {
            self.last_speech_instant = speech_at;
        } else if !is_end && !chunk.samples.is_empty() {
            self.last_speech_instant = std::time::Instant::now();
        }
        if !is_end && !chunk.samples.is_empty() {
            self.turn
                .audio_start
                .get_or_insert_with(std::time::Instant::now);
        }

        let mut samples = self.input.convert(chunk.samples, chunk_sr);
        // A detector with a rolling window (Smart Turn) is fed here; the
        // built-in silence and provider detectors ignore it.
        if !samples.is_empty() {
            sink.turn_detector
                .feed_audio(&samples, self.input.expected_sr);
        }
        if is_end {
            self.input.finish_utterance(&mut samples);
            // The clocks run from the first unanswered end of speech: a
            // caller who keeps talking must not keep a silent recogniser
            // alive by re-arming them.
            if self.final_due_since.is_none() {
                self.final_due_since = Some(std::time::Instant::now());
                self.final_overdue_warned = false;
                self.watchdog_deadline = tokio::time::Instant::now() + STT_FINAL_WATCHDOG;
                self.give_up_deadline = tokio::time::Instant::now() + STT_FINAL_GIVE_UP;
            }
            tracing::info!(
                utterance_id = self.turn.utterance_id,
                samples = samples.len(),
                "stt_task: end-of-utterance forwarded to streaming STT"
            );
        }
        // Fires every ~20ms while audio is flowing -- far too chatty for
        // DEBUG; the boundary is logged at INFO above.
        tracing::trace!(
            utterance_id = self.turn.utterance_id,
            is_last = is_end,
            samples = samples.len(),
            sample_rate = self.input.expected_sr,
            "stt_task: sending chunk to streaming STT"
        );
        let stt_chunk = crate::stt::AudioChunk {
            samples,
            is_last: is_end,
        };
        worker.send(stt_chunk, self.turn.utterance_id).await;
        self.last_audio_sr = chunk_sr;
    }

    /// One result from the recogniser: an interim one is passed to the
    /// speculative path, a final one folds into the turn, and either may let
    /// the detector end the turn. `false` once downstream has gone.
    async fn on_result(
        &mut self,
        result: crate::stt::TranscriptionResult,
        sink: &TurnSink<'_>,
        speculative_tx: Option<&mpsc::Sender<Utterance>>,
        vad_hook_tx: Option<&mpsc::Sender<crate::stt::TranscriptionResult>>,
    ) -> bool {
        // The VAD engine inspects results for early barge-in.
        if let Some(tx) = vad_hook_tx {
            let _ = tx.send(result.clone()).await;
        }
        tracing::info!(
            utterance_id = self.turn.utterance_id,
            chars = result.text.chars().count(),
            is_final = result.is_final,
            "STT partial/final result"
        );
        if result.is_final {
            self.turn.fold_final(sink, &result, self.cumulative_finals);
            self.final_due_since = None;
            self.final_overdue_warned = false;
        } else {
            if !result.text.trim().is_empty() {
                self.turn.saw_interim_text = true;
            }
            if let Some(tx) = speculative_tx {
                let _ = tx
                    .send(Utterance {
                        text: result.text.clone(),
                        utterance_id: self.turn.utterance_id,
                        confidence: result.confidence,
                        stt_latency: None,
                        source: crate::pipeline::types::UtteranceSource::default(),
                        endpointing_latency: None,
                        inference_metrics: None,
                        committed_at: None,
                    })
                    .await;
            }
        }

        let silence = self.last_speech_instant.elapsed();
        let signal = sink.turn_detector.evaluate(&result, silence).await;
        if result.is_final {
            self.turn.record_final(sink);
        }
        if !signal.is_complete() || self.turn.text.trim().is_empty() {
            return true;
        }
        self.turn
            .complete(
                sink,
                &signal,
                "result",
                self.last_audio_sr,
                result.confidence,
                self.last_speech_instant.elapsed(),
            )
            .await
    }

    /// The detector re-evaluated on the last final as silence grows.
    /// `false` once downstream has gone.
    async fn on_silence_tick(&mut self, sink: &TurnSink<'_>) -> bool {
        let Some(result) = self.turn.last_final_result.as_ref() else {
            return true;
        };
        let confidence = result.confidence;
        let silence = self.last_speech_instant.elapsed();
        let signal = sink.turn_detector.evaluate(result, silence).await;
        if !signal.is_complete() {
            return true;
        }
        self.turn
            .complete(
                sink,
                &signal,
                "silence timer",
                self.last_audio_sr,
                confidence,
                silence,
            )
            .await
    }

    /// The end-of-utterance went to the recogniser and no final came back:
    /// said once, so an operator can tell this from a VAD that never ended.
    /// The recogniser never answered the end of an utterance: keep the
    /// audio under `stt_timeout`, forget the turn, and restart the
    /// recogniser.
    async fn give_up_on_final(&mut self, sink: &TurnSink<'_>, worker: &mut Worker) {
        let waited_ms = self
            .final_due_since
            .map_or(0, |since| since.elapsed().as_millis());
        tracing::error!(
            utterance_id = self.turn.utterance_id,
            waited_ms,
            provider = sink.stt_name,
            "stt_task: no final STT result after end-of-utterance; giving the turn up and restarting the recogniser"
        );
        let heard_words = self.turn.saw_interim_text;
        self.turn.drop_turn(sink, "stt_timeout", heard_words);
        self.final_due_since = None;
        self.final_overdue_warned = false;
        worker.restart().await;
    }

    fn warn_final_overdue(&mut self, provider: &str) {
        if let Some(since) = self.final_due_since {
            self.final_overdue_warned = true;
            tracing::warn!(
                utterance_id = self.turn.utterance_id,
                elapsed_ms = since.elapsed().as_millis(),
                provider,
                "stt_task: waiting too long for final STT result after end-of-utterance"
            );
        }
    }
}

/// Audio on its way to the recogniser: converted to its rate, and at the
/// end of an utterance, flushed.
struct RecogniserInput {
    expected_sr: u32,
    /// Append half a second of silence at each utterance's end, so a
    /// streaming recogniser evaluates its lookahead when no trailing
    /// silence was forwarded.
    flush_silence: bool,
    resampler: Option<crate::audio::resample::StreamingResampler>,
    /// A client rate the resampler refused, so it is reported once, not per
    /// chunk.
    refused_rate: Option<u32>,
}

impl RecogniserInput {
    fn new(expected_sr: u32, flush_silence: bool) -> Self {
        Self {
            expected_sr,
            flush_silence,
            resampler: None,
            refused_rate: None,
        }
    }

    /// `samples` at `rate`, at the recogniser's rate. Audio at a rate that
    /// cannot be converted is dropped, not passed on as if it were at the
    /// recogniser's.
    fn convert(&mut self, samples: Vec<i16>, rate: u32) -> Vec<i16> {
        if rate == self.expected_sr {
            return samples;
        }
        if self
            .resampler
            .as_ref()
            .is_none_or(|r| r.from_rate() != rate)
        {
            self.resampler = match crate::audio::resample::StreamingResampler::new(
                rate,
                self.expected_sr,
            ) {
                Ok(r) => Some(r),
                Err(e) => {
                    if self.refused_rate != Some(rate) {
                        self.refused_rate = Some(rate);
                        tracing::warn!(error = %e, "client audio dropped: its sample rate cannot be converted");
                    }
                    None
                }
            };
        }
        match &mut self.resampler {
            Some(r) => r.process_chunk(&samples),
            None => Vec::new(),
        }
    }

    /// The end of an utterance: the resampler's tail, a fresh resampler for
    /// the next one, and the flush silence where the recogniser needs it.
    fn finish_utterance(&mut self, samples: &mut Vec<i16>) {
        if let Some(mut r) = self.resampler.take() {
            samples.append(&mut r.flush());
        }
        if self.flush_silence {
            let flush_samples = (self.expected_sr as f32 * 0.5) as usize;
            samples.resize(samples.len() + flush_samples, 0);
        }
    }
}

/// The streaming recogniser's worker. One serves the whole session; it is
/// respawned only if it exits while the session is still running.
struct Worker {
    stt: Arc<dyn SttProvider>,
    language: Option<String>,
    cancel: tokio_util::sync::CancellationToken,
    chunk_tx: Option<mpsc::Sender<crate::stt::AudioChunk>>,
    result_rx: mpsc::Receiver<crate::stt::TranscriptionResult>,
    handle: tokio::task::JoinHandle<Result<(), crate::stt::SttError>>,
    /// Drives the respawn backoff (see `worker_exit_backoff`); reset on
    /// clean exits and on results.
    consecutive_failures: u32,
    /// How many workers came before this one in the session.
    restarts: u32,
}

/// What starting a worker returns: its input, its results, its task.
type WorkerParts = (
    mpsc::Sender<crate::stt::AudioChunk>,
    mpsc::Receiver<crate::stt::TranscriptionResult>,
    tokio::task::JoinHandle<Result<(), crate::stt::SttError>>,
);

impl Worker {
    fn spawn(
        stt: Arc<dyn SttProvider>,
        language: Option<String>,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Self {
        let (chunk_tx, result_rx, handle) = Self::start(&stt, &language, &cancel, false);
        Self {
            stt,
            language,
            cancel,
            chunk_tx: Some(chunk_tx),
            result_rx,
            handle,
            consecutive_failures: 0,
            restarts: 0,
        }
    }

    fn start(
        stt: &Arc<dyn SttProvider>,
        language: &Option<String>,
        cancel: &tokio_util::sync::CancellationToken,
        replacement: bool,
    ) -> WorkerParts {
        let (chunk_tx, chunk_rx) = mpsc::channel::<crate::stt::AudioChunk>(100);
        let (result_tx, result_rx) = mpsc::channel::<crate::stt::TranscriptionResult>(100);
        let cancel = cancel.child_token();
        let stt = Arc::clone(stt);
        // The session's language rides on the request. A recogniser that
        // can act on it (a multilingual local model, a hosted service with a
        // locale) reads it there; every other one ignores the field.
        let language = language.clone();
        let handle = tokio::spawn(async move {
            let req = crate::stt::RecognitionRequest::default()
                .with_request_id("stream-0".into())
                .with_sample_rate(stt.expected_sample_rate())
                .with_language(language)
                // The worker this one replaces may still hold the slot.
                .with_slot_wait(replacement.then_some(WORKER_RESTART_SLOT_WAIT));
            stt.transcribe_stream(req, chunk_rx, result_tx, cancel)
                .await
        });
        (chunk_tx, result_rx, handle)
    }

    /// Learn why the worker stopped, back off after a failure, and start a
    /// new one.
    /// Close the worker's input and start another: for a recogniser that
    /// reads but never answers.
    async fn restart(&mut self) {
        self.chunk_tx = None;
        self.respawn().await;
    }

    async fn respawn(&mut self) {
        worker_exit_backoff(&mut self.handle, &mut self.consecutive_failures).await;
        self.restarts += 1;
        let (chunk_tx, result_rx, handle) =
            Self::start(&self.stt, &self.language, &self.cancel, true);
        self.chunk_tx = Some(chunk_tx);
        self.result_rx = result_rx;
        self.handle = handle;
    }

    /// Forward `chunk`; a worker whose input has closed is respawned and
    /// sent it again. A worker that takes nothing for [`STT_SEND_DEADLINE`]
    /// has stopped reading: the chunk is dropped and the worker replaced, so
    /// a stuck recogniser cannot hold the pipeline behind it.
    async fn send(&mut self, chunk: crate::stt::AudioChunk, utterance_id: u32) {
        let Some(tx) = &self.chunk_tx else {
            return;
        };
        match tokio::time::timeout(STT_SEND_DEADLINE, tx.send(chunk.clone())).await {
            Ok(Ok(())) => return,
            Ok(Err(_closed)) => {}
            Err(_stalled) => {
                tracing::warn!(
                    utterance_id,
                    "stt_task: the recogniser took no audio for {STT_SEND_DEADLINE:?}; \
                     dropping the chunk and restarting it"
                );
                // Closing its input first lets a worker that is only slow
                // finish; one that is stuck is aborted by the respawn.
                self.chunk_tx = None;
                self.respawn().await;
                return;
            }
        }
        tracing::warn!(
            utterance_id,
            "stt_task: streaming STT input channel closed; respawning worker"
        );
        self.respawn().await;
        if let Some(retry_tx) = &self.chunk_tx
            && retry_tx.send(chunk).await.is_err()
        {
            tracing::error!(
                utterance_id,
                "stt_task: failed to send chunk after worker respawn"
            );
        }
    }

    /// Graceful teardown on natural exit: close the input, signal
    /// cancellation, and give the worker a bounded window to terminate its
    /// provider session. (On abort, the task's drop-guard delivers the same
    /// cancel signal and the worker self-bounds via its own deadlines.)
    async fn shutdown(mut self) {
        drop(self.chunk_tx.take());
        self.cancel.cancel();
        if tokio::time::timeout(WORKER_JOIN_TIMEOUT, &mut self.handle)
            .await
            .is_err()
        {
            // A worker that ignores cancel must not outlive the
            // session — it may hold an open provider stream.
            tracing::warn!("streaming STT worker ignored cancel at teardown; aborting");
            self.handle.abort();
            let _ = tokio::time::timeout(std::time::Duration::from_secs(1), &mut self.handle).await;
        }
    }
}

/// The turn being heard: its words so far, its timing and its audio.
struct Turn {
    /// The id it commits under; the next turn's is one more.
    utterance_id: u32,
    /// The finals folded so far (see [`merge_final_text`]).
    text: String,
    /// Whether the recogniser has produced words for it, so a final that
    /// comes back empty can be told from silence.
    saw_interim_text: bool,
    last_final_segment: Option<u32>,
    /// The last final, so the silence timer can re-evaluate the detector
    /// after results stop arriving.
    last_final_result: Option<crate::stt::TranscriptionResult>,
    /// When its first audio arrived.
    audio_start: Option<std::time::Instant>,
    /// How long recognition took for its latest final.
    stt_latency: Option<std::time::Duration>,
    /// Its microphone audio, recorded with it when the session is captured.
    capture: CaptureBuffer,
    /// The rate of the audio in `capture`: the client's.
    capture_sr: u32,
    /// How many utterances were dropped under this id so far: the id is
    /// reused until a turn commits, and each dropped recording needs a name
    /// of its own.
    dropped: u32,
}

/// Where an ended turn goes, and what is told about it.
struct TurnSink<'a> {
    text_tx: &'a mpsc::Sender<Utterance>,
    audit: Option<&'a crate::audit::StreamingAuditSink>,
    /// The session's event channel; see [`stt_task`].
    events: Option<&'a tokio::sync::broadcast::Sender<super::events::PipelineEvent>>,
    turn_detector: &'a dyn TurnDetector,
    stt_name: &'a str,
    min_utterance_chars: usize,
}

impl Turn {
    fn new(utterance_id: u32) -> Self {
        Self {
            utterance_id,
            text: String::new(),
            saw_interim_text: false,
            last_final_segment: None,
            last_final_result: None,
            audio_start: None,
            stt_latency: None,
            capture: CaptureBuffer::default(),
            capture_sr: 0,
            dropped: 0,
        }
    }

    /// Keep `samples` for the capture. A turn that never finalises (a
    /// provider that stops emitting finals, an open mic with no speech) must
    /// not grow it without bound: past the cap the oldest samples go,
    /// drained to half so the shift amortises.
    fn hold_audio(&mut self, samples: &[i16], is_end: bool, rate: u32) {
        self.capture_sr = rate;
        let cap = rate.max(1) as usize * MAX_AUDIT_BUFFER_SECONDS;
        if let Some(drop) = self.capture.push(samples, is_end, cap) {
            tracing::warn!(
                utterance_id = self.utterance_id,
                dropped_samples = drop,
                cap,
                "stt_task: audit buffer exceeded cap; dropped oldest samples"
            );
        }
    }

    /// Fold a final into the turn's text. A final that leaves it empty ends
    /// the utterance: speech ended with nothing to commit, so no turn
    /// follows and nothing else will say why.
    fn fold_final(
        &mut self,
        sink: &TurnSink<'_>,
        result: &crate::stt::TranscriptionResult,
        cumulative_finals: bool,
    ) {
        merge_final_text(
            &mut self.text,
            self.last_final_segment,
            result,
            cumulative_finals,
        );
        self.last_final_segment = Some(result.segment_index);
        self.last_final_result = Some(result.clone());
        if self.text.trim().is_empty() {
            let heard_words = self.saw_interim_text;
            self.drop_turn(sink, "empty_transcript", heard_words);
        }
        self.saw_interim_text = false;
    }

    /// How long recognising the turn took so far, and its text, in the
    /// capture. The text is the turn's, not the last final's: a recogniser
    /// that finalises in pieces sends several finals per turn, and the
    /// capture must hold what the model is sent.
    fn record_final(&mut self, sink: &TurnSink<'_>) {
        let stt_latency = self.audio_start.map(|t| t.elapsed()).unwrap_or_default();
        self.stt_latency = Some(stt_latency);
        if let Some(a) = sink.audit {
            a.record(crate::audit::AuditEvent::SttResult {
                utterance_id: self.utterance_id,
                text: self.text.trim().to_string(),
                latency: stt_latency,
            });
        }
    }

    /// End the turn without sending it on, recorded as dropped for
    /// `reason`. Its words go, the next utterance is timed from its own
    /// start, its capture loses only this utterance's audio, and the
    /// detector starts afresh.
    fn drop_turn(&mut self, sink: &TurnSink<'_>, reason: &str, had_interim_text: bool) {
        self.dropped += 1;
        record_dropped(
            sink,
            self.utterance_id,
            reason,
            self.text.trim(),
            had_interim_text,
            self.audio_start,
        );
        self.text.clear();
        self.last_final_segment = None;
        self.last_final_result = None;
        // The audio stays with the capture under the reason it made no
        // turn: a session in which nothing was recognised is otherwise
        // silent on what the microphone actually carried.
        let samples = self.capture.take_oldest_utterance();
        if let Some(audit) = sink.audit
            && !samples.is_empty()
        {
            audit.record(crate::audit::AuditEvent::MicDropped {
                utterance_id: self.utterance_id,
                attempt: self.dropped,
                reason: reason.to_string(),
                audio: crate::audio::AudioFrame {
                    samples,
                    sample_rate: self.capture_sr,
                    channels: 1,
                },
            });
        }
        self.audio_start = None;
        sink.turn_detector.turn_ended();
    }

    /// The detector called the turn complete (`via` the result that came
    /// in, or the silence timer): too short, it is dropped as noise;
    /// otherwise it is committed. `false` once downstream has gone.
    async fn complete(
        &mut self,
        sink: &TurnSink<'_>,
        signal: &crate::stt::turn::TurnSignal,
        via: &'static str,
        sample_rate: u32,
        confidence: Option<f32>,
        endpointing_latency: std::time::Duration,
    ) -> bool {
        let chars = self.text.trim().chars().count();
        if chars < sink.min_utterance_chars {
            tracing::info!(
                utterance_id = self.utterance_id,
                chars,
                min = sink.min_utterance_chars,
                via,
                "discarding short utterance (likely noise)"
            );
            self.drop_turn(sink, "too_short", false);
            return true;
        }
        tracing::info!(
            utterance_id = self.utterance_id,
            signal = ?signal,
            detector = sink.turn_detector.name(),
            via,
            silence_ms = endpointing_latency.as_millis(),
            "turn complete"
        );
        self.commit_turn(sink, sample_rate, confidence, endpointing_latency)
            .await
    }

    /// Send the turn downstream, with its audio and timing in the capture,
    /// and start the next utterance. `false` once downstream has gone.
    async fn commit_turn(
        &mut self,
        sink: &TurnSink<'_>,
        sample_rate: u32,
        confidence: Option<f32>,
        endpointing_latency: std::time::Duration,
    ) -> bool {
        let text = std::mem::take(&mut self.text);
        self.last_final_segment = None;
        if let Some(a) = sink.audit {
            self.record_capture(a, sink.stt_name, sample_rate, &text, endpointing_latency);
        }
        let utterance = Utterance {
            text,
            utterance_id: self.utterance_id,
            confidence,
            stt_latency: self.stt_latency.take(),
            source: crate::pipeline::types::UtteranceSource::default(),
            endpointing_latency: Some(endpointing_latency),
            inference_metrics: self
                .last_final_result
                .as_ref()
                .and_then(|r| r.inference_metrics.clone()),
            committed_at: Some(std::time::Instant::now()),
        };
        if sink.text_tx.send(utterance).await.is_err() {
            return false;
        }
        self.last_final_result = None;
        self.utterance_id += 1;
        self.dropped = 0;
        self.audio_start = None;
        sink.turn_detector.turn_ended();
        true
    }

    /// The turn's microphone audio, and how long recognising and
    /// endpointing it took.
    fn record_capture(
        &mut self,
        audit: &crate::audit::StreamingAuditSink,
        stt_name: &str,
        sample_rate: u32,
        text: &str,
        endpointing_latency: std::time::Duration,
    ) {
        audit.record(crate::audit::AuditEvent::MicRaw {
            utterance_id: self.utterance_id,
            audio: crate::audio::AudioFrame {
                samples: self.capture.take(),
                sample_rate,
                channels: 1,
            },
        });
        if let Some(stt_dur) = self.stt_latency {
            let tenths = |d: std::time::Duration| (d.as_secs_f64() * 1000.0 * 10.0).round() / 10.0;
            audit.record(crate::audit::AuditEvent::Trace(
                crate::audit::TraceEvent::new(
                    format!("stt:{stt_name}"),
                    "stt_timing",
                    serde_json::json!({
                        "elapsed_ms": tenths(stt_dur),
                        "chars": text.trim().chars().count(),
                        "provider": stt_name,
                        "endpointing_ms": Some(tenths(endpointing_latency)),
                    }),
                ),
            ));
        }
    }
}

/// Bounded wait for the streaming-STT worker to finish its graceful
/// terminate handshake (`WsClient`'s post-`Terminate` deadline is 5 s).
const WORKER_JOIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(6);

/// Most mic audio retained for the current turn's capture. Sized in
/// seconds of the incoming rate; the newest audio is kept.
pub const MAX_AUDIT_BUFFER_SECONDS: usize = 120;

/// The microphone audio waiting to be recorded with a turn, and where each
/// utterance in it ends. Audio arrives ahead of the recogniser's answer, so
/// by the time an utterance is dropped (it transcribed to nothing, or to
/// too little) the next one may already be here; dropping takes only the
/// oldest utterance's audio, up to its end.
#[derive(Debug, Default)]
struct CaptureBuffer {
    samples: Vec<i16>,
    /// Sample offsets at which utterances ended, oldest first.
    ends: std::collections::VecDeque<usize>,
}

impl CaptureBuffer {
    /// Append `samples`, marking an utterance's end after them when
    /// `is_end`. Past `cap` the oldest samples go (drained to half, so the
    /// shift amortises); returns how many.
    fn push(&mut self, samples: &[i16], is_end: bool, cap: usize) -> Option<usize> {
        self.samples.extend_from_slice(samples);
        if is_end {
            self.ends.push_back(self.samples.len());
        }
        if self.samples.len() <= cap {
            return None;
        }
        let drop = self.samples.len() - cap / 2;
        self.samples.drain(..drop);
        self.ends.retain_mut(|end| {
            *end = end.saturating_sub(drop);
            *end > 0
        });
        Some(drop)
    }

    /// Remove and return the oldest utterance's audio: up to where it
    /// ended, or all of it when no end has arrived.
    fn take_oldest_utterance(&mut self) -> Vec<i16> {
        match self.ends.pop_front() {
            Some(end) => {
                let end = end.min(self.samples.len());
                let taken = self.samples.drain(..end).collect();
                for e in &mut self.ends {
                    *e = e.saturating_sub(end);
                }
                taken
            }
            None => std::mem::take(&mut self.samples),
        }
    }

    /// Everything held, for the turn being committed.
    fn take(&mut self) -> Vec<i16> {
        self.ends.clear();
        std::mem::take(&mut self.samples)
    }
}

/// Await an exited worker to learn why it stopped, then apply an exponential
/// respawn delay after failures so a persistent provider outage (bad key,
/// concurrency limit, network) cannot become a hot reconnect loop. Intra-
/// stream reconnects are the provider's job; this is the only respawn
/// authority at the pipeline layer.
/// How long a send to the recogniser may wait for room before the worker
/// is treated as stuck. Its input holds about two seconds of audio, so
/// five seconds without taking one chunk is not a recogniser falling behind
/// but one that stopped reading.
const STT_SEND_DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

/// How long a worker started to replace one that was given up on may
/// wait for the recogniser's inference slot. Aborting the old worker
/// cannot stop a blocking run it is inside; that run holds the slot until
/// the model returns (a model paging back in after a quiet spell on a
/// crowded host took tens of seconds), and a replacement refused after
/// the provider's own short wait failed every few seconds until the stall
/// cleared. A minute outlasts any stall worth recovering from.
const WORKER_RESTART_SLOT_WAIT: std::time::Duration = std::time::Duration::from_secs(60);

async fn worker_exit_backoff(
    handle: &mut tokio::task::JoinHandle<Result<(), crate::stt::SttError>>,
    consecutive_failures: &mut u32,
) {
    const BASE_DELAY_MS: u64 = 500;
    const MAX_DELAY: std::time::Duration = std::time::Duration::from_secs(10);

    let failed = match tokio::time::timeout(WORKER_JOIN_TIMEOUT, &mut *handle).await {
        // A clean return is not a failure, whatever its reason.
        Ok(Ok(Ok(()))) => false,
        Ok(Ok(Err(e))) => {
            tracing::error!(error = %e, "streaming STT failed");
            true
        }
        Ok(Err(join_err)) => {
            tracing::error!(error = %join_err, "streaming STT worker panicked");
            true
        }
        Err(_) => {
            // The caller is about to overwrite this handle with a
            // fresh worker. Dropping a `JoinHandle` detaches the task, so
            // without an explicit abort the old worker — typically holding
            // an open WebSocket to a paid STT provider — would keep running
            // invisibly, one leaked connection per stuck respawn.
            tracing::warn!("streaming STT worker did not exit in time; aborting it before respawn");
            handle.abort();
            // Abort is asynchronous; wait (briefly) for the future to be
            // dropped so the old provider stream is really gone before the
            // replacement opens a new one under the same identity.
            let _ = tokio::time::timeout(std::time::Duration::from_secs(1), &mut *handle).await;
            true
        }
    };

    if failed {
        *consecutive_failures += 1;
        let exp = consecutive_failures.saturating_sub(1).min(5);
        let delay = std::cmp::min(
            std::time::Duration::from_millis(BASE_DELAY_MS << exp),
            MAX_DELAY,
        );
        tracing::warn!(
            failures = *consecutive_failures,
            delay_ms = delay.as_millis() as u64,
            "streaming STT worker failed; delaying respawn"
        );
        tokio::time::sleep(delay).await;
    } else {
        *consecutive_failures = 0;
    }
}

/// Record that an utterance was thrown away, in the session's capture.
///
/// Written through the audit sink because this task is started before the
/// session's event channel exists; in-process observers therefore do not see
/// it, and the capture -- which is what a session is graded from -- does.
/// Announce speech that made no turn. On the session's event channel when
/// there is one, where the runner picks it up for the client and the
/// session recorder writes it to the capture; otherwise straight to the
/// capture. Never both: the recorder would write it twice.
fn record_dropped(
    sink: &TurnSink<'_>,
    utterance_id: u32,
    reason: &str,
    text: &str,
    had_interim_text: bool,
    audio_start: Option<std::time::Instant>,
) {
    let audio_ms = audio_start.map_or(0, |t| t.elapsed().as_millis() as u64);
    tracing::info!(
        utterance_id,
        reason,
        had_interim_text,
        audio_ms,
        "utterance dropped"
    );
    let event = crate::pipeline::events::PipelineEvent::UtteranceDropped {
        utterance_id,
        reason: reason.to_string(),
        text: text.to_string(),
        had_interim_text,
        audio_ms,
    };
    if let Some(events) = sink.events {
        // An error only means nobody is listening right now.
        let _ = events.send(event);
    } else if let Some(a) = sink.audit {
        a.record(crate::audit::AuditEvent::Pipeline(
            event,
            std::time::SystemTime::now(),
        ));
    }
}
