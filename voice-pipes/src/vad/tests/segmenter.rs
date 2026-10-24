use super::*;

#[test]
fn energy_vad_silence() {
    let mut vad = EnergyVad::default();
    let silence = vec![0i16; 320];
    let result = vad.process_chunk(&silence, 16000).unwrap();
    assert!(!result.is_speech);
    assert!(result.speech_prob < 0.01);
}

#[test]
fn energy_vad_speech() {
    let mut vad = EnergyVad::default();
    let loud = vec![5000i16; 320];
    let result = vad.process_chunk(&loud, 16000).unwrap();
    assert!(result.is_speech);
    assert!((result.speech_prob - 1.0).abs() < f32::EPSILON);
}

#[test]
fn energy_vad_custom_threshold() {
    let mut vad = EnergyVad::new(100.0);
    let samples = vec![150i16; 320];
    let result = vad.process_chunk(&samples, 16000).unwrap();
    assert!(result.is_speech);
}

#[test]
fn energy_vad_empty_chunk() {
    let mut vad = EnergyVad::default();
    let result = vad.process_chunk(&[], 16000).unwrap();
    assert!(!result.is_speech);
}

#[test]
fn vad_factory_names_are_unique() {
    let factories = vad_factories();
    let names: Vec<&str> = factories.iter().map(|f| f.name()).collect();
    let unique: std::collections::HashSet<&str> = names.iter().copied().collect();
    assert_eq!(
        names.len(),
        unique.len(),
        "duplicate VAD factory names: {names:?}"
    );
}

#[test]
fn vad_factory_includes_energy_and_passthrough() {
    let factories = vad_factories();
    let names: Vec<&str> = factories.iter().map(|f| f.name()).collect();
    assert!(names.contains(&"energy"), "missing energy factory");
    assert!(
        names.contains(&"passthrough"),
        "missing passthrough factory"
    );
}

// ── Component integration ───────────────────────────────

#[test]
fn energy_vad_exposes_component() {
    let vad = EnergyVad::default();
    let c = vad
        .as_component()
        .expect("energy backend exposes Component");
    assert_eq!(c.id(), "vad:energy");
    assert_eq!(c.kind(), ComponentKind::Vad);
}

#[test]
fn energy_vad_records_telemetry() {
    let mut vad = EnergyVad::default();
    let c = vad.as_component().unwrap();
    // Counter starts at zero.
    assert_eq!(c.telemetry().processed_total, 0);
    vad.process_chunk(&vec![100i16; 320], 16000).unwrap();
    vad.process_chunk(&vec![100i16; 320], 16000).unwrap();
    assert_eq!(c.telemetry().processed_total, 2);
}

#[test]
fn passthrough_vad_exposes_component() {
    let vad = PassthroughVad::new();
    let c = vad.as_component().unwrap();
    assert_eq!(c.id(), "vad:passthrough");
}

// ── Plugin backends ─────────────────────────────────────

/// A plugin backend as a crate on the SDK would write it: speech when any
/// sample exceeds the level its `[vad.options]` gives it.
struct LevelVad {
    level: i16,
}

impl voice_pipes_sdk::VadBackend for LevelVad {
    fn process_chunk(&mut self, samples: &[i16], _sample_rate: u32) -> Result<VadResult, VadError> {
        let loud = samples
            .iter()
            .any(|s| s.unsigned_abs() > self.level.unsigned_abs());
        Ok(VadResult {
            speech_prob: if loud { 1.0 } else { 0.0 },
            is_speech: loud,
        })
    }
    fn reset(&mut self) {}
    fn name(&self) -> &'static str {
        "test-plugin-level"
    }
}

struct LevelVadFactory;

impl voice_pipes_sdk::VadFactory for LevelVadFactory {
    fn name(&self) -> &'static str {
        "test-plugin-level"
    }
    fn validate(&self, cfg: &voice_pipes_sdk::VadConfig) -> Vec<crate::config::ConfigError> {
        if cfg
            .options
            .get("level")
            .and_then(serde_json::Value::as_i64)
            .is_none()
        {
            return vec![crate::config::ConfigError::new(
                "vad.options.level",
                "an integer level is required",
            )];
        }
        vec![]
    }
    fn build(
        &self,
        cfg: &voice_pipes_sdk::VadConfig,
        _ctx: &voice_pipes_sdk::BuildContext<'_>,
    ) -> Result<Box<dyn voice_pipes_sdk::VadBackend>, voice_pipes_sdk::BuildError> {
        let level = cfg.options["level"].as_i64().unwrap_or(1000) as i16;
        Ok(Box::new(LevelVad { level }))
    }
}

fn register_level_vad_once() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        register_vad_factory(|| Box::new(LevelVadFactory)).expect("test name is free");
    });
}

#[test]
fn a_plugin_backend_is_selected_by_mode_and_gets_its_options() {
    register_level_vad_once();
    let cfg: crate::config::PipelineConfig = toml::from_str(
        r#"
[vad]
mode = "test-plugin-level"
[vad.options]
level = 3000
"#,
    )
    .expect("config parses");
    let problems: Vec<_> = crate::config::validate(&cfg)
        .into_iter()
        .filter(|e| e.field.starts_with("vad"))
        .collect();
    assert!(problems.is_empty(), "{problems:?}");

    let mut vad = crate::config::build_vad(&cfg, None).expect("plugin backend builds");
    assert_eq!(vad.name(), "test-plugin-level");
    assert!(!vad.process_chunk(&[2000; 320], 16_000).unwrap().is_speech);
    assert!(vad.process_chunk(&[4000; 320], 16_000).unwrap().is_speech);
}

#[test]
fn config_validation_reports_a_plugin_backends_own_faults() {
    register_level_vad_once();
    let cfg: crate::config::PipelineConfig =
        toml::from_str("[vad]\nmode = \"test-plugin-level\"\n").expect("config parses");
    let errs: Vec<_> = crate::config::validate(&cfg)
        .into_iter()
        .filter(|e| e.field.starts_with("vad"))
        .collect();
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert_eq!(errs[0].field, "vad.options.level");
}
