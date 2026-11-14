//! Provider-owned parameter schema for the NeuTTS-Air TTS engine.

use voice_pipes_sdk::TtsConfig;
use voice_pipes_sdk::schema::{ParamDescriptor, ProviderSchema};

const KNOWN_KEYS: &[&str] = &[
    "pool_size",
    "threads",
    "vocoder",
    "reference_audio",
    "reference_text",
];

/// Collect parameters and values for the NeuTTS-Air provider.
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
            ParamDescriptor::static_path(
                "model",
                "Model file",
                "Path to the NeuTTS-Air GGUF language model.",
            ),
            serde_json::json!(p),
        );
    }
    if let Some(v) = cfg.options.get("vocoder") {
        s.push(
            ParamDescriptor::static_path(
                "vocoder",
                "Vocoder file",
                "Path to the NeuCodec ONNX decoder model.",
            ),
            v.clone(),
        );
    }
    if let Some(v) = cfg.options.get("reference_audio") {
        s.push(
            ParamDescriptor::static_path(
                "reference_audio",
                "Reference audio",
                "Path to 3-15s WAV reference audio for voice cloning.",
            ),
            v.clone(),
        );
    }
    if let Some(v) = cfg.options.get("reference_text") {
        s.push(
            ParamDescriptor::static_str(
                "reference_text",
                "Reference transcript",
                "Transcript of the reference audio for voice cloning.",
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
                "Intra-op threads per session; 0 lets runtime choose.",
            ),
            v.clone(),
        );
    }
    if let Some(v) = &cfg.voice {
        s.push(
            ParamDescriptor::static_str("voice", "Voice", "Voice profile ID or preset name."),
            serde_json::json!(v),
        );
    }

    for (k, v) in &cfg.options {
        if !KNOWN_KEYS.contains(&k.as_str()) {
            s.push(
                ParamDescriptor::static_str(k, k, "Provider-specific option."),
                v.clone(),
            );
        }
    }
    s
}
