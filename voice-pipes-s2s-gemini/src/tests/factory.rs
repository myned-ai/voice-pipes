use voice_pipes_sdk::BuildContext;
use voice_pipes_sdk::s2s::{RealtimeConfig, RealtimeFactory, SessionConfig};

use crate::GeminiLiveFactory;

/// Settings as the engine hands them over: the key already resolved.
fn cfg() -> RealtimeConfig {
    RealtimeConfig::new("gemini-live")
        .with_api_key(Some("AIza-test".into()))
        .with_api_key_var("GEMINI_API_KEY".into())
}

#[test]
fn answers_to_its_name_and_aliases() {
    for name in ["gemini-live", "gemini", "google-live"] {
        assert!(GeminiLiveFactory.matches(name), "{name}");
    }
    assert!(!GeminiLiveFactory.matches("openai"));
}

#[test]
fn reads_its_key_from_gemini_api_key_by_default() {
    assert_eq!(GeminiLiveFactory.default_api_key_env(), "GEMINI_API_KEY");
}

#[test]
fn a_missing_key_is_reported_and_a_present_one_is_sent() {
    let missing = cfg()
        .with_api_key(None)
        .with_api_key_var("VP_TEST_GEMINI_KEY".into());
    let errors = GeminiLiveFactory.validate(&missing);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].field, "realtime_provider.api_key_env");
    assert!(errors[0].message.contains("VP_TEST_GEMINI_KEY"));

    assert!(GeminiLiveFactory.validate(&cfg()).is_empty());
    let proto = GeminiLiveFactory
        .build(&cfg(), &BuildContext::new())
        .expect("build");
    // The key rides the URL query; Debug of the target never shows it.
    let info = proto.connect_info().expect("connect info");
    assert!(
        info.url.ends_with("key=AIza-test"),
        "the key the engine resolved"
    );
    assert!(!format!("{info:?}").contains("AIza-test"));
}

/// Gemini Live has no response kick: a session that speaks first opens
/// with a user turn instead, which one that waits never sends.
#[test]
fn a_session_that_speaks_first_sends_an_opener() {
    let envelopes = |session: SessionConfig| {
        GeminiLiveFactory
            .build(&cfg(), &BuildContext::new())
            .expect("build")
            .build_session_update(&session)
    };
    assert_eq!(envelopes(SessionConfig::default()).len(), 1);
    let calling = envelopes(SessionConfig::default().with_speaks_first(true));
    assert_eq!(calling.len(), 2, "{calling:?}");
    assert!(calling[1].get("clientContent").is_some(), "{calling:?}");
}

/// The wizard entry writes a config this factory accepts, with the
/// thinking budget as a number in the options table.
#[test]
fn the_wizard_entry_renders_a_valid_config() {
    let choices = crate::wizard_choices();
    assert_eq!(choices.len(), 1);
    assert!(
        choices[0].stage_default,
        "the stage's out-of-the-box choice"
    );
    let rendered = choices[0].render_defaults("provider", "gemini-live");
    let table: toml::Table = format!("[realtime_provider]\n{}", rendered.toml)
        .parse()
        .expect("valid TOML");
    let section = table["realtime_provider"].as_table().expect("a table");
    assert_eq!(section["provider"].as_str(), Some("gemini-live"));
    assert_eq!(
        section["model"].as_str(),
        Some("gemini-2.5-flash-native-audio-latest")
    );
    assert_eq!(section["options"]["thinking_budget"].as_integer(), Some(0));
    // Turn-taking: Gemini's defaults once left a request unanswered for
    // 21 s; the wizard writes the two options that make it close turns.
    assert_eq!(section["options"]["end_sensitivity"].as_str(), Some("high"));
    assert_eq!(section["options"]["vad_silence_ms"].as_integer(), Some(700));
    assert!(
        rendered.toml.contains("# Native-audio models"),
        "{}",
        rendered.toml
    );
}

/// `thinking_level`, which the 3.x Live models take, is an option the
/// factory reads (refused as unknown it would fail every such config), and
/// a level the API does not have is refused.
#[test]
fn thinking_level_is_read_and_bounded() {
    let with = |level: &str| {
        cfg().with_options(
            [("thinking_level".to_string(), serde_json::json!(level))]
                .into_iter()
                .collect(),
        )
    };
    assert!(GeminiLiveFactory.validate(&with("low")).is_empty());
    let errors = GeminiLiveFactory.validate(&with("extreme"));
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].field, "realtime_provider.options.thinking_level");
}
