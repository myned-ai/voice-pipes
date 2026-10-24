//! Run one pipeline component against a known input and report what came out.
//!
//! Every other endpoint on the debug API reports what a component *is*: its
//! health, its config, its provider name. None of them can answer the question
//! that matters when a session transcribes nothing -- *given this audio, what
//! does this component actually do?* Answering it has meant writing a script
//! outside the framework every time, and a stage nobody can exercise alone is
//! a stage nobody can clear of suspicion.
//!
//! The VAD probe is the reason this module exists. It is the only stage that
//! *discards* audio, and its decisions appear nowhere but a chunk count in a
//! debug log. `audio_discarded_before_onset_ms` is the figure that no other
//! surface reports: how much of an utterance the recogniser was never shown.
//!
//! These endpoints accept caller-supplied input and run models on it, so they
//! live on the token-gated debug API and never on the public transport port.
//! One probe runs at a time per worker (see [`PROBE_PERMIT`]): a probe competes
//! with live sessions for the same threads, and refusing is louder than
//! queueing.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use crate::api::types::{ApiResponse, WorkerState};
use crate::pipeline::AudioChunk;

/// Largest inline audio payload accepted, in decoded bytes (~30 s at 16 kHz
/// mono). A probe is for a phrase, not a recording session.
const MAX_AUDIO_BYTES: usize = 16_000 * 2 * 30;

/// Longest a single probe may run before it is abandoned. Generous enough for
/// a cold model load, short enough that a wedged provider frees the permit.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_mins(2);

/// Frame RMS below which audio is room tone rather than speech, as a fraction
/// of full scale. Set under a quiet talker and over the floor of a live but
/// idle microphone.
const SPEECH_FLOOR_RMS: f64 = 0.01;

/// The same floor relative to the loudest frame of the utterance, so a hot
/// microphone's room tone is not mistaken for speech because it clears the
/// absolute floor.
const SPEECH_FLOOR_OF_PEAK: f64 = 0.1;

/// One probe at a time per worker.
///
/// A probe loads and runs a real model on the worker's threads. Allowing
/// several would let a caller starve the live sessions this worker exists to
/// serve, and queueing them would hide that: the second caller would see a
/// slow probe rather than a busy worker. Refuse instead.
static PROBE_PERMIT: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);

// ── Request / response types ────────────────────────────────

/// Where a probe gets its audio.
///
/// A fixture name is the reproducible path -- the same bytes on every machine
/// -- and is given as `?fixture=name`. Anything else is sent as a raw WAV in
/// the request body, which is how a caller probes audio it just recorded.
///
/// Raw bytes rather than base64 in a JSON field: base64 is behind an optional
/// cargo feature here, and a diagnostic should not need one. A WAV body also
/// keeps the request a third smaller and lets `curl --data-binary @file.wav`
/// work without a wrapper.
#[derive(Debug, Default, Deserialize)]
pub struct AudioInput {
    /// Fixture name, without extension (e.g. `"hello-how-are-you"`).
    #[serde(default)]
    pub fixture: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TextInput {
    pub text: String,
    /// Overrides the pipeline's system prompt for this call only (LLM probe).
    #[serde(default)]
    pub system_prompt: Option<String>,
}

/// One utterance as the VAD segmented it.
#[derive(Debug, Serialize)]
pub struct ProbedUtterance {
    pub seq: u64,
    /// Where the forwarded audio begins, relative to the input.
    pub onset_ms: u64,
    pub end_ms: u64,
    pub forwarded_speech_chunks: u64,
    pub forwarded_non_speech_chunks: u64,
    /// Audio forwarded from before the detector fired, in ms.
    pub pre_roll_ms_kept: u64,
    /// All audio dropped ahead of this utterance, speech or not. Mostly the
    /// silence before someone starts talking, which costs nothing; reported
    /// so the next figure has its context.
    pub audio_discarded_before_onset_ms: u64,
    /// **The number nothing else reports.** Of the audio dropped ahead of the
    /// utterance, how much carried speech -- the part the recogniser never
    /// sees. Separated from the figure above because a count of dropped audio
    /// cannot tell a second of lead-in silence from a lost first word, and an
    /// earlier version of this probe reported the first as the second.
    pub speech_discarded_before_onset_ms: u64,
}

#[derive(Debug, Serialize)]
pub struct VadProbeResponse {
    pub backend: String,
    pub input_duration_ms: u64,
    pub sample_rate: u32,
    pub pre_roll_ms: u32,
    pub silence_timeout_ms: u64,
    pub utterances: Vec<ProbedUtterance>,
    pub total_forwarded_ms: u64,
    /// Everything the VAD dropped, across the whole input.
    pub total_discarded_ms: u64,
    pub elapsed_ms: f64,
}

#[derive(Debug, Serialize)]
pub struct SttProbeResponse {
    pub provider: String,
    pub transcript: String,
    pub input_duration_ms: u64,
    pub processing_ms: f64,
    /// Audio seconds per wall second. Below 1.0 means slower than real time.
    pub rtfx: f64,
    /// True when the transcript holds no alphanumeric character -- speech went
    /// in and nothing came back. Called out here so a caller does not have to
    /// eyeball `".."` and decide what it means.
    pub empty: bool,
}

#[derive(Debug, Serialize)]
pub struct TtsProbeResponse {
    pub provider: String,
    pub chars: usize,
    pub audio_duration_ms: u64,
    pub synthesis_ms: f64,
    pub rtf: f64,
    pub sample_rate: u32,
    /// Name of the synthesised audio, fetchable via
    /// `GET /debug/probe/audio/{name}`. Not inlined: a reply of any length
    /// would bloat the JSON, and the store already streams files.
    pub audio_file: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct LlmProbeResponse {
    pub reply: String,
    pub ttft_ms: Option<f64>,
    pub total_ms: f64,
    pub chars: usize,
}

// ── Shared helpers ──────────────────────────────────────────

fn err(status: StatusCode, msg: impl Into<String>) -> (StatusCode, Json<serde_json::Value>) {
    (
        status,
        Json(serde_json::json!({ "data": null, "error": { "message": msg.into() } })),
    )
}

type ProbeError = (StatusCode, Json<serde_json::Value>);

/// Acquire the single probe permit, or say the worker is busy.
fn permit() -> Result<tokio::sync::SemaphorePermit<'static>, ProbeError> {
    PROBE_PERMIT.try_acquire().map_err(|_busy| {
        err(
            StatusCode::TOO_MANY_REQUESTS,
            "another probe is already running on this worker; probes run one at a time \
             so they cannot starve live sessions",
        )
    })
}

/// Directory holding the committed fixtures.
fn fixture_dir() -> PathBuf {
    match std::env::var("VP_FIXTURE_DIR") {
        Ok(dir) => PathBuf::from(dir),
        Err(_) => PathBuf::from("fixtures"),
    }
}

/// Resolve a fixture name to a path inside the fixture directory.
///
/// The name is a caller-supplied string used to build a filesystem path, so it
/// is restricted to a flat, boring alphabet: no separators, no parent
/// components, no extension. Anything else is a rejected request rather than a
/// creative path.
fn fixture_path(name: &str) -> Result<PathBuf, ProbeError> {
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(err(
            StatusCode::BAD_REQUEST,
            format!("invalid fixture name {name:?}: letters, digits, '-' and '_' only"),
        ));
    }
    let path = fixture_dir().join(format!("{name}.wav"));
    if !path.is_file() {
        return Err(err(
            StatusCode::NOT_FOUND,
            format!("no fixture named {name:?} in {}", fixture_dir().display()),
        ));
    }
    Ok(path)
}

/// Resolve a probe's audio: a named fixture, or a WAV in the request body.
fn load_audio(input: &AudioInput, body: &[u8]) -> Result<(Vec<i16>, u32), ProbeError> {
    if let Some(name) = &input.fixture {
        let path = fixture_path(name)?;
        return read_wav_file(&path);
    }
    if body.is_empty() {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "supply ?fixture=<name> or a WAV file as the request body",
        ));
    }
    let bytes = body;
    if bytes.len() > MAX_AUDIO_BYTES {
        return Err(err(
            StatusCode::PAYLOAD_TOO_LARGE,
            format!(
                "audio is {} bytes; the probe accepts at most {MAX_AUDIO_BYTES}",
                bytes.len()
            ),
        ));
    }
    // Written to a temp file because the WAV reader takes a path. Removed
    // before returning either way.
    let tmp = std::env::temp_dir().join(format!("vp-probe-{}.wav", uuid::Uuid::new_v4()));
    std::fs::write(&tmp, bytes).map_err(|e| {
        err(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("temp write: {e}"),
        )
    })?;
    let out = read_wav_file(&tmp);
    let _ = std::fs::remove_file(&tmp);
    out
}

fn read_wav_file(path: &Path) -> Result<(Vec<i16>, u32), ProbeError> {
    let frame = crate::audio::wav_reader::read_wav(path)
        .map_err(|e| err(StatusCode::BAD_REQUEST, format!("cannot read WAV: {e}")))?;
    Ok((frame.samples, frame.sample_rate))
}

/// A transcript that holds no letters or digits is not a transcript.
fn is_empty_transcript(text: &str) -> bool {
    !text.chars().any(char::is_alphanumeric)
}

// ── GET /debug/probe/fixtures ───────────────────────────────

/// List the fixtures this worker can be probed with.
///
/// Served from the committed `manifest.json` rather than from a list kept in
/// the caller, so the two cannot drift: a fixture added to the repo appears
/// here without anyone editing a UI.
pub async fn probe_fixtures() -> Result<Json<serde_json::Value>, ProbeError> {
    let path = fixture_dir().join("manifest.json");
    let text = std::fs::read_to_string(&path).map_err(|e| {
        err(
            StatusCode::NOT_FOUND,
            format!("no fixture manifest at {}: {e}", path.display()),
        )
    })?;
    let manifest: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("manifest: {e}")))?;
    Ok(Json(serde_json::json!({ "data": manifest, "error": null })))
}

// ── GET /debug/probe/audio/{name} ───────────────────────────

/// Prefix every probe-written audio file carries.
const PROBE_AUDIO_PREFIX: &str = "probe-tts-";

/// Is `name` exactly a file the TTS probe could have written?
///
/// The name comes from the URL and selects a file in a store, so it is
/// matched against the one shape the probe produces -- the prefix, a UUID,
/// `.wav` -- rather than screened for bad characters. Anything else in the
/// tap store (captures, other taps) stays unreachable through this route.
fn is_probe_audio_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix(PROBE_AUDIO_PREFIX) else {
        return false;
    };
    let Some(id) = rest.strip_suffix(".wav") else {
        return false;
    };
    uuid::Uuid::parse_str(id).is_ok()
}

/// How many probe recordings are kept; older ones are deleted as new ones
/// are written. The retention sweep covers session captures, not these.
const PROBE_AUDIO_KEEP: usize = 20;

/// Delete all but the newest `keep` probe recordings. Best-effort: a
/// failure is logged and the probe still answers.
async fn prune_probe_audio(store: &dyn crate::audit::store::AuditStore, keep: usize) {
    let mut probes: Vec<_> = match store.list("").await {
        Ok(blobs) => blobs
            .into_iter()
            .filter(|b| is_probe_audio_name(&b.key))
            .collect(),
        Err(e) => {
            tracing::warn!(error = %e, "cannot list probe recordings to prune them");
            return;
        }
    };
    // Newest first; a blob with no time sorts last and goes first.
    probes.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| b.key.cmp(&a.key))
    });
    for old in probes.iter().skip(keep) {
        if let Err(e) = store.delete(&old.key).await {
            tracing::warn!(error = %e, "cannot delete an old probe recording");
        }
    }
}

/// Serve audio the TTS probe synthesised, so it can be heard and not just
/// measured. An rtf tells you synthesis is fast; only listening tells you it
/// is intelligible.
pub async fn probe_audio(
    State(state): State<Arc<WorkerState>>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    if !is_probe_audio_name(&name) {
        return err(StatusCode::BAD_REQUEST, "not a probe audio file").into_response();
    }
    crate::api::orchestrator::helpers::serve_blob(&*state.tap_store, &name).await
}

// ── POST /debug/probe/vad ───────────────────────────────────

/// Segment supplied audio with the pipeline's configured VAD and report what
/// survived.
///
/// Drives the real [`crate::pipeline::vad_filter_task`] rather than
/// reimplementing its logic: a second copy of the segmentation rules would
/// answer for itself and not for the pipeline, and would drift the first time
/// either changed.
pub async fn probe_vad(
    State(state): State<Arc<WorkerState>>,
    Query(input): Query<AudioInput>,
    body: Bytes,
) -> Result<Json<ApiResponse<VadProbeResponse>>, ProbeError> {
    let _permit = permit()?;
    let (samples, sample_rate) = load_audio(&input, &body)?;
    if sample_rate == 0 || samples.is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "audio is empty"));
    }

    let vad = crate::config::build_vad(&state.config, None)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("build VAD: {e}")))?;
    let backend = vad.name().to_string();
    let silence_ms = state.config.silence_timeout_ms();
    let pre_roll_ms = state
        .config
        .vad
        .as_ref()
        .map_or(crate::config::DEFAULT_VAD_PRE_ROLL_MS, |v| {
            v.effective_pre_roll_ms()
        });

    let started = std::time::Instant::now();
    let (raw_tx, raw_rx) = tokio::sync::mpsc::channel::<AudioChunk>(256);
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<AudioChunk>(4096);
    tokio::spawn(crate::pipeline::vad_filter_task(
        raw_rx,
        out_tx,
        vad,
        silence_ms,
        pre_roll_ms,
        None,
    ));

    // Feed in 20 ms frames, as the transport does, then close so the task
    // flushes and exits.
    let frame = (sample_rate as usize / 50).max(1);
    // Kept for after the run: where each utterance begins is found by
    // matching what came out against what went in.
    let input = samples.clone();
    tokio::spawn(async move {
        for chunk in samples.chunks(frame) {
            let c = AudioChunk {
                samples: chunk.to_vec(),
                sample_rate,
                channels: 1,
                is_end_of_utterance: false,
                ..Default::default()
            };
            if raw_tx.send(c).await.is_err() {
                return;
            }
        }
    });

    // Each utterance as the chunks the filter forwarded for it.
    let mut segments: Vec<Vec<Vec<i16>>> = Vec::new();
    let mut cur: Vec<Vec<i16>> = Vec::new();
    let collect = async {
        while let Some(c) = out_rx.recv().await {
            if c.is_end_of_utterance {
                if !cur.is_empty() {
                    segments.push(std::mem::take(&mut cur));
                }
                continue;
            }
            if !c.samples.is_empty() {
                cur.push(c.samples);
            }
        }
        // Audio still open when the input ended: an utterance the silence
        // timer never closed.
        if !cur.is_empty() {
            segments.push(std::mem::take(&mut cur));
        }
    };
    tokio::time::timeout(PROBE_TIMEOUT, collect)
        .await
        .map_err(|_elapsed| err(StatusCode::GATEWAY_TIMEOUT, "VAD probe timed out"))?;

    let frame_rms = frame_energies(&input, frame);
    let frame_ms = 1000.0 * frame as f64 / f64::from(sample_rate);
    let to_ms = |frames: usize| (frames as f64 * frame_ms).round() as u64;

    let mut utterances: Vec<ProbedUtterance> = Vec::new();
    let mut searched_from = 0usize; // input frame where the previous utterance ended
    let mut forwarded_frames_total = 0usize;
    for (i, chunks) in segments.iter().enumerate() {
        // The filter forwards exact 20 ms copies of the input, so the first
        // forwarded chunk sits at a frame-aligned offset and can be located
        // precisely rather than estimated.
        let Some(start) = locate(&input, frame, chunks, searched_from) else {
            continue;
        };
        let len = chunks.len();
        let end = (start + len).min(frame_rms.len());
        forwarded_frames_total += len;

        // What was dropped between the previous utterance and this one, and
        // how much of it carried speech. The distinction is the whole point:
        // dropping the second of silence before someone starts talking costs
        // nothing, dropping the first syllable costs the word, and a count of
        // dropped audio cannot tell the two apart.
        let speech_frames = speech_frames_dropped(&frame_rms, searched_from, start, end);

        let pre_roll_frames = (pre_roll_ms as usize).div_ceil(20);
        utterances.push(ProbedUtterance {
            seq: i as u64 + 1,
            onset_ms: to_ms(start),
            end_ms: to_ms(end),
            forwarded_speech_chunks: len as u64,
            forwarded_non_speech_chunks: 0,
            pre_roll_ms_kept: to_ms(pre_roll_frames.min(start - searched_from)),
            audio_discarded_before_onset_ms: to_ms(start - searched_from),
            speech_discarded_before_onset_ms: to_ms(speech_frames),
        });
        searched_from = end;
    }

    let input_duration_ms = (input.len() as f64 * 1000.0 / f64::from(sample_rate)).round() as u64;
    let total_forwarded_ms = to_ms(forwarded_frames_total).min(input_duration_ms);

    Ok(Json(ApiResponse::ok(VadProbeResponse {
        backend,
        input_duration_ms,
        sample_rate,
        pre_roll_ms,
        silence_timeout_ms: silence_ms,
        utterances,
        total_forwarded_ms,
        total_discarded_ms: input_duration_ms.saturating_sub(total_forwarded_ms),
        elapsed_ms: started.elapsed().as_secs_f64() * 1000.0,
    })))
}

/// Frame RMS, as a fraction of full scale, one value per `frame` samples.
pub(crate) fn frame_energies(samples: &[i16], frame: usize) -> Vec<f64> {
    samples
        .chunks(frame)
        .map(|c| {
            let sum: f64 = c.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
            (sum / c.len().max(1) as f64).sqrt() / 32768.0
        })
        .collect()
}

/// How many frames in `[from, start)` -- dropped ahead of an utterance that
/// runs `[start, end)` -- carried speech rather than room tone.
///
/// Judged against the utterance itself as well as an absolute floor, so a hot
/// microphone's room tone does not count as speech merely because it is loud.
pub(crate) fn speech_frames_dropped(
    frame_rms: &[f64],
    from: usize,
    start: usize,
    end: usize,
) -> usize {
    let end = end.min(frame_rms.len());
    let peak = frame_rms
        .get(start..end)
        .unwrap_or_default()
        .iter()
        .copied()
        .fold(0.0f64, f64::max);
    let floor = SPEECH_FLOOR_RMS.max(peak * SPEECH_FLOOR_OF_PEAK);
    frame_rms
        .get(from..start)
        .unwrap_or_default()
        .iter()
        .filter(|&&r| r >= floor)
        .count()
}

/// Frame index at which the forwarded `chunks` begin in `input`, searching
/// frame-aligned offsets from `from`.
///
/// Matches the whole run, not its first chunk. The first chunk is pre-roll,
/// and pre-roll over digital silence is a frame of zeros -- which matches the
/// first silent frame anywhere in the file, placing an utterance that begins
/// at 400 ms at zero. The run as a whole is contiguous input and occurs once.
fn locate(input: &[i16], frame: usize, chunks: &[Vec<i16>], from: usize) -> Option<usize> {
    let frames = input.len().div_ceil(frame);
    (from..frames).find(|&f| {
        chunks.iter().enumerate().all(|(k, chunk)| {
            let at = (f + k) * frame;
            input.get(at..at + chunk.len()) == Some(chunk.as_slice())
        })
    })
}

// ── POST /debug/probe/stt ───────────────────────────────────

pub async fn probe_stt(
    State(state): State<Arc<WorkerState>>,
    Query(input): Query<AudioInput>,
    body: Bytes,
) -> Result<Json<ApiResponse<SttProbeResponse>>, ProbeError> {
    let _permit = permit()?;
    let (samples, sample_rate) = load_audio(&input, &body)?;
    if samples.is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "audio is empty"));
    }

    let provider = crate::config::build_stt_provider(&state.config, None)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("build STT: {e}")))?;
    let provider_name = crate::config::stt_provider_name(&state.config).unwrap_or_default();

    // Match the rate the provider expects, exactly as the pipeline does.
    let want = provider.expected_sample_rate();
    let frame = crate::audio::resample::resample(
        crate::audio::AudioFrame {
            samples,
            sample_rate,
            channels: 1,
        },
        want,
    )
    .map_err(|e| err(StatusCode::BAD_REQUEST, e.to_string()))?;
    let input_duration_ms =
        (frame.samples.len() as f64 / frame.sample_rate as f64 * 1000.0).round() as u64;

    let started = std::time::Instant::now();
    let result = tokio::time::timeout(PROBE_TIMEOUT, provider.transcribe(&frame))
        .await
        .map_err(|_elapsed| err(StatusCode::GATEWAY_TIMEOUT, "STT probe timed out"))?
        .map_err(|e| {
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("transcribe: {e}"),
            )
        })?;
    let processing_ms = started.elapsed().as_secs_f64() * 1000.0;

    let transcript = result.text.trim().to_string();
    Ok(Json(ApiResponse::ok(SttProbeResponse {
        provider: provider_name,
        empty: is_empty_transcript(&transcript),
        transcript,
        input_duration_ms,
        processing_ms,
        rtfx: if processing_ms > 0.0 {
            input_duration_ms as f64 / processing_ms
        } else {
            0.0
        },
    })))
}

// ── POST /debug/probe/tts ───────────────────────────────────

pub async fn probe_tts(
    State(state): State<Arc<WorkerState>>,
    Json(input): Json<TextInput>,
) -> Result<Json<ApiResponse<TtsProbeResponse>>, ProbeError> {
    let _permit = permit()?;
    if input.text.trim().is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "text is empty"));
    }

    let store = crate::config::build_tts_param_store(&state.config);
    let provider = crate::config::build_tts_provider(&state.config, None, Some(&store))
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("build TTS: {e}")))?;
    let provider_name = crate::config::tts_provider_name(&state.config).unwrap_or_default();

    let started = std::time::Instant::now();
    let frame = tokio::time::timeout(PROBE_TIMEOUT, provider.synthesize(&input.text))
        .await
        .map_err(|_elapsed| err(StatusCode::GATEWAY_TIMEOUT, "TTS probe timed out"))?
        .map_err(|e| {
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("synthesize: {e}"),
            )
        })?;
    let synthesis_ms = started.elapsed().as_secs_f64() * 1000.0;

    let audio_duration_ms =
        (frame.samples.len() as f64 / frame.sample_rate.max(1) as f64 * 1000.0).round() as u64;

    // Written to the tap store rather than inlined: the caller fetches it from
    // `GET /debug/probe/audio/{name}`, which streams it, and the JSON stays
    // small enough to read. Only the newest few are kept.
    let name = format!("probe-tts-{}.wav", uuid::Uuid::new_v4());
    let audio_file = match crate::audio::wav_writer::encode_wav(&frame) {
        Ok(bytes) => match state.tap_store.put(&name, &bytes).await {
            Ok(()) => {
                prune_probe_audio(&*state.tap_store, PROBE_AUDIO_KEEP).await;
                Some(name)
            }
            Err(e) => {
                tracing::warn!(error = %e, "TTS probe could not store audio");
                None
            }
        },
        Err(e) => {
            tracing::warn!(error = %e, "TTS probe could not encode audio");
            None
        }
    };

    Ok(Json(ApiResponse::ok(TtsProbeResponse {
        provider: provider_name,
        chars: input.text.chars().count(),
        audio_duration_ms,
        synthesis_ms,
        rtf: if audio_duration_ms > 0 {
            synthesis_ms / audio_duration_ms as f64
        } else {
            0.0
        },
        sample_rate: frame.sample_rate,
        audio_file,
    })))
}

// ── POST /debug/probe/llm ───────────────────────────────────

pub async fn probe_llm(
    State(state): State<Arc<WorkerState>>,
    Json(input): Json<TextInput>,
) -> Result<Json<ApiResponse<LlmProbeResponse>>, ProbeError> {
    let _permit = permit()?;
    if input.text.trim().is_empty() {
        return Err(err(StatusCode::BAD_REQUEST, "text is empty"));
    }

    let mut cfg = state.config.clone();
    // A probe asks what the model says to one message, so the prompt override
    // applies to this call only and never touches the running pipeline.
    if let (Some(prompt), Some(tp)) = (&input.system_prompt, cfg.text_processor.first_mut()) {
        tp.options.insert(
            "system_prompt".into(),
            serde_json::Value::String(prompt.clone()),
        );
    }

    let mut processors = crate::config::build_text_processors(&cfg, None, None)
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("build LLM: {e}")))?;
    if processors.is_empty() {
        return Err(err(
            StatusCode::BAD_REQUEST,
            "this pipeline has no text processor configured",
        ));
    }
    // The first processor is the model; later ones are filters over its
    // output, and a probe is asking what the model itself says.
    let processor = processors.remove(0);

    let started = std::time::Instant::now();
    let reply = tokio::time::timeout(PROBE_TIMEOUT, processor.process(&input.text))
        .await
        .map_err(|_elapsed| err(StatusCode::GATEWAY_TIMEOUT, "LLM probe timed out"))?
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, format!("generate: {e}")))?;
    let total_ms = started.elapsed().as_secs_f64() * 1000.0;

    Ok(Json(ApiResponse::ok(LlmProbeResponse {
        chars: reply.chars().count(),
        reply,
        // The batch path reports no first-token time; the streaming session
        // path is where TTFT is measured.
        ttft_ms: None,
        total_ms,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_transcript_of_punctuation_counts_as_empty() {
        // The exact shape a recogniser returns when it was handed speech and
        // found no words in it.
        for t in ["..", "", "   ", "...", "?!", "—"] {
            assert!(is_empty_transcript(t), "{t:?} should count as empty");
        }
        for t in ["hello", "42", "o, how are you?"] {
            assert!(!is_empty_transcript(t), "{t:?} should not count as empty");
        }
    }

    #[test]
    fn silence_before_speech_is_not_counted_as_lost_speech() {
        // The false alarm this replaced: a recording with a second of quiet
        // before the speaker began was reported as "900 ms cut from the
        // front of the utterance -- enough to lose the first word", because
        // the probe counted dropped audio rather than dropped speech. Here,
        // fifty frames of room tone precede an utterance and none of it is
        // speech.
        let mut rms = vec![0.002; 50];
        rms.extend(vec![0.2; 40]);
        assert_eq!(speech_frames_dropped(&rms, 0, 50, 90), 0);
    }

    #[test]
    fn speech_dropped_before_the_onset_is_counted() {
        // The detector fired late: twelve frames of the word's quiet start
        // were dropped before forwarding began.
        let mut rms = vec![0.002; 30];
        rms.extend(vec![0.05; 12]); // "the", unstressed but plainly speech
        rms.extend(vec![0.3; 40]); // what the detector caught
        assert_eq!(speech_frames_dropped(&rms, 0, 42, 82), 12);
    }

    #[test]
    fn a_hot_microphone_s_room_tone_is_not_speech() {
        // Room tone that clears the absolute floor but is a small fraction of
        // the utterance's level is still room tone.
        let mut rms = vec![0.015; 30];
        rms.extend(vec![0.4; 40]);
        assert_eq!(speech_frames_dropped(&rms, 0, 30, 70), 0);
    }

    #[test]
    fn forwarded_audio_is_located_exactly_in_the_input() {
        let frame = 320;
        let mut input = vec![0i16; frame * 10];
        for (i, s) in input[frame * 6..frame * 7].iter_mut().enumerate() {
            *s = i as i16 + 1;
        }
        let run = vec![input[frame * 6..frame * 7].to_vec()];
        assert_eq!(locate(&input, frame, &run, 0), Some(6));
        assert_eq!(
            locate(&input, frame, &run, 7),
            None,
            "search must honour `from`"
        );
    }

    #[test]
    fn pre_roll_over_digital_silence_is_not_placed_at_the_start() {
        // Twenty silent frames, then speech from frame 20. The filter forwards
        // five frames of pre-roll (all zeros) and then the speech. Matching the
        // first chunk alone finds a zero frame at 0; the run occurs only at 15.
        let frame = 320;
        let mut input = vec![0i16; frame * 30];
        for s in &mut input[frame * 20..] {
            *s = 1000;
        }
        let run: Vec<Vec<i16>> = (15..25)
            .map(|f| input[f * frame..(f + 1) * frame].to_vec())
            .collect();
        assert_eq!(locate(&input, frame, &run, 0), Some(15));
    }

    #[test]
    fn only_files_the_tts_probe_writes_can_be_fetched() {
        // The route reads from the tap store, which also holds captures; the
        // name must be exactly the shape the probe produces.
        let ok = format!("probe-tts-{}.wav", uuid::Uuid::new_v4());
        assert!(is_probe_audio_name(&ok));
        for bad in [
            "probe-tts-.wav",
            "probe-tts-not-a-uuid.wav",
            "session_audio.wav",
            "../probe-tts-00000000-0000-0000-0000-000000000000.wav",
            "probe-tts-00000000-0000-0000-0000-000000000000.wav.txt",
            "probe-tts-00000000-0000-0000-0000-000000000000",
        ] {
            assert!(!is_probe_audio_name(bad), "{bad:?} must not be servable");
        }
    }

    #[test]
    fn fixture_names_cannot_escape_the_fixture_directory() {
        // The name builds a filesystem path from caller input.
        for bad in [
            "../secret",
            "a/b",
            "a\\b",
            "..",
            "",
            "with space",
            "semi;colon",
        ] {
            assert!(
                fixture_path(bad).is_err(),
                "{bad:?} should be rejected as a fixture name"
            );
        }
    }

    /// Writing one probe recording past the limit leaves the limit, and the
    /// recording that goes is the oldest; other files in the store stay.
    #[tokio::test]
    async fn probe_recordings_are_kept_to_the_newest_few() {
        let dir = std::env::temp_dir().join(format!("vp-probe-prune-{}", uuid::Uuid::new_v4()));
        let store = crate::audit::store::LocalFsAuditStore::new(dir.clone());
        let mut names = Vec::new();
        for _ in 0..4 {
            let name = format!("{PROBE_AUDIO_PREFIX}{}.wav", uuid::Uuid::new_v4());
            crate::audit::store::AuditStore::put(&store, &name, b"RIFF")
                .await
                .unwrap();
            names.push(name);
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        crate::audit::store::AuditStore::put(&store, "a-capture.wav", b"RIFF")
            .await
            .unwrap();

        prune_probe_audio(&store, 3).await;

        let left: Vec<String> = crate::audit::store::AuditStore::list(&store, "")
            .await
            .unwrap()
            .into_iter()
            .map(|b| b.key)
            .collect();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!left.contains(&names[0]), "the oldest stayed: {left:?}");
        for kept in &names[1..] {
            assert!(left.contains(kept), "{kept} went: {left:?}");
        }
        assert!(left.contains(&"a-capture.wav".to_string()), "{left:?}");
    }
}
