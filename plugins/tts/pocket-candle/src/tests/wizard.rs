use crate::wizard_choices;

#[test]
fn wizard_entry_renders_valid_pocket_candle_config() {
    let choices = wizard_choices();
    assert_eq!(choices.len(), 1);
    let c = &choices[0];
    assert_eq!(c.feature.as_deref(), Some("tts-pocket-candle"));
    assert_eq!(c.fields.len(), 2);
}
