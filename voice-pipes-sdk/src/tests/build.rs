use super::*;

#[test]
fn a_refused_config_keeps_each_problem() {
    let e = BuildError::from(vec![
        ConfigError::new("stt.a", "must be set"),
        ConfigError::new("stt.b", "out of range"),
    ]);
    assert_eq!(e.config_errors().len(), 2);
    let shown = e.to_string();
    assert!(
        shown.contains("stt.a") && shown.contains("stt.b"),
        "{shown}"
    );
}

#[test]
fn another_failure_keeps_its_cause() {
    let e = BuildError::from(anyhow::anyhow!("model.onnx: no such file"));
    assert!(e.config_errors().is_empty());
    assert!(e.to_string().contains("model.onnx"), "{e}");
    let e = BuildError::failed("pool is empty");
    assert_eq!(e.to_string(), "pool is empty");
}
