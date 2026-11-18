//! Provider-owned parameter schema for the Piper TTS engine.

use voice_pipes_sdk::TtsConfig;
use voice_pipes_sdk::schema::{ParamDescriptor, ProviderSchema};

const KNOWN_KEYS: &[&str] = &[
    "pool_size",
    "threads",
    "noise_scale",
    "length_scale",
    "noise_w",
    "espeak_voice",
];

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
    if let Some(v) = cfg.options.get("noise_scale") {
        s.push(
            ParamDescriptor::static_str(
                "noise_scale",
                "Noise scale",
                "Acoustic noise generator variance (0.0 to 2.0; default 0.667). Lower values produce clearer, less raspy speech.",
            ),
            v.clone(),
        );
    }
    if let Some(v) = cfg.options.get("length_scale") {
        s.push(
            ParamDescriptor::static_str(
                "length_scale",
                "Length scale",
                "Speech pacing multiplier (0.1 to 5.0; default 1.0). Higher values speak slower with more articulation.",
            ),
            v.clone(),
        );
    }
    if let Some(v) = cfg.options.get("noise_w") {
        s.push(
            ParamDescriptor::static_str(
                "noise_w",
                "Noise W",
                "Phoneme duration variation scale (0.0 to 2.0; default 0.8). Lower values smooth cadence rhythm.",
            ),
            v.clone(),
        );
    }
    if let Some(v) = cfg.options.get("espeak_voice") {
        s.push(
            ParamDescriptor::static_str(
                "espeak_voice",
                "eSpeak voice",
                "eSpeak-ng voice/language identifier (e.g. 'en-us'). Defaults to the model's configured voice.",
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
