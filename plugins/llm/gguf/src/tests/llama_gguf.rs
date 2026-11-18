use super::*;

#[test]
fn init_backend_succeeds() {
    let backend = get_or_init_backend();
    assert!(backend.is_ok(), "backend initialization should succeed");
}

#[test]
fn missing_model_fails_cleanly() {
    let result = LlamaGgufBackend::load(Path::new("non_existent_model_file.gguf"), "cpu");
    assert!(result.is_err(), "missing model file should fail");
    let err = result.err().expect("expected error");
    assert!(
        err.message.contains("failed to load GGUF model"),
        "error message should describe model load failure: {}",
        err.message
    );
}
