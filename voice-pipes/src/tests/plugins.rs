use super::*;

/// A name, or an alias, that an existing provider answers to is refused and
/// the message names both; a free name passes.
#[test]
fn a_taken_name_or_alias_is_refused() {
    let taken = || {
        vec![
            ("parakeet-tdt".to_string(), vec!["parakeet".to_string()]),
            ("google".to_string(), vec![]),
        ]
    };
    let err = check_new_name("STT", "google", &[], taken()).unwrap_err();
    assert!(
        matches!(&err, RegistrationError::NameTaken { name, existing, .. }
            if name == "google" && existing == "google"),
        "{err:?}"
    );

    let err = check_new_name("STT", "my-stt", &["parakeet"], taken()).unwrap_err();
    assert_eq!(
        err,
        RegistrationError::NameTaken {
            stage: "STT",
            provider: "my-stt".into(),
            name: "parakeet".into(),
            existing: "parakeet-tdt".into(),
        }
    );

    assert!(check_new_name("STT", "my-stt", &["mine"], taken()).is_ok());
}

/// A factory that registers a name a built-in has is refused instead of
/// shadowing it.
#[test]
fn a_plugin_cannot_take_a_built_in_name() {
    struct Echo2;
    impl voice_pipes_sdk::TextProcessorFactory for Echo2 {
        fn name(&self) -> &'static str {
            "echo"
        }
        fn validate(
            &self,
            _cfg: &voice_pipes_sdk::TextProcessorConfig,
        ) -> Vec<voice_pipes_sdk::ConfigError> {
            vec![]
        }
        fn build(
            &self,
            _cfg: &voice_pipes_sdk::TextProcessorConfig,
            _ctx: &voice_pipes_sdk::BuildContext<'_>,
        ) -> Result<Box<dyn voice_pipes_sdk::TextProcessor>, voice_pipes_sdk::BuildError> {
            Err(voice_pipes_sdk::BuildError::failed("never built"))
        }
    }
    let err = crate::text::register_text_processor_factory(|| Box::new(Echo2)).unwrap_err();
    assert!(
        matches!(&err, RegistrationError::NameTaken { existing, .. } if existing == "echo"),
        "{err:?}"
    );
    assert_eq!(
        crate::text::text_processor_factories()
            .iter()
            .filter(|f| f.name() == "echo")
            .count(),
        1,
        "the built-in is still the only echo"
    );
}

fn entry(label: &str, rank: u16, stage_default: bool) -> WizardChoice {
    WizardChoice::new(label, voice_pipes_sdk::WizardDeploy::None)
        .with_stage_default(stage_default)
        .with_menu_rank(rank)
}

/// Entries for a provider nobody registered are refused: a typo in the
/// provider name would otherwise write a config nothing can build.
#[test]
fn entries_for_an_unregistered_provider_are_refused() {
    let err = register_wizard_choices(
        Stage::Llm,
        "no-such-processor",
        vec![entry("Nothing", 0, false)],
    )
    .unwrap_err();
    assert!(
        matches!(&err, RegistrationError::NoSuchProvider { provider, .. } if provider == "no-such-processor"),
        "{err:?}"
    );
}

/// The turn-detection stage is the one no other test registers entries in,
/// so this owns it: entries come back by rank then label, whatever order
/// they were registered in, and a second default is refused.
#[test]
fn entries_are_ordered_by_rank_and_one_default_per_stage() {
    // Registered out of menu order.
    register_wizard_choices(
        Stage::Turn,
        "silence",
        vec![entry("Zed", 20, false), entry("Beta", 10, true)],
    )
    .unwrap();
    register_wizard_choices(Stage::Turn, "silence", vec![entry("Alpha", 10, false)]).unwrap();

    let labels: Vec<String> = wizard_choices(Stage::Turn)
        .into_iter()
        .map(|(provider, c)| {
            assert_eq!(provider, "silence");
            c.label
        })
        .collect();
    assert_eq!(labels, ["Alpha", "Beta", "Zed"]);

    let err =
        register_wizard_choices(Stage::Turn, "silence", vec![entry("Also", 5, true)]).unwrap_err();
    assert!(
        matches!(err, RegistrationError::TwoDefaults { .. }),
        "{err:?}"
    );
    assert_eq!(wizard_choices(Stage::Turn).len(), 3, "nothing registered");
}

/// `Stage::ALL` holds every stage exactly once. The match is
/// exhaustive, so a new stage stops this compiling until it is numbered
/// here, and then the count fails until it is in `ALL`.
#[test]
fn every_wizard_stage_is_in_all() {
    let number = |s: Stage| match s {
        Stage::Stt => 0,
        Stage::Llm => 1,
        Stage::Tts => 2,
        Stage::Realtime => 3,
        Stage::Turn => 4,
    };
    let mut seen: Vec<usize> = Stage::ALL.into_iter().map(number).collect();
    seen.sort_unstable();
    assert_eq!(seen, [0, 1, 2, 3, 4]);
}

/// A plugin whose factory constructor consults its own stage's registry
/// (to wrap or delegate to another processor) registers and resolves. The
/// registries used to construct every factory while holding their lock, so
/// that lookup waited on the lock its caller held, forever.
#[test]
fn a_constructor_that_looks_up_its_own_stage_does_not_deadlock() {
    struct Wrapper;
    impl voice_pipes_sdk::TextProcessorFactory for Wrapper {
        fn name(&self) -> &'static str {
            "registry-lookup-test"
        }
        fn validate(
            &self,
            _cfg: &voice_pipes_sdk::TextProcessorConfig,
        ) -> Vec<voice_pipes_sdk::ConfigError> {
            vec![]
        }
        fn build(
            &self,
            _cfg: &voice_pipes_sdk::TextProcessorConfig,
            _ctx: &voice_pipes_sdk::BuildContext<'_>,
        ) -> Result<Box<dyn voice_pipes_sdk::TextProcessor>, voice_pipes_sdk::BuildError> {
            Err(voice_pipes_sdk::BuildError::failed("never built"))
        }
    }
    fn wrapper() -> Box<dyn voice_pipes_sdk::TextProcessorFactory> {
        // What a wrapping plugin does in its constructor.
        let _inner = crate::text::text_processor_factories();
        Box::new(Wrapper)
    }

    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        crate::text::register_text_processor_factory(wrapper).expect("free name");
        let found = crate::text::text_processor_factories()
            .iter()
            .any(|f| f.name() == "registry-lookup-test");
        let _ = done_tx.send(found);
    });
    let found = done_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("registry deadlocked: a constructor ran under the registry's lock");
    assert!(found);
}

/// A VAD backend under a name and an alias, for the alias tests.
struct AliasedVad;

impl voice_pipes_sdk::vad::VadFactory for AliasedVad {
    fn name(&self) -> &'static str {
        "aliased-vad-under-test"
    }
    fn aliases(&self) -> &[&str] {
        &["avut"]
    }
    fn validate(&self, _cfg: &voice_pipes_sdk::VadConfig) -> Vec<voice_pipes_sdk::ConfigError> {
        vec![]
    }
    fn build(
        &self,
        _cfg: &voice_pipes_sdk::VadConfig,
        _ctx: &voice_pipes_sdk::BuildContext<'_>,
    ) -> Result<Box<dyn voice_pipes_sdk::vad::VadBackend>, voice_pipes_sdk::BuildError> {
        Err(voice_pipes_sdk::BuildError::failed("never built"))
    }
}

/// Every stage resolves aliases now, VAD among the four that had none: a
/// plugin found by its alias, and an alias another factory already has is
/// refused like a name.
#[test]
fn a_vad_plugin_resolves_by_its_alias_and_its_alias_is_taken() {
    register_plugin(Plugin::Vad(|| Box::new(AliasedVad)), Vec::new()).unwrap();
    let found = crate::vad::segmenter::lookup_vad_factory("avut").expect("found by alias");
    assert_eq!(found.name(), "aliased-vad-under-test");

    struct Squatter;
    impl voice_pipes_sdk::vad::VadFactory for Squatter {
        fn name(&self) -> &'static str {
            "avut"
        }
        fn validate(&self, _cfg: &voice_pipes_sdk::VadConfig) -> Vec<voice_pipes_sdk::ConfigError> {
            vec![]
        }
        fn build(
            &self,
            _cfg: &voice_pipes_sdk::VadConfig,
            _ctx: &voice_pipes_sdk::BuildContext<'_>,
        ) -> Result<Box<dyn voice_pipes_sdk::vad::VadBackend>, voice_pipes_sdk::BuildError>
        {
            Err(voice_pipes_sdk::BuildError::failed("never built"))
        }
    }
    let err = crate::vad::segmenter::register_vad_factory(|| Box::new(Squatter)).unwrap_err();
    assert!(
        matches!(&err, RegistrationError::NameTaken { name, .. } if name == "avut"),
        "{err:?}"
    );
}

/// The setup wizard has no VAD stage: entries for a VAD plugin are refused
/// before anything registers.
#[test]
fn wizard_entries_for_a_vad_plugin_are_refused() {
    struct Unlisted;
    impl voice_pipes_sdk::vad::VadFactory for Unlisted {
        fn name(&self) -> &'static str {
            "unlisted-vad-under-test"
        }
        fn validate(&self, _cfg: &voice_pipes_sdk::VadConfig) -> Vec<voice_pipes_sdk::ConfigError> {
            vec![]
        }
        fn build(
            &self,
            _cfg: &voice_pipes_sdk::VadConfig,
            _ctx: &voice_pipes_sdk::BuildContext<'_>,
        ) -> Result<Box<dyn voice_pipes_sdk::vad::VadBackend>, voice_pipes_sdk::BuildError>
        {
            Err(voice_pipes_sdk::BuildError::failed("never built"))
        }
    }
    let err = register_plugin(
        Plugin::Vad(|| Box::new(Unlisted)),
        vec![entry("VAD", 0, false), entry("VAD again", 1, false)],
    )
    .unwrap_err();
    assert_eq!(err, RegistrationError::NoStage { stage: "VAD" });
    assert!(crate::vad::segmenter::lookup_vad_factory("unlisted-vad-under-test").is_none());
}

/// A VAD backend's one entry is the `[vad]` settings it gives the wizard,
/// kept against the name it registered under. No test makes it the auto
/// backend, so no wizard run sees it.
#[test]
fn a_vad_plugins_one_entry_is_kept_as_its_settings() {
    struct Settled;
    impl voice_pipes_sdk::vad::VadFactory for Settled {
        fn name(&self) -> &'static str {
            "settled-vad-under-test"
        }
        fn validate(&self, _cfg: &voice_pipes_sdk::VadConfig) -> Vec<voice_pipes_sdk::ConfigError> {
            vec![]
        }
        fn build(
            &self,
            _cfg: &voice_pipes_sdk::VadConfig,
            _ctx: &voice_pipes_sdk::BuildContext<'_>,
        ) -> Result<Box<dyn voice_pipes_sdk::vad::VadBackend>, voice_pipes_sdk::BuildError>
        {
            Err(voice_pipes_sdk::BuildError::failed("never built"))
        }
    }
    register_plugin(
        Plugin::Vad(|| Box::new(Settled)),
        vec![entry("Settled VAD", 0, false)],
    )
    .expect("one entry is a VAD backend's settings");
    assert_eq!(
        crate::plugins::vad_wizard_entry("settled-vad-under-test").map(|c| c.label),
        Some("Settled VAD".to_string())
    );
    assert!(crate::plugins::vad_wizard_entry("no-such-vad").is_none());
}

/// The auto backend must be a registered VAD backend.
#[test]
fn an_unregistered_auto_backend_is_refused() {
    assert!(matches!(
        crate::vad::segmenter::register_auto_backend("no-such-vad-backend"),
        Err(RegistrationError::NoSuchProvider { stage: "VAD", .. })
    ));
}

/// One call registers a plugin's factory and its wizard entries, which
/// select it by the name it registered under.
#[test]
fn register_plugin_registers_the_factory_and_its_entries() {
    struct Listed;
    impl voice_pipes_sdk::TtsFactory for Listed {
        fn name(&self) -> &'static str {
            "listed-tts-under-test"
        }
        fn validate(&self, _cfg: &voice_pipes_sdk::TtsConfig) -> Vec<voice_pipes_sdk::ConfigError> {
            vec![]
        }
        fn build(
            &self,
            _cfg: &voice_pipes_sdk::TtsConfig,
            _ctx: &voice_pipes_sdk::BuildContext<'_>,
        ) -> Result<Box<dyn voice_pipes_sdk::TtsProvider>, voice_pipes_sdk::BuildError> {
            Err(voice_pipes_sdk::BuildError::failed("never built"))
        }
    }
    // Ranked last, so the TTS menu indices other tests take (the wizard's
    // stubs) do not move while this runs beside them.
    register_plugin(
        Plugin::Tts(|| Box::new(Listed)),
        vec![entry("Listed", u16::MAX, false)],
    )
    .unwrap();
    assert!(crate::tts::lookup_tts_factory("listed-tts-under-test").is_some());
    assert!(
        wizard_choices(Stage::Tts)
            .iter()
            .any(|(provider, c)| provider == "listed-tts-under-test" && c.label == "Listed")
    );
}

/// A TTS voice under a name and an alias, for the stage listing.
struct AliasedTts;

impl voice_pipes_sdk::TtsFactory for AliasedTts {
    fn name(&self) -> &'static str {
        "aliased-tts-under-test"
    }
    fn aliases(&self) -> &[&str] {
        &["atut"]
    }
    fn validate(&self, _cfg: &voice_pipes_sdk::TtsConfig) -> Vec<voice_pipes_sdk::ConfigError> {
        vec![]
    }
    fn build(
        &self,
        _cfg: &voice_pipes_sdk::TtsConfig,
        _ctx: &voice_pipes_sdk::BuildContext<'_>,
    ) -> Result<Box<dyn voice_pipes_sdk::TtsProvider>, voice_pipes_sdk::BuildError> {
        Err(voice_pipes_sdk::BuildError::failed("never built"))
    }
}

/// A stage lists its providers with their aliases (`inspect` showed none
/// for TTS, text processors and turn detectors), and names them when a
/// config asks for one it does not have.
#[test]
fn a_stage_lists_its_providers_and_names_them_for_an_unknown_one() {
    crate::tts::register_tts_factory(|| Box::new(AliasedTts)).unwrap();
    let listed = Stage::Tts
        .providers()
        .into_iter()
        .find(|p| p.name == "aliased-tts-under-test")
        .expect("listed");
    assert_eq!(listed.aliases, ["atut"]);

    let err = Stage::Tts.unknown_provider("tts.provider", "no-such-voice");
    assert_eq!(err.field, "tts.provider");
    assert_eq!(err.kind, voice_pipes_sdk::ConfigErrorKind::UnknownName);
    assert!(err.message.contains("no-such-voice"), "{}", err.message);
    assert!(
        err.message.contains("\"aliased-tts-under-test\""),
        "{}",
        err.message
    );
}

/// What a stage says about itself agrees with the config it is chosen in.
#[test]
fn every_stage_names_its_table_and_key() {
    for stage in Stage::ALL {
        let toml = format!("[{}]\n{} = \"x\"\n", stage.section(), stage.provider_key());
        let toml = if stage == Stage::Llm {
            format!(
                "[[{}]]\n{} = \"x\"\n",
                stage.section(),
                stage.provider_key()
            )
        } else {
            toml
        };
        let parsed: Result<crate::config::PipelineConfig, _> = toml::from_str(&toml);
        assert!(parsed.is_ok(), "{stage:?}: {toml}: {parsed:?}");
        assert!(!stage.menu_title().is_empty() && !stage.feature_hint().is_empty());
    }
}

/// With no hint registered the wizard names a plugin crate for the stage;
/// the binary's hint replaces that, and a later one replaces it in turn.
/// (Realtime, which no other test gives a hint.)
#[test]
fn a_build_hint_is_the_binarys_to_give() {
    let stage = crate::plugins::Stage::Realtime;
    assert!(
        stage.feature_hint().starts_with("a plugin crate"),
        "{}",
        stage.feature_hint()
    );
    crate::plugins::register_build_hint(stage, "s2s-first");
    crate::plugins::register_build_hint(stage, "s2s-second");
    assert_eq!(stage.feature_hint(), "s2s-second");
}

/// A VAD backend that refuses every config, under a name and an alias.
struct RefusingVad;

impl voice_pipes_sdk::vad::VadFactory for RefusingVad {
    fn name(&self) -> &'static str {
        "refusing-vad-under-test"
    }
    fn aliases(&self) -> &[&str] {
        &["rvut"]
    }
    fn validate(&self, _cfg: &voice_pipes_sdk::VadConfig) -> Vec<voice_pipes_sdk::ConfigError> {
        vec![voice_pipes_sdk::ConfigError::new(
            "vad.model_path",
            "refused under test",
        )]
    }
    fn build(
        &self,
        _cfg: &voice_pipes_sdk::VadConfig,
        _ctx: &voice_pipes_sdk::BuildContext<'_>,
    ) -> Result<Box<dyn voice_pipes_sdk::vad::VadBackend>, voice_pipes_sdk::BuildError> {
        Err(voice_pipes_sdk::BuildError::failed("never built"))
    }
}

/// A backend chosen by its alias checks its settings at load, as one
/// chosen by its name does.
#[test]
fn a_vad_backend_chosen_by_its_alias_validates_its_settings() {
    register_plugin(Plugin::Vad(|| Box::new(RefusingVad)), Vec::new()).unwrap();
    for mode in ["refusing-vad-under-test", "rvut"] {
        let cfg: crate::config::PipelineConfig =
            toml::from_str(&format!("[vad]\nmode = \"{mode}\"\n")).unwrap();
        let errors = crate::config::validate(&cfg);
        assert!(
            errors.iter().any(|e| e.message == "refused under test"),
            "mode {mode}: {errors:?}"
        );
    }
}

/// A turn detector with one setting of its own, under a name and an alias.
struct AliasedTurn;

impl voice_pipes_sdk::turn::TurnDetectorFactory for AliasedTurn {
    fn name(&self) -> &'static str {
        "aliased-turn-under-test"
    }
    fn aliases(&self) -> &[&str] {
        &["atut"]
    }
    fn validate(
        &self,
        _cfg: &voice_pipes_sdk::turn::TurnConfig,
    ) -> Vec<voice_pipes_sdk::ConfigError> {
        vec![]
    }
    fn build(
        &self,
        _cfg: &voice_pipes_sdk::turn::TurnConfig,
        _ctx: &voice_pipes_sdk::BuildContext<'_>,
    ) -> Result<Box<dyn voice_pipes_sdk::turn::TurnDetector>, voice_pipes_sdk::BuildError> {
        Err(voice_pipes_sdk::BuildError::failed("never built"))
    }
    fn param_schema(&self) -> Vec<voice_pipes_sdk::ParamDescriptor> {
        vec![voice_pipes_sdk::ParamDescriptor::mutable_int(
            "aliased_turn_window_ms",
            "Window",
            "A setting only this detector has.",
            0,
            1000,
        )]
    }
}

/// The console lists a detector's own settings when it is chosen by its
/// alias, as when it is chosen by its name.
#[test]
fn a_turn_detector_chosen_by_its_alias_describes_its_settings() {
    use crate::api::param_schema::ComponentParamSchema as _;
    register_plugin(Plugin::Turn(|| Box::new(AliasedTurn)), Vec::new()).unwrap();
    for mode in ["aliased-turn-under-test", "atut"] {
        let cfg: crate::config::TurnDetectionConfig =
            toml::from_str(&format!("mode = \"{mode}\"\n")).unwrap();
        let keys: Vec<String> = cfg.param_schema().into_iter().map(|d| d.key).collect();
        assert!(
            keys.iter().any(|k| k == "aliased_turn_window_ms"),
            "mode {mode}: {keys:?}"
        );
    }
}
