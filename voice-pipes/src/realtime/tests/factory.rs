use super::{RealtimeFactory, lookup_realtime_factory};
use voice_pipes_sdk::s2s::RealtimeConfig;

fn cfg(provider: &str) -> RealtimeConfig {
    RealtimeConfig::new(provider)
}

/// A provider registered from outside the engine is found by name and by
/// alias, and a second provider claiming one of those names is refused.
#[test]
fn a_registered_provider_is_found_and_cannot_be_shadowed() {
    use crate::realtime::tests::mock::MockRealtimeProtocol;
    use voice_pipes_sdk::s2s::RealtimeProtocol;

    struct Mock;
    impl RealtimeFactory for Mock {
        fn name(&self) -> &'static str {
            "registry-test-s2s"
        }
        fn aliases(&self) -> &[&str] {
            &["registry-test-alias"]
        }
        fn default_api_key_env(&self) -> &'static str {
            "REGISTRY_TEST_KEY"
        }
        fn validate(&self, _cfg: &RealtimeConfig) -> Vec<crate::config::ConfigError> {
            vec![]
        }
        fn build(
            &self,
            _cfg: &RealtimeConfig,
            _ctx: &voice_pipes_sdk::BuildContext<'_>,
        ) -> Result<Box<dyn RealtimeProtocol>, voice_pipes_sdk::BuildError> {
            Ok(Box::new(MockRealtimeProtocol::new("ws://127.0.0.1:1")))
        }
    }
    struct Shadow;
    impl RealtimeFactory for Shadow {
        fn name(&self) -> &'static str {
            "registry-test-shadow"
        }
        fn aliases(&self) -> &[&str] {
            &["registry-test-alias"]
        }
        fn default_api_key_env(&self) -> &'static str {
            "REGISTRY_TEST_KEY"
        }
        fn validate(&self, _cfg: &RealtimeConfig) -> Vec<crate::config::ConfigError> {
            vec![]
        }
        fn build(
            &self,
            _cfg: &RealtimeConfig,
            _ctx: &voice_pipes_sdk::BuildContext<'_>,
        ) -> Result<Box<dyn RealtimeProtocol>, voice_pipes_sdk::BuildError> {
            Err(voice_pipes_sdk::BuildError::failed("never built"))
        }
    }

    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        crate::realtime::register_realtime_factory(|| Box::new(Mock)).expect("free name");
    });
    for name in ["registry-test-s2s", "registry-test-alias"] {
        let f = lookup_realtime_factory(name).expect("registered");
        assert_eq!(f.name(), "registry-test-s2s");
        assert_eq!(
            f.build(&cfg(name), &voice_pipes_sdk::BuildContext::new())
                .expect("build")
                .name(),
            "mock"
        );
    }
    let err = crate::realtime::register_realtime_factory(|| Box::new(Shadow)).unwrap_err();
    assert!(err.to_string().contains("\"registry-test-alias\""), "{err}");
}
