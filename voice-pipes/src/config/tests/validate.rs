use super::*;

fn field_errors(toml_text: &str) -> Vec<String> {
    let cfg: PipelineConfig = toml::from_str(toml_text).unwrap();
    validate(&cfg).into_iter().map(|e| e.field).collect()
}

/// Every range the runtime divides by, sizes a pool or channel from, or
/// compares as an ordered threshold set.
#[test]
fn runtime_ranges_are_validated() {
    let cases: &[(&str, &str)] = &[
        ("[transport]\nenabled = true\nport = 0\n", "transport.port"),
        ("[transport]\nmax_sessions = 0\n", "transport.max_sessions"),
        (
            "[transport]\nwrite_timeout_ms = 0\n",
            "transport.write_timeout_ms",
        ),
        (
            "[pipeline.audit]\nbuffer_size = 0\n",
            "pipeline.audit.buffer_size",
        ),
        ("[agent]\nidle_timeout_ms = 0\n", "agent.idle_timeout_ms"),
        (
            "[adaptive_quality]\nenabled = true\ndowngrade_observations = 0\n",
            "adaptive_quality.downgrade_observations",
        ),
        (
            "[adaptive_quality]\nenabled = true\nreduced_arrival_rtf = 3.0\nminimal_arrival_rtf = 2.0\n",
            "adaptive_quality.*_arrival_rtf",
        ),
        (
            "[adaptive_quality]\nenabled = true\nreduced_send_latency_ms = 900\nminimal_send_latency_ms = 100\n",
            "adaptive_quality.*_send_latency_ms",
        ),
        (
            "[adaptive_quality]\nenabled = true\nreduced_underruns = 5\nminimal_underruns = 2\n",
            "adaptive_quality.*_underruns",
        ),
        (
            "[[audio_post_processor]]\ntype = \"noop\"\n[audio_post_processor.circuit_breaker]\ncooldown_ms = 0\n",
            "audio_post_processor[0].circuit_breaker.cooldown_ms",
        ),
        (
            "[realtime_provider]\nprovider = \"openai-realtime\"\n[realtime_provider.endpoint_observer]\nbackend = \"magic\"\n",
            "realtime_provider.endpoint_observer.backend",
        ),
        (
            "[realtime_provider]\nprovider = \"openai-realtime\"\n[realtime_provider.endpoint_observer]\nspeech_prob = 1.5\n",
            "realtime_provider.endpoint_observer.speech_prob",
        ),
        (
            "[realtime_provider]\nprovider = \"openai-realtime\"\n[realtime_provider.options]\noutput_emit_rate = 1234\n",
            "realtime_provider.options.output_emit_rate",
        ),
        (
            "[vad]\nmode = \"auto\"\nmin_speech_ms = 0\n",
            "vad.min_speech_ms",
        ),
        (
            "[vad]\nmode = \"auto\"\nneg_threshold = 2.0\n",
            "vad.neg_threshold",
        ),
    ];
    for (toml_text, field) in cases {
        let errs = field_errors(toml_text);
        assert!(errs.iter().any(|f| f == field), "{field}: {errs:?}");
    }
    // Defaults pass all of the above.
    let errs = field_errors("[transport]\nenabled = true\n[adaptive_quality]\nenabled = true\n");
    assert!(
        !errs
            .iter()
            .any(|f| f.starts_with("transport.") || f.starts_with("adaptive_quality.")),
        "{errs:?}"
    );
}

/// Prompt strings ride along with every request or session, and repeated
/// tables are instantiated per session; both are capped.
#[test]
fn prompt_length_and_table_counts_are_capped() {
    let long = "x".repeat(MAX_PROMPT_BYTES + 1);
    let errs = field_errors(&format!(
        "[realtime_provider]\nprovider = \"openai-realtime\"\ninstructions = \"{long}\"\n"
    ));
    assert!(
        errs.contains(&"realtime_provider.instructions".to_string()),
        "{errs:?}"
    );
    let errs = field_errors(&format!(
        "[[text_processor]]\ntype = \"openai-compat\"\nsystem_prompt = \"{long}\"\n"
    ));
    assert!(
        errs.contains(&"text_processor[0].system_prompt".to_string()),
        "{errs:?}"
    );
    let errs = field_errors(&format!("[agent]\nidle_message = \"{long}\"\n"));
    assert!(errs.contains(&"agent.idle_message".to_string()), "{errs:?}");

    let many = "[[text_processor]]\ntype = \"openai-compat\"\n".repeat(MAX_TEXT_PROCESSORS + 1);
    let errs = field_errors(&many);
    assert!(errs.contains(&"text_processor".to_string()), "{errs:?}");
    let many = "[[audio_post_processor]]\ntype = \"noop\"\n".repeat(MAX_POST_PROCESSORS + 1);
    let errs = field_errors(&many);
    assert!(
        errs.contains(&"audio_post_processor".to_string()),
        "{errs:?}"
    );
}

/// A zero silence timeout would end every utterance on the first
/// post-onset chunk; the VAD filter derives its silence budget from it.
#[test]
fn zero_silence_timeout_is_a_config_error() {
    let cfg: PipelineConfig =
        toml::from_str("[turn_detection]\nmode = \"silence\"\nsilence_timeout_ms = 0\n").unwrap();
    let errs = validate(&cfg);
    assert!(
        errs.iter()
            .any(|e| e.field == "turn_detection.silence_timeout_ms"),
        "{errs:?}"
    );
    let ok: PipelineConfig =
        toml::from_str("[turn_detection]\nmode = \"silence\"\nsilence_timeout_ms = 450\n").unwrap();
    assert!(
        validate(&ok)
            .iter()
            .all(|e| e.field != "turn_detection.silence_timeout_ms")
    );
}

/// The observer's backend checks its own settings, reported under the
/// observer's fields: a model backend without a model names the observer's
/// `model`.
#[test]
fn an_observer_backend_reports_its_own_settings_under_the_observer() {
    crate::vad::segmenter::stand_in::register();
    let cfg: PipelineConfig = toml::from_str(&format!(
        "[realtime_provider]\nprovider = \"openai-realtime\"\n\
         [realtime_provider.endpoint_observer]\nbackend = \"{}\"\n",
        crate::vad::segmenter::stand_in::NAME
    ))
    .unwrap();
    let errors = validate(&cfg);
    assert!(
        errors
            .iter()
            .any(|e| e.field == "realtime_provider.endpoint_observer.model"),
        "{errors:?}"
    );
}

/// A text-processor factory's error names the entry it is about: with two
/// processors of one type, the field says which one is wrong.
#[test]
fn a_text_processor_error_names_its_entry() {
    struct Picky;
    impl crate::text::TextProcessorFactory for Picky {
        fn name(&self) -> &'static str {
            "validate-index-test"
        }
        fn validate(&self, cfg: &crate::config::TextProcessorConfig) -> Vec<ConfigError> {
            if cfg.options.contains_key("bad") {
                vec![ConfigError::new("text_processor.bad", "not allowed")]
            } else {
                Vec::new()
            }
        }
        fn build(
            &self,
            _cfg: &crate::config::TextProcessorConfig,
            _ctx: &voice_pipes_sdk::BuildContext<'_>,
        ) -> Result<Box<dyn crate::text::TextProcessor>, voice_pipes_sdk::BuildError> {
            Err(voice_pipes_sdk::BuildError::failed("never built"))
        }
    }
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        crate::text::register_text_processor_factory(|| Box::new(Picky)).expect("free name");
    });
    let errs = field_errors(
        "[[text_processor]]\ntype = \"validate-index-test\"\n\n\
         [[text_processor]]\ntype = \"validate-index-test\"\nbad = true\n",
    );
    assert!(
        errs.contains(&"text_processor[1].bad".to_string()),
        "{errs:?}"
    );
}

/// With no `[turn_detection]` section a turn ends after the documented
/// default of 1000 ms, for every reader: the grading rule used 1500 there
/// and refused an achievable 1200 ms target, and the VAD filter waited
/// 1500 ms while the turn detector waited 1000.
#[test]
fn the_silence_default_is_one_number_without_the_section() {
    let cfg: PipelineConfig = toml::from_str("").unwrap();
    assert_eq!(cfg.silence_timeout_ms(), 1000);
    let with_section: PipelineConfig =
        toml::from_str("[turn_detection]\nmode = \"silence\"\n").unwrap();
    assert_eq!(with_section.silence_timeout_ms(), 1000);

    assert!(
        !field_errors("[grading.targets]\nendpointing_ms = 1200\n")
            .contains(&"grading.targets.endpointing_ms".to_string()),
        "1200 ms can be met with the 1000 ms default"
    );
    assert!(
        field_errors("[grading.targets]\nendpointing_ms = 900\n")
            .contains(&"grading.targets.endpointing_ms".to_string())
    );
}

/// Without the `realtime` feature no speech-to-speech provider exists; a
/// `[realtime_provider]` section must be refused rather than loaded into
/// a worker that would never hear the caller.
#[cfg(not(feature = "realtime"))]
#[test]
fn realtime_section_is_refused_by_a_build_without_realtime() {
    let errs = field_errors("[realtime_provider]\nprovider = \"x\"\n");
    assert!(
        errs.contains(&"realtime_provider.provider".to_string()),
        "{errs:?}"
    );
}

#[test]
fn audio_filter_validates_unknown_provider_and_indexes_errors() {
    struct PickyFilter;
    impl voice_pipes_sdk::filter::AudioFilterFactory for PickyFilter {
        fn name(&self) -> &'static str {
            "validate-filter-index-test"
        }
        fn validate(&self, cfg: &voice_pipes_sdk::FilterStageConfig) -> Vec<ConfigError> {
            if cfg.options.contains_key("bad") {
                vec![ConfigError::new("audio_filter.bad", "not allowed")]
            } else {
                Vec::new()
            }
        }
        fn build(
            &self,
            _cfg: &voice_pipes_sdk::FilterStageConfig,
            _ctx: &voice_pipes_sdk::BuildContext<'_>,
        ) -> Result<Box<dyn voice_pipes_sdk::filter::AudioFilter>, voice_pipes_sdk::BuildError>
        {
            Err(voice_pipes_sdk::BuildError::failed("never built"))
        }
    }
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        crate::pipeline::filter::register_filter_factory(|| Box::new(PickyFilter))
            .expect("free name");
    });

    let unknown_errs = field_errors("[[audio_filter]]\nprovider = \"nonexistent-filter\"\n");
    assert!(
        unknown_errs.contains(&"audio_filter[0].provider".to_string()),
        "{unknown_errs:?}"
    );

    let indexed_errs = field_errors(
        "[[audio_filter]]\nprovider = \"validate-filter-index-test\"\n\n\
         [[audio_filter]]\nprovider = \"validate-filter-index-test\"\nbad = true\n",
    );
    assert!(
        indexed_errs.contains(&"audio_filter[1].bad".to_string()),
        "{indexed_errs:?}"
    );
}
