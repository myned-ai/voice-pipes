use super::*;

fn choice(fields: Vec<WizardField>) -> WizardChoice {
    WizardChoice {
        label: "test".into(),
        feature: None,
        deploy: WizardDeploy::None,
        fields,
        notes: vec![],
        stage_default: false,
        menu_rank: 0,
    }
}

#[test]
fn defaults_render_every_kind_as_the_wizard_writes_it() {
    let c = choice(vec![
        WizardField::fixed("model_name", "chirp_3"),
        WizardField::text("location", "Location", "us"),
        WizardField::text("project_id", "Project", "").optional(),
        WizardField::list("language_codes", "Languages", "en-US, hr-HR"),
        WizardField::path("model_dir", "Model directory", "models/m"),
        WizardField::secret("MY_API_KEY", "API key"),
    ]);
    let r = c.render_defaults("provider", "p");
    assert_eq!(
        r.toml,
        "provider = \"p\"\nmodel_name = \"chirp_3\"\nlocation = \"us\"\n\
         language_codes = [\"en-US\", \"hr-HR\"]\nmodel_dir = \"models/m\"\n"
    );
    assert!(r.secrets.is_empty(), "a blank secret stores nothing");
    assert_eq!(r.model_path.as_deref(), Some("models/m"));
}

#[test]
fn answers_are_trimmed_and_a_secret_goes_to_env_not_toml() {
    let c = choice(vec![
        WizardField::text("project_id", "Project", "").optional(),
        WizardField::secret("MY_API_KEY", "API key"),
    ]);
    let r = c
        .render("type", "p", |f| {
            Ok(match f.kind {
                WizardFieldKind::Secret => "  sk-123  ".into(),
                _ => "  my-project ".into(),
            })
        })
        .unwrap();
    assert_eq!(r.toml, "type = \"p\"\nproject_id = \"my-project\"\n");
    assert_eq!(r.secrets, vec![("MY_API_KEY".into(), "sk-123".into())]);
}

#[test]
fn a_fixed_field_is_never_asked() {
    let c = choice(vec![WizardField::fixed("model_name", "chirp_3")]);
    let r = c
        .render("provider", "p", |f| panic!("asked for {}", f.key))
        .unwrap();
    assert_eq!(r.toml, "provider = \"p\"\nmodel_name = \"chirp_3\"\n");
}

#[test]
fn a_blank_answer_to_an_ordinary_field_is_still_written() {
    let c = choice(vec![WizardField::text("location", "Location", "")]);
    assert_eq!(
        c.render_defaults("provider", "p").toml,
        "provider = \"p\"\nlocation = \"\"\n"
    );
}

#[test]
fn toml_strings_escape_what_toml_does_not_allow_raw() {
    assert_eq!(toml_string(r#"a"b\c"#), r#""a\"b\\c""#);
    assert_eq!(toml_string("tab\tnl\n"), r#""tab\tnl\n""#);
    assert_eq!(toml_string("del\u{7f}"), r#""del\u007F""#);
    assert_eq!(toml_string("unicodé/путь"), "\"unicodé/путь\"");
}

/// The keys a user typed never reach `Debug` output; their names do.
#[test]
fn a_rendered_choices_debug_hides_the_typed_keys() {
    let rendered = RenderedChoice {
        toml: "provider = \"x\"\n".into(),
        secrets: vec![("X_API_KEY".into(), "sk-typed".into())],
        model_path: None,
    };
    let shown = format!("{rendered:?}");
    assert!(!shown.contains("sk-typed"), "{shown}");
    assert!(shown.contains("X_API_KEY"), "{shown}");
}

/// A literal field is written unquoted and never asked, so a number or a
/// boolean reaches the TOML as that type.
#[test]
fn a_literal_field_is_written_unquoted_without_asking() {
    let c = choice(vec![
        WizardField::fixed("model", "gemma"),
        WizardField::fixed_literal("max_tokens", "1024"),
        WizardField::fixed_literal("temperature", "1.0"),
    ]);
    let r = c
        .render("type", "openai-compat", |f| panic!("asked for {}", f.key))
        .unwrap();
    assert_eq!(
        r.toml,
        "type = \"openai-compat\"\nmodel = \"gemma\"\nmax_tokens = 1024\ntemperature = 1.0\n"
    );
}

/// A comment is written as `#` lines where it stands among the settings,
/// one per line of its text, and never asked.
#[test]
fn a_comment_is_written_in_place_without_asking() {
    let c = choice(vec![
        WizardField::comment("Zero disables thinking;\n-1 is dynamic."),
        WizardField::fixed_literal("options.thinking_budget", "0"),
    ]);
    let r = c
        .render("provider", "gemini-live", |f| panic!("asked for {}", f.key))
        .unwrap();
    assert_eq!(
        r.toml,
        "provider = \"gemini-live\"\n# Zero disables thinking;\n# -1 is dynamic.\n\
         options.thinking_budget = 0\n"
    );
}

/// A required field refuses a blank answer, naming what was asked for; any
/// other answer is written as usual.
#[test]
fn a_required_field_refuses_a_blank_answer() {
    let c = choice(vec![
        WizardField::text("options.agent_id", "Agent id", "").required(),
    ]);
    let err = c
        .render("provider", "agents", |_| Ok("  ".into()))
        .unwrap_err();
    assert!(err.to_string().contains("Agent id is required"), "{err}");
    let r = c
        .render("provider", "agents", |_| Ok("agent_1".into()))
        .unwrap();
    assert!(
        r.toml.ends_with("options.agent_id = \"agent_1\"\n"),
        "{}",
        r.toml
    );
}
