use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;
use tokio::sync::mpsc;
use tracing::debug;

use crate::audit::{AuditEvent, StreamingAuditSink};
use crate::pipeline::events::{InterruptionTrigger, PipelineEvent};
use crate::pipeline::{TurnComponentMetrics, TurnMetricsSnapshot};
use crate::transport::{ServerControl, ServerMessage};

#[cfg(feature = "realtime")]
use crate::realtime::{RealtimeClient, RealtimeProtocol, SessionConfig as RealtimeSessionConfig};

/// The "user finished speaking (or submitted text)" instant, captured in
/// both clock domains: the monotonic `instant` drives the TTFB
/// measurement, and `epoch_ms` (wall clock) is surfaced on the turn
/// snapshot as `user_speech_end_epoch_ms` so the admin capture reader
/// can anchor the perceived-E2E metric. S2S turns have no `stt_timing`
/// event, and the `utterance_received` event fires at input-transcript
/// *arrival* (often after response audio already started), so without
/// this stamp the reader has no valid end-of-speech wallclock.
#[derive(Debug, Clone, Copy)]
pub struct RequestAnchor {
    pub instant: Instant,
    pub epoch_ms: u64,
    /// Where the anchor came from — on the wire as `ttfb_anchor_source`.
    pub source: AnchorSource,
}

/// Provenance of a [`RequestAnchor`]. Surfaced on the turn snapshot as
/// `ttfb_anchor_source` so the report can say how honest `ttfb_ms` /
/// `e2e_perceived_ms` are instead of printing a proxy as if it were the
/// user's end of speech.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AnchorSource {
    /// The provider reported end-of-speech (`InputSpeechStopped`; `OpenAI`).
    ProviderSpeechStopped,
    /// Last input-transcript delta before the response (Gemini Live has no
    /// stop event) — measured 0.45–1 s after the user actually stopped.
    InputTranscriptProxy,
    /// vp's own uplink end-of-speech observer; used only when the provider
    /// offered no anchor of its own.
    VpVad,
    /// The client's `timing_report speech_end` (server receipt instant).
    ClientSpeechEnd,
    /// A text submission.
    TextSubmit,
}

impl AnchorSource {
    /// Stable wire name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProviderSpeechStopped => "provider_speech_stopped",
            Self::InputTranscriptProxy => "input_transcript_proxy",
            Self::VpVad => "vp_vad",
            Self::ClientSpeechEnd => "client_speech_end",
            Self::TextSubmit => "text_submit",
        }
    }

    /// Anchors the *provider* produced — the ones `provider_endpointing_ms`
    /// is measured against.
    pub fn is_provider(self) -> bool {
        matches!(
            self,
            Self::ProviderSpeechStopped | Self::InputTranscriptProxy
        )
    }
}

fn epoch_ms_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

impl RequestAnchor {
    pub fn now(source: AnchorSource) -> Self {
        Self {
            instant: Instant::now(),
            epoch_ms: epoch_ms_now(),
            source,
        }
    }

    /// An anchor at an already-captured instant (vp observer stamps).
    pub fn at(stamp: SpeechEndStamp, source: AnchorSource) -> Self {
        Self {
            instant: stamp.instant,
            epoch_ms: stamp.epoch_ms,
            source,
        }
    }

    /// An input-transcript proxy that landed with the model's first output
    /// (reply audio, text or a tool call) at `now`, rather than at the end
    /// of speech; see [`PROXY_WITH_REPLY_WINDOW`].
    pub fn is_reply_time_proxy(&self, now: Instant) -> bool {
        self.source == AnchorSource::InputTranscriptProxy
            && now.saturating_duration_since(self.instant) <= PROXY_WITH_REPLY_WINDOW
    }
}

/// The instant vp's own uplink observer last saw speech end — both clock
/// domains, like [`RequestAnchor`]. On the wire as
/// `vp_speech_end_epoch_ms`; the difference to a provider anchor is
/// `provider_endpointing_ms`.
#[derive(Debug, Clone, Copy)]
pub struct SpeechEndStamp {
    pub instant: Instant,
    pub epoch_ms: u64,
    /// Level / noise floor / clipping of the speech that ended here,
    /// filled by the observer. Rides with
    /// the stamp so it lands on exactly the turn the stamp does, and is
    /// dropped with it when stale.
    pub input_audio: Option<crate::audio::InputAudioStats>,
}

/// Two observer segments whose gap is at most this are one utterance with
/// a pause longer than the observer's hangover; their input stats pool.
pub const SEGMENT_MERGE_GAP: std::time::Duration = std::time::Duration::from_millis(1_500);

impl SpeechEndStamp {
    pub fn now() -> Self {
        Self {
            instant: Instant::now(),
            epoch_ms: epoch_ms_now(),
            input_audio: None,
        }
    }

    /// Pool an earlier segment's input stats into this stamp when the two
    /// belong to the same utterance. The segment start is estimated as
    /// `instant − speech_ms`; in-segment pauses make that later than the
    /// true start, so the gap is over- rather than under-estimated and a
    /// separate exchange is never merged.
    pub fn absorb_earlier(&mut self, earlier: &Self) {
        let (Some(stats), Some(prev)) = (self.input_audio.as_mut(), earlier.input_audio.as_ref())
        else {
            return;
        };
        let start = self
            .instant
            .checked_sub(std::time::Duration::from_millis(stats.speech_ms()))
            .unwrap_or(self.instant);
        if start.saturating_duration_since(earlier.instant) <= SEGMENT_MERGE_GAP {
            stats.merge_earlier(prev);
        }
    }
}

/// A vp observer stamp older than this (relative to the turn's anchor or
/// start) belongs to an earlier exchange and is dropped rather than
/// reported as this turn's end of speech.
pub const VP_SPEECH_END_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(10);

/// An input-transcript proxy at most this long before the model's first
/// output arrived with the reply rather than at the end of speech. Gemini
/// 2.5 streamed its input transcript during speech, 0.7 s or more ahead of
/// the first output; `gemini-3.8-live` sends it 0–270 ms ahead.
pub const PROXY_WITH_REPLY_WINDOW: std::time::Duration = std::time::Duration::from_millis(400);

/// Tracks one provider response (analogous to one "turn" in the classic
/// pipeline) so the realtime runner can emit a `TurnMetricsSnapshot`
/// shaped like `pipeline::realtime_descriptors()`.
pub struct RealtimeTurn {
    pub utterance_id: u32,
    pub started_at: Instant,
    /// Anchor for the time-to-first-byte measurement: the instant the
    /// user finished speaking (provider `InputSpeechStopped`, the last
    /// `InputTranscript` before the response, or a text submission).
    /// When `None`, TTFB falls back to `started_at`. For providers that
    /// create the turn lazily at the first audio delta (e.g. Gemini Live,
    /// which emits no `ResponseStarted`), `started_at` equals the
    /// first-byte instant, so without this anchor TTFB would always be 0.
    pub ttfb_anchor: Option<RequestAnchor>,
    /// Anchor → first provider **audio** delta. Never stamped on text.
    pub ttfb_ms: Option<u64>,
    /// Anchor → first output-transcript delta. Gemini streams the opening
    /// transcription before it flushes its first audio buffer, so this
    /// leads `ttfb_ms` by 0.3–1.2 s; diagnostic ("the model started").
    pub first_text_ms: Option<u64>,
    /// `StateChanged(Speaking)` already sent for this turn (first text or
    /// audio delta, whichever came first).
    pub speaking_announced: bool,
    /// vp's own end-of-speech stamp for the user speech this turn answers
    /// (see [`SpeechEndStamp`]); drives `provider_endpointing_ms`.
    pub vp_speech_end: Option<SpeechEndStamp>,
    pub audio_samples: u64,
    pub output_sample_rate: Option<u32>,
    pub assistant_text: String,
    pub text_delta_idx: u32,
    /// Provider-side handle for the assistant message item (`OpenAI`
    /// `response.output_item.added.item.id`). Needed to send
    /// `conversation.item.truncate` after a client barge-in so the
    /// model's stored history reflects only what the user heard.
    pub item_id: Option<String>,
    /// Snapshots of `(cumulative_audio_ms, assistant_text.len())`
    /// captured at every text-delta arrival. Used to recover the
    /// "what was actually heard" prefix from a `PlaybackReport`'s
    /// `last_played_offset_ms` when the client doesn't echo
    /// `last_played_text` itself. Sorted by `audio_ms` (deltas are
    /// monotonic).
    pub text_timeline: Vec<(u64, usize)>,
    /// Monotonic per-turn counter used as `fragment_idx` when we
    /// record `AuditEvent::TtsFragment` for every `ResponseAudioDelta`.
    /// Makes the captured `{utt:03}_tts_frag_{idx:03}.wav` filenames
    /// unique within a turn so the admin-UI Audio Capture widget
    /// can play the assistant output back.
    pub output_fragment_idx: u32,
    /// Virtual cursor (in milliseconds) tracking estimated audio offset of text deltas.
    pub estimated_text_ms: u64,
    /// The session-cumulative audio duration (in ms) emitted before this turn started.
    pub start_audio_ms: u64,
    /// The user transcript (STT or text) that initiated this turn.
    pub user_transcript: Option<String>,
    /// True once the provider emitted a `FunctionCall` within this turn.
    /// Together with `audio_samples == 0` it identifies a *phantom*
    /// transcript: Gemini native-audio transcribes what the model
    /// "planned to say" around a tool call even when no audio is
    /// produced — text the user never heard, which must not reach the
    /// client transcript.
    pub saw_function_call: bool,
    /// The turn was opened by a bare tool call (no response, text or audio
    /// came first). Such a leg ends when the user speaks again even if the
    /// provider never sends its completion.
    pub opened_by_tool_call: bool,
    /// Assistant transcript messages withheld while the turn has
    /// produced no audio yet. Flushed to the client on the first audio
    /// delta (spoken turn) or at turn end (genuine text-only turn);
    /// dropped when the turn ends audio-less after a function call.
    pub withheld_transcript: Vec<crate::transport::PipelineMessage>,
    /// Provider-supplied word alignment accumulated across the turn's
    /// audio deltas (`ElevenLabs`; empty for providers without it).
    /// `end_ms` is session-cumulative, matching `text_timeline`'s
    /// coordinate. When non-empty, [`RealtimeTurn::truncate_text_at`]
    /// uses these for word-exact truncation instead of the 15 chars/s
    /// estimate.
    pub aligned_words: Vec<AlignedWord>,
    /// Streaming-phase throughput (`output` component): provider deltas
    /// received vs chunks forwarded.
    pub output: crate::pipeline::OutputThroughput,
    /// Per-processor inline post-audio chain stats (`audio_post:<name>`).
    pub chain: crate::pipeline::ChainStats,
}

/// One alignment-derived word with the session-cumulative audio offset
/// (ms) at which it finishes being spoken.
#[derive(Debug, Clone, PartialEq)]
pub struct AlignedWord {
    pub word: String,
    pub end_ms: u64,
}

/// Word-exact "what was heard" truncation from provider alignment.
///
/// Walks `words` (whose concatenated text matches `full_text` modulo
/// whitespace — verified live for `ElevenLabs`, guarded here), advancing
/// a byte cursor through `full_text`, and cuts after the last word
/// fully played by `played_ms`. Returns `None` when there is no
/// alignment or the words stop matching the transcript (normalization
/// drift) — callers fall back to the estimate timeline. A word split
/// across two audio chunks arrives as two partials ("Hel" + "lo");
/// both match at the cursor without an intervening whitespace skip.
pub fn truncate_aligned(words: &[AlignedWord], full_text: &str, played_ms: u64) -> Option<String> {
    if words.is_empty() {
        return None;
    }
    let mut pos = 0usize;
    let mut heard_end = 0usize;
    for w in words {
        let rest = &full_text[pos..];
        pos += rest.len() - rest.trim_start().len();
        if w.word.is_empty() || !full_text[pos..].starts_with(w.word.as_str()) {
            tracing::warn!(
                word = %w.word,
                at_byte = pos,
                "alignment words diverge from transcript; falling back to estimated timeline"
            );
            return None;
        }
        pos += w.word.len();
        if w.end_ms <= played_ms {
            heard_end = pos;
        } else {
            break;
        }
    }
    Some(full_text[..heard_end].trim_end().to_string())
}

impl RealtimeTurn {
    pub fn new(utterance_id: u32, start_audio_ms: u64) -> Self {
        Self {
            utterance_id,
            started_at: Instant::now(),
            ttfb_anchor: None,
            ttfb_ms: None,
            first_text_ms: None,
            speaking_announced: false,
            vp_speech_end: None,
            audio_samples: 0,
            output_sample_rate: None,
            assistant_text: String::new(),
            text_delta_idx: 0,
            item_id: None,
            text_timeline: Vec::new(),
            output_fragment_idx: 0,
            estimated_text_ms: start_audio_ms,
            start_audio_ms,
            user_transcript: None,
            saw_function_call: false,
            opened_by_tool_call: false,
            withheld_transcript: Vec::new(),
            aligned_words: Vec::new(),
            output: crate::pipeline::OutputThroughput::default(),
            chain: crate::pipeline::ChainStats::default(),
        }
    }

    /// Instant from which time-to-first-byte is measured. Prefers the
    /// `ttfb_anchor` (end of user speech) and falls back to `started_at`
    /// (turn creation) when no anchor was captured.
    pub fn ttfb_base(&self) -> Instant {
        self.ttfb_anchor.map_or(self.started_at, |a| a.instant)
    }

    /// Current cumulative audio-output duration in milliseconds based
    /// on the deltas seen so far. Returns 0 before
    /// `output_sample_rate` is known.
    pub fn audio_emitted_ms(&self) -> u64 {
        match self.output_sample_rate {
            Some(rate) if rate > 0 => self.audio_samples.saturating_mul(1000) / rate as u64,
            _ => 0,
        }
    }

    /// Best-effort: given the audio offset the client actually played
    /// (in ms), return the prefix of `assistant_text` that had been
    /// emitted by that moment. Trims to a whitespace boundary when
    /// possible so the UI doesn't show a half-cut word.
    pub fn truncate_text_at(&self, played_ms: u64) -> String {
        // Provider alignment (when present) is word-exact — prefer it.
        if let Some(exact) = truncate_aligned(&self.aligned_words, &self.assistant_text, played_ms)
        {
            return exact;
        }
        if self.text_timeline.is_empty() {
            return String::new();
        }
        // Find the last timeline entry whose audio_ms <= played_ms.
        // Binary search keeps this O(log n) even if a long response
        // accumulates thousands of deltas.
        let idx = match self
            .text_timeline
            .binary_search_by_key(&played_ms, |&(ms, _)| ms)
        {
            Ok(i) => i,
            Err(0) => return String::new(),
            Err(i) => i - 1,
        };
        let len = self.text_timeline[idx].1.min(self.assistant_text.len());
        let prefix = &self.assistant_text[..len];
        // Snap to the last whitespace inside the prefix so we don't
        // chop a word in half. Falls through to the raw prefix when
        // the played span contains no whitespace (very short turn).
        match prefix.rfind(char::is_whitespace) {
            Some(ws) if ws > 0 => prefix[..ws].trim_end().to_string(),
            _ => prefix.trim_end().to_string(),
        }
    }

    /// Snap the turn into a `TurnMetricsSnapshot` shaped to match
    /// [`crate::pipeline::realtime_descriptors`]. `input_audio_ms` is
    /// the total uplinked audio duration accumulated across the
    /// response window.
    pub fn finish(
        self,
        interrupted: bool,
        input_audio_ms: u64,
        provider_label: &str,
    ) -> TurnMetricsSnapshot {
        let total_ms = self.started_at.elapsed().as_millis() as u64;
        let sample_rate = self.output_sample_rate.unwrap_or(0);
        let audio_output_ms = if sample_rate > 0 {
            self.audio_samples.saturating_mul(1000) / sample_rate as u64
        } else {
            0
        };

        let mut extras = serde_json::Map::new();
        if let Some(ttfb) = self.ttfb_ms {
            extras.insert("ttfb_ms".into(), serde_json::Value::from(ttfb));
        }
        if let Some(ms) = self.first_text_ms {
            extras.insert("first_text_ms".into(), serde_json::Value::from(ms));
        }
        // Provenance of the TTFB anchor — `"turn_start"` means there was
        // none and `ttfb_ms` (≈ 0) must never be read as latency.
        extras.insert(
            "ttfb_anchor_source".into(),
            serde_json::Value::from(self.ttfb_anchor.map_or("turn_start", |a| a.source.as_str())),
        );
        // End-of-user-speech wall clock (see `RequestAnchor`). Only stamped
        // when a genuine anchor existed — its presence also tells the reader
        // that `ttfb_ms` is speech-end-anchored rather than measured from
        // turn creation, i.e. usable as a perceived-E2E estimate.
        if let Some(anchor) = self.ttfb_anchor
            && anchor.epoch_ms > 0
        {
            extras.insert(
                "user_speech_end_epoch_ms".into(),
                serde_json::Value::from(anchor.epoch_ms),
            );
        }
        // vp's own end-of-speech stamp and the provider's endpointing lag
        // against it. A stamp that
        // predates the anchor (or the turn) by more than the freshness
        // window is an earlier exchange's and is dropped.
        if let Some(vp) = self.vp_speech_end {
            let reference = self.ttfb_anchor.map_or(self.started_at, |a| a.instant);
            if reference.saturating_duration_since(vp.instant) <= VP_SPEECH_END_MAX_AGE {
                extras.insert(
                    "vp_speech_end_epoch_ms".into(),
                    serde_json::Value::from(vp.epoch_ms),
                );
                if let Some(stats) = vp.input_audio {
                    stats.write_extras(&mut extras);
                }
                if let Some(anchor) = self.ttfb_anchor
                    && anchor.source.is_provider()
                {
                    extras.insert(
                        "provider_endpointing_ms".into(),
                        serde_json::Value::from(
                            anchor
                                .instant
                                .saturating_duration_since(vp.instant)
                                .as_millis() as u64,
                        ),
                    );
                }
            }
        }
        extras.insert(
            "audio_output_ms".into(),
            serde_json::Value::from(audio_output_ms),
        );
        extras.insert(
            "input_audio_ms".into(),
            serde_json::Value::from(input_audio_ms),
        );
        if sample_rate > 0 {
            extras.insert("sample_rate".into(), serde_json::Value::from(sample_rate));
        }

        let component = TurnComponentMetrics {
            component_id: "realtime".into(),
            kind: "realtime".into(),
            started_at_ms: 0,
            finished_at_ms: Some(total_ms),
            latency_ms: Some(total_ms),
            provider: Some(provider_label.to_string()),
            cold_start: false,
            error: None,
            extras,
        };

        // `output` (emit/source rtf, chain overhead) and `audio_post:<name>`
        // ride next to the fused provider component so a slow post-chain is
        // never blamed on the model.
        let mut components = vec![component];
        components.extend(self.output.finish());
        components.extend(self.chain.components());

        TurnMetricsSnapshot {
            utterance_id: self.utterance_id,
            total_ms,
            interrupted,
            initiator: crate::pipeline::metrics::TurnInitiator::default(),
            components,
            llm_response_text: if self.assistant_text.is_empty() {
                None
            } else {
                Some(self.assistant_text)
            },
        }
    }
}

/// Records everything we need from a freshly-flushed interrupted turn
/// to honour the client's eventual `PlaybackReport`: emit a
/// `TextTruncated` server control and (for `OpenAI`) prune the model's
/// stored history via `conversation.item.truncate`. We can't do this
/// inside `flush_turn_as_interrupted` because the played-offset isn't
/// known until the client tells us.
#[derive(Debug, Clone)]
pub struct PendingInterruption {
    pub utterance_id: u32,
    /// Full assistant text accumulated up to the cancel point. We
    /// fall back to this verbatim when the client doesn't tell us how
    /// many ms it played (rare but possible).
    pub full_text: String,
    /// `(cumulative_audio_ms, text_byte_len)` snapshots from the
    /// flushed turn, used to truncate `full_text` proportionally to
    /// the played offset.
    pub text_timeline: Vec<(u64, usize)>,
    /// Total audio (in ms) that we had emitted by cancel time.
    /// Caps the client's reported offset so a buggy/late report can't
    /// claim more was played than ever existed.
    pub audio_emitted_ms: u64,
    /// Provider-side assistant message id, when available. `None` for
    /// providers without per-item truncation (Gemini Live) or when
    /// the cancel arrived before any output item was attached.
    pub item_id: Option<String>,
    /// The session-cumulative audio duration (in ms) emitted before this turn started.
    pub start_audio_ms: u64,
    /// Provider word alignment captured up to the cancel point —
    /// enables word-exact truncation when the `PlaybackReport` arrives.
    pub aligned_words: Vec<AlignedWord>,
    pub snapshot: TurnMetricsSnapshot,
    pub user_transcript: Option<String>,
}

/// Outcome of [`flush_turn_as_interrupted`]: the metrics snapshot for
/// the just-finalised turn (so the caller can persist a `TurnRecord`
/// to the orchestrator) plus the [`PendingInterruption`] used to
/// reconcile the client's eventual `PlaybackReport`.
#[derive(Debug)]
pub struct FlushedTurn {
    pub snapshot: TurnMetricsSnapshot,
    pub pending: PendingInterruption,
    pub user_transcript: Option<String>,
}

/// A completed turn whose audio the client may still be playing.
///
/// Realtime S2S providers generate audio far faster than it plays:
/// `response.done` closes the server-side turn seconds before the user
/// stops *hearing* it, so a `speech_started` in that window used to
/// no-op (`current_turn` was already `None`) and the whole audible tail
/// of every turn was uninterruptible. The tail keeps everything needed
/// to honour a late barge-in through the existing
/// [`PendingInterruption`] → `PlaybackReport` flow: client audio flush,
/// interrupted re-persist of the turn record, and provider-side history
/// truncation.
///
/// Lifecycle: armed on `ResponseCompleted` (only when the turn emitted
/// audio); dropped on the client's `PlaybackComplete` for the same
/// utterance, on the next turn's creation, or after `max_live_ms` of
/// wall clock (an upper bound on remaining playback — the full audio
/// duration plus slack — so clients that never report playback can't
/// leave a stale tail that flags long-after speech as a barge-in).
#[derive(Debug)]
pub struct PlaybackTail {
    pub pending: PendingInterruption,
    armed_at: std::time::Instant,
    max_live_ms: u64,
}

/// Slack added to the audio duration when bounding how long a tail stays
/// interruptible: covers client prebuffer, network delivery, and device
/// output latency.
const PLAYBACK_TAIL_SLACK_MS: u64 = 2_000;

impl PlaybackTail {
    pub fn new(pending: PendingInterruption) -> Self {
        let max_live_ms = pending.audio_emitted_ms + PLAYBACK_TAIL_SLACK_MS;
        Self {
            pending,
            armed_at: std::time::Instant::now(),
            max_live_ms,
        }
    }

    #[cfg(test)]
    pub fn with_max_live_ms(mut self, max_live_ms: u64) -> Self {
        self.max_live_ms = max_live_ms;
        self
    }

    /// Whether the client could plausibly still be playing this turn.
    pub fn is_live(&self) -> bool {
        u64::try_from(self.armed_at.elapsed().as_millis()).unwrap_or(u64::MAX) <= self.max_live_ms
    }

    pub fn utterance_id(&self) -> u32 {
        self.pending.utterance_id
    }
}

/// Finalise the in-flight turn as interrupted: stamp metrics, emit the
/// `Interrupted` / `TurnMetricsReady` events, push the `ClearAudio`
/// control frame down to the client through the pacer (which flushes
/// any buffered tail immediately), and record the interruption in the
/// audit sink. Returns a [`FlushedTurn`] capturing both the metrics
/// snapshot (so the caller can persist a `TurnRecord`) and the data
/// needed to later honour the client's `PlaybackReport`
/// (text-truncation + provider history hygiene). Returns `None` when
/// there is no active turn.
#[expect(
    clippy::too_many_arguments,
    reason = "session-local channel/state handles; bundling them into a struct would duplicate RealtimeSessionContext for one call site"
)]
pub async fn flush_turn_as_interrupted(
    current_turn: &mut Option<RealtimeTurn>,
    input_audio_ms_acc: &mut u64,
    provider_label: &str,
    trigger: InterruptionTrigger,
    reason: &str,
    out_tx: &mpsc::Sender<ServerMessage>,
    pe_tx: &tokio::sync::broadcast::Sender<PipelineEvent>,
    streaming_audit: Option<&StreamingAuditSink>,
    annotators: &crate::pipeline::AnnotatorHubs,
) -> Option<FlushedTurn> {
    let turn = current_turn.take()?;
    let utterance_id = turn.utterance_id;
    let audio_emitted_ms = turn.audio_emitted_ms();
    let full_text = turn.assistant_text.clone();
    let text_timeline = turn.text_timeline.clone();
    let item_id = turn.item_id.clone();
    let start_audio_ms = turn.start_audio_ms;
    let user_transcript = turn.user_transcript.clone();
    let aligned_words = turn.aligned_words.clone();

    // Off-path annotators: drop pending work for the interrupted utterance
    // and fold their per-turn counters into the snapshot.
    annotators.cancel(utterance_id);
    let mut snapshot = turn.finish(true, *input_audio_ms_acc, provider_label);
    snapshot
        .components
        .extend(annotators.turn_components(utterance_id));
    *input_audio_ms_acc = 0;

    let pending = PendingInterruption {
        utterance_id,
        full_text,
        text_timeline,
        audio_emitted_ms,
        item_id,
        start_audio_ms,
        aligned_words,
        snapshot: snapshot.clone(),
        user_transcript: user_transcript.clone(),
    };

    // Tell the client to drop any buffered audio for this utterance
    // *before* we publish metrics so the dashboard's interrupt latency
    // measurement starts from the right moment.
    let _ = out_tx
        .send(ServerMessage::Control(ServerControl::ClearAudio {
            utterance_id,
            reason: reason.to_string(),
        }))
        .await;

    let interrupted_event = PipelineEvent::Interrupted {
        utterance_id,
        trigger: trigger.clone(),
        played_ms: None,
    };
    // Recorded once, by the session's event recorder (`session_events`).
    let _ = pe_tx.send(interrupted_event);
    if let Some(audit) = streaming_audit {
        if !snapshot
            .llm_response_text
            .as_deref()
            .unwrap_or_default()
            .is_empty()
        {
            audit.record(AuditEvent::LlmOutput {
                utterance_id: snapshot.utterance_id,
                text: snapshot.llm_response_text.clone().unwrap_or_default(),
                spoken_text: None,
                tts_directives: Vec::new(),
                stripped_directives: Vec::new(),
                latency: std::time::Duration::from_millis(snapshot.total_ms),
                is_partial: false,
            });
        }
        audit.record(AuditEvent::UtteranceDone {
            utterance_id,
            interrupted: true,
            terminated: false,
            stt_provider: provider_label.to_string(),
            tts_provider: provider_label.to_string(),
        });
    }

    // Client-facing `turn_metrics` control frame (parity with cascade's
    // `emit_turn_complete`): the interrupted snapshot carries the audio
    // actually emitted, which barge-in tooling reads off the wire.
    let _ = out_tx
        .send(ServerMessage::Control(ServerControl::TurnMetrics {
            snapshot: snapshot.clone(),
        }))
        .await;
    let _ = pe_tx.send(PipelineEvent::TurnMetricsReady {
        utterance_id: snapshot.utterance_id,
        metrics: snapshot.clone(),
    });

    Some(FlushedTurn {
        snapshot,
        pending,
        user_transcript,
    })
}

/// Gate on the outbound audio pump after a `ClearAudio`.
///
/// Armed for the utterance whose audio the client was just told to drop,
/// so provider audio still in flight for that utterance never reaches the
/// client after the flush. Released when the client confirms the flush
/// (`PlaybackComplete` for that utterance or a later one) or when a new
/// response starts, whichever comes first — so a release never depends on
/// a message the client has no reason to send. Only ever armed together
/// with a `ClearAudio`: arming with nothing for the client to acknowledge
/// would leave the session mute.
pub struct SuppressGate {
    flag: Arc<AtomicBool>,
    armed_for: parking_lot::Mutex<Option<u32>>,
}

impl SuppressGate {
    /// Wrap the flag the session pump polls.
    pub fn new(flag: Arc<AtomicBool>) -> Self {
        Self {
            flag,
            armed_for: parking_lot::Mutex::new(None),
        }
    }

    /// Drop outbound audio until the flush of `utterance_id` is confirmed.
    pub fn arm(&self, utterance_id: u32) {
        *self.armed_for.lock() = Some(utterance_id);
        self.flag.store(true, Ordering::Release);
    }

    /// Let audio through again regardless of what was armed.
    pub fn release(&self) {
        *self.armed_for.lock() = None;
        self.flag.store(false, Ordering::Release);
    }

    /// The client confirmed playback of `utterance_id` ended. Releases the
    /// gate when that confirmation covers the armed utterance (the same one
    /// or a later one); an older, stale confirmation leaves it armed.
    /// Returns whether the gate was released.
    pub fn release_for(&self, utterance_id: u32) -> bool {
        let mut armed = self.armed_for.lock();
        match *armed {
            Some(target) if target <= utterance_id => {
                *armed = None;
                self.flag.store(false, Ordering::Release);
                true
            }
            _ => false,
        }
    }

    #[cfg(test)]
    pub fn is_suppressed(&self) -> bool {
        self.flag.load(Ordering::Acquire)
    }

    /// The utterance the gate is currently armed for.
    #[cfg(test)]
    pub fn armed_for(&self) -> Option<u32> {
        *self.armed_for.lock()
    }
}

/// What the runner believes about the provider's response lifecycle.
///
/// Agent-initiated speech must never race a response the provider is
/// already producing (providers reject a second `response.create`), so the
/// runner tracks whether a response is active and which one. Terminal
/// events are matched against that: the event for a response we cancelled
/// while immediately starting its replacement, or for one that was
/// replaced before its terminal event arrived, must not be taken as "the
/// provider is idle now".
#[derive(Debug, Default)]
pub struct ResponseTracker {
    active: bool,
    /// Id the provider gave the active response (`None` for providers
    /// without response ids, or before `ResponseStarted` arrived).
    active_id: Option<String>,
    /// A response we cancelled while requesting its replacement; its
    /// terminal event is still expected and must not deactivate the
    /// replacement.
    superseded_id: Option<String>,
    /// The same, for providers whose events carry no id: how many
    /// terminal events belong to superseded responses.
    idless_supersessions: u32,
}

/// How a response terminal event (`ResponseCompleted` /
/// `ResponseCancelled`) relates to what the runner is tracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalVerdict {
    /// The active response ended: finish its turn and release any queued
    /// agent work.
    Current,
    /// A response we replaced ended: finish its turn bookkeeping, but the
    /// replacement is still running, so nothing queued is released.
    Superseded,
    /// A response that is neither active nor awaited (its replacement has
    /// already started): ignore it entirely.
    Stale,
}

impl ResponseTracker {
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// The provider announced a response.
    pub fn on_started(&mut self, response_id: Option<String>) {
        self.active = true;
        self.active_id = response_id;
    }

    /// The runner asked the provider for a response (`response.create`).
    pub fn on_created(&mut self) {
        self.active = true;
    }

    /// The active response is about to be cancelled and immediately
    /// replaced: remember that its terminal event is still coming.
    pub fn note_superseded(&mut self) {
        if !self.active {
            return;
        }
        match self.active_id.take() {
            Some(id) => self.superseded_id = Some(id),
            None => self.idless_supersessions += 1,
        }
    }

    /// Classify a terminal event and update the active flag accordingly.
    pub fn on_terminal(&mut self, response_id: Option<&str>) -> TerminalVerdict {
        if let Some(id) = response_id {
            if self.superseded_id.as_deref() == Some(id) {
                self.superseded_id = None;
                return TerminalVerdict::Superseded;
            }
            if self.active_id.as_deref().is_some_and(|active| active != id) {
                return TerminalVerdict::Stale;
            }
        }
        // Without an id to match on, an expected terminal event of a
        // superseded response is taken to be the next one that arrives.
        if self.idless_supersessions > 0 && (response_id.is_none() || self.active_id.is_none()) {
            self.idless_supersessions -= 1;
            return TerminalVerdict::Superseded;
        }
        self.active = false;
        self.active_id = None;
        TerminalVerdict::Current
    }

    /// A completion the runner inferred itself (idle window on providers
    /// without an end-of-response event): whatever was tracked is over.
    pub fn finish_inferred(&mut self) {
        *self = Self::default();
    }

    /// A fresh provider session after a reconnect: nothing from the old
    /// one is still running.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// Tool calls the provider withdrew while their tools ran (the user
/// spoke over them). A withdrawn call's result is not sent: the model no
/// longer expects it, and answering would start a response about a
/// question the user has moved on from. Bounded, oldest first out: a
/// provider that withdraws calls the session never ran must not grow it.
#[derive(Debug, Default)]
pub struct WithdrawnCalls {
    ids: std::collections::VecDeque<String>,
}

impl WithdrawnCalls {
    /// How many withdrawn ids are remembered.
    pub const CAPACITY: usize = 32;

    pub fn withdraw(&mut self, call_ids: impl IntoIterator<Item = String>) {
        for id in call_ids {
            if self.ids.len() == Self::CAPACITY {
                self.ids.pop_front();
            }
            self.ids.push_back(id);
        }
    }

    /// Whether `call_id` was withdrawn. Each id answers once: the call's
    /// result arrives once.
    pub fn take(&mut self, call_id: &str) -> bool {
        match self.ids.iter().position(|id| id == call_id) {
            Some(at) => {
                self.ids.remove(at);
                true
            }
            None => false,
        }
    }

    /// A fresh provider session after a reconnect: its calls are new.
    pub fn reset(&mut self) {
        self.ids.clear();
    }
}

/// Whether, and how, this session can be resumed on a new provider
/// connection with its context: the latest token the provider issued,
/// and whether a connection presenting one is still unacknowledged.
#[derive(Debug, Default)]
pub struct Resumption {
    token: Option<voice_pipes_sdk::secrets::Secret>,
    /// A connection opened with the token has not yet had its session
    /// acknowledged.
    unconfirmed: bool,
}

/// What a provider close means once [`Resumption`] has read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CloseReading {
    pub class: crate::realtime::protocol::CloseClass,
    /// The close refused a resumption: the token is spent, and the next
    /// connection starts afresh.
    pub refused_resumption: bool,
}

impl Resumption {
    /// The provider issued a token, superseding any before it.
    pub fn issued(&mut self, token: voice_pipes_sdk::secrets::Secret) {
        self.token = Some(token);
    }

    pub fn can_resume(&self) -> bool {
        self.token.is_some()
    }

    /// The token for a new connection to present, if there is one. The
    /// connection counts as unacknowledged until [`confirmed`](Self::confirmed).
    pub fn for_reconnect(&mut self) -> Option<voice_pipes_sdk::secrets::Secret> {
        self.unconfirmed = self.token.is_some();
        self.token.clone()
    }

    /// The provider acknowledged the connection's session.
    pub fn confirmed(&mut self) {
        self.unconfirmed = false;
    }

    /// Read a provider close. A close the protocol classes as refusing
    /// the token, or any close of a resumed connection before its session
    /// was acknowledged (a refusal can arrive without its close frame),
    /// spends the token and is answered by a fresh session; without a
    /// token to spend, a refusal is final. A fatal close stays fatal.
    pub fn on_close(&mut self, class: crate::realtime::protocol::CloseClass) -> CloseReading {
        use crate::realtime::protocol::CloseClass;
        let refused = match class {
            CloseClass::Unresumable => true,
            CloseClass::Fatal => false,
            _ => self.unconfirmed,
        };
        self.unconfirmed = false;
        if !refused {
            return CloseReading {
                class,
                refused_resumption: false,
            };
        }
        match self.token.take() {
            Some(_) => CloseReading {
                class: CloseClass::Transient,
                refused_resumption: true,
            },
            None if class == CloseClass::Unresumable => CloseReading {
                class: CloseClass::Fatal,
                refused_resumption: false,
            },
            None => CloseReading {
                class,
                refused_resumption: false,
            },
        }
    }
}

/// Rebuild the realtime protocol from the saved pipeline config and
/// re-apply the negotiated [`RealtimeSessionConfig`]. Returns the
/// reconnected client on success; the caller is responsible for
/// gating audio + emitting wire-level events.
#[cfg(feature = "realtime")]
pub async fn try_reconnect(
    pipeline_cfg: &crate::config::PipelineConfig,
    session_cfg: &RealtimeSessionConfig,
    session_id: &str,
    provider_label: &str,
    param_store: Option<&crate::api::param_store::AtomicParamStore>,
) -> Result<RealtimeClient<Box<dyn RealtimeProtocol>>, anyhow::Error> {
    let proto = crate::config::build_realtime_protocol(pipeline_cfg, param_store)?
        .ok_or_else(|| anyhow::anyhow!("realtime_provider config disappeared during reconnect"))?;
    debug!(
        session_id = %session_id,
        provider = %provider_label,
        "realtime: opening replacement websocket"
    );
    let emit_rate = pipeline_cfg
        .realtime_provider
        .as_ref()
        .map_or(
            Ok(crate::config::DEFAULT_OUTPUT_EMIT_RATE),
            crate::config::RealtimeProviderConfig::output_emit_rate,
        )
        .map_err(|e| anyhow::anyhow!("{}: {}", e.field, e.message))?;
    let mut client = RealtimeClient::connect(proto, emit_rate).await?;
    client.apply_session_config(session_cfg.clone()).await?;
    Ok(client)
}
