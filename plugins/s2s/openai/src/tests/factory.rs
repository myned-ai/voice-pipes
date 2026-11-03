use voice_pipes_sdk::BuildContext;
use voice_pipes_sdk::s2s::{RealtimeConfig, RealtimeFactory, SessionConfig};

use crate::OpenAiRealtimeFactory;

/// Settings as the engine hands them over: the key already resolved.
fn cfg() -> RealtimeConfig {
    RealtimeConfig::new("openai-realtime")
        .with_api_key(Some("sk-test".into()))
        .with_api_key_var("OPENAI_API_KEY".into())
}

fn ctx() -> BuildContext<'static> {
    BuildContext::new()
}

#[test]
fn answers_to_its_name_and_aliases() {
    for name in ["openai-realtime", "openai", "gpt-realtime"] {
        assert!(OpenAiRealtimeFactory.matches(name), "{name}");
    }
    assert!(!OpenAiRealtimeFactory.matches("gemini"));
}

#[test]
fn reads_its_key_from_openai_api_key_by_default() {
    assert_eq!(
        OpenAiRealtimeFactory.default_api_key_env(),
        "OPENAI_API_KEY"
    );
}

#[test]
fn a_config_with_its_key_validates_and_builds() {
    let cfg = cfg();
    let errors = OpenAiRealtimeFactory.validate(&cfg);
    assert!(errors.is_empty(), "{errors:?}");
    let proto = OpenAiRealtimeFactory.build(&cfg, &ctx()).expect("build");
    assert_eq!(proto.name(), "openai");
    // The key the engine resolved is the one sent.
    let info = proto.connect_info().expect("connect info");
    assert!(
        info.headers
            .iter()
            .any(|(k, v)| k == "Authorization" && v == "Bearer sk-test")
    );
}

#[test]
fn a_missing_key_is_reported_with_the_variable_to_set() {
    let cfg = cfg()
        .with_api_key(None)
        .with_api_key_var("VP_TEST_REALTIME_KEY".into());
    let errors = OpenAiRealtimeFactory.validate(&cfg);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].field, "realtime_provider.api_key_env");
    assert!(
        errors[0].message.contains("VP_TEST_REALTIME_KEY"),
        "{}",
        errors[0].message
    );
    // The build refuses it as validation does, before any connection.
    voice_pipes_sdk::testing::build_refuses_what_validate_refuses(
        &[cfg],
        |c| OpenAiRealtimeFactory.validate(c),
        |c| OpenAiRealtimeFactory.build(c, &ctx()),
    );
}

#[test]
fn a_model_override_builds() {
    let cfg = cfg().with_model(Some("gpt-realtime-mini".into()));
    let proto = OpenAiRealtimeFactory.build(&cfg, &ctx()).expect("build");
    assert_eq!(proto.name(), "openai");
}

/// A session that waits to be spoken to gets no opening kick and has its
/// input transcribed; one that speaks first is kicked, and one that asks
/// for no transcript gets none.
#[test]
fn the_session_decides_who_speaks_first_and_what_is_transcribed() {
    let envelopes = |session: SessionConfig| {
        OpenAiRealtimeFactory
            .build(&cfg(), &ctx())
            .expect("build")
            .build_session_update(&session)
    };
    let answering = envelopes(SessionConfig::default());
    assert_eq!(answering.len(), 1, "no opening kick: {answering:?}");
    assert_eq!(
        answering[0]["session"]["audio"]["input"]["transcription"]["model"],
        crate::DEFAULT_TRANSCRIPTION_MODEL
    );
    let calling = envelopes(
        SessionConfig::default()
            .with_speaks_first(true)
            .with_transcribe_input(false),
    );
    assert_eq!(
        calling.len(),
        2,
        "the first response is kicked: {calling:?}"
    );
    assert_eq!(calling[1]["type"], "response.create");
    assert!(
        calling[0]["session"]["audio"]["input"]
            .get("transcription")
            .is_none(),
        "{calling:?}"
    );
}

/// The wizard entry writes a config this factory accepts.
#[test]
fn the_wizard_entry_renders_a_valid_config() {
    let choices = crate::wizard_choices();
    assert_eq!(choices.len(), 1);
    let rendered = choices[0].render_defaults("provider", "openai-realtime");
    assert!(
        rendered.toml.contains("options.output_emit_rate = 16000\n"),
        "{}",
        rendered.toml
    );
    let table: toml::Table = format!("[realtime_provider]\n{}", rendered.toml)
        .parse()
        .expect("valid TOML");
    let section = table["realtime_provider"].as_table().expect("a table");
    assert_eq!(section["model"].as_str(), Some("gpt-realtime"));
    assert_eq!(
        section["options"]["output_emit_rate"].as_integer(),
        Some(16_000)
    );
}
