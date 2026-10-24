use super::{ErrorAction, ErrorPolicy, classify_streaming_error};
use crate::component::{Component, ComponentKind, ComponentTelemetry, HealthStatus};

#[test]
fn error_policy_default_is_propagate() {
    let p = ErrorPolicy::default();
    assert!(matches!(p, ErrorPolicy::Propagate));
}

/// Test stub that lets us inject any [`ErrorPolicy`] without touching
/// real provider code.
struct PolicyStub {
    id: String,
    policy: ErrorPolicy,
}
impl Component for PolicyStub {
    fn id(&self) -> &str {
        &self.id
    }
    fn kind(&self) -> ComponentKind {
        ComponentKind::Other
    }
    fn health(&self) -> HealthStatus {
        HealthStatus::healthy()
    }
    fn telemetry(&self) -> ComponentTelemetry {
        ComponentTelemetry::default()
    }
    fn error_policy(&self) -> ErrorPolicy {
        self.policy.clone()
    }
}

#[test]
fn classify_propagate_when_policy_is_default() {
    let c = PolicyStub {
        id: "stt:x".into(),
        policy: ErrorPolicy::Propagate,
    };
    assert_eq!(
        classify_streaming_error(Some(&c as &dyn Component), "stt"),
        ErrorAction::Propagate
    );
}

#[test]
fn classify_propagate_when_no_component() {
    // Providers that don't expose a Component get Propagate.
    assert_eq!(
        classify_streaming_error(None, "tts"),
        ErrorAction::Propagate
    );
}

#[test]
fn classify_suppress_for_log_and_continue() {
    let c = PolicyStub {
        id: "text:llm".into(),
        policy: ErrorPolicy::LogAndContinue,
    };
    assert_eq!(
        classify_streaming_error(Some(&c as &dyn Component), "text"),
        ErrorAction::Suppress
    );
}

#[test]
fn classify_retry_returns_retry_action_for_tts() {
    let c = PolicyStub {
        id: "tts:flaky".into(),
        policy: ErrorPolicy::RetryWithBackoff { max: 3 },
    };
    assert_eq!(
        classify_streaming_error(Some(&c as &dyn Component), "tts"),
        ErrorAction::Retry { max_attempts: 3 }
    );
}

#[test]
fn classify_retry_returns_retry_action_for_text() {
    // LLM call site honours RetryWithBackoff; classify must surface
    // Retry for the "text" kind too.
    let c = PolicyStub {
        id: "text:llm".into(),
        policy: ErrorPolicy::RetryWithBackoff { max: 2 },
    };
    assert_eq!(
        classify_streaming_error(Some(&c as &dyn Component), "text"),
        ErrorAction::Retry { max_attempts: 2 }
    );
}

#[test]
fn classify_retry_falls_through_to_propagate_for_stt() {
    // STT cannot safely replay a partial bidirectional stream; keep
    // it on Propagate even when the policy is RetryWithBackoff.
    let c = PolicyStub {
        id: "stt:whisper".into(),
        policy: ErrorPolicy::RetryWithBackoff { max: 3 },
    };
    assert_eq!(
        classify_streaming_error(Some(&c as &dyn Component), "stt"),
        ErrorAction::Propagate
    );
}
