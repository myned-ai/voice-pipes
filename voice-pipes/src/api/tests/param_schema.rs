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
fn validate_int_accepts_whole_float_rejects_fraction() {
    let d = ParamDescriptor::mutable_int("n", "N", "", 1, 10);
    assert!(matches!(
        d.validate(serde_json::json!(5.0)).unwrap(),
        ParamValue::Int(5)
    ));
    assert!(matches!(
        d.validate(serde_json::json!(5.5)).unwrap_err(),
        ParamError::TypeMismatch { .. }
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

/// A mutable string is bounded: the default cap applies unless the
/// descriptor sets its own, and an over-long value is a validation error.
#[test]
fn validate_string_length() {
    let d = ParamDescriptor::mutable_str("s", "S", "");
    let ok = "x".repeat(voice_pipes_sdk::schema::DEFAULT_MAX_STR_LEN);
    assert!(d.validate(serde_json::json!(ok)).is_ok());
    let long = "x".repeat(voice_pipes_sdk::schema::DEFAULT_MAX_STR_LEN + 1);
    assert!(matches!(
        d.validate(serde_json::json!(long)).unwrap_err(),
        ParamError::TooLong { .. }
    ));

    let mut tight = ParamDescriptor::mutable_str("s", "S", "");
    tight.constraints.as_mut().unwrap().max_len = Some(3);
    assert!(tight.validate(serde_json::json!("abc")).is_ok());
    assert!(matches!(
        tight.validate(serde_json::json!("abcd")).unwrap_err(),
        ParamError::TooLong { len: 4, max_len: 3 }
    ));
}

#[test]
fn validate_type_mismatch() {
    let d = ParamDescriptor::mutable_bool("b", "B", "");
    assert!(matches!(
        d.validate(serde_json::json!("yes")).unwrap_err(),
        ParamError::TypeMismatch { .. }
    ));
}
