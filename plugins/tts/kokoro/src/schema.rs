//! Provider-owned parameter schema for the Kokoro TTS engine.

use voice_pipes_sdk::TtsConfig;
use voice_pipes_sdk::schema::{ParamDescriptor, ProviderSchema};

const KNOWN_KEYS: &[&str] = &["pool_size", "threads", "voices", "voices_path"];

pub fn schema(cfg: &TtsConfig) -> ProviderSchema {
    let mut s = ProviderSchema::default();
    s.push(
        ParamDescriptor::static_str(
            "provider",
            "Provider",
            "The TTS framework or service used for speech synthesis.",
        ),
        serde_json::json!(cfg.provider),
    );
    if let Some(p) = cfg.model.as_deref() {
        s.push(
            ParamDescriptor::static_path("model", "Model file", "Path to the Kokoro ONNX model."),
            serde_json::json!(p),
        );
    }
    if let Some(v) = cfg
        .options
        .get("voices")
        .or_else(|| cfg.options.get("voices_path"))
    {
        s.push(
            ParamDescriptor::static_path(
                "voices",
                "Voices file",
                "Path to the Kokoro voice embeddings (`voices-v1.0.bin`).",
            ),
            v.clone(),
        );
    }
    if let Some(v) = cfg.options.get("pool_size") {
        s.push(
            ParamDescriptor::static_int_display(
                "pool_size",
                "Pool size",
                "Number of concurrent synthesis engine instances.",
            ),
            v.clone(),
        );
    }
    if let Some(v) = cfg.options.get("threads") {
        s.push(
            ParamDescriptor::static_int_display(
                "threads",
                "Threads",
                "Intra-op threads per ONNX session; 0 lets ONNX Runtime choose.",
            ),
            v.clone(),
        );
    }
    if let Some(v) = &cfg.voice {
        // Static, not mutable: Kokoro's factory doesn't adopt a live
        // voice cell, so a PATCH was silently ignored. Advertise it honestly as
        // a config-only value rather than an editable one.
        s.push(
            ParamDescriptor::static_str(
                "voice",
                "Voice",
                "Default voice ID (e.g. `af_heart`). Set in config; not live-editable.",
            ),
            serde_json::json!(v),
        );
    }
    s.passthrough_options(&cfg.options, KNOWN_KEYS, "Kokoro-specific option.");
    s
}
