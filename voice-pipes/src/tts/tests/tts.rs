use super::*;
use crate::audio::AudioFrame;
use async_trait::async_trait;

pub struct MockTtsProvider {
    pub mock_sample_rate: u32,
}

#[async_trait]
impl TtsProvider for MockTtsProvider {
    async fn synthesize(&self, _text: &str) -> Result<AudioFrame, TtsError> {
        // 1 second of silence
        Ok(AudioFrame {
            samples: vec![0i16; self.mock_sample_rate as usize],
            sample_rate: self.mock_sample_rate,
            channels: 1,
        })
    }

    fn sample_rate(&self) -> u32 {
        self.mock_sample_rate
    }

    fn name(&self) -> &'static str {
        "mock"
    }
}

#[tokio::test]
async fn mock_provider_returns_silence() {
    let provider = MockTtsProvider {
        mock_sample_rate: 22050,
    };
    let frame = provider.synthesize("hello").await.unwrap();
    assert_eq!(frame.samples.len(), 22050);
    assert_eq!(frame.sample_rate, 22050);
    assert_eq!(frame.channels, 1);
    assert!(frame.samples.iter().all(|&s| s == 0));
}

// ── Factory registry tests ──────────────────────────────

#[test]
fn tts_factory_names_are_unique() {
    let factories = tts_factories();
    let names: Vec<&str> = factories.iter().map(|f| f.name()).collect();
    let unique: std::collections::HashSet<&str> = names.iter().copied().collect();
    assert_eq!(
        names.len(),
        unique.len(),
        "duplicate TTS factory names: {names:?}"
    );
}

#[test]
fn tts_factory_lookup_unknown_returns_none() {
    let factories = tts_factories();
    assert!(!factories.iter().any(|f| f.name() == "nonexistent"));
}

// ── TtsSession scaffolding tests ──────────────

/// Default `open_tts_session` returns `None` so all stateless providers
/// (Piper, Pocket-HTTP, Gemini, `OpenAI`, gRPC, Kokoro) keep working
/// without any change.
#[tokio::test]
async fn default_open_tts_session_is_none() {
    let provider = MockTtsProvider {
        mock_sample_rate: 22050,
    };
    let cfg = TtsSessionConfig::new("sess_test").with_output_sample_rate(22050);
    let opened = provider.open_tts_session(&cfg).await.unwrap();
    assert!(
        opened.is_none(),
        "stateless TtsProviders must default to None"
    );
}

/// A trivial in-memory `TtsSession` impl. Mirrors what the runner will
/// do once the integration points land: tee user audio ->
/// commit -> synthesize -> frames flow back via `tx`.
pub struct MockTtsSession {
    pub user_audio_frames: usize,
    pub commits: usize,
    pub cancels: usize,
    pub output_sample_rate: u32,
}

#[async_trait]
impl TtsSession for MockTtsSession {
    async fn push_user_audio(&mut self, _frame: &AudioFrame) -> Result<(), TtsError> {
        self.user_audio_frames += 1;
        Ok(())
    }

    async fn commit_user_turn(&mut self) -> Result<(), TtsError> {
        self.commits += 1;
        Ok(())
    }

    async fn synthesize_turn(
        &mut self,
        text: &str,
        _hints: Option<&SynthesisHints>,
        tx: tokio::sync::mpsc::Sender<AudioFrame>,
    ) -> Result<(), TtsError> {
        // 1 sample per character at the configured output rate, so tests
        // can assert frame.samples.len() == text.len().
        let frame = AudioFrame {
            samples: vec![0i16; text.len()],
            sample_rate: self.output_sample_rate,
            channels: 1,
        };
        tx.send(frame)
            .await
            .map_err(|e| TtsError::InferenceFailed(e.to_string()))?;
        Ok(())
    }

    async fn cancel(&mut self) -> Result<(), TtsError> {
        self.cancels += 1;
        Ok(())
    }

    async fn close(self: Box<Self>) -> Result<(), TtsError> {
        Ok(())
    }
}

#[tokio::test]
async fn mock_session_roundtrips_user_audio_and_synthesis() {
    let mut session = MockTtsSession {
        user_audio_frames: 0,
        commits: 0,
        cancels: 0,
        output_sample_rate: 24000,
    };

    // Push two frames of user audio + commit.
    let frame = AudioFrame {
        samples: vec![0i16; 480], // 30ms @ 16kHz
        sample_rate: 16000,
        channels: 1,
    };
    session.push_user_audio(&frame).await.unwrap();
    session.push_user_audio(&frame).await.unwrap();
    session.commit_user_turn().await.unwrap();
    assert_eq!(session.user_audio_frames, 2);
    assert_eq!(session.commits, 1);

    // Synthesize and drain the channel.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<AudioFrame>(4);
    session.synthesize_turn("hello", None, tx).await.unwrap();
    let out = rx.recv().await.expect("session must emit one frame");
    assert_eq!(out.samples.len(), 5, "mock emits 1 sample per input char");
    assert_eq!(out.sample_rate, 24000);

    // Cancel + close.
    session.cancel().await.unwrap();
    assert_eq!(session.cancels, 1);
    Box::new(session).close().await.unwrap();
}

// ── A plugin voice with a live parameter ────────────────────

/// A voice built from outside the engine that reads its voice from a live
/// cell, as the hosted voices do.
struct LiveVoiceTts {
    voice: std::sync::Arc<arc_swap::ArcSwap<String>>,
}

#[async_trait]
impl voice_pipes_sdk::TtsProvider for LiveVoiceTts {
    async fn synthesize(&self, _text: &str) -> Result<AudioFrame, TtsError> {
        // The voice in use is reported through the sample count, so the test
        // can observe it through the provider interface alone.
        Ok(AudioFrame {
            samples: vec![0i16; self.voice.load().len()],
            sample_rate: 16_000,
            channels: 1,
        })
    }
    fn sample_rate(&self) -> u32 {
        16_000
    }
    fn name(&self) -> &'static str {
        "live-voice-stub"
    }
}

struct LiveVoiceFactory;

impl voice_pipes_sdk::TtsFactory for LiveVoiceFactory {
    fn name(&self) -> &'static str {
        "live-voice-stub"
    }
    fn validate(&self, _cfg: &voice_pipes_sdk::TtsConfig) -> Vec<voice_pipes_sdk::ConfigError> {
        vec![]
    }
    fn build(
        &self,
        _cfg: &voice_pipes_sdk::TtsConfig,
        ctx: &voice_pipes_sdk::BuildContext<'_>,
    ) -> Result<Box<dyn voice_pipes_sdk::TtsProvider>, voice_pipes_sdk::BuildError> {
        let voice = ctx
            .params
            .and_then(|p| p.string_cell("voice"))
            .ok_or_else(|| anyhow::anyhow!("no live voice cell"))?;
        Ok(Box::new(LiveVoiceTts { voice }))
    }
}

/// A plugin voice receives the worker's live store, and a change made
/// through the parameter registry reaches it on the next synthesis.
#[tokio::test]
async fn a_plugin_voice_sees_a_live_parameter_change() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        register_tts_factory(|| Box::new(LiveVoiceFactory)).expect("test name is free");
    });

    let cfg: crate::config::PipelineConfig =
        toml::from_str("[tts]\nprovider = \"live-voice-stub\"\nvoice = \"alba\"\n").unwrap();
    let store = crate::config::build_tts_param_store(&cfg);
    let registry = crate::api::param_store::ParamRegistry::new();
    registry.register(
        "tts",
        vec![voice_pipes_sdk::ParamDescriptor::mutable_str(
            "voice", "Voice", "d",
        )],
        std::sync::Arc::<crate::api::param_store::AtomicParamStore>::clone(&store),
    );
    let tts = crate::config::build_tts_provider(&cfg, None, Some(&store)).unwrap();
    assert_eq!(
        tts.synthesize("x").await.unwrap().samples.len(),
        "alba".len()
    );

    registry
        .apply("tts", "voice", serde_json::json!("marius"))
        .await
        .unwrap();
    assert_eq!(
        tts.synthesize("x").await.unwrap().samples.len(),
        "marius".len()
    );
}
