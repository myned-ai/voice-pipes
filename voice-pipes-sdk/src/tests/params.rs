use super::*;

#[tokio::test]
async fn a_set_value_is_what_a_held_cell_reads_next() {
    let store = AtomicParamStore::builder()
        .string("voice", "alba")
        .float("temperature", 0.7)
        .int("max_tokens", 256)
        .bool("stream", true)
        .build();
    let voice = store.string_cell("voice").unwrap();
    let temp = store.float_cell("temperature").unwrap();

    store
        .set_param("voice", ParamValue::Str("marius".into()))
        .await
        .unwrap();
    store
        .set_param("temperature", ParamValue::Float(1.1))
        .await
        .unwrap();

    assert_eq!(**voice.load(), "marius");
    assert!((f64::from_bits(temp.load(Ordering::Relaxed)) - 1.1).abs() < f64::EPSILON);
    assert_eq!(store.int("max_tokens"), Some(256));
    assert!(matches!(
        store.get_param("stream").await,
        Some(ParamValue::Bool(true))
    ));
}

#[tokio::test]
async fn an_undeclared_key_is_refused() {
    let store = AtomicParamStore::builder().string("voice", "alba").build();
    assert!(matches!(
        store.set_param("model", ParamValue::Str("x".into())).await,
        Err(ParamError::UnknownKey(_))
    ));
}

#[test]
fn a_schema_describes_its_store() {
    use crate::schema::{ParamDescriptor, ProviderSchema};
    let mut schema = ProviderSchema::default();
    schema
        .push(
            ParamDescriptor::mutable_str("system_prompt", "p", "d"),
            "be brief",
        )
        .push(
            ParamDescriptor::mutable_int("max_tokens", "m", "d", 1, 10),
            7,
        )
        .push(
            ParamDescriptor::mutable_float("top_p", "t", "d", 0.0, 1.0),
            serde_json::Value::Null,
        )
        .push(
            ParamDescriptor::mutable_str("stop", "s", "d"),
            serde_json::json!(["a", "b"]),
        )
        .push(ParamDescriptor::static_str("model", "m", "d"), "gpt");
    let store = AtomicParamStore::from_schema(&schema);
    assert_eq!(
        store.string("system_prompt").as_deref().map(String::as_str),
        Some("be brief")
    );
    assert_eq!(store.int("max_tokens"), Some(7));
    assert!(store.float("top_p").is_some_and(f64::is_nan));
    assert_eq!(
        store.string("stop").as_deref().map(String::as_str),
        Some("a\nb")
    );
    // Static fields are not live.
    assert!(store.string("model").is_none());
}
