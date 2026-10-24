use super::*;

/// Only the options a detector does not know are refused, each named with
/// its section, in a stable order.
#[test]
fn unknown_options_are_named_and_known_ones_pass() {
    let mut cfg = TurnConfig::new("smart", 1000);
    cfg.options
        .insert("smart_threshold".into(), serde_json::json!(0.5));
    cfg.options
        .insert("smart_treshold".into(), serde_json::json!(0.5));
    cfg.options.insert("a_typo".into(), serde_json::json!(1));
    let errors = cfg.unknown_options(&["smart_threshold"]);
    let fields: Vec<&str> = errors.iter().map(|e| e.field.as_str()).collect();
    assert_eq!(
        fields,
        ["turn_detection.a_typo", "turn_detection.smart_treshold"]
    );
    assert!(
        errors[0].message.contains("\"smart\""),
        "{}",
        errors[0].message
    );
    assert!(
        cfg.unknown_options(&["smart_threshold", "smart_treshold", "a_typo"])
            .is_empty()
    );
}

/// A bounded number reads its default when unset, its value when in range,
/// and says which is wrong otherwise: a fraction where a whole number is
/// wanted is a type error, not an out-of-range one.
#[expect(deprecated, reason = "the deprecated readers keep working until 2.0")]
#[test]
fn bounded_options_tell_a_wrong_type_from_a_wrong_value() {
    let mut cfg = TurnConfig::new("smart", 1000);
    assert_eq!(cfg.whole_option("ms", 0..=100, 7).ok(), Some(7));
    assert_eq!(cfg.number_option("p", 0.0..=1.0, 0.5).ok(), Some(0.5));

    let message = |r: Result<f64, ConfigError>| {
        let e = r.expect_err("refused");
        assert_eq!(e.field, "turn_detection.x");
        e.message
    };
    for (value, whole, number) in [
        (serde_json::json!(50), Ok(50), "must be in [0, 1], got 50"),
        (
            serde_json::json!(8.5),
            Err("must be a whole number in [0, 100], got 8.5"),
            "must be in [0, 1], got 8.5",
        ),
        (
            serde_json::json!(-5),
            Err("must be a whole number in [0, 100], got -5"),
            "must be in [0, 1], got -5",
        ),
        (
            serde_json::json!(500),
            Err("must be in [0, 100], got 500"),
            "must be in [0, 1], got 500",
        ),
        (
            serde_json::json!("high"),
            Err("must be a whole number in [0, 100], got \"high\""),
            "must be a number in [0, 1], got \"high\"",
        ),
    ] {
        cfg.options.insert("x".into(), value.clone());
        match whole {
            Ok(n) => assert_eq!(cfg.whole_option("x", 0..=100, 7).ok(), Some(n)),
            Err(want) => {
                let e = cfg.whole_option("x", 0..=100, 7).expect_err("refused");
                assert_eq!(e.field, "turn_detection.x");
                assert_eq!(e.message, want, "{value}");
            }
        }
        assert_eq!(
            message(cfg.number_option("x", 0.0..=1.0, 0.5)),
            number,
            "{value}"
        );
    }
}

#[test]
fn a_rule_signal_has_no_opinion_and_a_forecast_keeps_its_lead() {
    let s = TurnSignal::incomplete();
    assert!(!s.is_complete());
    assert_eq!(s.confidence, None);
    let s = TurnSignal::likely_complete()
        .with_confidence(0.9)
        .ending_in(Duration::ZERO);
    assert!(s.is_complete());
    assert_eq!(s.confidence, Some(0.9));
    assert_eq!(s.expected_end_in, Some(Duration::ZERO));
    assert!(TurnSignal::from(TurnDecision::ForcedComplete).is_complete());
}
