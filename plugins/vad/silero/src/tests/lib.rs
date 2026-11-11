use super::*;

#[test]
fn hysteresis_enters_on_high_and_holds_until_low() {
    let (hi, lo) = (0.5_f32, 0.35_f32);
    // Not speaking: a probability in the hold band must NOT start speech.
    assert!(!hysteresis_is_speech(false, 0.40, hi, lo));
    // Not speaking: crossing the high threshold starts speech.
    assert!(hysteresis_is_speech(false, 0.50, hi, lo));
    // Speaking: a probability in the hold band KEEPS speech (no flapping).
    assert!(hysteresis_is_speech(true, 0.40, hi, lo));
    // Speaking: dropping below the low threshold ends speech.
    assert!(!hysteresis_is_speech(true, 0.30, hi, lo));
}

#[test]
fn hysteresis_degenerates_when_neg_ge_threshold() {
    let (hi, lo) = (0.5_f32, 0.5_f32);
    assert!(!hysteresis_is_speech(true, 0.49, hi, lo));
    assert!(hysteresis_is_speech(false, 0.50, hi, lo));
}

#[test]
fn without_a_model_is_reported_once() {
    let errs: Vec<_> = factory()
        .validate(&VadConfig::new("silero"))
        .into_iter()
        .filter(|e| e.field == "vad.model")
        .collect();
    assert_eq!(errs.len(), 1, "{errs:?}");
}

#[test]
fn without_a_model_it_does_not_build() {
    let err = factory()
        .build(&VadConfig::new("silero"), &BuildContext::new())
        .err()
        .expect("no model, no backend");
    assert!(err.to_string().contains("model"), "{err}");
}

/// Auto runs Silero only with its model on disk; the test's own executable
/// stands in for a file that exists (nothing is loaded).
#[test]
fn it_can_run_only_with_its_model_on_disk() {
    let present = std::env::current_exe().expect("the test binary");
    assert!(factory().can_run(&VadConfig::new("silero").with_model(Some(present))));
    let missing = std::path::PathBuf::from("no/such/silero_vad.onnx");
    assert!(!factory().can_run(&VadConfig::new("silero").with_model(Some(missing))));
    assert!(!factory().can_run(&VadConfig::new("silero")));
}

#[test]
fn its_one_wizard_entry_fetches_the_registry_model() {
    let choices = wizard_choices();
    let [choice] = choices.as_slice() else {
        panic!("one entry: {choices:?}");
    };
    assert!(
        matches!(&choice.deploy, voice_pipes_sdk::WizardDeploy::Registry { key } if key == "silero")
    );
}

/// A model that answers every step with a fixed probability and a state of
/// `state_len` values.
struct StubModel {
    prob: f32,
    state_len: usize,
}

impl SileroModel for StubModel {
    fn step(&mut self, _input: Vec<f32>, _state: Vec<f32>) -> Result<(f32, Vec<f32>), VadError> {
        Ok((self.prob, vec![0.0; self.state_len]))
    }
}

fn stub_vad(state_len: usize) -> (SileroVad, Arc<ComponentCore>) {
    let core = Arc::new(ComponentCore::new("vad:silero", ComponentKind::Vad));
    let model = Box::new(StubModel {
        prob: 0.9,
        state_len,
    });
    let vad = SileroVad::with_model(model, Arc::clone(&core), 0.5, 0.35);
    (vad, core)
}

/// A broken export is an error, not silence: the chunk fails and the
/// component counts one error and no success.
#[test]
fn an_inference_error_is_an_error_not_silence() {
    let (mut vad, core) = stub_vad(STATE_SIZE - 1);
    let window = vec![0_i16; SILERO_WINDOW];
    let result = vad.process_chunk(&window, SILERO_SR);
    assert!(result.is_err(), "a failed window reported {result:?}");
    let telemetry = core.telemetry();
    assert_eq!(telemetry.errors_total, 1);
    assert_eq!(telemetry.processed_total, 0);
}

/// A model that answers correctly gives its probability.
#[test]
fn a_working_model_gives_its_probability() {
    let (mut vad, core) = stub_vad(STATE_SIZE);
    let window = vec![0_i16; SILERO_WINDOW];
    let result = vad
        .process_chunk(&window, SILERO_SR)
        .expect("a good window");
    assert!((result.speech_prob - 0.9).abs() < f32::EPSILON);
    assert!(result.is_speech);
    let telemetry = core.telemetry();
    assert_eq!(telemetry.errors_total, 0);
    assert_eq!(telemetry.processed_total, 1);
}

#[test]
fn metric_conformance() {
    let res = voice_pipes_sdk::testing::assert_vad_metrics_conformance(
        std::time::Duration::from_millis(500),
        std::time::Duration::from_millis(1000),
        std::time::Duration::from_millis(200),
    );
    assert!(res.is_ok(), "{res:?}");
}

#[test]
fn pool_size_and_threads_bounds_are_validated() {
    let present = std::env::current_exe().expect("the test binary");
    let mut cfg = VadConfig::new("silero").with_model(Some(present));
    cfg.options.insert("pool_size".to_owned(), 0_u64.into());
    cfg.options.insert("threads".to_owned(), 300_u64.into());
    let errs = factory().validate(&cfg);
    assert!(
        errs.iter().any(|e| e.field == "vad.options.pool_size"),
        "{errs:?}"
    );
    assert!(
        errs.iter().any(|e| e.field == "vad.options.threads"),
        "{errs:?}"
    );
}
