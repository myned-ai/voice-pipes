use super::*;
use std::time::Instant;

const RATE: u32 = 16_000;

fn observer() -> EndpointObserver {
    EndpointObserver::new(Box::new(EnergyVad::new(200.0)), 300, 150)
}

fn loud(ms: u64) -> Vec<i16> {
    vec![3000; (RATE as u64 * ms / 1000) as usize]
}

fn quiet(ms: u64) -> Vec<i16> {
    vec![0; (RATE as u64 * ms / 1000) as usize]
}

/// Feed `frames` of 20 ms; return the first stamp produced (and its index).
fn feed(obs: &mut EndpointObserver, pattern: &[(bool, u64)]) -> Vec<(usize, SpeechEndStamp)> {
    let mut out = Vec::new();
    let mut idx = 0;
    for &(speech, ms) in pattern {
        let mut left = ms;
        while left > 0 {
            let step = left.min(20);
            let chunk = if speech { loud(step) } else { quiet(step) };
            if let Some(s) = obs.observe(&chunk, RATE) {
                out.push((idx, s));
            }
            left -= step;
            idx += 1;
        }
    }
    out
}

#[test]
fn declares_end_after_hangover_and_stamps_last_speech_frame() {
    let mut obs = observer();
    let before = Instant::now();
    let stamps = feed(&mut obs, &[(false, 100), (true, 400), (false, 1000)]);
    assert_eq!(stamps.len(), 1, "exactly one end per segment: {stamps:?}");
    // Declared once 300 ms of silence accumulated: frames 0-4 silence,
    // 5-24 speech, silence from 25; hangover met at frame 25 + 15 - 1.
    assert_eq!(stamps[0].0, 39);
    let stamp = stamps[0].1;
    assert!(stamp.instant >= before && stamp.instant <= Instant::now());
    assert!(stamp.epoch_ms > 0);
}

#[test]
fn speech_resuming_inside_hangover_cancels_the_end() {
    let mut obs = observer();
    // 200 ms silence gaps never reach the 300 ms hangover.
    let stamps = feed(
        &mut obs,
        &[
            (true, 300),
            (false, 200),
            (true, 300),
            (false, 200),
            (true, 300),
            (false, 1000),
        ],
    );
    assert_eq!(stamps.len(), 1, "{stamps:?}");
    // Single end, after the last speech burst: 15+10+15+10+15 = 65 frames
    // of content, then 15 silence frames → index 79.
    assert_eq!(stamps[0].0, 79);
}

#[test]
fn short_bursts_below_min_speech_are_ignored() {
    let mut obs = observer();
    let stamps = feed(&mut obs, &[(true, 100), (false, 1000)]);
    assert!(stamps.is_empty(), "{stamps:?}");
    // But the observer is back to Idle and a real segment still works.
    let stamps = feed(&mut obs, &[(true, 400), (false, 1000)]);
    assert_eq!(stamps.len(), 1);
}

#[test]
fn stamp_carries_input_audio_of_its_segment_only() {
    let mut obs = observer();
    // A rejected click, then a real 400 ms segment.
    let stamps = feed(
        &mut obs,
        &[(true, 100), (false, 1000), (true, 400), (false, 1000)],
    );
    assert_eq!(stamps.len(), 1, "{stamps:?}");
    let stats = stamps[0]
        .1
        .input_audio
        .expect("observer meters the segment");
    assert_eq!(stats.speech_ms(), 400, "click discarded, not pooled");
    assert_eq!(stats.source, "vp_energy");
    // 3000 / 32768 full scale.
    assert!(
        (stats.speech_dbfs() + 20.77).abs() < 0.01,
        "{}",
        stats.speech_dbfs()
    );
    assert!(stats.noise_dbfs().is_some(), "silence before and after");
}

#[test]
fn reset_forgets_segment_in_progress() {
    let mut obs = observer();
    feed(&mut obs, &[(true, 400)]);
    obs.reset();
    let stamps = feed(&mut obs, &[(false, 1000)]);
    assert!(stamps.is_empty());
}

#[test]
fn empty_or_zero_rate_chunks_are_noops() {
    let mut obs = observer();
    assert!(obs.observe(&[], RATE).is_none());
    assert!(obs.observe(&loud(20), 0).is_none());
    assert_eq!(obs.state, State::Idle);
}

#[test]
fn from_config_respects_enabled_and_backend_fallback() {
    assert!(EndpointObserver::from_config(None).is_some());
    let off = EndpointObserverConfig {
        enabled: false,
        ..EndpointObserverConfig::default()
    };
    assert!(EndpointObserver::from_config(Some(&off)).is_none());
    let bogus = EndpointObserverConfig {
        backend: "nope".into(),
        ..EndpointObserverConfig::default()
    };
    let obs = EndpointObserver::from_config(Some(&bogus)).expect("enabled");
    assert_eq!(obs.backend_name(), "energy");
}

/// A VAD backend a plugin registers serves the observer like a built-in
/// one, and validation accepts its name: the observer asks the VAD
/// factories, not a list of built-in names.
#[test]
fn a_plugin_vad_backend_serves_the_observer() {
    struct AlwaysSpeech;
    impl voice_pipes_sdk::VadBackend for AlwaysSpeech {
        fn process_chunk(
            &mut self,
            _samples: &[i16],
            _sample_rate: u32,
        ) -> Result<voice_pipes_sdk::VadResult, voice_pipes_sdk::VadError> {
            Ok(voice_pipes_sdk::VadResult {
                speech_prob: 1.0,
                is_speech: true,
            })
        }
        fn reset(&mut self) {}
        fn name(&self) -> &'static str {
            "observer-stub-vad"
        }
    }
    struct AlwaysSpeechFactory;
    impl voice_pipes_sdk::VadFactory for AlwaysSpeechFactory {
        fn name(&self) -> &'static str {
            "observer-stub-vad"
        }
        fn validate(&self, _cfg: &voice_pipes_sdk::VadConfig) -> Vec<voice_pipes_sdk::ConfigError> {
            vec![]
        }
        fn build(
            &self,
            _cfg: &voice_pipes_sdk::VadConfig,
            _ctx: &voice_pipes_sdk::BuildContext<'_>,
        ) -> Result<Box<dyn voice_pipes_sdk::VadBackend>, voice_pipes_sdk::BuildError> {
            Ok(Box::new(AlwaysSpeech))
        }
    }
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        crate::vad::segmenter::register_vad_factory(|| Box::new(AlwaysSpeechFactory))
            .expect("test name is free");
    });

    let cfg = EndpointObserverConfig {
        backend: "observer-stub-vad".into(),
        ..EndpointObserverConfig::default()
    };
    let obs = EndpointObserver::from_config(Some(&cfg)).expect("enabled");
    assert_eq!(obs.backend_name(), "observer-stub-vad");

    let pipeline: crate::config::PipelineConfig = toml::from_str(
        "[realtime_provider]\nprovider = \"openai-realtime\"\n\
         [realtime_provider.endpoint_observer]\nbackend = \"observer-stub-vad\"\n",
    )
    .unwrap();
    let errors = crate::config::validate(&pipeline);
    assert!(
        !errors
            .iter()
            .any(|e| e.field.starts_with("realtime_provider.endpoint_observer")),
        "{errors:?}"
    );
}
