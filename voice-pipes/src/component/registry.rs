//! Process-wide registry of live [`Component`]s.

use std::fmt;
use std::sync::Arc;

use dashmap::DashMap;

use super::{Component, HealthState};

/// Process-wide registry of every live [`Component`].
///
/// The worker registers each provider here at startup so the `/debug/health`
/// and (future) `/debug/components` endpoints can iterate without knowing
/// the concrete provider types.
#[derive(Default, Clone)]
pub struct ComponentRegistry {
    inner: Arc<DashMap<String, Arc<dyn Component>>>,
}

impl ComponentRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register or replace the component under `component.id()`.
    pub fn register(&self, component: Arc<dyn Component>) {
        let id = component.id().to_string();
        self.inner.insert(id, component);
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn Component>> {
        self.inner.get(id).map(|e| e.clone())
    }

    pub fn remove(&self, id: &str) -> Option<Arc<dyn Component>> {
        self.inner.remove(id).map(|(_, v)| v)
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Snapshot all components into a Vec sorted by id (stable output for
    /// HTTP responses and tests).
    pub fn snapshot(&self) -> Vec<Arc<dyn Component>> {
        let mut out: Vec<_> = self
            .inner
            .iter()
            .map(|e| std::sync::Arc::clone(e.value()))
            .collect();
        out.sort_by(|a, b| a.id().cmp(b.id()));
        out
    }

    /// Aggregate worst-case health across all registered components.
    /// Returns `Healthy` when the registry is empty.
    pub fn aggregate_health(&self) -> HealthState {
        let mut worst = HealthState::Healthy;
        for entry in self.inner.iter() {
            worst = HealthState::worst(worst, entry.value().health().state);
        }
        worst
    }
}

impl fmt::Debug for ComponentRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ids: Vec<String> = self.inner.iter().map(|e| e.key().clone()).collect();
        f.debug_struct("ComponentRegistry")
            .field("ids", &ids)
            .finish()
    }
}

#[cfg(test)]
#[path = "tests/registry.rs"]
mod tests;
