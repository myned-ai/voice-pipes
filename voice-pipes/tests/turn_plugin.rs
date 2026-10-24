//! A turn detector registered from outside the engine, through the public
//! API alone, as a plugin crate would: its mode resolves by name, it checks
//! its own settings, a second detector of the same name is refused, and its
//! decision is what commits a turn in the STT stage.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::sync::mpsc;
use voice_pipes::config::PipelineConfig;
use voice_pipes::sdk::turn::{TurnConfig, TurnDetector, TurnDetectorFactory, TurnSignal};
use voice_pipes::sdk::{BuildContext, ConfigError, TranscriptionResult};
use voice_pipes::stt::{
    AudioChunk as SttChunk, RecognitionRequest, SttCapability, SttError, SttProvider,
};

/// Ends a turn only when the transcript ends with a configured word.
struct SuffixDetector {
    suffix: String,
}

#[async_trait::async_trait]
impl TurnDetector for SuffixDetector {
    async fn evaluate(&self, result: &TranscriptionResult, _silence: Duration) -> TurnSignal {
        if result.text.trim_end().ends_with(self.suffix.as_str()) {
            TurnSignal::likely_complete()
        } else {
            TurnSignal::incomplete()
        }
    }

    fn name(&self) -> &'static str {
        "suffix"
    }
}

const SUFFIX_KEYS: &[&str] = &["suffix_word"];

struct SuffixFactory;

impl TurnDetectorFactory for SuffixFactory {
    fn name(&self) -> &'static str {
        "suffix"
    }

    fn validate(&self, cfg: &TurnConfig) -> Vec<ConfigError> {
        let mut errors = cfg.unknown_options(SUFFIX_KEYS);
        if cfg.options().string("suffix_word").is_none() {
            errors.push(ConfigError::new("turn_detection.suffix_word", "required"));
        }
        errors
    }

    fn build(
        &self,
        cfg: &TurnConfig,
        _ctx: &BuildContext<'_>,
    ) -> Result<Box<dyn TurnDetector>, voice_pipes::sdk::BuildError> {
        let suffix = cfg
            .options()
            .string("suffix_word")
            .ok_or_else(|| anyhow::anyhow!("turn_detection.suffix_word is required"))?;
        Ok(Box::new(SuffixDetector { suffix }))
    }
}

fn suffix_factory() -> Box<dyn TurnDetectorFactory> {
    Box::new(SuffixFactory)
}

/// A plugin that tries to take a built-in mode's name.
struct ShadowingFactory;

impl TurnDetectorFactory for ShadowingFactory {
    fn name(&self) -> &'static str {
        "silence"
    }

    fn validate(&self, _cfg: &TurnConfig) -> Vec<ConfigError> {
        Vec::new()
    }

    fn build(
        &self,
        _cfg: &TurnConfig,
        _ctx: &BuildContext<'_>,
    ) -> Result<Box<dyn TurnDetector>, voice_pipes::sdk::BuildError> {
        Err(voice_pipes_sdk::BuildError::failed("never built"))
    }
}

fn shadowing_factory() -> Box<dyn TurnDetectorFactory> {
    Box::new(ShadowingFactory)
}

/// The registry is process-wide; every test registers through here once.
fn register() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        voice_pipes::stt::turn::register_turn_factory(suffix_factory)
            .expect("the first registration succeeds");
    });
}

fn config(section: &str) -> PipelineConfig {
    toml::from_str(&format!("[turn_detection]\n{section}")).expect("parses")
}

fn turn_errors(cfg: &PipelineConfig) -> Vec<String> {
    voice_pipes::config::validate(cfg)
        .into_iter()
        .filter(|e| e.field.starts_with("turn_detection"))
        .map(|e| format!("{}: {}", e.field, e.message))
        .collect()
}

#[test]
fn the_mode_resolves_by_name_and_checks_its_own_settings() {
    register();
    assert!(voice_pipes::stt::turn::lookup_turn_factory("suffix").is_some());

    let good = config("mode = \"suffix\"\nsuffix_word = \"over\"\n");
    assert!(turn_errors(&good).is_empty(), "{:?}", turn_errors(&good));

    let misspelt = config("mode = \"suffix\"\nsufix_word = \"over\"\n");
    let errors = turn_errors(&misspelt);
    assert!(
        errors
            .iter()
            .any(|e| e.starts_with("turn_detection.sufix_word")),
        "{errors:?}"
    );
    assert!(
        errors
            .iter()
            .any(|e| e.starts_with("turn_detection.suffix_word")),
        "{errors:?}"
    );
}

#[test]
fn a_second_detector_with_a_taken_name_is_refused() {
    register();
    assert!(
        voice_pipes::stt::turn::register_turn_factory(suffix_factory).is_err(),
        "a plugin registered twice"
    );
    assert!(
        voice_pipes::stt::turn::register_turn_factory(shadowing_factory).is_err(),
        "a plugin taking a built-in mode's name"
    );
    // The refused registrations left the registry as it was.
    let names: Vec<String> = voice_pipes::stt::turn::turn_detector_factories()
        .iter()
        .map(|f| f.name().to_string())
        .collect();
    assert_eq!(
        names.iter().filter(|n| *n == "silence").count(),
        1,
        "{names:?}"
    );
    assert_eq!(
        names.iter().filter(|n| *n == "suffix").count(),
        1,
        "{names:?}"
    );
}

/// A recogniser that answers each utterance with one fixed final result.
struct OneAnswerStt {
    text: &'static str,
}

#[async_trait::async_trait]
impl SttProvider for OneAnswerStt {
    async fn transcribe(
        &self,
        _audio: &voice_pipes::audio::AudioFrame,
    ) -> Result<TranscriptionResult, SttError> {
        Ok(TranscriptionResult::new(self.text, true))
    }

    async fn transcribe_stream(
        &self,
        _request: RecognitionRequest,
        mut audio_rx: mpsc::Receiver<SttChunk>,
        tx: mpsc::Sender<TranscriptionResult>,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> Result<(), SttError> {
        while let Some(chunk) = audio_rx.recv().await {
            if chunk.is_last {
                break;
            }
        }
        let _ = tx.send(TranscriptionResult::new(self.text, true)).await;
        // Hold the result channel open so the stage's silence timer runs.
        while audio_rx.recv().await.is_some() {}
        Ok(())
    }

    fn expected_sample_rate(&self) -> u32 {
        16_000
    }

    fn name(&self) -> &'static str {
        "one-answer"
    }

    fn capabilities(&self) -> std::collections::HashSet<SttCapability> {
        std::collections::HashSet::new()
    }
}

/// Run one utterance whose transcript is `heard` through the STT stage with
/// the plugin detector set to end turns on `word`; return what was committed
/// within `wait`.
async fn committed(heard: &'static str, word: &str, wait: Duration) -> Option<String> {
    register();
    let cfg = config(&format!(
        "mode = \"suffix\"\nsilence_timeout_ms = 100\nsuffix_word = \"{word}\"\n"
    ));
    let detector =
        voice_pipes::config::build_turn_detector(&cfg, None).expect("the plugin detector builds");
    let (mic_tx, mic_rx) = mpsc::channel(8);
    let (text_tx, mut text_rx) = mpsc::channel(4);
    let stage = tokio::spawn(voice_pipes::pipeline::stt_task(
        mic_rx,
        text_tx,
        None,
        Arc::new(OneAnswerStt { text: heard }),
        detector,
        None,
        None,
        0,
        None,
        None,
    ));
    mic_tx
        .send(voice_pipes::pipeline::AudioChunk {
            samples: vec![0i16; 16_000],
            sample_rate: 16_000,
            channels: 1,
            is_end_of_utterance: true,
            caption: None,
            utterance_id: 0,
            full_text: None,
            extras: None,
            last_speech_at: None,
            synth_duration: None,
        })
        .await
        .expect("the stage takes audio");
    let got = tokio::time::timeout(wait, text_rx.recv())
        .await
        .ok()
        .flatten()
        .map(|u| u.text);
    stage.abort();
    drop(mic_tx);
    got
}

#[tokio::test]
async fn the_plugin_decision_commits_the_turn() {
    let got = committed("hello, over", "over", Duration::from_secs(5)).await;
    assert_eq!(got.as_deref(), Some("hello, over"));
}

#[tokio::test]
async fn the_plugin_decision_holds_the_turn_open() {
    // The stage re-asks the detector every 100 ms once the result is in; a
    // detector that keeps saying "incomplete" keeps the turn open.
    let got = committed("hello, over", "out", Duration::from_millis(1_500)).await;
    assert_eq!(got, None);
}
