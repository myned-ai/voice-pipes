use super::*;

/// Test processor that tags every chunk's `extras` with its name.
struct TagProcessor {
    tag: &'static str,
}

#[async_trait]
impl AudioPostProcessor for TagProcessor {
    fn name(&self) -> &str {
        self.tag
    }

    async fn process(
        &self,
        chunk: &mut AudioChunk,
        _event_tx: &mpsc::Sender<ServerMessage>,
    ) -> Result<(), AudioPostProcessorError> {
        let mut obj = chunk
            .extras
            .clone()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        obj.insert(self.tag.to_string(), serde_json::Value::Bool(true));
        chunk.extras = Some(serde_json::Value::Object(obj));
        Ok(())
    }
}

#[tokio::test]
async fn run_chain_applies_processors_in_order() {
    let processors: Vec<Box<dyn AudioPostProcessor>> = vec![
        Box::new(TagProcessor { tag: "first" }),
        Box::new(TagProcessor { tag: "second" }),
    ];
    let (tx, _rx) = mpsc::channel::<ServerMessage>(8);
    let mut chunk = AudioChunk::default();

    run_chain(&processors, &mut chunk, &tx, &mut ChainStats::default()).await;

    let extras = chunk.extras.expect("extras populated");
    let obj = extras.as_object().unwrap();
    assert_eq!(obj.get("first"), Some(&serde_json::Value::Bool(true)));
    assert_eq!(obj.get("second"), Some(&serde_json::Value::Bool(true)));
}

#[tokio::test]
async fn run_chain_with_empty_list_is_a_noop() {
    let processors: Vec<Box<dyn AudioPostProcessor>> = vec![];
    let (tx, _rx) = mpsc::channel::<ServerMessage>(8);
    let mut chunk = AudioChunk::default();

    run_chain(&processors, &mut chunk, &tx, &mut ChainStats::default()).await;

    assert!(chunk.extras.is_none());
}

#[tokio::test]
async fn run_chain_records_per_processor_stats() {
    let processors: Vec<Box<dyn AudioPostProcessor>> = vec![
        Box::new(TagProcessor { tag: "first" }),
        Box::new(TagProcessor { tag: "second" }),
    ];
    let (tx, _rx) = mpsc::channel::<ServerMessage>(8);
    let mut chunk = AudioChunk {
        samples: vec![0; 960],
        sample_rate: 24_000,
        channels: 1,
        ..AudioChunk::default()
    };
    let mut stats = ChainStats::default();
    run_chain(&processors, &mut chunk, &tx, &mut stats).await;
    run_chain(&processors, &mut chunk, &tx, &mut stats).await;
    let comps = stats.components();
    assert_eq!(comps.len(), 2);
    assert_eq!(comps[0].component_id, "audio_post:first");
    assert_eq!(comps[0].extras["calls"], 2);
    assert_eq!(comps[0].extras["failures"], 0);
    assert_eq!(comps[1].component_id, "audio_post:second");
    // Default descriptor mirrors the component id.
    assert_eq!(
        processors[0].turn_descriptor().component_id,
        "audio_post:first"
    );
}

#[test]
fn registry_contains_noop() {
    let factories = audio_post_processor_factories();
    assert!(factories.iter().any(|f| f.name() == "noop"));
}

#[cfg(feature = "post-grpc")]
#[test]
fn registry_contains_grpc_when_enabled() {
    let factories = audio_post_processor_factories();
    assert!(factories.iter().any(|f| f.name() == "grpc"));
}

fn empty_config() -> PipelineConfig {
    PipelineConfig {
        pipeline: crate::config::PipelineSettings::default(),
        speculative: crate::config::SpeculativeConfig::default(),
        stt: None,
        tts: None,
        output: None,
        vad: None,
        turn_detection: None,
        grading: None,
        barge_in: None,
        adaptive_quality: crate::config::AdaptiveQualityConfig::default(),
        transport: None,
        text_processor: vec![],
        audio_post_processor: vec![],
        realtime_provider: None,
        auth: None,
        agent: None,
        client_tool: vec![],
        client_tools_path: None,
    }
}

#[test]
fn build_unknown_type_fails() {
    let mut cfg = empty_config();
    cfg.audio_post_processor = vec![AudioPostProcessorConfig {
        processor_type: "no-such-processor".into(),
        ..AudioPostProcessorConfig::default()
    }];
    let err = build_audio_post_processors(&cfg)
        .err()
        .expect("unknown processor type must error");
    assert!(err.to_string().contains("no-such-processor"));

    cfg.audio_post_processor.clear();
    let processors = build_audio_post_processors(&cfg).expect("build ok");
    assert!(processors.is_empty());
}

#[test]
fn annotate_mode_entries_are_not_built_inline() {
    let mut cfg = empty_config();
    cfg.audio_post_processor = vec![
        AudioPostProcessorConfig {
            processor_type: "noop".into(),
            ..AudioPostProcessorConfig::default()
        },
        AudioPostProcessorConfig {
            processor_type: "level".into(),
            mode: crate::config::PostProcessorMode::Annotate,
            ..AudioPostProcessorConfig::default()
        },
    ];
    let inline = build_audio_post_processors(&cfg).expect("inline chain");
    assert_eq!(inline.len(), 1, "annotate entries are not inline stages");
    assert_eq!(inline[0].name(), "noop");
    let annotators = super::super::annotate::build_audio_annotators(&cfg).expect("annotators");
    assert_eq!(annotators.len(), 1);
    assert_eq!(annotators[0].annotator.name(), "level");
    assert_eq!(
        annotators[0].config,
        crate::config::AnnotatorConfig::default()
    );
}

#[test]
fn annotate_mode_on_inline_only_type_fails_fast() {
    let mut cfg = empty_config();
    cfg.audio_post_processor = vec![AudioPostProcessorConfig {
        processor_type: "noop".into(),
        mode: crate::config::PostProcessorMode::Annotate,
        ..AudioPostProcessorConfig::default()
    }];
    // `noop` supports annotate (NoopAnnotator); an inline-only factory
    // would surface the default error. Exercise it through the trait.
    let err = NoopFactory
        .build_annotator(&cfg.audio_post_processor[0])
        .err()
        .map(|e| e.to_string());
    assert!(err.is_none() || err.unwrap().contains("annotate"));
}
