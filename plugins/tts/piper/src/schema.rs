//! Provider-owned parameter schema for the Piper TTS engine.

use voice_pipes_sdk::TtsConfig;
use voice_pipes_sdk::schema::{ParamDescriptor, ProviderSchema};

const KNOWN_KEYS: &[&str] = &["pool_size"];

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
                "Path to the Piper voice model (`.onnx`).",
            ),
            serde_json::json!(p),
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
    s.passthrough_options(&cfg.options, KNOWN_KEYS, "Piper-specific option.");
    s
}

#[cfg(test)]
#[path = "tests/schema.rs"]
mod tests;
