use std::collections::HashMap;

use voice_pipes_sdk::s2s::{RealtimeConfig, RealtimeFactory};

use crate::ElevenLabsFactory;

/// Settings as the engine hands them over: the key already resolved.
fn cfg() -> RealtimeConfig {
    RealtimeConfig::new("elevenlabs")
        .with_api_key(Some("xi".into()))
        .with_api_key_var("ELEVENLABS_API_KEY".into())
}

/// An environment holding exactly `vars`.
fn env_with(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
    let vars: HashMap<String, String> = vars
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    move |name| vars.get(name).cloned()
}

#[test]
fn answers_to_its_name_and_aliases() {
    for name in ["elevenlabs", "elevenlabs-agents", "11labs"] {
        assert!(ElevenLabsFactory.matches(name), "{name}");
    }
}

#[test]
fn reads_its_key_from_elevenlabs_api_key_by_default() {
    assert_eq!(
        ElevenLabsFactory.default_api_key_env(),
        "ELEVENLABS_API_KEY"
    );
}

#[test]
fn an_agent_id_is_required() {
    let env = env_with(&[]);
    let mut cfg = cfg();
    let errors = ElevenLabsFactory::validate_with(&cfg, &env);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].field, "realtime_provider.options.agent_id");
    assert!(ElevenLabsFactory::build_with(&cfg, &env).is_err());

    cfg.options
        .insert("agent_id".to_string(), serde_json::json!("agent_123"));
    assert!(ElevenLabsFactory::validate_with(&cfg, &env).is_empty());
    let proto = ElevenLabsFactory::build_with(&cfg, &env).expect("build");
    assert_eq!(proto.name(), "elevenlabs");
}

#[test]
fn the_agent_id_can_come_from_the_environment() {
    let env = env_with(&[("VP_TEST_XI_AGENT_ID", "agent_from_env")]);
    let mut cfg = cfg();
    cfg.options.insert(
        "agent_id_env".to_string(),
        serde_json::json!("VP_TEST_XI_AGENT_ID"),
    );
    assert!(ElevenLabsFactory::validate_with(&cfg, &env).is_empty());
    ElevenLabsFactory::build_with(&cfg, &env).expect("build from env");

    // A literal agent_id wins over the env fallback.
    cfg.options
        .insert("agent_id".to_string(), serde_json::json!("agent_literal"));
    assert!(ElevenLabsFactory::validate_with(&cfg, &env).is_empty());

    // An unset variable behaves like a missing agent id.
    cfg.options.remove("agent_id");
    cfg.options.insert(
        "agent_id_env".to_string(),
        serde_json::json!("VP_TEST_XI_AGENT_UNSET"),
    );
    let errors = ElevenLabsFactory::validate_with(&cfg, &env);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].field, "realtime_provider.options.agent_id");
    assert!(ElevenLabsFactory::build_with(&cfg, &env).is_err());
}

#[test]
fn an_out_of_range_completion_idle_is_refused() {
    let env = env_with(&[]);
    let mut cfg = cfg();
    cfg.options
        .insert("agent_id".to_string(), serde_json::json!("agent_123"));
    cfg.options
        .insert("completion_idle_ms".to_string(), serde_json::json!(50));
    let errors = ElevenLabsFactory::validate_with(&cfg, &env);
    assert!(
        errors
            .iter()
            .any(|e| e.field == "realtime_provider.options.completion_idle_ms"),
        "{errors:?}"
    );
    cfg.options
        .insert("completion_idle_ms".to_string(), serde_json::json!(2000));
    assert!(ElevenLabsFactory::validate_with(&cfg, &env).is_empty());
}

#[test]
fn a_public_agent_needs_no_key() {
    let env = env_with(&[]);
    let mut cfg = cfg().with_api_key(None);
    cfg.options
        .insert("agent_id".to_string(), serde_json::json!("agent_123"));
    let errors = ElevenLabsFactory::validate_with(&cfg, &env);
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].field, "realtime_provider.api_key_env");

    cfg.options
        .insert("public_agent".to_string(), serde_json::json!(true));
    assert!(ElevenLabsFactory::validate_with(&cfg, &env).is_empty());
}

/// A public agent sends no key, even when one is configured: the endpoint
/// check lets a plain `ws://` endpoint through for a public agent, so a key
/// on that connection would travel in clear.
#[test]
fn a_public_agent_never_sends_the_key() {
    let env = env_with(&[]);
    let mut cfg = cfg();
    cfg.options
        .insert("agent_id".to_string(), serde_json::json!("agent_123"));
    cfg.options
        .insert("public_agent".to_string(), serde_json::json!(true));
    cfg.options
        .insert("endpoint".to_string(), serde_json::json!("ws://x"));
    let errors = ElevenLabsFactory::validate_with(&cfg, &env);
    assert!(errors.is_empty(), "{errors:?}");

    let protocol =
        ElevenLabsFactory::build_with(&cfg, &env).unwrap_or_else(|e| panic!("build failed: {e}"));
    let info = protocol.connect_info().expect("connect info");
    assert!(
        !info
            .headers
            .iter()
            .any(|(name, _)| name.eq_ignore_ascii_case("xi-api-key")),
        "a public agent sent its key: {:?}",
        info.headers.iter().map(|(n, _)| n).collect::<Vec<_>>()
    );
}

/// A private agent still authenticates with the key.
#[test]
fn a_private_agent_sends_the_key() {
    let env = env_with(&[]);
    let mut cfg = cfg();
    cfg.options
        .insert("agent_id".to_string(), serde_json::json!("agent_123"));
    let protocol =
        ElevenLabsFactory::build_with(&cfg, &env).unwrap_or_else(|e| panic!("build failed: {e}"));
    let info = protocol.connect_info().expect("connect info");
    assert!(info.headers.iter().any(|(name, _)| name == "xi-api-key"));
}

/// The wizard entry asks for the agent id and refuses to go on without
/// one; given one, it writes a config this factory accepts.
#[test]
fn the_wizard_entry_needs_an_agent_id() {
    let choices = crate::wizard_choices();
    assert_eq!(choices.len(), 1);
    let answer_with = |id: &'static str| {
        move |f: &voice_pipes_sdk::WizardField| {
            Ok(if f.key == "options.agent_id" {
                id.to_string()
            } else {
                f.default.clone()
            })
        }
    };
    let err = choices[0]
        .render("provider", "elevenlabs", answer_with(""))
        .unwrap_err();
    assert!(err.to_string().contains("agent id"), "{err}");

    let rendered = choices[0]
        .render("provider", "elevenlabs", answer_with("agent_abc"))
        .expect("render");
    let table: toml::Table = format!("[realtime_provider]\n{}", rendered.toml)
        .parse()
        .expect("valid TOML");
    let section = table["realtime_provider"].as_table().expect("a table");
    assert_eq!(section["options"]["agent_id"].as_str(), Some("agent_abc"));
    assert!(
        section["options"].get("public_agent").is_none(),
        "hints stay comments"
    );

    let mut cfg = cfg();
    cfg.options
        .insert("agent_id".to_string(), serde_json::json!("agent_abc"));
    let env = env_with(&[]);
    assert!(ElevenLabsFactory::validate_with(&cfg, &env).is_empty());
}
