//! The settings VAP reads from `[turn_detection]`, their defaults and bounds.

use std::path::Path;

use voice_pipes_sdk::ConfigError;
use voice_pipes_sdk::turn::TurnConfig;

/// Above this much confidence that the user has yielded, the turn commits.
const DEFAULT_THRESHOLD: f32 = 0.5;

/// Shortest gap between two inferences.
///
/// VAP is cheap enough to run during speech, which is the point of it, but
/// `evaluate` is called on every interim transcript and every tick of the
/// silence timer -- rates that have nothing to do with how fast the answer
/// can usefully change.
const DEFAULT_MIN_INTERVAL_MS: u64 = 200;

/// Accepted `vap_min_interval_ms`: past a few seconds between runs the
/// prediction is stale before it is used.
pub(crate) const MIN_INTERVAL_MS: std::ops::RangeInclusive<u64> = 0..=10_000;

/// The settings VAP reads from `[turn_detection]`.
pub(crate) const VAP_KEYS: &[&str] = &["vap_model_path", "vap_threshold", "vap_min_interval_ms"];

/// VAP's settings, parsed from the section's options.
#[derive(Debug)]
pub(crate) struct VapSettings {
    pub(crate) model_path: String,
    pub(crate) threshold: f32,
    pub(crate) min_interval_ms: u64,
}

impl VapSettings {
    /// Every problem with the settings, or the settings.
    pub(crate) fn parse(cfg: &TurnConfig) -> Result<Self, Vec<ConfigError>> {
        let mut errors = cfg.unknown_options(VAP_KEYS);
        let err =
            |key: &str, message: String| ConfigError::new(format!("turn_detection.{key}"), message);
        let model_path = match cfg.options.get("vap_model_path") {
            None => {
                errors.push(err(
                    "vap_model_path",
                    "required when turn_detection.mode = \"vap\" (export one \
                     with scripts/export-vap-onnx.py)"
                        .into(),
                ));
                String::new()
            }
            Some(v) => match v.as_str() {
                Some(path) if Path::new(path).is_file() => path.to_string(),
                Some(path) => {
                    errors.push(err(
                        "vap_model_path",
                        format!("model file not found: {path}"),
                    ));
                    String::new()
                }
                None => {
                    errors.push(err("vap_model_path", "must be a path".into()));
                    String::new()
                }
            },
        };
        let mut o = cfg.options();
        let threshold = o.f64_in("vap_threshold", 0.0..=1.0, f64::from(DEFAULT_THRESHOLD));
        #[expect(
            clippy::cast_possible_truncation,
            reason = "a probability in [0, 1]; f32 is the model's precision"
        )]
        let threshold = threshold as f32;
        let min_interval_ms = o.u64_in(
            "vap_min_interval_ms",
            MIN_INTERVAL_MS,
            DEFAULT_MIN_INTERVAL_MS,
        );
        errors.extend(o.into_value_errors());
        if errors.is_empty() {
            Ok(Self {
                model_path,
                threshold,
                min_interval_ms,
            })
        } else {
            Err(errors)
        }
    }
}
