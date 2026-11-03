//! Provider-owned parameter schema for the local Parakeet TDT STT.

use voice_pipes_sdk::SttConfig;
use voice_pipes_sdk::schema::{ParamDescriptor, ProviderSchema};

pub fn schema(cfg: &SttConfig) -> ProviderSchema {
    let mut s = ProviderSchema::default();

    s.push(
        ParamDescriptor::static_str(
            "provider",
            "Provider",
            "The STT framework or service used for transcription.",
        ),
        cfg.provider.clone(),
    );
    if let Some(p) = cfg.model_dir.as_deref() {
        s.push(
            ParamDescriptor::static_path(
                "model_dir",
                "Model directory",
                "Directory that contains the Parakeet model weight files \
                 loaded at startup.",
            ),
            p.to_string_lossy().into_owned(),
        );
    }
    if let Some(v) = cfg.options.get("pool_size") {
        s.push(
            ParamDescriptor::static_int_display(
                "pool_size",
                "Pool size",
                "Number of concurrent inference sessions. Higher values allow more \
                 simultaneous transcriptions at the cost of additional VRAM/RAM.",
            ),
            v.clone(),
        );
    }

    // Pass through any other Parakeet tunables (initial_buffer_ms, inference_step_ms, ...)
    s.passthrough_options(
        &cfg.options,
        &["pool_size"],
        "Parakeet provider-specific option.",
    );

    s
}
