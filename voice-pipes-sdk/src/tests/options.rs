use super::*;

fn options(pairs: &[(&str, serde_json::Value)]) -> HashMap<String, serde_json::Value> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.clone()))
        .collect()
}

/// Values in range and of the right type come back; missing ones take
/// their defaults; nothing is reported.
#[test]
fn good_values_are_read_and_missing_ones_default() {
    let o = options(&[
        ("speed", serde_json::json!(1.5)),
        ("timeout_ms", serde_json::json!(5_000)),
        ("voice", serde_json::json!("alloy")),
        ("stream", serde_json::json!(false)),
        ("mode", serde_json::json!("fast")),
    ]);
    let mut r = ProviderOptions::new("tts", &o);
    assert!((r.f64_in("speed", 0.25..=4.0, 1.0) - 1.5).abs() < f64::EPSILON);
    assert_eq!(r.u64_in("timeout_ms", 1..=60_000, 30_000), 5_000);
    assert_eq!(r.string("voice").as_deref(), Some("alloy"));
    assert!(!r.bool("stream", true));
    assert_eq!(r.one_of("mode", &["fast", "slow"], "slow"), "fast");
    assert_eq!(r.u32_in("absent", 1..=10, 7), 7);
    assert_eq!(r.optional_f64_in("also_absent", 0.0..=1.0), None);
    assert!(r.finish().is_ok());
}

/// Every problem is reported, with the field named and the default used
/// meanwhile: out of range, wrong type, not an allowed word, and a key
/// nothing read (as an unknown key).
#[test]
fn every_problem_is_reported_and_the_default_used() {
    let o = options(&[
        ("speed", serde_json::json!(9.0)),
        ("timeout_ms", serde_json::json!(0)),
        ("voice", serde_json::json!(3)),
        ("mode", serde_json::json!("warp")),
        ("stream", serde_json::json!("yes")),
        ("spede", serde_json::json!(1.0)),
    ]);
    let mut r = ProviderOptions::new("tts", &o);
    assert!((r.f64_in("speed", 0.25..=4.0, 1.0) - 1.0).abs() < f64::EPSILON);
    assert_eq!(r.u64_in("timeout_ms", 1..=60_000, 30_000), 30_000);
    assert_eq!(r.string("voice"), None);
    assert_eq!(r.one_of("mode", &["fast", "slow"], "slow"), "slow");
    assert!(r.bool("stream", true));
    let errors = r.finish().unwrap_err();
    let fields: Vec<(&str, ConfigErrorKind)> =
        errors.iter().map(|e| (e.field.as_str(), e.kind)).collect();
    assert_eq!(
        fields,
        [
            ("tts.speed", ConfigErrorKind::Invalid),
            ("tts.timeout_ms", ConfigErrorKind::Invalid),
            ("tts.voice", ConfigErrorKind::Invalid),
            ("tts.mode", ConfigErrorKind::Invalid),
            ("tts.stream", ConfigErrorKind::Invalid),
            ("tts.spede", ConfigErrorKind::UnknownKey),
        ]
    );
}

/// Keys the engine reads are not the component's to refuse; a key read
/// through `value` or checked by the caller counts as read.
#[test]
fn engine_keys_and_raw_values_are_not_unknown() {
    let o = options(&[
        (ALLOW_INSECURE_OPTION, serde_json::json!(true)),
        ("voices", serde_json::json!({ "a": 1 })),
        ("model", serde_json::json!("x")),
    ]);
    let mut r = ProviderOptions::new("stt", &o).with_engine_keys(&[ALLOW_INSECURE_OPTION]);
    assert!(r.value("voices").is_some_and(serde_json::Value::is_object));
    if r.string("model").as_deref() == Some("x") {
        r.error("model", "not a model this build has");
    }
    let errors = r.into_errors();
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].field, "stt.model");
}

/// A setting's `f64` keeps its value at `f32` where it fits, and becomes
/// an infinity (or stays NaN) rather than panicking where it does not.
#[test]
fn as_f32_keeps_the_value_and_saturates() {
    assert_eq!(as_f32(1.25), 1.25_f32);
    assert_eq!(as_f32(-0.5), -0.5_f32);
    assert!((as_f32(0.1) - 0.1_f32).abs() < f32::EPSILON);
    assert_eq!(as_f32(1e300), f32::INFINITY);
    assert_eq!(as_f32(-1e300), f32::NEG_INFINITY);
    assert!(as_f32(f64::NAN).is_nan());
}

/// A `u32` read refuses a number past `u32` instead of truncating it.
#[test]
fn a_u32_read_does_not_truncate() {
    let o = options(&[("rate", serde_json::json!((1_u64 << 32) + 16_000))]);
    let mut r = ProviderOptions::new("tts", &o);
    assert_eq!(r.u32_in("rate", 8_000..=48_000, 24_000), 24_000);
    assert_eq!(r.finish().unwrap_err()[0].field, "tts.rate");
}
