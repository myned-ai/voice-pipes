//! Provider-owned parameter schema for the `AssemblyAI` streaming STT.
//!
//! This is the **only** file the graph editor consults to discover the
//! `AssemblyAI`-specific knobs. The shared
//! [`SttConfig`](voice_pipes_sdk::SttConfig) implementation in
//! `config::param_schema_impls` does nothing more than dispatch to
//! [`schema()`] via the [`SttFactory::provider_schema`](voice_pipes_sdk::SttFactory)
//! trait method. Removing `AssemblyAI` support is a matter of deleting this
//! module + the one line in `stt::stt_factories()`.

use voice_pipes_sdk::SttConfig;
use voice_pipes_sdk::schema::{ParamDescriptor, ProviderSchema};

/// Build the schema + current-value snapshot for an `AssemblyAI` configuration.
///
/// All emitted descriptors are currently `static` (read-only in the editor).
/// `AssemblyAI`'s V3 streaming API does support an `UpdateConfiguration` WS
/// message for `keyterms_prompt`, `prompt`, `min_turn_silence` and
/// `max_turn_silence` -- once that wiring is hooked up in
/// [`crate::ws_client::WsClient::run`] those four rows
/// should be promoted to `mutable_*` here.
pub fn schema(cfg: &SttConfig) -> ProviderSchema {
    let mut s = ProviderSchema::default();

    // ── Identity ──────────────────────────────────────────
    s.push(
        ParamDescriptor::static_str(
            "provider",
            "Provider",
            "The STT framework or service used for transcription.",
        ),
        cfg.provider.clone(),
    );
    if let Some(model_name) = &cfg.model_name {
        s.push(
            ParamDescriptor::static_str(
                "model_name",
                "Model",
                "AssemblyAI speech model identifier (e.g. `u3-rt-pro`, \
                 `universal-streaming-english`).",
            ),
            model_name.clone(),
        );
    }
    if let Some(endpoint) = &cfg.endpoint {
        s.push(
            ParamDescriptor::static_str(
                "endpoint",
                "Endpoint",
                "Override for the streaming WebSocket endpoint \
                 (default: `wss://streaming.assemblyai.com/v3/ws`).",
            ),
            endpoint.clone(),
        );
    }
    s.push(
        ParamDescriptor::static_bool(
            "api_key_set",
            "API key",
            "Whether an AssemblyAI API key is configured (env: `ASSEMBLYAI_API_KEY`).",
        ),
        cfg.api_key.is_some(),
    );

    // ── Streaming / endpointing knobs ──────────────────────
    s.push(
        ParamDescriptor::static_int_display(
            "sample_rate",
            "Sample rate",
            "PCM input sample rate in Hz sent on the wire (default 16000).",
        ),
        opt_or_default(&cfg.options, "sample_rate", serde_json::json!(16_000)),
    );
    s.push(
        ParamDescriptor::static_int_display(
            "min_turn_silence",
            "Min turn silence (ms)",
            "Minimum silence (ms) before a turn can end. Lower = more aggressive \
             endpointing. Takes effect on next session.",
        ),
        opt_or_null(&cfg.options, "min_turn_silence"),
    );
    s.push(
        ParamDescriptor::static_int_display(
            "max_turn_silence",
            "Max turn silence (ms)",
            "Maximum silence (ms) before the server forces a turn to end. \
             Takes effect on next session.",
        ),
        opt_or_null(&cfg.options, "max_turn_silence"),
    );
    s.push(
        ParamDescriptor::static_str(
            "end_of_turn_confidence_threshold",
            "EOT confidence threshold",
            "Confidence (0.0-1.0) the model must reach before emitting end-of-turn. \
             Takes effect on next session.",
        ),
        opt_or_null(&cfg.options, "end_of_turn_confidence_threshold"),
    );
    s.push(
        ParamDescriptor::static_bool(
            "format_turns",
            "Format turns",
            "Apply punctuation/casing formatting to final turns. \
             Takes effect on next session.",
        ),
        opt_or_null(&cfg.options, "format_turns"),
    );
    s.push(
        ParamDescriptor::static_bool(
            "language_detection",
            "Language detection",
            "Enable automatic language detection (Universal-3 multilingual models only). \
             Takes effect on next session.",
        ),
        opt_or_null(&cfg.options, "language_detection"),
    );
    s.push(
        ParamDescriptor::static_str(
            "prompt",
            "Prompt",
            "Free-form prompt text used to bias transcription. \
             Takes effect on next session.",
        ),
        opt_or_null(&cfg.options, "prompt"),
    );
    s.push(
        ParamDescriptor::static_str(
            "keyterms_prompt",
            "Key terms",
            "JSON array of key-term phrases to bias recognition (max 100, \
             each <= 50 chars). Takes effect on next session.",
        ),
        opt_or_null(&cfg.options, "keyterms_prompt"),
    );

    // ── Reconnect tuning ───────────────────────────────────
    s.push(
        ParamDescriptor::static_int_display(
            "max_reconnect_attempts",
            "Max reconnects",
            "Number of reconnect attempts on transient WebSocket failures.",
        ),
        opt_or_null(&cfg.options, "max_reconnect_attempts"),
    );
    s.push(
        ParamDescriptor::static_int_display(
            "reconnect_delay_ms",
            "Reconnect base delay (ms)",
            "Initial backoff delay between reconnect attempts.",
        ),
        opt_or_null(&cfg.options, "reconnect_delay_ms"),
    );
    s.push(
        ParamDescriptor::static_int_display(
            "reconnect_delay_max_ms",
            "Reconnect max delay (ms)",
            "Upper-bound backoff delay between reconnect attempts.",
        ),
        opt_or_null(&cfg.options, "reconnect_delay_max_ms"),
    );

    // Pass-through any unknown options, skipping known/handled keys + secrets.
    s.passthrough_options(
        &cfg.options,
        &[
            "sample_rate",
            "min_turn_silence",
            "max_turn_silence",
            "end_of_turn_confidence_threshold",
            "format_turns",
            "language_detection",
            "prompt",
            "keyterms_prompt",
            "max_reconnect_attempts",
            "reconnect_delay_ms",
            "reconnect_delay_max_ms",
        ],
        "AssemblyAI provider-specific option.",
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
