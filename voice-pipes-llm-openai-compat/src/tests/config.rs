use super::*;

fn make_opts(pairs: &[(&str, Value)]) -> HashMap<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}

#[test]
fn defaults() {
    let cfg = OpenAiCompatConfig::from_options(&HashMap::new()).unwrap();
    assert_eq!(cfg.base_url, DEFAULT_BASE_URL);
    assert_eq!(cfg.model, DEFAULT_MODEL);
    assert_eq!(cfg.max_tokens, DEFAULT_MAX_TOKENS);
    assert_eq!(cfg.temperature, Some(DEFAULT_TEMPERATURE));
    assert!(cfg.top_p.is_none());
    assert!(cfg.stop.is_none());
    assert_eq!(cfg.timeout_secs, DEFAULT_TIMEOUT_SECS);
    assert_eq!(cfg.max_retries, DEFAULT_MAX_RETRIES);
    assert_eq!(cfg.history_max_turns, DEFAULT_HISTORY_MAX_TURNS);
}

#[test]
fn history_max_turns_custom_and_clamped() {
    let opts = make_opts(&[("history_max_turns", serde_json::json!(5))]);
    assert_eq!(
        OpenAiCompatConfig::from_options(&opts)
            .unwrap()
            .history_max_turns,
        5
    );

    // 0 would drop the turn being recorded: refused, not clamped.
    let opts = make_opts(&[("history_max_turns", serde_json::json!(0))]);
    assert!(OpenAiCompatConfig::from_options(&opts).is_err());
}

#[test]
fn custom_values() {
    let opts = make_opts(&[
        ("base_url", Value::String("http://localhost:8080/v1".into())),
        ("model", Value::String("local-llama".into())),
        ("max_tokens", serde_json::json!(512)),
        ("temperature", serde_json::json!(0.5)),
        ("top_p", serde_json::json!(0.9)),
        ("presence_penalty", serde_json::json!(0.3)),
        ("frequency_penalty", serde_json::json!(0.2)),
        ("seed", serde_json::json!(123)),
        ("timeout_secs", serde_json::json!(60)),
        ("max_retries", serde_json::json!(5)),
        ("response_format", Value::String("json_object".into())),
    ]);
    let cfg = OpenAiCompatConfig::from_options(&opts).unwrap();
    assert_eq!(cfg.base_url, "http://localhost:8080/v1");
    assert_eq!(cfg.model, "local-llama");
    assert_eq!(cfg.max_tokens, 512);
    assert_eq!(cfg.temperature, Some(0.5));
    assert_eq!(cfg.top_p, Some(0.9));
    assert_eq!(cfg.presence_penalty, Some(0.3));
    assert_eq!(cfg.frequency_penalty, Some(0.2));
    assert_eq!(cfg.seed, Some(123));
    assert_eq!(cfg.timeout_secs, 60);
    assert_eq!(cfg.max_retries, 5);
    assert_eq!(cfg.response_format.as_deref(), Some("json_object"));
}

/// The retry count and read timeout are clamped to values the turn
/// budget can absorb.
#[test]
fn network_knobs_are_clamped() {
    let opts = make_opts(&[
        ("max_retries", serde_json::json!(50)),
        ("timeout_secs", serde_json::json!(0)),
    ]);
    let fields: Vec<String> = OpenAiCompatConfig::from_options(&opts)
        .unwrap_err()
        .into_iter()
        .map(|e| e.field)
        .collect();
    assert_eq!(
        fields,
        ["text_processor.timeout_secs", "text_processor.max_retries"],
        "refused, not clamped"
    );
}

/// A config dump (logs, panics, diagnostics) never carries the key.
#[test]
fn debug_output_redacts_the_api_key() {
    let tp: voice_pipes_sdk::TextProcessorConfig =
        toml::from_str("type = \"openai-compat\"\napi_key = \"sk-very-secret\"").unwrap();
    assert!(!format!("{tp:?}").contains("sk-very-secret"));
    let cfg = OpenAiCompatConfig::from_config(&tp).unwrap();
    let dump = format!("{cfg:?}");
    assert!(!dump.contains("sk-very-secret"), "{dump}");
    assert!(dump.contains("<redacted>"), "{dump}");
    let empty = format!(
        "{:?}",
        OpenAiCompatConfig::from_options(&HashMap::new()).unwrap()
    );
    assert!(
        empty.contains("<empty>") || empty.contains("<redacted>"),
        "{empty}"
    );
}

#[test]
fn stop_as_array() {
    let opts = make_opts(&[("stop", serde_json::json!(["END", "STOP"]))]);
    let cfg = OpenAiCompatConfig::from_options(&opts).unwrap();
    assert_eq!(
        cfg.stop.as_deref(),
        Some(&["END".to_string(), "STOP".to_string()][..])
    );
}

#[test]
fn stop_as_string() {
    let opts = make_opts(&[("stop", Value::String("DONE".into()))]);
    let cfg = OpenAiCompatConfig::from_options(&opts).unwrap();
    assert_eq!(cfg.stop.as_deref(), Some(&["DONE".to_string()][..]));
}

#[test]
fn completions_url_strips_trailing_slash() {
    let opts = make_opts(&[(
        "base_url",
        Value::String("http://localhost:8080/v1/".into()),
    )]);
    let cfg = OpenAiCompatConfig::from_options(&opts).unwrap();
    assert_eq!(
        cfg.completions_url(),
        "http://localhost:8080/v1/chat/completions"
    );
}

/// The key comes from the config the host resolved, never from the
/// options: an `api_key` or `api_key_env` there is a section field, and in
/// the options map an unknown key.
#[test]
fn the_key_comes_from_the_config_not_the_options() {
    let cfg: voice_pipes_sdk::TextProcessorConfig =
        toml::from_str("type = \"openai-compat\"\napi_key = \"explicit-key\"").unwrap();
    assert!(cfg.options.is_empty());
    let resolved = OpenAiCompatConfig::from_config(&cfg).unwrap();
    assert_eq!(resolved.api_key.expose(), "explicit-key");

    let opts = make_opts(&[("api_key", Value::String("in-options".into()))]);
    assert!(OpenAiCompatConfig::from_options(&opts).is_err());
}

#[test]
fn no_key_is_valid_for_a_local_server() {
    let cfg: voice_pipes_sdk::TextProcessorConfig =
        toml::from_str("type = \"openai-compat\"").unwrap();
    let resolved = OpenAiCompatConfig::from_config(&cfg).unwrap();
    assert!(resolved.api_key.is_empty());
}

#[test]
fn validate_temperature_range() {
    let cfg = voice_pipes_sdk::TextProcessorConfig::new("openai-compat")
        .with_options(make_opts(&[("temperature", serde_json::json!(3.0))]));
    let errs = validate(&cfg);
    assert_eq!(errs.len(), 1);
    assert_eq!(errs[0].field, "text_processor.temperature");
}

#[test]
fn validate_top_p_range() {
    let cfg = voice_pipes_sdk::TextProcessorConfig::new("openai-compat")
        .with_options(make_opts(&[("top_p", serde_json::json!(1.5))]));
    let errs = validate(&cfg);
    assert_eq!(errs.len(), 1);
    assert_eq!(errs[0].field, "text_processor.top_p");
}

#[test]
fn validate_valid_config() {
    let cfg =
        voice_pipes_sdk::TextProcessorConfig::new("openai-compat").with_options(make_opts(&[
            ("temperature", serde_json::json!(0.8)),
            ("top_p", serde_json::json!(0.95)),
        ]));
    let errs = validate(&cfg);
    assert!(errs.is_empty());
}

/// A misspelt setting is refused against its own key, while every key the
/// config reads, and the one the engine adds for audits, passes.
#[test]
fn validate_refuses_an_unknown_option() {
    let cfg =
        voice_pipes_sdk::TextProcessorConfig::new("openai-compat").with_options(make_opts(&[
            ("temprature", serde_json::json!(0.5)),
            (
                voice_pipes_sdk::text::AUDIT_FULL_PROMPTS_OPTION,
                serde_json::json!(true),
            ),
        ]));
    let fields: Vec<String> = validate(&cfg).into_iter().map(|e| e.field).collect();
    assert_eq!(fields, ["text_processor.temprature"]);
}

/// A thinking model behind llama-server answers only when the template is
/// told not to think; the fields that say so are the server's, not the
/// common API's, so they pass through as given.
#[test]
fn extra_body_passes_a_table_through_and_refuses_owned_keys() {
    let opts = make_opts(&[(
        "extra_body",
        serde_json::json!({"chat_template_kwargs": {"enable_thinking": false}, "top_k": 40}),
    )]);
    let cfg = OpenAiCompatConfig::from_options(&opts).unwrap();
    let extra = cfg.extra_body.expect("table kept");
    assert_eq!(extra["chat_template_kwargs"]["enable_thinking"], false);
    assert_eq!(extra["top_k"], 40);

    let opts = make_opts(&[("extra_body", serde_json::json!("enable_thinking=false"))]);
    assert!(
        OpenAiCompatConfig::from_options(&opts).is_err(),
        "a string is not a table"
    );

    let opts = make_opts(&[("extra_body", serde_json::json!({"model": "other"}))]);
    assert!(
        OpenAiCompatConfig::from_options(&opts).is_err(),
        "the model is set by its own option"
    );
    assert!(
        OpenAiCompatConfig::from_options(&HashMap::new())
            .unwrap()
            .extra_body
            .is_none()
    );
}
