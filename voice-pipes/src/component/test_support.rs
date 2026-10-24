//! Shared test helpers for the component submodules.

use std::sync::Arc;

use super::{Component, ComponentCore, ComponentKind, ComponentTelemetry, HealthStatus};

/// A [`Component`] backed entirely by a [`ComponentCore`]. The wrapped core is
/// public so tests can drive it (`record_ok`, `mark_degraded`, ...).
pub(crate) struct CoreBacked(pub ComponentCore);

impl Component for CoreBacked {
    fn id(&self) -> &str {
        self.0.id()
    }
    fn kind(&self) -> ComponentKind {
        self.0.kind()
    }
    fn health(&self) -> HealthStatus {
        self.0.health()
    }
    fn telemetry(&self) -> ComponentTelemetry {
        self.0.telemetry()
    }
    fn set_health(&self, status: HealthStatus) {
        self.0.set_health(status);
    }
}

pub(crate) fn comp(id: &str, kind: ComponentKind) -> Arc<CoreBacked> {
    Arc::new(CoreBacked(ComponentCore::new(id, kind)))
}
