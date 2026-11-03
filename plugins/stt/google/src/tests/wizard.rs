use super::*;

/// The TOML the setup wizard writes for this entry when every prompt is
/// accepted: the golden the engine's wizard held the built-in entry to
/// before the provider moved here, unchanged.
#[test]
fn the_wizard_entry_writes_what_the_built_in_one_did() {
    let choice = super::wizard_choices()
        .into_iter()
        .next()
        .expect("offered in the wizard");
    assert_eq!(
        choice
            .render_defaults("provider", GoogleSttFactory.name())
            .toml,
        "provider = \"google\"\nmodel_name = \"chirp_3\"\nlocation = \"us\"\n\
         language_codes = [\"en-US\"]\n"
    );
}

/// A project and a key path, when given, are written; several languages
/// become an array.
#[test]
fn answered_optional_fields_and_several_languages_are_written() {
    let choice = super::wizard_choices().into_iter().next().unwrap();
    let rendered = choice
        .render("provider", "google", |f| {
            Ok(match f.key.as_str() {
                "project_id" => "my-project".into(),
                "language_codes" => "hr-HR, en-US".into(),
                "credentials_path" => "models/gcp/sa.json".into(),
                _ => f.default.clone(),
            })
        })
        .unwrap();
    assert_eq!(
        rendered.toml,
        "provider = \"google\"\nmodel_name = \"chirp_3\"\nlocation = \"us\"\n\
         project_id = \"my-project\"\nlanguage_codes = [\"hr-HR\", \"en-US\"]\n\
         credentials_path = \"models/gcp/sa.json\"\n"
    );
    assert!(rendered.secrets.is_empty(), "there is no API key to store");
}
