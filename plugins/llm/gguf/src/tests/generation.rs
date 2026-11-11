use super::build_llm_detail_payload;

#[test]
fn llm_detail_first_token_ms_is_null_when_absent() {
    let payload =
        build_llm_detail_payload(0.2, 145.6, 24, 1.1, 0, None, None, 0.0, "cpu", false, 0.0);
    assert!(
        payload["first_token_ms"].is_null(),
        "missing first_token_ms must serialize as null, got {}",
        payload["first_token_ms"]
    );
}
