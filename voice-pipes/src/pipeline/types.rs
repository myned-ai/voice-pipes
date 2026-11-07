use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use tokio_util::sync::CancellationToken;

use crate::audio::AudioFrame;

// ── Pipeline state & session ────────────────────────────────

/// Pipeline processing state, tracked atomically so the interrupt
/// monitor can read it without locking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TurnState {
    Listening = 0,
    Processing = 1,
    Speaking = 2,
    Draining = 3,
}

impl TurnState {
    /// Convert a raw `u8` back into a [`TurnState`], defaulting to `Listening`.
    pub fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Processing,
            2 => Self::Speaking,
            3 => Self::Draining,
            _ => Self::Listening,
        }
    }
}

/// Shared atomic pipeline state.
pub type SharedTurnState = Arc<AtomicU8>;

/// Create a new shared turn state initialised to [`TurnState::Listening`].
pub fn new_turn_state() -> SharedTurnState {
    Arc::new(AtomicU8::new(TurnState::Listening as u8))
}

/// Atomically update the shared turn state.
pub fn set_turn_state(state: &SharedTurnState, s: TurnState) {
    state.store(s as u8, Ordering::Release);
}

/// Atomically read the current turn state.
pub fn get_turn_state(state: &SharedTurnState) -> TurnState {
    TurnState::from_u8(state.load(Ordering::Acquire))
}

/// Per-utterance session.  When the user interrupts, call
/// `cancel.cancel()` to stop all in-flight LLM/TTS/playback work.
#[derive(Debug)]
pub struct PipelineTurn {
    pub cancel: CancellationToken,
}

impl Default for PipelineTurn {
    fn default() -> Self {
        Self::new()
    }
}

impl PipelineTurn {
    /// Start a new turn with a fresh cancellation token.
    pub fn new() -> Self {
        Self {
            cancel: CancellationToken::new(),
        }
    }
}

// ── Pipeline message types ──────────────────────────────────

/// Word-level timing boundary within a TTS fragment.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WordTiming {
    /// The word text.
    pub word: String,
    /// Offset from fragment audio start, in milliseconds.
    pub offset_ms: u32,
    /// Duration of the word in milliseconds (0 if unknown).
    pub duration_ms: u32,
}

/// A provider's word boundary, placed on the fragment's timeline.
impl From<voice_pipes_sdk::tts::WordBoundary> for WordTiming {
    fn from(b: voice_pipes_sdk::tts::WordBoundary) -> Self {
        Self {
            word: b.word,
            offset_ms: b.start_ms,
            duration_ms: b.duration_ms,
        }
    }
}

/// Caption metadata attached to the first audio chunk of a TTS fragment.
/// Associates a text sentence/fragment with the audio that speaks it.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CaptionMeta {
    /// The text being spoken in this audio segment.
    pub text: String,
    /// Zero-based index of this fragment within the utterance.
    pub fragment_idx: u32,
    /// True if this is the last fragment of the utterance.
    pub is_final: bool,
    /// The total audio duration of this fragment in milliseconds,
    /// computed before audio streaming begins.
    #[serde(rename = "estimated_duration_ms")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_fragment_duration_ms: Option<u32>,
    /// Word-level timing boundaries, if available from the TTS provider.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub word_timings: Option<Vec<WordTiming>>,
    /// Spoken text sent to the TTS synthesizer (after sanitization, tag filtering, and lexicon).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub spoken_text: Option<String>,
    /// Applied inline directives and pronunciation lexicon entries.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tts_directives: Vec<voice_pipes_sdk::TtsDirectiveRecord>,
    /// Directives stripped because unsupported by the provider or not in passthrough allowlist.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stripped_directives: Vec<voice_pipes_sdk::StrippedDirectiveRecord>,
}

/// A chunk of PCM audio flowing through the streaming pipeline.
#[derive(Debug, Clone, Default)]
pub struct AudioChunk {
    pub samples: Vec<i16>,
    pub sample_rate: u32,
    pub channels: u16,
    /// True if this is the last chunk of the current utterance.
    pub is_end_of_utterance: bool,
    /// Caption metadata -- present on the first audio chunk of each TTS fragment.
    pub caption: Option<CaptionMeta>,
    /// Server-side utterance ID.  Set by the TTS task so the client can
    /// validate that caption audio belongs to the currently active turn.
    /// Defaults to 0; non-captioned chunks carry whatever value was last set.
    pub utterance_id: u32,
    /// Full LLM response text -- set ONLY on the end-of-utterance sentinel.
    /// Travels through the same ordered pipeline as audio so it cannot race
    /// with per-fragment caption data.  The client uses this as a final
    /// confirmation / fallback once all captions have been delivered.
    pub full_text: Option<String>,
    /// Open extension slot for [`AudioPostProcessor`](crate::pipeline::AudioPostProcessor)s.
    /// When present, the value is forwarded verbatim to the client under
    /// the `extras` key of the `audio_meta` JSON frame.
    pub extras: Option<serde_json::Value>,
    /// Monotonic timestamp of the last VAD-classified speech frame
    /// (`prob >= threshold`), populated by `vad_filter_task` so downstream
    /// `stt_task` measures `endpointing_ms` from the true end of speech
    /// (including VAD silence hangover).
    pub last_speech_at: Option<std::time::Instant>,
    /// Active TTS synthesis time across fragments (populated on the end-of-utterance sentinel).
    pub synth_duration: Option<std::time::Duration>,
}

// ── AudioLedger: text<->audio mapping ────────────────────────

/// Tracks the correspondence between text fragments and produced audio,
/// enabling precise truncation when a playback position (ms) is known.
///
/// Built incrementally inside `process_utterance` as audio chunks arrive.
/// Stored per-utterance in the runner so that a `PlaybackReport` can look
/// up exactly which text the user heard.
#[derive(Debug, Clone, Default)]
pub struct AudioLedger {
    /// One entry per text fragment, in order.
    pub entries: Vec<AudioLedgerEntry>,
    /// Total PCM samples across all entries.
    pub total_samples: u64,
    /// Total playback duration in milliseconds.
    pub total_duration_ms: u64,
}

/// A single fragment's audio mapping inside the ledger.
#[derive(Debug, Clone)]
pub struct AudioLedgerEntry {
    /// The text that produced this audio.
    pub text: String,
    /// Byte offset into the full concatenated LLM response.
    pub text_offset: usize,
    /// Zero-based fragment index.
    pub fragment_idx: u32,
    /// Total PCM samples produced for this fragment.
    pub sample_count: u64,
    /// Duration of this fragment's audio in milliseconds.
    pub duration_ms: u64,
    /// Cumulative playback position at the *end* of this fragment (ms).
    pub cumulative_ms: u64,
    /// Word-level timing boundaries within this fragment, if available.
    /// Offsets are relative to the *fragment* audio start.
    pub word_timings: Option<Vec<WordTiming>>,
    /// The sanitized spoken text sent to the synthesizer.
    pub spoken_text: Option<String>,
}

/// Return the prefix of `text` containing up to `count` whitespace-delimited words.
/// UTF-8 safe and never slices inside a code point.
fn nth_word_prefix(text: &str, count: usize) -> &str {
    if count == 0 {
        return "";
    }
    let mut words_seen = 0;
    let mut in_word = false;
    let mut end_byte = 0;
    for (idx, ch) in text.char_indices() {
        if ch.is_whitespace() {
            if in_word {
                in_word = false;
                if words_seen >= count {
                    return text.get(..end_byte).unwrap_or(text);
                }
            }
        } else {
            if !in_word {
                in_word = true;
                words_seen += 1;
            }
            end_byte = idx + ch.len_utf8();
        }
    }
    if words_seen <= count {
        text
    } else {
        text.get(..end_byte).unwrap_or(text)
    }
}

/// Return the prefix of `text` corresponding to `frac` (0.0..=1.0) of its
/// characters, snapped back to the last whole word so we never surface a
/// mid-word cut. Used to approximate how much of a fragment was heard when
/// no word-level timings are available. UTF-8 safe.
fn proportional_prefix(text: &str, frac: f64) -> &str {
    if frac >= 1.0 {
        return text;
    }
    if frac <= 0.0 {
        return "";
    }
    let total_chars = text.chars().count();
    let target = ((total_chars as f64) * frac).round() as usize;
    if target == 0 {
        return "";
    }
    // Byte index just past the `target`-th character.
    let byte_idx = text
        .char_indices()
        .nth(target)
        .map_or(text.len(), |(b, _)| b);
    let slice = &text[..byte_idx];
    // If the cut lands exactly on a word boundary (next char is whitespace, or
    // we hit the end), the final word is whole — keep it. Otherwise snap back to
    // the previous boundary so we never surface a mid-word cut as "heard". A
    // single-word fragment (no interior whitespace) keeps its proportional
    // character prefix so long single-word fragments still reflect partial play.
    let cut_at_boundary = text[byte_idx..]
        .chars()
        .next()
        .is_none_or(char::is_whitespace);
    if cut_at_boundary {
        slice.trim_end()
    } else {
        match slice.trim_end().rfind(char::is_whitespace) {
            Some(ws) => &text[..ws],
            None => slice.trim_end(),
        }
    }
}

impl AudioLedger {
    /// Record audio samples for a fragment.  Call once per fragment after
    /// all its audio chunks have been forwarded.
    pub fn push(
        &mut self,
        text: String,
        spoken_text: Option<String>,
        text_offset: usize,
        fragment_idx: u32,
        sample_count: u64,
        sample_rate: u32,
        word_timings: Option<Vec<WordTiming>>,
    ) {
        let duration_ms = if sample_rate > 0 {
            sample_count * 1000 / sample_rate as u64
        } else {
            0
        };
        self.total_samples += sample_count;
        self.total_duration_ms += duration_ms;
        self.entries.push(AudioLedgerEntry {
            text,
            spoken_text,
            text_offset,
            fragment_idx,
            sample_count,
            duration_ms,
            cumulative_ms: self.total_duration_ms,
            word_timings,
        });
    }

    /// Given a playback position in milliseconds, return the text that was
    /// played up to that point.
    ///
    /// When word-level timings are available for the fragment that was
    /// playing at `played_ms`, the text is truncated to the last word
    /// whose *start* was reached.  When they are absent, the fragment is
    /// approximated by the *fraction* of its audio that was heard
    /// (proportional character prefix snapped to a word boundary) rather
    /// than claiming the whole fragment — otherwise a provider that emits
    /// one big sentence-fragment with no word timings (e.g. pocket-http)
    /// would retain the entire sentence for any offset that merely reaches
    /// into it, badly over-claiming what the user heard.
    ///
    /// Returns `None` if the ledger is empty or `played_ms` is 0.
    pub fn text_at_offset(&self, played_ms: u64) -> Option<String> {
        if self.entries.is_empty() || played_ms == 0 {
            return None;
        }
        let mut result = String::new();
        for entry in &self.entries {
            let fragment_start_ms = entry.cumulative_ms.saturating_sub(entry.duration_ms);
            if played_ms > fragment_start_ms {
                // Playback reached into (or past) this fragment.
                if played_ms >= entry.cumulative_ms {
                    // Fully played -- include the entire fragment text.
                    result.push_str(&entry.text);
                } else if let Some(ref wt) = entry.word_timings {
                    // Partially played and we have word timings.
                    // `offset_within` is ms from the fragment's audio start.
                    let offset_within = played_ms - fragment_start_ms;
                    let mut last_included = 0usize;
                    // Search position advances to handle repeated words.
                    let mut search_from = 0usize;
                    let mut heard_count = 0usize;
                    let total_words = wt.len();
                    for w in wt {
                        if (w.offset_ms as u64) <= offset_within {
                            heard_count += 1;
                            // Word playback started -> user heard at least
                            // the beginning of this word.
                            if let Some(pos) =
                                entry.text.get(search_from..).and_then(|s| s.find(&w.word))
                            {
                                let abs_pos = search_from + pos;
                                last_included = abs_pos + w.word.len();
                                search_from = last_included;
                            }
                        } else {
                            break;
                        }
                    }
                    if heard_count >= total_words && total_words > 0 {
                        result.push_str(&entry.text);
                    } else {
                        let word_prefix = nth_word_prefix(&entry.text, heard_count);
                        let slice_len = last_included.max(word_prefix.len());
                        if slice_len > 0
                            && let Some(prefix) = entry.text.get(..slice_len)
                        {
                            result.push_str(prefix);
                        }
                    }
                } else {
                    // No word timings -- approximate by the fraction of this
                    // fragment's audio that was heard instead of claiming the
                    // whole fragment. Snapped to a word boundary so we never
                    // report a mid-word cut as "heard".
                    let frac = if entry.duration_ms > 0 {
                        ((played_ms - fragment_start_ms) as f64 / entry.duration_ms as f64)
                            .clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    result.push_str(proportional_prefix(&entry.text, frac));
                }
            } else {
                break;
            }
        }
        if result.is_empty() {
            None
        } else {
            Some(result)
        }
    }
}

impl AudioChunk {
    /// Wrap an [`AudioFrame`] into a chunk, marking whether it ends the utterance.
    pub fn from_frame(frame: AudioFrame, is_end: bool) -> Self {
        Self {
            samples: frame.samples,
            sample_rate: frame.sample_rate,
            channels: frame.channels,
            is_end_of_utterance: is_end,
            caption: None,
            utterance_id: 0,
            full_text: None,
            extras: None,
            last_speech_at: None,
            synth_duration: None,
        }
    }

    /// Clone this chunk back into an [`AudioFrame`].
    pub fn to_frame(&self) -> AudioFrame {
        AudioFrame {
            samples: self.samples.clone(),
            sample_rate: self.sample_rate,
            channels: self.channels,
        }
    }
}

/// Whether the utterance originated from speech (STT), text input, or
/// the agent itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UtteranceSource {
    /// Transcribed from audio via STT.
    #[default]
    Audio,
    /// Typed text input (bypassed STT).
    Text,
    /// Agent-initiated turn: `text` is the assistant output, spoken
    /// verbatim via a TTS-only path (LLM chain skipped). No user
    /// message precedes this turn.
    AgentSay,
    /// Agent-initiated turn: `text` is injected context (a tool
    /// result, an external event) and the LLM chain generates the
    /// spoken reply via `TextProcessor::process_context_stream`. No
    /// user message precedes this turn.
    AgentGenerate,
}

impl UtteranceSource {
    /// `true` for turns initiated by the agent rather than the user.
    pub fn is_agent(self) -> bool {
        matches!(
            self,
            UtteranceSource::AgentSay | UtteranceSource::AgentGenerate
        )
    }
}

/// Transcription result passed from STT to LLM.
#[derive(Debug, Clone, Default)]
pub struct Utterance {
    pub text: String,
    pub utterance_id: u32,
    /// Overall confidence score from STT (0.0-1.0), if available.
    pub confidence: Option<f32>,
    /// STT processing time (end-of-speech audio flush -> final transcript).
    pub stt_latency: Option<std::time::Duration>,
    /// Where this utterance came from.
    pub source: UtteranceSource,
    /// Time from last speech audio frame to turn-commit decision (VAD silence wait).
    pub endpointing_latency: Option<std::time::Duration>,
    /// Streaming-inference metrics reported by the STT provider on the
    /// final result (`RTFx`, algorithmic delay, inference count, ...).
    /// `None` for text input and providers that don't report.
    pub inference_metrics: Option<crate::stt::InferenceMetrics>,
    /// Monotonic instant when the turn was committed (`stt_task` turn commit,
    /// text injection, or agent-initiated turn creation). Used by
    /// `process_utterance` and `TurnTracker` so every downstream stage is
    /// anchored at the true turn commit rather than task-spawn time.
    pub committed_at: Option<std::time::Instant>,
}

// ── Interrupt monitor settings ──────────────────────────────

/// Configuration for the interrupt (barge-in) monitor.
///
/// Fields are stored in atomic cells so the live parameter editor can
/// mutate them at runtime without rebuilding the engine. `Clone` shares
/// the cells (cheap `Arc` bump), so handing the same settings to multiple
/// engines means they all observe live updates.
#[derive(Debug, Clone)]
pub struct BargeInSettings {
    speech_threshold: std::sync::Arc<std::sync::atomic::AtomicU64>, // f64 bits
    min_chunks: std::sync::Arc<std::sync::atomic::AtomicI64>,
    barge_in_during_processing: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl BargeInSettings {
    /// Construct from plain values (each engine gets its own atomics).
    pub fn new(speech_threshold: f64, min_chunks: u32, barge_in_during_processing: bool) -> Self {
        use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64};
        Self {
            speech_threshold: Arc::new(AtomicU64::new(speech_threshold.to_bits())),
            min_chunks: Arc::new(AtomicI64::new(min_chunks as i64)),
            barge_in_during_processing: Arc::new(AtomicBool::new(barge_in_during_processing)),
        }
    }

    /// RMS threshold for speech detection during interrupt monitoring.
    pub fn speech_threshold(&self) -> f64 {
        f64::from_bits(
            self.speech_threshold
                .load(std::sync::atomic::Ordering::Relaxed),
        )
    }

    /// Minimum consecutive speech chunks required before triggering an
    /// interrupt.  Prevents false positives from brief noise bursts.
    pub fn min_chunks(&self) -> u32 {
        self.min_chunks
            .load(std::sync::atomic::Ordering::Relaxed)
            .max(0) as u32
    }

    /// Whether to allow the VAD to trigger barge-in during the Processing state
    /// (before any audio has actually played to the user).
    pub fn barge_in_during_processing(&self) -> bool {
        self.barge_in_during_processing
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Test helper / runtime override.
    pub fn set_barge_in_during_processing(&self, v: bool) {
        self.barge_in_during_processing
            .store(v, std::sync::atomic::Ordering::Relaxed);
    }

    /// Replace the speech-threshold cell with one shared from a parameter store.
    #[must_use]
    pub fn with_speech_threshold_cell(
        mut self,
        cell: std::sync::Arc<std::sync::atomic::AtomicU64>,
    ) -> Self {
        self.speech_threshold = cell;
        self
    }

    /// Replace the min-chunks cell with one shared from a parameter store.
    #[must_use]
    pub fn with_min_chunks_cell(
        mut self,
        cell: std::sync::Arc<std::sync::atomic::AtomicI64>,
    ) -> Self {
        self.min_chunks = cell;
        self
    }

    /// Replace the barge-in-during-processing cell with one shared from a parameter store.
    #[must_use]
    pub fn with_barge_in_during_processing_cell(
        mut self,
        cell: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        self.barge_in_during_processing = cell;
        self
    }
}

impl Default for BargeInSettings {
    fn default() -> Self {
        Self::new(200.0, 3, false)
    }
}

#[cfg(test)]
#[path = "tests/types.rs"]
mod tests;
