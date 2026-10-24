use super::*;

/// Turn detectors are listed under their own kind, the built-in modes with
/// them, and one can be inspected by name.
#[test]
fn turn_detectors_are_a_kind_of_their_own() {
    assert_eq!(InspectKind::parse("turn").unwrap(), InspectKind::Turn);
    assert_eq!(
        InspectKind::parse("turn_detection").unwrap(),
        InspectKind::Turn
    );
    let rows = collect_rows(InspectKind::Turn);
    let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
    for mode in ["silence", "provider", "layered"] {
        assert!(names.contains(&mode), "{names:?}");
    }
    assert!(rows.iter().all(|r| r.kind == "turn"));

    let details = lookup_provider_details("layered", InspectKind::Turn).expect("found");
    assert_eq!(details.len(), 1);
    let detail = &details[0];
    assert_eq!(detail.kind, "turn");
    // A turn mode is not found under another kind.
    assert!(lookup_provider_details("layered", InspectKind::Stt).is_err());
}

/// The speech-to-speech kind parses under both its names; with no plugin
/// registered in the engine's own tests, it lists nothing.
#[test]
fn realtime_is_a_kind_and_lists_only_registered_providers() {
    assert_eq!(
        InspectKind::parse("realtime").unwrap(),
        InspectKind::Realtime
    );
    assert_eq!(InspectKind::parse("s2s").unwrap(), InspectKind::Realtime);
    assert!(
        collect_rows(InspectKind::Realtime)
            .iter()
            .all(|r| r.kind == "realtime" && !r.is_local)
    );
    let err = InspectKind::parse("nope").unwrap_err().to_string();
    assert!(err.contains("turn, realtime"), "{err}");
}

/// A name several stages share is shown for each of them, not only the
/// first stage searched.
#[test]
fn a_name_in_two_stages_is_shown_for_both() {
    const NAME: &str = "inspect-two-stages-test";
    struct Text;
    impl crate::text::TextProcessorFactory for Text {
        fn name(&self) -> &'static str {
            NAME
        }
        fn validate(
            &self,
            _cfg: &crate::config::TextProcessorConfig,
        ) -> Vec<crate::config::ConfigError> {
            vec![]
        }
        fn build(
            &self,
            _cfg: &crate::config::TextProcessorConfig,
            _ctx: &voice_pipes_sdk::BuildContext<'_>,
        ) -> Result<Box<dyn crate::text::TextProcessor>, voice_pipes_sdk::BuildError> {
            Err(voice_pipes_sdk::BuildError::failed("never built"))
        }
    }
    struct Vad;
    impl voice_pipes_sdk::vad::VadFactory for Vad {
        fn name(&self) -> &str {
            NAME
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
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        crate::text::register_text_processor_factory(|| Box::new(Text)).expect("free name");
        crate::vad::segmenter::register_vad_factory(|| Box::new(Vad)).expect("free name");
    });
    let kinds: Vec<&str> = lookup_provider_details(NAME, InspectKind::All)
        .expect("found")
        .iter()
        .map(|d| d.kind)
        .collect();
    assert_eq!(kinds, ["text", "vad"]);
}

/// A provider looked up by name reports its other names, whatever its
/// stage, as the listing does.
#[test]
fn a_providers_details_carry_its_aliases() {
    struct Nicknamed;
    impl voice_pipes_sdk::turn::TurnDetectorFactory for Nicknamed {
        fn name(&self) -> &'static str {
            "nicknamed-turn-under-test"
        }
        fn aliases(&self) -> &[&str] {
            &["nntut"]
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
        ) -> Result<Box<dyn voice_pipes_sdk::turn::TurnDetector>, voice_pipes_sdk::BuildError>
        {
            Err(voice_pipes_sdk::BuildError::failed("never built"))
        }
    }
    crate::plugins::register_plugin(
        crate::plugins::Plugin::Turn(|| Box::new(Nicknamed)),
        Vec::new(),
    )
    .unwrap();
    let details =
        lookup_provider_details("nicknamed-turn-under-test", InspectKind::Turn).expect("found");
    assert_eq!(details[0].aliases, vec!["nntut".to_string()]);
}
