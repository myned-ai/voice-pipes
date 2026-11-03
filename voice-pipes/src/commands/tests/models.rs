use super::{default_set, specs_for};
use crate::commands::config_wizard::registry::{EMBEDDED_REGISTRY, Registry};

fn embedded() -> Registry {
    serde_json::from_str(EMBEDDED_REGISTRY).expect("the embedded registry parses")
}

/// `models fetch` with no names downloads exactly what `examples/minimal.toml`
/// reads: every model path in the example is a default location of the set.
#[test]
fn the_default_set_is_what_the_minimal_example_reads() {
    let registry = embedded();
    let defaults: Vec<String> = default_set(&registry)
        .into_iter()
        .map(|key| registry[key].default_path.clone())
        .collect();
    let example = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/minimal.toml"),
    )
    .expect("examples/minimal.toml");
    let table: toml::Table = example.parse().expect("valid TOML");
    let mut paths = vec![
        table["vad"]["model"].as_str(),
        table["stt"]["model_dir"].as_str(),
        table["tts"]["model"].as_str(),
        table["text_processor"][0]["model"].as_str(),
    ];
    paths.sort_unstable();
    let mut expected: Vec<Option<&str>> = defaults.iter().map(|p| Some(p.as_str())).collect();
    expected.sort_unstable();
    assert_eq!(paths, expected);
}

/// An unknown name is refused with the names that exist.
#[test]
fn an_unknown_model_lists_the_known_ones() {
    let registry = embedded();
    let err = specs_for(&["no-such-model"], &registry)
        .expect_err("unknown")
        .to_string();
    assert!(err.contains("no-such-model"), "{err}");
    assert!(
        err.contains("parakeet") && err.contains("smollm2-360m"),
        "{err}"
    );
}
