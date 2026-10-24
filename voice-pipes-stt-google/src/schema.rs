//! Provider-owned parameter schema for the Google STT provider.
//!
//! The only file the graph editor consults for Google-specific knobs; the
//! shared [`SttConfig`](voice_pipes_sdk::config::SttConfig) schema impl dispatches
//! here through [`SttFactory::provider_schema`](voice_pipes_sdk::stt::SttFactory).

use voice_pipes_sdk::config::SttConfig;
use voice_pipes_sdk::schema::{ParamDescriptor, ProviderSchema};

use super::config::{EndpointingSensitivity, KNOWN_MODELS};

/// Build the schema + current-value snapshot for a Google configuration.
///
/// Everything is `static` (read-only in the editor): the recognition config
/// is sent once per stream and Google has no mid-stream update message.
pub fn schema(cfg: &SttConfig) -> ProviderSchema {
    let mut s = ProviderSchema::default();
    let o = &cfg.options;

    // ── Identity ──────────────────────────────────────────
    s.push(
        ParamDescriptor::static_str(
            "provider",
            "Provider",
            "The STT framework or service used for transcription.",
        ),
        cfg.provider.clone(),
    );
    s.push(
        ParamDescriptor::static_enum(
            "model_name",
            "Model",
            "Speech v2 recognition model. `chirp_3` is the multilingual model \
             (the only streaming model that covers Croatian).",
            KNOWN_MODELS,
        ),
        cfg.model_name
            .clone()
            .unwrap_or_else(|| "chirp_3".to_string()),
    );
    s.push(
        ParamDescriptor::static_str(
            "location",
            "Location",
            "Google Cloud location the recognizer runs in. `chirp_3` is served \
             from the `us` and `eu` multi-regions only; the endpoint host is \
             derived from it.",
        ),
        opt_or_default(o, "location", serde_json::json!("us")),
    );
    s.push(
        ParamDescriptor::static_str(
            "project_id",
            "Project",
            "GCP project for the recognizer path and quota attribution \
             (env: `GOOGLE_CLOUD_PROJECT`; else taken from the credential).",
        ),
        opt_or_null(o, "project_id"),
    );
    s.push(
        ParamDescriptor::static_str(
            "language_codes",
            "Languages",
            "BCP-47 language codes to recognise (e.g. [\"hr-HR\"]).",
        ),
        o.get("language_codes")
            .or_else(|| o.get("language_code"))
            .cloned()
            .unwrap_or(serde_json::Value::Null),
    );
    if let Some(endpoint) = &cfg.endpoint {
        s.push(
            ParamDescriptor::static_str(
                "endpoint",
                "Endpoint",
                "Override for the gRPC endpoint (default: \
                 `https://{location}-speech.googleapis.com`).",
            ),
            endpoint.clone(),
        );
    }
    s.push(
        ParamDescriptor::static_bool(
            "credentials_set",
            "Credentials",
            "Whether a service-account key path or static token is configured \
             (otherwise Application Default Credentials are used).",
        ),
        o.get("credentials_path").is_some()
            || o.get("access_token").is_some()
            || cfg.api_key.is_some(),
    );

    // ── Streaming / endpointing knobs ──────────────────────
    s.push(
        ParamDescriptor::static_int_display(
            "sample_rate",
            "Sample rate",
            "PCM input sample rate in Hz sent on the wire (default 16000).",
        ),
        opt_or_default(o, "sample_rate", serde_json::json!(16_000)),
    );
    s.push(
        ParamDescriptor::static_bool(
            "interim_results",
            "Interim results",
            "Emit partial transcripts while the utterance is in progress.",
        ),
        opt_or_default(o, "interim_results", serde_json::json!(true)),
    );
    s.push(
        ParamDescriptor::static_bool(
            "enable_voice_activity_events",
            "Voice activity events",
            "Ask Google for SPEECH_ACTIVITY_BEGIN/END events — required for \
             provider-mode turn detection and early barge-in.",
        ),
        opt_or_default(o, "enable_voice_activity_events", serde_json::json!(true)),
    );
    s.push(
        ParamDescriptor::static_bool(
            "enable_automatic_punctuation",
            "Automatic punctuation",
            "Punctuate and case the transcript.",
        ),
        opt_or_default(o, "enable_automatic_punctuation", serde_json::json!(true)),
    );
    s.push(
        ParamDescriptor::static_bool(
            "enable_word_time_offsets",
            "Word timestamps",
            "Return per-word start/end offsets (the WordTimestamps capability).",
        ),
        opt_or_default(o, "enable_word_time_offsets", serde_json::json!(false)),
    );
    s.push(
        ParamDescriptor::static_enum(
            "endpointing_sensitivity",
            "Endpointing sensitivity",
            "How eagerly Google closes a segment on silence. Takes effect on \
             the next stream.",
            EndpointingSensitivity::NAMES,
        ),
        opt_or_null(o, "endpointing_sensitivity"),
    );
    s.push(
        ParamDescriptor::static_int_display(
            "speech_end_timeout_ms",
            "Speech end timeout (ms)",
            "Silence after speech before Google finalises the segment \
             (voice_activity_timeout.speech_end_timeout).",
        ),
        opt_or_null(o, "speech_end_timeout_ms"),
    );
    s.push(
        ParamDescriptor::static_bool(
            "persistent_session",
            "Persistent stream",
            "Keep one upstream stream across utterances (needs continuous \
             audio); off = one stream per utterance.",
        ),
        opt_or_default(o, "persistent_session", serde_json::json!(false)),
    );
    s.push(
        ParamDescriptor::static_int_display(
            "stream_rotate_ms",
            "Stream rotation (ms)",
            "Proactively reopen the upstream stream after this long — Google \
             closes every stream at 5 minutes.",
        ),
        opt_or_default(o, "stream_rotate_ms", serde_json::json!(240_000)),
    );

    // ── Reconnect tuning ───────────────────────────────────
    s.push(
        ParamDescriptor::static_int_display(
            "max_reconnect_attempts",
            "Max reconnects",
            "Number of reconnect attempts on transient gRPC failures.",
        ),
        opt_or_null(o, "max_reconnect_attempts"),
    );
    s.push(
        ParamDescriptor::static_int_display(
            "reconnect_delay_ms",
            "Reconnect base delay (ms)",
            "Initial backoff delay between reconnect attempts.",
        ),
        opt_or_null(o, "reconnect_delay_ms"),
    );

    s.passthrough_options(
        o,
        &[
            "location",
            "project_id",
            "language_codes",
            "language_code",
            "credentials_path",
            "access_token",
            "sample_rate",
            "interim_results",
            "enable_voice_activity_events",
            "enable_automatic_punctuation",
            "enable_word_time_offsets",
            "endpointing_sensitivity",
            "speech_end_timeout_ms",
            "persistent_session",
            "stream_rotate_ms",
            "max_reconnect_attempts",
            "reconnect_delay_ms",
        ],
        "Google STT provider-specific option.",
    );

    s
}

fn opt_or_default(
    opts: &std::collections::HashMap<String, serde_json::Value>,
    key: &str,
    default: serde_json::Value,
) -> serde_json::Value {
    opts.get(key).cloned().unwrap_or(default)
}

fn opt_or_null(
    opts: &std::collections::HashMap<String, serde_json::Value>,
    key: &str,
) -> serde_json::Value {
    opts.get(key).cloned().unwrap_or(serde_json::Value::Null)
}

#[cfg(test)]
#[path = "tests/schema.rs"]
mod tests;
