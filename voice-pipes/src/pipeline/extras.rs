//! Typed extras structs serialised into `TurnComponentMetrics::extras`.
//!
//! These keys are the canonical per-turn metric vocabulary — the single source
//! of truth for STT/LLM/TTS latencies. The admin BFF projects them via
//! `captures.reader.project_snapshot_metrics`; do not introduce parallel names.
//!
//! Co-located here so per-stage call sites can share the same key vocabulary
//! without duplicating string literals (which is how key drift starts --
//! `tokens_per_sec` vs `tps` vs `tokens_s`). One canonical struct per
//! well-known kind; `Option` everywhere because individual sub-measurements
//! may be missing on cancelled / partial turns.
//!
//! Producers serialise via [`super::metrics::ComponentSpan::extras_typed`].
//! Free-form keys for ad-hoc / experimental components remain available via
//! [`super::metrics::ComponentSpan::extras_mut`].

use serde::Serialize;

/// Extras for a `kind = "stt"` component.
#[derive(Debug, Clone, Default, Serialize)]
pub struct SttExtras {
    /// End-of-speech flush -> final transcript (ms).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub processing_ms: Option<u64>,
    /// Last speech frame -> turn-commit decision (VAD silence wait, ms).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpointing_ms: Option<u64>,

    // ── Streaming-inference metrics (Microsoft on-device streaming ASR
    //    paper, arXiv:2604.14493v2  3.2). Populated by providers that
    //    report `TranscriptionResult::inference_metrics` on the final
    //    result; absent for providers (e.g. cloud ASR) that don't.
    /// Total audio duration fed to the provider for this utterance (ms).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stt_audio_duration_ms: Option<u64>,
    /// Real-time factor: `audio_secs` / `inference_wall_secs` (higher = faster).
    /// Standard "`RTFx`" used by Open ASR Leaderboard.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stt_rtfx: Option<f32>,
    /// Static algorithmic delay floor for the provider's streaming
    /// configuration (ms).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stt_algorithmic_delay_ms: Option<u64>,
    /// Effective per-step latency: `algorithmic_delay` x (1 + 1/RTFx) (ms).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stt_effective_latency_ms: Option<u64>,
    /// Number of encoder/decoder invocations across the utterance -- catches
    /// O(N²) regressions when streaming long audio.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stt_inference_count: Option<u32>,
    /// Bound on the audio window re-encoded per inference step (ms);
    /// absent when the provider runs unbounded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stt_streaming_window_ms: Option<u32>,
}

/// Extras for a `kind = "llm"` component.
#[derive(Debug, Clone, Default, Serialize)]
pub struct LlmExtras {
    /// Time to first token (ms) -- utterance start -> first LLM fragment.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<u64>,
    /// Full generation duration (ms) -- first token -> last token.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generation_ms: Option<u64>,
    /// Whitespace-split estimate (used as fallback when the provider does
    /// not report `usage`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_estimate: Option<u32>,
    /// Authoritative provider-reported output token count.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_count: Option<u32>,
    /// Authoritative provider-reported prompt/input token count.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u32>,
    /// Prompt tokens served from the provider's KV/prefix cache, when reported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u32>,
    /// Wall-clock TTFT (ms) measured from `process_utterance` entry to the
    /// first LLM sentence fragment received by the bridge. Includes Tokio task
    /// scheduling, audit recording, history-mutex acquisition,
    /// `build_request`, (for cloud providers) HTTP/TLS setup, and sentence
    /// buffering until the first sentence boundary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_perceived_ttft_ms: Option<u64>,
    /// Engine-owned critical-path stage 2 (`t_first_frag - t_commit`, ms, 0.1 ms
    /// resolution): turn commit to the first sentence fragment forwarded to
    /// `tts_frag_tx`. `None` when the LLM stage was bypassed (`"agent_say"` or
    /// `"speculative_accept"`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub llm_wait_ms: Option<f64>,
    /// Delay (ms) spent accumulating streaming tokens into the first speakable
    /// text fragment handed to TTS (`first_token -> first_text_fragment`).
    /// `None` when the provider did not report `first_token_ms`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text_aggregation_ms: Option<u64>,
    /// Reason why the LLM stage was bypassed on this turn (`"agent_say"` or
    /// `"speculative_accept"`), when `ttft_ms` and `generation_ms` are `None`
    /// by design rather than missing due to cancellation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage_skipped_reason: Option<String>,
    /// Count of KV-cache rollbacks/restarts triggered by STT flickering
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speculative_rollbacks: Option<u32>,
    /// Total speculative tokens generated during the turn
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speculative_tokens_generated: Option<u32>,
    /// Speculative tokens accepted in the final committed response
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speculative_tokens_accepted: Option<u32>,
    /// Acceptance rate / efficiency ratio (accepted / generated)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speculative_efficiency: Option<f32>,
}

/// Extras for a `kind = "tts"` component.
#[derive(Debug, Clone, Default, Serialize)]
pub struct TtsExtras {
    /// Time to first byte (ms) -- first LLM fragment -> first audio sample
    /// (pipeline-internal, pre-pacer). Kept as integer-ms legacy alias of
    /// [`Self::tts_wait_ms`]; never overwritten with `commit_to_first_audio_ms`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttfb_ms: Option<u64>,
    /// Leading silence (ms) inside the synthesized TTS PCM stream before the
    /// first audible speech frame (10 ms RMS >= -42 dBFS).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tts_leading_silence_ms: Option<u64>,
    /// Perceived TTS Time-to-First-Audio (ms): `ttfb_ms + tts_leading_silence_ms`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tts_audible_ttfb_ms: Option<u64>,
    /// Engine-owned critical-path stage 3 (`t_first_tts_audio - t_first_frag`,
    /// ms, 0.1 ms resolution): first sentence fragment forwarded to `tts_frag_tx`
    /// to first non-empty audio chunk received from `tts_audio_rx`. `None` when
    /// pre-synthesized speculative audio was used (`stage_skipped_reason =
    /// Some("speculative_accept")`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tts_wait_ms: Option<f64>,
    /// Engine-owned critical-path stage 4 (`t_first_emit - t_first_tts_audio`,
    /// ms, 0.1 ms resolution): post-audio chain + channel/pacer/WS send for the
    /// first non-empty audio chunk.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_wait_ms: Option<f64>,
    /// Engine-owned critical-path composite (`t_first_emit - t_commit`, ms,
    /// 0.1 ms resolution): equals `llm_wait_ms + tts_wait_ms + output_wait_ms`
    /// by telescoping construction.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit_to_first_audio_ms: Option<f64>,
    /// Reason why the TTS synthesis stage was bypassed on this turn
    /// (`"speculative_accept"` when pre-synthesized speculative audio was used).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage_skipped_reason: Option<String>,
    /// Total synthesis span (ms) -- first LLM fragment -> last audio chunk.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synthesis_ms: Option<u64>,
    /// Wall-clock duration (ms) of the produced audio.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_duration_ms: Option<u64>,
    /// Native sample rate of the provider for this synthesis.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_rate: Option<u32>,
    /// Number of input characters fed to TTS.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chars: Option<u32>,
    /// Realtime factor: `synthesis_ms / audio_duration_ms`
    /// (lower = faster -- historical convention; kept for backward
    /// compatibility with existing capture data and the frontend).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rtf: Option<f32>,
    /// `RTFx`: `audio_duration / synthesis_wall_time` (higher = faster).
    /// Open ASR Leaderboard / paper convention; preferred for new code.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rtfx: Option<f32>,
}

/// Round a [`std::time::Duration`] to `0.1 ms` resolution (`f64`) for wire
/// serialization.
#[inline]
pub fn round_tenths_ms(d: std::time::Duration) -> f64 {
    (d.as_secs_f64() * 1000.0 * 10.0).round() / 10.0
}

/// Streaming zero-allocation detector for leading silence at the start of a
/// TTS PCM stream. Scans 10 ms mono windows until RMS crosses `-42 dBFS`
/// (`0.008` full-scale) or the 2000 ms cap is reached.
#[derive(Debug, Default)]
pub struct LeadingSilenceTracker {
    detected_ms: Option<u64>,
    silent_windows: u64,
    sum_sq: f64,
    win_frames: usize,
    exhausted: bool,
}

impl LeadingSilenceTracker {
    /// Amplitude RMS floor (~ -42 dBFS), matching `audio_verify::acoustic`.
    const AUDIBLE_RMS_THRESHOLD: f64 = 0.008;
    /// Stop scanning after 200 × 10 ms = 2000 ms to bound hot-path work.
    const MAX_WINDOWS: u64 = 200;

    pub fn push(&mut self, samples: &[i16], sample_rate: u32, channels: u16) {
        if self.detected_ms.is_some() || self.exhausted || sample_rate == 0 || samples.is_empty() {
            return;
        }
        let ch = usize::from(channels.max(1));
        let win_len = (usize::try_from(sample_rate).unwrap_or(16_000) / 100).max(1);
        for frame in samples.chunks(ch) {
            let mono: f64 =
                frame.iter().map(|&s| f64::from(s) / 32768.0).sum::<f64>() / (frame.len() as f64);
            self.sum_sq += mono * mono;
            self.win_frames += 1;
            if self.win_frames >= win_len {
                let rms = (self.sum_sq / (self.win_frames as f64)).sqrt();
                self.sum_sq = 0.0;
                self.win_frames = 0;
                if rms >= Self::AUDIBLE_RMS_THRESHOLD {
                    self.detected_ms = Some(self.silent_windows * 10);
                    return;
                }
                self.silent_windows += 1;
                if self.silent_windows >= Self::MAX_WINDOWS {
                    self.exhausted = true;
                    return;
                }
            }
        }
    }

    pub fn leading_silence_ms(&self) -> Option<u64> {
        self.detected_ms
    }
}
