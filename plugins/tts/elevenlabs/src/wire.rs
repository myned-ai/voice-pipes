//! REST request and response bodies, char-to-word alignment grouping, and
//! error-body mapping.

use serde::{Deserialize, Serialize};
use voice_pipes_sdk::tts::WordGrouper;
use voice_pipes_sdk::{TtsError, WordBoundary};

// ── REST API request types ──────────────────────────────────

#[derive(Serialize)]
pub(crate) struct ElevenLabsTtsRequest<'a> {
    pub(crate) text: &'a str,
    pub(crate) model_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) language_code: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) voice_settings: Option<VoiceSettings>,
}

#[derive(Serialize, Default, PartialEq)]
pub(crate) struct VoiceSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) stability: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) similarity_boost: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) style: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) use_speaker_boost: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) speed: Option<f32>,
}

// ── REST API response types (with-timestamps stream) ────────

/// One newline-delimited JSON chunk of a `with-timestamps` response.
#[derive(Deserialize)]
pub(crate) struct TimestampChunk {
    #[serde(default)]
    pub(crate) audio_base64: Option<String>,
    /// Char timing for the *original* text -- matches what downstream
    /// karaoke matching expects (same choice as the realtime provider).
    #[serde(default)]
    pub(crate) alignment: Option<Alignment>,
    /// Timing for the normalized text ("$1" → "one dollar"); used only
    /// when `alignment` is absent.
    #[serde(default)]
    pub(crate) normalized_alignment: Option<Alignment>,
}

/// Char-level timing. Times are seconds, cumulative from the start of
/// the full response audio (not chunk-relative like the Agents WS).
#[derive(Deserialize)]
pub(crate) struct Alignment {
    #[serde(default)]
    pub(crate) characters: Vec<String>,
    #[serde(default)]
    pub(crate) character_start_times_seconds: Vec<f64>,
    #[serde(default)]
    pub(crate) character_end_times_seconds: Vec<f64>,
}

#[derive(Deserialize)]
struct ElevenLabsErrorResponse {
    detail: serde_json::Value,
}

// ── Word grouping ───────────────────────────────────────────

impl Alignment {
    /// This chunk's characters into `words`, whose finished words land in
    /// `out`. HTTP timestamps are cumulative across the response, so one
    /// grouper across the chunks stitches a word split between two JSON
    /// chunks into one boundary. Ragged arrays truncate to the shortest
    /// length: a malformed chunk degrades to fewer words, never a panic.
    pub(crate) fn group_into(&self, words: &mut WordGrouper, out: &mut Vec<WordBoundary>) {
        let timed = self
            .characters
            .iter()
            .zip(&self.character_start_times_seconds)
            .zip(&self.character_end_times_seconds);
        for ((ch, &start), &end) in timed {
            words.push(ch, whole_ms(start), whole_ms(end), out);
        }
    }
}

// ── Helpers ─────────────────────────────────────────────────

pub(crate) fn map_http_error(status: u16, body: &str) -> TtsError {
    // ElevenLabs errors: {"detail": {"status": "...", "message": "..."}}
    // or {"detail": "plain string"}.
    let message = match serde_json::from_str::<ElevenLabsErrorResponse>(body) {
        Ok(err) => match &err.detail {
            serde_json::Value::String(s) => s.clone(),
            other => other
                .get("message")
                .and_then(|m| m.as_str())
                .map_or_else(|| other.to_string(), str::to_string),
        },
        Err(_) => body.to_string(),
    };
    TtsError::from_http_status("ElevenLabs TTS", status, &message)
}

/// A vendor's seconds as whole milliseconds: negative and NaN become 0.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a float to int cast saturates and maps NaN to 0; it cannot panic"
)]
fn whole_ms(secs: f64) -> u64 {
    (secs.max(0.0) * 1000.0) as u64
}
