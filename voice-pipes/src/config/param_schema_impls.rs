//! [`ComponentParamSchema`] implementations for every pipeline component config.
//!
//! Each config struct declares its own parameter schema: what params exist,
//! their descriptions, visibility conditions, and mutability. The graph builder
//! and frontend renderer consume these generically -- they contain zero
//! component-specific knowledge.
//!
//! To add a new component type, implement the trait on its config struct here.
//! Nothing else needs to change.

use crate::api::param_schema::{ComponentParamSchema, ParamDescriptor};
use crate::config::types::{
    BargeInConfig, RealtimeProviderConfig, SttConfig, TextProcessorConfig, TtsConfig,
    TurnDetectionConfig, VadSettings,
};

// ── VAD ─────────────────────────────────────────────────────

impl ComponentParamSchema for VadSettings {
    fn param_schema(&self) -> Vec<ParamDescriptor> {
        // The modes this build has: auto, then every registered backend.
        let backends: Vec<String> = crate::vad::segmenter::vad_factories()
            .iter()
            .map(|f| f.name().to_string())
            .collect();
        let mut modes: Vec<&str> = vec!["auto"];
        modes.extend(backends.iter().map(String::as_str));
        // The model and probability settings belong to the model-based
        // backends: every mode but the two built-in rule-based ones.
        let model_modes: Vec<&str> = modes
            .iter()
            .copied()
            .filter(|m| !matches!(*m, "energy" | "passthrough"))
            .collect();
        vec![
            ParamDescriptor::static_enum(
                "mode",
                "Mode",
                "VAD backend: 'auto' runs the binary's model-based backend when its model \
                 is available, else energy; 'energy' uses RMS amplitude; 'passthrough' \
                 classifies nothing; any other mode names a model-based backend.",
                &modes,
            ),
            ParamDescriptor::static_path(
                "model",
                "Model",
                "Path to the VAD model file, for a model-based backend (and auto).",
            )
            .only_when("mode", &model_modes),
            // Energy threshold -- only relevant when energy detector is active
            ParamDescriptor::mutable_float(
                "energy_threshold",
                "Energy threshold",
                "RMS amplitude threshold for energy VAD. Frames with RMS below this value \
                 are treated as silence. Increase to reduce false positives from background noise.",
                0.0,
                10_000.0,
            )
            .only_when("mode", &["energy", "auto"]),
            // Probability thresholds -- only relevant for a model-based backend
            ParamDescriptor::mutable_float(
                "speech_prob",
                "Speech probability",
                "Probability threshold above which a frame is labelled as speech \
                 (model-based backends, range 0.0-1.0, default 0.5). Lower values \
                 increase sensitivity.",
                0.0,
                1.0,
            )
            .only_when("mode", &model_modes),
            ParamDescriptor::mutable_float(
                "neg_threshold",
                "End-of-speech probability",
                "Probability below which an ongoing speech segment is considered ended \
                 (model-based backends, range 0.0-1.0, default 0.35).",
                0.0,
                1.0,
            )
            .only_when("mode", &model_modes),
            // NOTE: `min_speech_ms` / `min_silence_ms` are not in the live
            // schema: the VAD does not read them, so editing them would do
            // nothing. They belong here once duration debouncing is
            // implemented; they remain accepted TOML fields.
        ]
    }

    fn current_values(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut m = serde_json::Map::new();
        m.insert("mode".into(), serde_json::json!(self.mode));
        // Always emit effective threshold values so the frontend can display them
        // even if the raw config field is None (defaults are computed).
        m.insert(
            "energy_threshold".into(),
            serde_json::json!(self.effective_energy_threshold()),
        );
        m.insert(
            "speech_prob".into(),
            serde_json::json!(self.effective_speech_prob()),
        );
        // Optional model-backend fields -- emit null when not set so only_when
        // filtering on the frontend can still evaluate correctly.
        if let Some(p) = self.model.as_deref() {
            m.insert("model".into(), serde_json::json!(p));
        }
        if let Some(v) = self.neg_threshold {
            m.insert("neg_threshold".into(), serde_json::json!(v));
        }
        m
    }
}

// ── STT / TTS / Text processor -- provider-owned schemas ────
//
// The blocks below are thin dispatchers. Each provider lives in its own
// module (e.g. `crate::stt::assemblyai::schema`) and is wired in via the
// factory trait's `provider_schema()` method. Adding or removing a provider
// requires zero changes to this file -- only the provider's own module and
// its registration in the corresponding `*_factories()` list.

use crate::api::param_schema::ProviderSchema;

impl ComponentParamSchema for SttConfig {
    fn param_schema(&self) -> Vec<ParamDescriptor> {
        stt_schema(self).descriptors
    }

    fn current_values(&self) -> serde_json::Map<String, serde_json::Value> {
        stt_schema(self).current_values
    }
}

/// A free function because `SttConfig` is defined in voice-pipes-sdk, so we
/// can't add inherent methods to it here.
fn stt_schema(cfg: &SttConfig) -> ProviderSchema {
    match crate::stt::lookup_stt_factory(&cfg.provider) {
        Some(f) => f.provider_schema(cfg),
        None => ProviderSchema::fallback(
            &cfg.provider,
            "Unknown STT provider -- no schema registered.",
        ),
    }
}

impl ComponentParamSchema for TtsConfig {
    fn param_schema(&self) -> Vec<ParamDescriptor> {
        tts_schema(self).descriptors
    }

    fn current_values(&self) -> serde_json::Map<String, serde_json::Value> {
        tts_schema(self).current_values
    }
}

/// A free function because `TtsConfig` is defined in voice-pipes-sdk, so we
/// can't add inherent methods to it here.
fn tts_schema(cfg: &TtsConfig) -> ProviderSchema {
    match crate::tts::lookup_tts_factory(&cfg.provider) {
        Some(f) => f.provider_schema(cfg),
        None => ProviderSchema::fallback(
            &cfg.provider,
            "Unknown TTS provider -- no schema registered.",
        ),
    }
}

impl ComponentParamSchema for TextProcessorConfig {
    fn param_schema(&self) -> Vec<ParamDescriptor> {
        text_processor_schema(self).descriptors
    }

    fn current_values(&self) -> serde_json::Map<String, serde_json::Value> {
        text_processor_schema(self).current_values
    }
}

/// A free function because `TextProcessorConfig` is defined in
/// voice-pipes-sdk, so we can't add inherent methods to it here.
fn text_processor_schema(cfg: &TextProcessorConfig) -> ProviderSchema {
    match crate::text::lookup_text_processor_factory(&cfg.processor_type) {
        Some(f) => f.provider_schema(cfg),
        None => ProviderSchema::fallback(
            &cfg.processor_type,
            "Unknown text-processor type -- no schema registered.",
        ),
    }
}

// ── Barge-in ─────────────────────────────────────────────────

impl ComponentParamSchema for BargeInConfig {
    fn param_schema(&self) -> Vec<ParamDescriptor> {
        vec![
            ParamDescriptor::static_bool(
                "enabled",
                "Enabled",
                "Whether barge-in detection is active. When enabled, sustained speech \
                 from the user while the assistant is speaking triggers an interruption.",
            ),
            ParamDescriptor::mutable_int(
                "min_chunks",
                "Min chunks",
                "Consecutive speech chunks required to trigger an interrupt. Each chunk \
                 is ~20 ms, so the default of 3 requires ~60 ms of sustained speech.",
                1,
                50,
            ),
            ParamDescriptor::mutable_float(
                "speech_threshold",
                "Speech threshold",
                "RMS amplitude threshold for speech detection during barge-in. Higher \
                 values reduce false positives from background noise (default 200.0).",
                0.0,
                10_000.0,
            ),
        ]
    }

    fn current_values(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut m = serde_json::Map::new();
        m.insert("enabled".into(), serde_json::json!(self.enabled));
        m.insert("min_chunks".into(), serde_json::json!(self.min_chunks));
        m.insert(
            "speech_threshold".into(),
            serde_json::json!(self.speech_threshold),
        );
        m
    }
}

// ── Realtime Provider ──────────────────────────────────────────

impl ComponentParamSchema for RealtimeProviderConfig {
    fn param_schema(&self) -> Vec<ParamDescriptor> {
        vec![
            ParamDescriptor::mutable_str(
                "model",
                "Model",
                "Provider-specific model identifier (e.g. gpt-realtime-2).",
            ),
            ParamDescriptor::mutable_str(
                "voice",
                "Voice",
                "Provider-specific voice / speaker ID (e.g. alloy, sage).",
            ),
            ParamDescriptor::mutable_str(
                "vad",
                "VAD",
                "Turn-detection mode hint (e.g. semantic_vad).",
            ),
            ParamDescriptor::mutable_str(
                "instructions",
                "Instructions",
                "System / persona instructions passed at session-start time.",
            ),
        ]
    }

    fn current_values(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut m = serde_json::Map::new();
        if let Some(model) = self.model.as_deref() {
            m.insert("model".into(), serde_json::json!(model));
        }
        if let Some(voice) = self.voice.as_deref() {
            m.insert("voice".into(), serde_json::json!(voice));
        }
        if let Some(vad) = self.vad.as_deref() {
            m.insert("vad".into(), serde_json::json!(vad));
        }
        if let Some(inst) = self.instructions.as_deref() {
            m.insert("instructions".into(), serde_json::json!(inst));
        }
        m
    }
}

// ── Turn Detection ───────────────────────────────────────────

impl ComponentParamSchema for TurnDetectionConfig {
    fn param_schema(&self) -> Vec<ParamDescriptor> {
        let factories = crate::stt::turn::turn_detector_factories();
        let modes: Vec<&str> = factories.iter().map(|f| f.name()).collect();
        let mut out = vec![
            ParamDescriptor::static_enum(
                "mode",
                "Mode",
                "Turn detection mode: 'silence' uses silence timeout; 'provider' uses provider end signal; \
                 'layered' combines both; any other mode is a detector a plugin registered.",
                &modes,
            ),
            ParamDescriptor::mutable_int(
                "silence_timeout_ms",
                "Silence timeout (ms)",
                "Maximum silence duration in milliseconds before a turn is forced complete (fallback or primary).",
                0,
                30_000,
            ),
        ];
        // The selected detector describes its own settings.
        if let Some(factory) = factories.iter().find(|f| f.matches(&self.mode)) {
            out.extend(
                factory
                    .param_schema()
                    .into_iter()
                    .map(|d| d.only_when("mode", &[self.mode.as_str()])),
            );
        }
        out
    }

    fn current_values(&self) -> serde_json::Map<String, serde_json::Value> {
        let mut m = serde_json::Map::new();
        m.insert("mode".into(), serde_json::json!(self.mode));
        m.insert(
            "silence_timeout_ms".into(),
            serde_json::json!(self.silence_timeout_ms),
        );
        for (k, v) in &self.options {
            m.insert(k.clone(), v.clone());
        }
        m
    }
}
