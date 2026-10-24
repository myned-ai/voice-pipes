//! A model-based VAD backend for the engine's own tests. The engine builds
//! none: the binary links one (Silero) and names it the auto backend. This
//! one is shaped as Silero is (a model it must have, the model registry's
//! `silero` files in its wizard entry), so the auto mode, the observer's
//! checks and the wizard's `[vad]` block are tested as they run.

use voice_pipes_sdk::vad::{VadBackend, VadFactory};
use voice_pipes_sdk::{BuildContext, BuildError, ConfigError, VadConfig};

/// The name the stand-in registers under.
pub(crate) const NAME: &str = "stand-in-model-vad";

struct ModelVad;

impl VadFactory for ModelVad {
    fn name(&self) -> &'static str {
        NAME
    }
    fn validate(&self, cfg: &VadConfig) -> Vec<ConfigError> {
        if cfg.model.is_none() {
            vec![ConfigError::new("vad.model", "needs a model path")]
        } else {
            vec![]
        }
    }
    fn can_run(&self, cfg: &VadConfig) -> bool {
        cfg.model.as_deref().is_some_and(std::path::Path::exists)
    }
    fn build(
        &self,
        _cfg: &VadConfig,
        _ctx: &BuildContext<'_>,
    ) -> Result<Box<dyn VadBackend>, BuildError> {
        Err(BuildError::failed("the stand-in loads no model"))
    }
}

/// Register the stand-in with its wizard entry and name it the auto
/// backend, once per process. Every test that depends on the auto backend
/// calls this, so each sees the same state alone or after others.
pub(crate) fn register() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        crate::plugins::register_plugin(
            crate::plugins::Plugin::Vad(|| Box::new(ModelVad)),
            vec![
                voice_pipes_sdk::WizardChoice::new(
                    "Stand-in model VAD",
                    voice_pipes_sdk::WizardDeploy::Registry {
                        key: "silero".into(),
                    },
                )
                .with_fields(vec![voice_pipes_sdk::WizardField::path(
                    "model",
                    "Stand-in VAD model path",
                    "models/silero_vad.onnx",
                )]),
            ],
        )
        .expect("the stand-in VAD registers");
        crate::vad::segmenter::register_auto_backend(NAME).expect("the stand-in is auto's");
    });
}
