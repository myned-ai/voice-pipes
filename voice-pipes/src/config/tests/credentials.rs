use super::*;

struct KeyedStt;

impl SttFactory for KeyedStt {
    fn name(&self) -> &'static str {
        "keyed"
    }
    fn default_api_key_env(&self) -> Option<&'static str> {
        Some("KEYED_API_KEY")
    }
    fn validate(&self, _cfg: &SttConfig) -> Vec<voice_pipes_sdk::ConfigError> {
        Vec::new()
    }
    fn build(
        &self,
        _cfg: &SttConfig,
        _ctx: &voice_pipes_sdk::BuildContext<'_>,
    ) -> Result<Box<dyn voice_pipes_sdk::SttProvider>, voice_pipes_sdk::BuildError> {
        Err(voice_pipes_sdk::BuildError::failed(
            "not built in this test",
        ))
    }
}

struct KeyedText;

impl TextProcessorFactory for KeyedText {
    fn name(&self) -> &'static str {
        "keyed"
    }
    fn default_api_key_env(&self) -> Option<&'static str> {
        Some("KEYED_API_KEY")
    }
    fn validate(&self, _cfg: &TextProcessorConfig) -> Vec<voice_pipes_sdk::ConfigError> {
        Vec::new()
    }
    fn build(
        &self,
        _cfg: &TextProcessorConfig,
        _ctx: &voice_pipes_sdk::BuildContext<'_>,
    ) -> Result<Box<dyn voice_pipes_sdk::TextProcessor>, voice_pipes_sdk::BuildError> {
        Err(voice_pipes_sdk::BuildError::failed(
            "not built in this test",
        ))
    }
}

fn env(name: &str) -> Option<String> {
    match name {
        "KEYED_API_KEY" => Some("sk-default".into()),
        "OTHER_KEY" => Some("sk-other".into()),
        _ => None,
    }
}

fn key(k: Option<&voice_pipes_sdk::secrets::Secret>) -> Option<&str> {
    k.map(voice_pipes_sdk::secrets::Secret::expose)
}

#[test]
fn a_configured_key_wins_and_the_factory_variable_fills_a_missing_one() {
    let mut cfg: SttConfig = toml::from_str("provider = \"keyed\"").unwrap();
    assert_eq!(
        key(stt(&cfg, &KeyedStt, &env).api_key.as_ref()),
        Some("sk-default")
    );
    cfg.api_key = Some("sk-config".into());
    assert_eq!(
        key(stt(&cfg, &KeyedStt, &env).api_key.as_ref()),
        Some("sk-config")
    );
    assert_eq!(
        key(stt(&cfg, &KeyedStt, &|_| None).api_key.as_ref()),
        Some("sk-config")
    );
}

#[test]
fn nothing_configured_and_nothing_set_is_no_key() {
    let cfg: SttConfig = toml::from_str("provider = \"keyed\"\napi_key = \"  \"").unwrap();
    assert_eq!(stt(&cfg, &KeyedStt, &|_| None).api_key, None);
}

/// `api_key_env` names the variable instead of the factory's, and the
/// key never stays in the options, where a provider would have to read it.
#[test]
fn a_text_processor_reads_the_variable_its_config_names() {
    let cfg: TextProcessorConfig =
        toml::from_str("type = \"keyed\"\napi_key_env = \"OTHER_KEY\"").unwrap();
    assert!(cfg.options.is_empty(), "{:?}", cfg.options.keys());
    assert_eq!(
        key(text_processor(&cfg, &KeyedText, &env).api_key.as_ref()),
        Some("sk-other")
    );
    let cfg: TextProcessorConfig = toml::from_str("type = \"keyed\"").unwrap();
    assert_eq!(
        key(text_processor(&cfg, &KeyedText, &env).api_key.as_ref()),
        Some("sk-default")
    );
}
