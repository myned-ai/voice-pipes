//! The factory the binary registers to build the English Nemotron recogniser.

use std::path::PathBuf;

use voice_pipes_sdk::stt::SttProvider;

use crate::config::NemotronOptions;
use crate::provider::NemotronStt;

/// The factory the binary registers.
pub fn factory() -> Box<dyn voice_pipes_sdk::SttFactory> {
    Box::new(NemotronFactory)
}

#[derive(Debug)]
pub(crate) struct NemotronFactory;

impl voice_pipes_sdk::SttFactory for NemotronFactory {
    fn name(&self) -> &'static str {
        "nemotron-streaming"
    }

    fn aliases(&self) -> &[&str] {
        &["nemotron"]
    }

    fn is_local(&self) -> bool {
        true
    }

    fn validate(&self, cfg: &voice_pipes_sdk::SttConfig) -> Vec<voice_pipes_sdk::ConfigError> {
        NemotronOptions::read(cfg).err().unwrap_or_default()
    }

    fn build(
        &self,
        cfg: &voice_pipes_sdk::SttConfig,
        _ctx: &voice_pipes_sdk::BuildContext<'_>,
    ) -> Result<Box<dyn SttProvider>, voice_pipes_sdk::BuildError> {
        let default_dir = PathBuf::from("models/nemotron-streaming/7-10-7");
        let model_dir = cfg.model_dir.as_deref().unwrap_or(&default_dir);

        let NemotronOptions {
            threads,
            blank_id,
            max_tokens_per_step,
            pool_size,
        } = NemotronOptions::read(cfg).map_err(voice_pipes_sdk::options::refused)?;

        let provider = NemotronStt::with_options(
            model_dir,
            threads,
            blank_id,
            max_tokens_per_step,
            pool_size,
        )?;
        Ok(Box::new(provider))
    }
}
