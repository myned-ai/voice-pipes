use super::*;

#[test]
fn validate_immutable_rejects() {
    let d = ParamDescriptor::static_str("x", "X", "");
    assert!(matches!(
        d.validate(serde_json::json!("hello")).unwrap_err(),
        ParamError::Immutable
    ));
}

#[test]
fn validate_float_range() {
    let d = ParamDescriptor::mutable_float("t", "T", "", 0.0, 1.0);
    assert!(d.validate(serde_json::json!(0.5)).is_ok());
    assert!(matches!(
        d.validate(serde_json::json!(2.0)).unwrap_err(),
        ParamError::OutOfRange { .. }
    ));
}

#[test]
fn validate_enum_options() {
    let mut d = ParamDescriptor::static_enum("m", "M", "", &["a", "b"]);
    d.mutable = true;
    assert!(d.validate(serde_json::json!("a")).is_ok());
    assert!(matches!(
        d.validate(serde_json::json!("c")).unwrap_err(),
        ParamError::NotInOptions(_)
    ));
}

#[test]
fn from_json_coerces_whole_floats_to_int() {
    assert!(matches!(
        ParamValue::from_json(serde_json::json!(3.0), ParamValueType::Int).unwrap(),
        ParamValue::Int(3)
    ));
    assert!(ParamValue::from_json(serde_json::json!(3.5), ParamValueType::Int).is_err());
    assert!(matches!(
        ParamValue::from_json(serde_json::json!("a"), ParamValueType::Enum).unwrap(),
        ParamValue::Str(_)
    ));
    assert!(ParamValue::from_json(serde_json::json!(true), ParamValueType::Str).is_err());
}

#[test]
fn value_views_and_round_trip() {
    assert!((ParamValue::Int(4).as_f64().unwrap() - 4.0).abs() < f64::EPSILON);
    assert_eq!(ParamValue::Float(4.0).as_i64().unwrap(), 4);
    assert!(ParamValue::Float(4.5).as_i64().is_err());
    assert!(ParamValue::Bool(true).as_bool().unwrap());
    assert_eq!(ParamValue::Str("s".into()).as_str().unwrap(), "s");
    assert_eq!(ParamValue::Int(4).to_json(), serde_json::json!(4));
    assert!(ParamValue::Str("s".into()).as_f64().is_err());
}

#[test]
fn passthrough_skips_secrets_and_listed_keys() {
    let mut s = ProviderSchema::default();
    let mut opts = std::collections::HashMap::new();
    opts.insert("api_key".to_string(), serde_json::json!("SECRET"));
    opts.insert("x-api-key".to_string(), serde_json::json!("SECRET2"));
    opts.insert("access_token".to_string(), serde_json::json!("SECRET3"));
    opts.insert("voice".to_string(), serde_json::json!("alba"));
    opts.insert(
        "endpoint".to_string(),
        serde_json::json!("https://u:p@host/v1?key=k"),
    );
    opts.insert("skipme".to_string(), serde_json::json!(1));
    s.passthrough_options(&opts, &["skipme"], "opt");
    assert!(s.current_values.contains_key("voice"));
    for secret in ["api_key", "x-api-key", "access_token"] {
        assert!(
            !s.current_values.contains_key(secret),
            "{secret} must never enter current_values"
        );
    }
    assert_eq!(
        s.current_values["endpoint"],
        serde_json::json!("https://***@host/v1?key=***")
    );
    assert!(!s.current_values.contains_key("skipme"));
}

#[test]
fn push_and_push_null_track_values() {
    let mut s = ProviderSchema::fallback("piper", "test");
    s.push(ParamDescriptor::static_str("voice", "Voice", ""), "alba");
    s.push_null(ParamDescriptor::static_str("style", "Style", ""));
    assert_eq!(s.current_values["voice"], serde_json::json!("alba"));
    assert!(s.current_values["style"].is_null());
    assert_eq!(s.descriptors.len(), 3, "fallback + 2 pushed");
}
