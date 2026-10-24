use super::*;

fn vad_like_store() -> Arc<AtomicParamStore> {
    Arc::new(
        AtomicParamStore::builder()
            .float("energy_threshold", 200.0)
            .int("min_speech_ms", 250)
            .bool("enabled", true)
            .string("voice", "alba")
            .build(),
    )
}

#[tokio::test]
async fn atomic_store_roundtrip_float() {
    let s = vad_like_store();
    let v = s
        .set_param("energy_threshold", ParamValue::Float(350.0))
        .await
        .unwrap();
    assert!(matches!(v, ParamValue::Float(x) if (x - 350.0).abs() < 1e-9));
    assert_eq!(s.float("energy_threshold"), Some(350.0));
}

#[tokio::test]
async fn atomic_store_roundtrip_int_bool_string() {
    let s = vad_like_store();
    s.set_param("min_speech_ms", ParamValue::Int(400))
        .await
        .unwrap();
    assert_eq!(s.int("min_speech_ms"), Some(400));
    s.set_param("enabled", ParamValue::Bool(false))
        .await
        .unwrap();
    assert_eq!(s.bool("enabled"), Some(false));
    s.set_param("voice", ParamValue::Str("af_bella".into()))
        .await
        .unwrap();
    assert_eq!(
        s.string("voice").as_deref().map(String::as_str),
        Some("af_bella")
    );
}

#[tokio::test]
async fn atomic_store_unknown_key() {
    let s = vad_like_store();
    let err = s
        .set_param("nope", ParamValue::Float(1.0))
        .await
        .unwrap_err();
    assert!(matches!(err, ParamError::UnknownKey(_)));
}

#[tokio::test]
async fn registry_applies_through_validation() {
    let store = vad_like_store();
    let schema = vec![
        ParamDescriptor::mutable_float("energy_threshold", "Energy threshold", "", 0.0, 10_000.0),
        ParamDescriptor::mutable_int("min_speech_ms", "Min speech (ms)", "", 0, 5_000),
        ParamDescriptor::static_bool("enabled", "Enabled", ""),
    ];
    let reg = ParamRegistry::new();
    reg.register(
        "vad",
        schema,
        std::sync::Arc::<AtomicParamStore>::clone(&store),
    );

    // ok
    let v = reg
        .apply("vad", "energy_threshold", serde_json::json!(123.0))
        .await
        .unwrap();
    assert!(matches!(v, ParamValue::Float(x) if (x - 123.0).abs() < 1e-9));

    // immutable
    assert!(matches!(
        reg.apply("vad", "enabled", serde_json::json!(false))
            .await
            .unwrap_err(),
        ParamError::Immutable
    ));

    // out of range
    assert!(matches!(
        reg.apply("vad", "energy_threshold", serde_json::json!(99_999.0))
            .await
            .unwrap_err(),
        ParamError::OutOfRange { .. }
    ));

    // unknown component / key
    assert!(matches!(
        reg.apply("missing", "x", serde_json::json!(1))
            .await
            .unwrap_err(),
        ParamError::UnknownComponent(_)
    ));
    assert!(matches!(
        reg.apply("vad", "missing", serde_json::json!(1))
            .await
            .unwrap_err(),
        ParamError::UnknownKey(_)
    ));
}

#[tokio::test]
async fn registry_get_uses_store_readback() {
    let store = vad_like_store();
    let schema = vec![ParamDescriptor::mutable_float(
        "energy_threshold",
        "",
        "",
        0.0,
        10_000.0,
    )];
    let reg = ParamRegistry::new();
    reg.register("vad", schema, store);

    let v = reg.get("vad", "energy_threshold").await.unwrap();
    assert!(matches!(v, ParamValue::Float(x) if (x - 200.0).abs() < 1e-9));
}
